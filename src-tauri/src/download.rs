use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::future::{select, Either};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

type DownloadError = Box<dyn std::error::Error + Send + Sync>;

/// Error string returned to the frontend when the user pauses a download.
pub const DOWNLOAD_CANCELLED: &str = "download_cancelled";

/// Returned when a download stops because its [`CancelToken`] fired. The
/// partial file is kept so the next attempt can resume from it.
#[derive(Debug)]
pub struct DownloadCancelled;

impl std::fmt::Display for DownloadCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(DOWNLOAD_CANCELLED)
    }
}

impl std::error::Error for DownloadCancelled {}

pub fn is_cancelled(error: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    error.downcast_ref::<DownloadCancelled>().is_some()
}

/// Cooperative cancellation shared between a running download and the command
/// that pauses it. Also wakes a download that is stalled waiting on the network.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub async fn cancelled(&self) {
        let mut notified = std::pin::pin!(self.0.notify.notified());
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// Allows one download of a given artifact at a time and lets another command
/// pause it. Two writers on the same `.part` file would corrupt it.
pub struct DownloadSlot(Mutex<Option<CancelToken>>);

impl DownloadSlot {
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub fn begin(&'static self) -> Result<DownloadGuard, String> {
        let mut active = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return Err("This download is already running".into());
        }
        let token = CancelToken::default();
        *active = Some(token.clone());
        Ok(DownloadGuard { slot: self, token })
    }

    /// Pause the running download, if any. Returns whether one was running.
    pub fn cancel(&self) -> bool {
        let active = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(token) = active.as_ref() {
            token.cancel();
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn is_active(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

pub struct DownloadGuard {
    slot: &'static DownloadSlot,
    token: CancelToken,
}

impl DownloadGuard {
    pub fn token(&self) -> &CancelToken {
        &self.token
    }
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        *self.slot.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub async fn download_verified(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_sha256: &str,
    progress: impl Fn(u64, u64),
) -> Result<u64, DownloadError> {
    validate_expected_hash(expected_sha256)?;
    let _ = std::fs::remove_file(partial_path(destination));
    download_to_partial(client, url, destination, expected_sha256, &progress, None).await
}

/// Like [`download_verified`], but continues from an existing `.part` file with
/// an HTTP range request and stops early (keeping the partial) when `cancel` fires.
pub async fn download_verified_resumable(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_sha256: &str,
    progress: impl Fn(u64, u64),
    cancel: &CancelToken,
) -> Result<u64, DownloadError> {
    validate_expected_hash(expected_sha256)?;
    let result = download_to_partial(
        client,
        url,
        destination,
        expected_sha256,
        &progress,
        Some(cancel),
    )
    .await;
    match result {
        Err(error) if error.downcast_ref::<StalePartialDownload>().is_some() => {
            // The partial did not belong to the published file (for example it
            // was left over from an older artifact). Start over once.
            log::warn!("[Download] Discarding stale partial for {url}: {error}");
            let _ = std::fs::remove_file(partial_path(destination));
            download_to_partial(
                client,
                url,
                destination,
                expected_sha256,
                &progress,
                Some(cancel),
            )
            .await
        }
        result => result,
    }
}

/// Bytes already on disk from an interrupted download of `destination`.
pub fn partial_bytes(destination: &Path) -> u64 {
    std::fs::metadata(partial_path(destination))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

#[derive(Debug)]
struct StalePartialDownload(String);

impl std::fmt::Display for StalePartialDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StalePartialDownload {}

async fn download_to_partial(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected_sha256: &str,
    progress: &impl Fn(u64, u64),
    cancel: Option<&CancelToken>,
) -> Result<u64, DownloadError> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let partial = partial_path(destination);
    let resume_from = partial_bytes(destination);

    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }
    let response = until_cancelled(cancel, request.send()).await??;
    let status = response.status();

    if resume_from > 0 && status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // The partial already holds every byte the server has.
        drop(response);
        progress(resume_from, resume_from);
        let hasher = hash_existing(&partial).await?;
        verify_hash(hasher, expected_sha256, url, &partial, true)?;
        replace_destination(&partial, destination)?;
        return Ok(resume_from);
    }
    if !status.is_success() {
        return Err(format!("Download failed: HTTP {status} for {url}").into());
    }

    let resume_range = (resume_from > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT)
        .then(|| content_range(response.headers()))
        .flatten()
        .filter(|(start, _)| *start == resume_from);

    let (mut file, mut hasher, mut downloaded, total) = if let Some((_, total)) = resume_range {
        let total = total.unwrap_or_else(|| resume_from + response.content_length().unwrap_or(0));
        let hasher = hash_existing(&partial).await?;
        let file = std::fs::OpenOptions::new().append(true).open(&partial)?;
        (file, hasher, resume_from, total)
    } else {
        // Fresh download, or the server ignored the range request.
        let total = response.content_length().unwrap_or(0);
        (std::fs::File::create(&partial)?, Sha256::new(), 0, total)
    };
    let resumed = resume_range.is_some();
    progress(downloaded, total);

    let mut stream = response.bytes_stream();
    loop {
        let next = match until_cancelled(cancel, stream.next()).await {
            Ok(next) => next,
            Err(cancelled) => {
                // Keep what we have so the next attempt resumes from here.
                file.sync_all()?;
                return Err(cancelled.into());
            }
        };
        let Some(chunk) = next else { break };
        let chunk = chunk?;
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        downloaded += chunk.len() as u64;
        progress(downloaded, total);
    }
    file.sync_all()?;
    drop(file);

    verify_hash(hasher, expected_sha256, url, &partial, resumed)?;
    replace_destination(&partial, destination)?;
    Ok(downloaded)
}

fn verify_hash(
    hasher: Sha256,
    expected_sha256: &str,
    url: &str,
    partial: &Path,
    resumed: bool,
) -> Result<(), DownloadError> {
    let actual = format!("{:x}", hasher.finalize());
    if actual.eq_ignore_ascii_case(expected_sha256) {
        return Ok(());
    }

    let _ = std::fs::remove_file(partial);
    let message =
        format!("SHA-256 mismatch for {url}: expected {expected_sha256}, received {actual}");
    if resumed {
        Err(Box::new(StalePartialDownload(message)))
    } else {
        Err(message.into())
    }
}

fn replace_destination(partial: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(partial, destination)
}

/// Await `future`, or return [`DownloadCancelled`] as soon as `cancel` fires.
async fn until_cancelled<T>(
    cancel: Option<&CancelToken>,
    future: impl std::future::Future<Output = T>,
) -> Result<T, DownloadCancelled> {
    let Some(cancel) = cancel else {
        return Ok(future.await);
    };
    if cancel.is_cancelled() {
        return Err(DownloadCancelled);
    }
    let future = std::pin::pin!(future);
    let cancelled = std::pin::pin!(cancel.cancelled());
    match select(future, cancelled).await {
        Either::Left((output, _)) => Ok(output),
        Either::Right(_) => Err(DownloadCancelled),
    }
}

/// Hash the bytes already downloaded so a resumed download still verifies the
/// whole file. Runs off the async workers because partials can be hundreds of MB.
async fn hash_existing(path: &Path) -> Result<Sha256, DownloadError> {
    let path = path.to_path_buf();
    let hasher = tokio::task::spawn_blocking(move || -> std::io::Result<Sha256> {
        let mut hasher = Sha256::new();
        let mut file = std::fs::File::open(&path)?;
        std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
        Ok(hasher)
    })
    .await??;
    Ok(hasher)
}

/// Parse `Content-Range: bytes <start>-<end>/<total>`; total is `None` for `*`.
fn content_range(headers: &reqwest::header::HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    parse_content_range(value)
}

fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let range = value.trim().strip_prefix("bytes ")?;
    let (span, total) = range.split_once('/')?;
    let (start, _end) = span.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let total = total.trim().parse().ok();
    Some((start, total))
}

pub fn verify_file_sha256(path: &Path, expected_sha256: &str) -> bool {
    sha256_file(path)
        .map(|actual| actual.eq_ignore_ascii_case(expected_sha256))
        .unwrap_or(false)
}

pub fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
    Ok(format!("{:x}", hasher.finalize()))
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn validate_expected_hash(expected_sha256: &str) -> Result<(), String> {
    if expected_sha256.len() == 64 && expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("Expected SHA-256 must be 64 hexadecimal characters".into())
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "download".into());
    name.push(".part");
    destination.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_a_known_sha256() {
        let path = std::env::temp_dir().join(format!("annotate-sha256-{}.txt", std::process::id()));
        std::fs::write(&path, b"annotate").unwrap();
        assert!(verify_file_sha256(
            &path,
            "9c299759567c9c3963748e57fb44cc7f676b69219e955a0d74a3bebb3607c78d"
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parses_content_range_headers() {
        assert_eq!(
            parse_content_range("bytes 100-999/1000"),
            Some((100, Some(1000)))
        );
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range("bytes oops"), None);
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn test_body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    /// A scratch directory for one test, removed when the test ends.
    struct TempDestination(PathBuf);

    impl std::ops::Deref for TempDestination {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDestination {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn temp_destination(name: &str) -> TempDestination {
        let dir =
            std::env::temp_dir().join(format!("annotate-download-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDestination(dir.join("model.bin"))
    }

    /// Minimal HTTP server for one file. Honors `Range: bytes=N-` when asked
    /// and can stall partway through the body to exercise cancellation.
    fn serve(body: Vec<u8>, honor_range: bool, stall_after: Option<usize>) -> String {
        use std::io::Read;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let start = request
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .and_then(|range| range.trim().trim_end_matches('-').parse::<usize>().ok())
                    .filter(|_| honor_range);

                let (status, slice, content_range) = match start {
                    Some(start) if start >= body.len() => (
                        "416 Range Not Satisfiable",
                        &body[0..0],
                        format!("Content-Range: bytes */{}\r\n", body.len()),
                    ),
                    Some(start) => (
                        "206 Partial Content",
                        &body[start..],
                        format!(
                            "Content-Range: bytes {}-{}/{}\r\n",
                            start,
                            body.len() - 1,
                            body.len()
                        ),
                    ),
                    None => ("200 OK", &body[..], String::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{content_range}Connection: close\r\n\r\n",
                    slice.len()
                );
                let _ = stream.write_all(head.as_bytes());
                match stall_after {
                    Some(limit) if limit < slice.len() => {
                        let _ = stream.write_all(&slice[..limit]);
                        let _ = stream.flush();
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                    _ => {
                        let _ = stream.write_all(slice);
                    }
                }
            }
        });
        url
    }

    fn resume(url: &str, destination: &Path, body: &[u8]) -> Result<u64, DownloadError> {
        block_on(download_verified_resumable(
            &reqwest::Client::new(),
            url,
            destination,
            &sha256_hex(body),
            |_, _| {},
            &CancelToken::default(),
        ))
    }

    #[test]
    fn resumes_from_a_partial_file_with_a_range_request() {
        let body = test_body();
        let url = serve(body.clone(), true, None);
        let destination = temp_destination("resume");
        std::fs::write(partial_path(&destination), &body[..75_000]).unwrap();

        let first_progress = Mutex::new(None);
        let written = block_on(download_verified_resumable(
            &reqwest::Client::new(),
            &url,
            &destination,
            &sha256_hex(&body),
            |done, total| {
                first_progress.lock().unwrap().get_or_insert((done, total));
            },
            &CancelToken::default(),
        ))
        .unwrap();

        assert_eq!(written, body.len() as u64);
        assert_eq!(std::fs::read(&*destination).unwrap(), body);
        assert_eq!(partial_bytes(&destination), 0);
        assert_eq!(
            *first_progress.lock().unwrap(),
            Some((75_000, body.len() as u64)),
            "progress starts from the bytes already on disk"
        );
    }

    #[test]
    fn starts_over_when_the_partial_is_stale_or_ranges_are_ignored() {
        let body = test_body();

        let stale = temp_destination("stale");
        std::fs::write(partial_path(&stale), vec![0xAB; 50_000]).unwrap();
        resume(&serve(body.clone(), true, None), &stale, &body).unwrap();
        assert_eq!(std::fs::read(&*stale).unwrap(), body);

        let no_range = temp_destination("no-range");
        std::fs::write(partial_path(&no_range), &body[..50_000]).unwrap();
        resume(&serve(body.clone(), false, None), &no_range, &body).unwrap();
        assert_eq!(std::fs::read(&*no_range).unwrap(), body);
    }

    #[test]
    fn installs_a_partial_that_is_already_complete() {
        let body = test_body();
        let destination = temp_destination("complete");
        std::fs::write(partial_path(&destination), &body).unwrap();

        resume(&serve(body.clone(), true, None), &destination, &body).unwrap();
        assert_eq!(std::fs::read(&*destination).unwrap(), body);
        assert_eq!(partial_bytes(&destination), 0);
    }

    #[test]
    fn cancelling_a_stalled_download_keeps_the_partial_for_later() {
        let body = test_body();
        let destination = temp_destination("cancel");
        let url = serve(body.clone(), true, Some(60_000));

        let cancel = CancelToken::default();
        let canceller = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            canceller.cancel();
        });

        let started = std::time::Instant::now();
        let error = block_on(download_verified_resumable(
            &reqwest::Client::new(),
            &url,
            &destination,
            &sha256_hex(&body),
            |_, _| {},
            &cancel,
        ))
        .unwrap_err();

        assert!(is_cancelled(error.as_ref()), "unexpected error: {error}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "cancel woke the stalled read"
        );
        assert_eq!(partial_bytes(&destination), 60_000);
        assert!(!destination.exists());

        resume(&serve(body.clone(), true, None), &destination, &body).unwrap();
        assert_eq!(std::fs::read(&*destination).unwrap(), body);
    }

    #[test]
    fn download_slot_allows_one_download_and_clears_on_drop() {
        static SLOT: DownloadSlot = DownloadSlot::new();
        assert!(!SLOT.cancel());

        let guard = SLOT.begin().expect("first download starts");
        assert!(SLOT.is_active());
        assert!(SLOT.begin().is_err(), "second download is rejected");
        assert!(SLOT.cancel());
        assert!(guard.token().is_cancelled());

        drop(guard);
        assert!(!SLOT.is_active());
        assert!(!SLOT.begin().unwrap().token().is_cancelled());
    }
}
