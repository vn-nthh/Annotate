//! Local whisper transcription via a child worker process.
//!
//! The whisper.cpp GPU code lives in the Vulkan worker executable.
//!
//! This module spawns the selected process on-demand and communicates via
//! stdin/stdout JSON. When unloaded, the process is killed and ALL GPU memory
//! is freed by the OS.

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    LazyLock,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// Model download URL — Cloudflare R2 (backed up from ggerganov/whisper.cpp HuggingFace)
const MODEL_URL: &str =
    "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/ggml-large-v3-turbo-q5_0.bin";
const MODEL_FILENAME: &str = "ggml-large-v3-turbo-q5_0.bin";
const MODEL_SHA256: &str = "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2";
/// Exact byte size of the published model (used for a fast presence check).
const MODEL_SIZE_BYTES: u64 = 574_041_195;
const MODEL_VERIFIED_MARKER: &str = "ggml-large-v3-turbo-q5_0.bin.verified";

// ── Acceleration backend ───────────────────────────────

/// GPU acceleration backend for local Whisper inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelerationBackend {
    Vulkan,
}

impl AccelerationBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vulkan => "vulkan",
        }
    }

    pub fn parse(value: Option<&str>) -> Self {
        let _ = value; // Includes older persisted acceleration selections.
        Self::Vulkan
    }

    fn worker_bin_stem(self) -> &'static str {
        match self {
            Self::Vulkan => "whisper-worker-vulkan",
        }
    }
}

// ── Worker Process State ──────────────────────────────

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout_reader: BufReader<ChildStdout>,
    backend: AccelerationBackend,
    #[cfg(windows)]
    _job: WorkerJob,
}

/// The OS closes this handle on app exit, including crashes and active inference.
#[cfg(windows)]
struct WorkerJob(usize);

#[cfg(windows)]
impl WorkerJob {
    fn attach(child: &Child) -> Result<Self, String> {
        use windows::Win32::{Foundation::HANDLE, System::JobObjects::*};
        unsafe {
            let handle = CreateJobObjectW(None, None).map_err(|e| e.to_string())?;
            let job = Self(handle.0 as usize);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
            .map_err(|e| format!("Could not protect the Whisper worker: {e}"))?;
            let process = child.raw_handle().ok_or("Whisper worker already exited")?;
            AssignProcessToJobObject(handle, HANDLE(process)).map_err(|e| {
                format!("Could not assign the Whisper worker to its process job: {e}")
            })?;
            Ok(job)
        }
    }
}

#[cfg(windows)]
impl Drop for WorkerJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(
                self.0 as *mut _,
            ));
        }
    }
}

static WORKER: LazyLock<Mutex<Option<WorkerProcess>>> = LazyLock::new(|| Mutex::new(None));

/// Flag tracking whether the model is loaded in the worker.
static MODEL_LOADED: AtomicBool = AtomicBool::new(false);

// ── Path Helpers ──────────────────────────────────────

/// Return the path where the model should live.
pub fn model_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(MODEL_FILENAME)
}

fn model_verified_marker(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(MODEL_VERIFIED_MARKER)
}

fn write_model_verified_marker(data_dir: &std::path::Path) {
    if let Err(err) = std::fs::write(model_verified_marker(data_dir), MODEL_SHA256) {
        log::warn!("[WhisperLocal] Failed to write model verified marker: {err}");
    }
}

/// Check whether the model file is present and previously verified.
///
/// Avoids re-hashing the ~574 MB weights on every settings/status check (that
/// full SHA-256 scan was a major startup freeze, especially next to Vulkan init).
/// Integrity is enforced at download time; day-to-day checks only use size + marker.
pub fn is_model_downloaded(data_dir: &std::path::Path) -> bool {
    let path = model_path(data_dir);
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    // Partial / wrong artifact — do not scan hundreds of MB on the UI path.
    if meta.len() != MODEL_SIZE_BYTES {
        return false;
    }

    let marker = model_verified_marker(data_dir);
    if std::fs::read_to_string(&marker)
        .map(|hash| hash.trim().eq_ignore_ascii_case(MODEL_SHA256))
        .unwrap_or(false)
    {
        return true;
    }

    // Migration path for installs that predate the marker file: size matches the
    // known good artifact, so trust it and write the marker without re-hashing.
    write_model_verified_marker(data_dir);
    true
}

/// Check if the worker is currently running and model loaded.
pub fn is_loaded() -> bool {
    MODEL_LOADED.load(Ordering::Acquire)
}

/// Download the model file.
pub async fn download_model(
    data_dir: &std::path::Path,
    progress_cb: impl Fn(u64, u64) + Send + 'static,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let dest = model_path(data_dir);
    log::info!("[WhisperLocal] Downloading model to {:?}", dest);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;

    let downloaded =
        crate::download::download_verified(&client, MODEL_URL, &dest, MODEL_SHA256, progress_cb)
            .await?;

    write_model_verified_marker(data_dir);
    log::info!("[WhisperLocal] Download complete: {} bytes", downloaded);
    Ok(dest)
}

// ── Worker Process Management ──────────────────────────

/// Find the whisper worker executable for the given backend next to the main app.
fn worker_exe_path(backend: AccelerationBackend) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    let stem = backend.worker_bin_stem();
    let name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    dir.join(name)
}

fn clear_loaded_flags() {
    MODEL_LOADED.store(false, Ordering::Release);
}

fn mark_loaded() {
    MODEL_LOADED.store(true, Ordering::Release);
}

/// Spawn the worker process for `backend` and load the model.
pub async fn ensure_loaded(
    data_dir: &std::path::Path,
    backend: AccelerationBackend,
) -> Result<(), String> {
    // Hold the same lock through startup so simultaneous requests share one model.
    let mut guard = WORKER.lock().await;
    if let Some(worker) = guard.as_mut() {
        match worker.child.try_wait() {
            Ok(None) => {
                mark_loaded();
                return Ok(());
            }
            Ok(Some(status)) => {
                log::warn!(
                    "[WhisperLocal] Existing worker exited ({:?}); respawning",
                    status
                );
                *guard = None;
                clear_loaded_flags();
            }
            Err(err) => {
                log::warn!(
                    "[WhisperLocal] Failed to query worker state ({}); respawning",
                    err
                );
                *guard = None;
                clear_loaded_flags();
            }
        }
    }

    clear_loaded_flags();

    let worker_path = worker_exe_path(backend);
    if !worker_path.exists() {
        return Err(format!(
            "Whisper {} worker not found at: {}. Run scripts/build-native.ps1.",
            backend.as_str(),
            worker_path.display()
        ));
    }

    let model = model_path(data_dir);
    if !model.exists() {
        return Err("Model not downloaded yet".into());
    }

    log::info!(
        "[WhisperLocal] Spawning {} worker: {:?}",
        backend.as_str(),
        worker_path
    );
    let start = std::time::Instant::now();

    let mut cmd = tokio::process::Command::new(&worker_path);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);

    // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS keeps loading below UI work.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000 | 0x0000_4000);

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to spawn whisper-worker ({}): {}",
            backend.as_str(),
            e
        )
    })?;

    #[cfg(windows)]
    let job = WorkerJob::attach(&child)?;

    let stdin = child.stdin.take().ok_or("No stdin for worker")?;
    let stdout = child.stdout.take().ok_or("No stdout for worker")?;
    let stdout_reader = BufReader::new(stdout);

    let mut worker = WorkerProcess {
        child,
        stdin,
        stdout_reader,
        backend,
        #[cfg(windows)]
        _job: job,
    };

    // Send load command
    let data_dir_str = data_dir.to_string_lossy().to_string();
    let load_cmd = serde_json::json!({"cmd": "load", "data_dir": data_dir_str});
    let resp = send_command(&mut worker, &load_cmd).await?;

    if resp["ok"].as_bool() != Some(true) {
        let err = resp["error"].as_str().unwrap_or("Unknown error");
        // Kill the worker on failure
        let _ = worker.child.kill().await;
        return Err(format!(
            "Worker load failed ({}): {}",
            backend.as_str(),
            err
        ));
    }

    log::info!(
        "[WhisperLocal] {} worker ready in {:.1}s",
        backend.as_str(),
        start.elapsed().as_secs_f64()
    );

    *guard = Some(worker);
    mark_loaded();

    Ok(())
}

/// Unload: kill the worker process, freeing ALL GPU memory held by the sidecar.
pub async fn unload() {
    let mut guard = WORKER.lock().await;
    if let Some(mut worker) = guard.take() {
        let backend = worker.backend;
        // Try graceful quit first
        let quit_cmd = serde_json::json!({"cmd": "quit"});
        let _ = send_command_no_response(&mut worker, &quit_cmd).await;

        // Give it a moment, then force kill
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let _ = worker.child.kill().await;
        let _ = worker.child.wait().await;

        log::info!(
            "[WhisperLocal] {} worker process killed — GPU memory freed",
            backend.as_str()
        );
    }

    clear_loaded_flags();
}

/// Transcribe audio via the worker process.
pub async fn transcribe_pcm_b64(
    audio_b64: &str,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<String, String> {
    let mut guard = WORKER.lock().await;
    let worker = guard.as_mut().ok_or("Whisper worker not running")?;

    let cmd = serde_json::json!({
        "cmd": "transcribe",
        "audio_b64": audio_b64,
        "prompt": initial_prompt.unwrap_or(""),
        "language": language
    });

    let resp = match send_command(worker, &cmd).await {
        Ok(response) => response,
        Err(error) => {
            *guard = None;
            return Err(error);
        }
    };

    if resp["ok"].as_bool() != Some(true) {
        let err = resp["error"].as_str().unwrap_or("Unknown error");
        return Err(format!("Transcription failed: {}", err));
    }

    Ok(resp["text"].as_str().unwrap_or("").to_string())
}

/// Transcribe audio via the worker process, returning timestamped segments.
/// Used by the subtitle pipeline.
pub async fn transcribe_segments_b64(
    audio_b64: &str,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<Vec<crate::subtitle::WhisperSegment>, String> {
    let mut guard = WORKER.lock().await;
    let worker = guard.as_mut().ok_or("Whisper worker not running")?;

    let cmd = serde_json::json!({
        "cmd": "transcribe_segments",
        "audio_b64": audio_b64,
        "prompt": initial_prompt.unwrap_or(""),
        "language": language
    });

    let resp = match send_command(worker, &cmd).await {
        Ok(response) => response,
        Err(error) => {
            *guard = None;
            return Err(error);
        }
    };

    if resp["ok"].as_bool() != Some(true) {
        let err = resp["error"].as_str().unwrap_or("Unknown error");
        return Err(format!("Segment transcription failed: {}", err));
    }

    let segments: Vec<crate::subtitle::WhisperSegment> =
        serde_json::from_value(resp["segments"].clone())
            .map_err(|e| format!("Failed to parse segments: {}", e))?;

    Ok(segments)
}

// ── IPC Helpers ────────────────────────────────────────

async fn send_command(
    worker: &mut WorkerProcess,
    cmd: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let result = async {
        let mut msg = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
        msg.push('\n');
        worker
            .stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to worker: {e}"))?;
        worker
            .stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush worker stdin: {e}"))?;
        let mut response_line = String::new();
        worker
            .stdout_reader
            .read_line(&mut response_line)
            .await
            .map_err(|e| format!("Failed to read from worker: {e}"))?;
        serde_json::from_str(&response_line)
            .map_err(|e| format!("Invalid response from worker: {e}"))
    }
    .await;
    if result.is_err() {
        clear_loaded_flags();
        let _ = worker.child.kill().await;
    }
    result
}

async fn send_command_no_response(
    worker: &mut WorkerProcess,
    cmd: &serde_json::Value,
) -> Result<(), String> {
    let mut msg = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    msg.push('\n');

    worker
        .stdin
        .write_all(msg.as_bytes())
        .await
        .map_err(|e| format!("Failed to write to worker: {}", e))?;
    worker
        .stdin
        .flush()
        .await
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        is_model_downloaded, model_path, model_verified_marker, AccelerationBackend,
        MODEL_SIZE_BYTES,
    };

    #[test]
    fn parses_acceleration_backend() {
        assert_eq!(
            AccelerationBackend::parse(Some("vulkan")),
            AccelerationBackend::Vulkan
        );
        assert_eq!(
            AccelerationBackend::parse(Some("vk")),
            AccelerationBackend::Vulkan
        );
        assert_eq!(
            AccelerationBackend::parse(Some("cuda")),
            AccelerationBackend::Vulkan
        );
        assert_eq!(
            AccelerationBackend::parse(None),
            AccelerationBackend::Vulkan
        );
        assert_eq!(
            AccelerationBackend::parse(Some("unknown")),
            AccelerationBackend::Vulkan
        );
    }

    #[test]
    fn worker_stems_match_external_bin_names() {
        assert_eq!(
            AccelerationBackend::Vulkan.worker_bin_stem(),
            "whisper-worker-vulkan"
        );
    }

    #[cfg(windows)]
    #[test]
    fn closing_the_worker_job_terminates_a_busy_child() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut child = tokio::process::Command::new("ping.exe")
                .args(["-n", "60", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x0800_0000)
                .spawn()
                .unwrap();
            let job = super::WorkerJob::attach(&child).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert!(child.try_wait().unwrap().is_none());
            drop(job);
            tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                .await
                .expect("worker survived its job")
                .unwrap();
        });
    }

    #[cfg(windows)]
    #[test]
    fn a_crashed_worker_clears_the_loaded_status() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut child = tokio::process::Command::new("ping.exe")
                .args(["-n", "60", "127.0.0.1"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .creation_flags(0x0800_0000)
                .spawn()
                .unwrap();
            let job = super::WorkerJob::attach(&child).unwrap();
            let stdin = child.stdin.take().unwrap();
            let stdout_reader = tokio::io::BufReader::new(child.stdout.take().unwrap());
            child.kill().await.unwrap();
            let mut worker = super::WorkerProcess {
                child,
                stdin,
                stdout_reader,
                backend: AccelerationBackend::Vulkan,
                _job: job,
            };
            super::mark_loaded();
            assert!(
                super::send_command(&mut worker, &serde_json::json!({"cmd":"backend"}))
                    .await
                    .is_err()
            );
            assert!(!super::is_loaded());
        });
    }

    #[test]
    fn model_presence_uses_size_and_marker_not_full_hash() {
        let dir =
            std::env::temp_dir().join(format!("annotate-whisper-presence-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(!is_model_downloaded(&dir));

        // Wrong size → not present (tiny file, cheap hash miss).
        std::fs::write(model_path(&dir), b"too-small").unwrap();
        assert!(!is_model_downloaded(&dir));

        // Exact size without marker → accepted and marker written (migration).
        {
            use std::io::Write;
            let mut file = std::fs::File::create(model_path(&dir)).unwrap();
            file.set_len(MODEL_SIZE_BYTES).unwrap();
            file.flush().unwrap();
        }
        assert!(is_model_downloaded(&dir));
        assert!(model_verified_marker(&dir).exists());

        // Correct size is enough; a stale marker is rewritten.
        std::fs::write(model_verified_marker(&dir), "deadbeef").unwrap();
        assert!(is_model_downloaded(&dir));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
