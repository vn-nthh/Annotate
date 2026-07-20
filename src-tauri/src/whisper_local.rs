//! Local whisper transcription via a child worker process.
//!
//! The actual whisper.cpp GPU code lives in separate sidecar binaries:
//! - `whisper-worker` — CUDA backend
//! - `whisper-worker-vulkan` — Vulkan backend
//!
//! This module spawns the selected process on-demand and communicates via
//! stdin/stdout JSON. When unloaded, the process is killed and ALL GPU memory
//! is freed by the OS.

use std::path::PathBuf;
use std::sync::LazyLock;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// Model download URL — Cloudflare R2 (backed up from ggerganov/whisper.cpp HuggingFace)
const MODEL_URL: &str =
    "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/ggml-large-v3-turbo-q5_0.bin";
const MODEL_FILENAME: &str = "ggml-large-v3-turbo-q5_0.bin";
const MODEL_SHA256: &str = "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2";

// ── Acceleration backend ───────────────────────────────

/// GPU acceleration backend for local Whisper inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelerationBackend {
    Cuda,
    Vulkan,
}

impl AccelerationBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
        }
    }

    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim).unwrap_or("cuda") {
            "vulkan" | "vk" => Self::Vulkan,
            _ => Self::Cuda,
        }
    }

    fn worker_bin_stem(self) -> &'static str {
        match self {
            Self::Cuda => "whisper-worker",
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
}

static WORKER: LazyLock<Mutex<Option<WorkerProcess>>> = LazyLock::new(|| Mutex::new(None));

/// Flag tracking whether the model is loaded in the worker.
static MODEL_LOADED: LazyLock<std::sync::Mutex<bool>> =
    LazyLock::new(|| std::sync::Mutex::new(false));

/// Backend currently loaded (if any).
static LOADED_BACKEND: LazyLock<std::sync::Mutex<Option<AccelerationBackend>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

// ── Path Helpers ──────────────────────────────────────

/// Return the path where the model should live.
pub fn model_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join(MODEL_FILENAME)
}

/// Check whether the model file already exists on disk.
pub fn is_model_downloaded(data_dir: &std::path::Path) -> bool {
    crate::download::verify_file_sha256(&model_path(data_dir), MODEL_SHA256)
}

/// Check if the worker is currently running and model loaded.
pub fn is_loaded() -> bool {
    MODEL_LOADED.lock().ok().map_or(false, |g| *g)
}

/// Backend the running worker was started with, if loaded.
pub fn loaded_backend() -> Option<AccelerationBackend> {
    LOADED_BACKEND.lock().ok().and_then(|g| *g)
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

    let downloaded = crate::download::download_verified(
        &client,
        MODEL_URL,
        &dest,
        MODEL_SHA256,
        progress_cb,
    )
    .await?;

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
    if let Ok(mut loaded) = MODEL_LOADED.lock() {
        *loaded = false;
    }
    if let Ok(mut backend) = LOADED_BACKEND.lock() {
        *backend = None;
    }
}

fn mark_loaded(backend: AccelerationBackend) {
    if let Ok(mut loaded) = MODEL_LOADED.lock() {
        *loaded = true;
    }
    if let Ok(mut current) = LOADED_BACKEND.lock() {
        *current = Some(backend);
    }
}

/// Spawn the worker process for `backend` and load the model.
///
/// If a worker is already running for a different backend, it is unloaded first.
pub async fn ensure_loaded(
    data_dir: &std::path::Path,
    backend: AccelerationBackend,
) -> Result<(), String> {
    // If we already have a running worker for this backend, keep using it.
    // If it exited/crashed or was started with a different backend, clear and respawn.
    {
        let mut guard = WORKER.lock().await;
        if let Some(worker) = guard.as_mut() {
            let same_backend = worker.backend == backend;
            match worker.child.try_wait() {
                Ok(None) if same_backend => {
                    mark_loaded(backend);
                    return Ok(());
                }
                Ok(None) => {
                    log::info!(
                        "[WhisperLocal] Switching worker backend {} → {}",
                        worker.backend.as_str(),
                        backend.as_str()
                    );
                    // Drop the lock before unload() which also needs it.
                    drop(guard);
                    unload().await;
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
    }

    clear_loaded_flags();

    let worker_path = worker_exe_path(backend);
    if !worker_path.exists() {
        return Err(format!(
            "Whisper {} worker not found at: {}. Rebuild with the matching backend feature.",
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
        .stderr(std::process::Stdio::inherit()); // worker logs go to our stderr

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to spawn whisper-worker ({}): {}",
            backend.as_str(),
            e
        )
    })?;

    let stdin = child.stdin.take().ok_or("No stdin for worker")?;
    let stdout = child.stdout.take().ok_or("No stdout for worker")?;
    let stdout_reader = BufReader::new(stdout);

    let mut worker = WorkerProcess {
        child,
        stdin,
        stdout_reader,
        backend,
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

    *WORKER.lock().await = Some(worker);
    mark_loaded(backend);

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
) -> Result<String, String> {
    let mut guard = WORKER.lock().await;
    let worker = guard.as_mut().ok_or("Whisper worker not running")?;

    let cmd = serde_json::json!({
        "cmd": "transcribe",
        "audio_b64": audio_b64,
        "prompt": initial_prompt.unwrap_or("")
    });

    let resp = send_command(worker, &cmd).await?;

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

    let resp = send_command(worker, &cmd).await?;

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
        .map_err(|e| format!("Failed to flush worker stdin: {}", e))?;

    let mut response_line = String::new();
    worker
        .stdout_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| format!("Failed to read from worker: {}", e))?;

    serde_json::from_str(&response_line)
        .map_err(|e| format!("Invalid response from worker: {} (raw: {})", e, response_line))
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

// ── CUDA Runtime DLL Management ────────────────────────

/// Base DLL names — the `{VER}` placeholder is replaced with the CUDA major version (e.g. 12, 13).
const CUDA_DLL_TEMPLATES: &[&str] = &["cublas64_{VER}.dll", "cublasLt64_{VER}.dll", "cudart64_{VER}.dll"];

/// NVIDIA redistribution package URLs (CUDA 12.6 — used as fallback download)
const CUDART_REDIST_URL: &str =
    "https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.6.77-archive.zip";
const CUDART_REDIST_SHA256: &str =
    "7a313bc0c93b1a50bb03aa9783a199ae70c3b66e2d8084da65e8254a8577b925";
const CUBLAS_REDIST_URL: &str =
    "https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.6.4.1-archive.zip";
const CUBLAS_REDIST_SHA256: &str =
    "1a87ec80f8c0e5a39badc87010d479930c5b63abd788b3a05bd688a5980a3d07";

/// Return the directory where the executable lives.
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("."))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

/// Detect the CUDA major version from the CUDA_PATH environment variable.
fn detect_cuda_major_version() -> Option<u32> {
    let cuda_path = std::env::var("CUDA_PATH").ok()?;
    let path = PathBuf::from(&cuda_path);

    if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
        let version_str = dir_name.strip_prefix('v').unwrap_or(dir_name);
        if let Some(major_str) = version_str.split('.').next() {
            if let Ok(major) = major_str.parse::<u32>() {
                return Some(major);
            }
        }
    }

    for sub in &["bin", "bin/x64"] {
        let search_dir = path.join(sub);
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("cublas64_") && name.ends_with(".dll") {
                    let ver_part = name
                        .strip_prefix("cublas64_")
                        .and_then(|s| s.strip_suffix(".dll"));
                    if let Some(ver) = ver_part.and_then(|v| v.parse::<u32>().ok()) {
                        return Some(ver);
                    }
                }
            }
        }
    }

    None
}

/// Get the concrete DLL filenames for the detected CUDA version.
fn cuda_dll_names() -> Vec<String> {
    let ver = detect_cuda_major_version().unwrap_or(12);
    CUDA_DLL_TEMPLATES
        .iter()
        .map(|tpl| tpl.replace("{VER}", &ver.to_string()))
        .collect()
}

/// Check which CUDA DLLs are missing.
pub fn missing_cuda_dlls() -> Vec<String> {
    let dlls = cuda_dll_names();
    let exe = exe_dir();
    let cuda_path = std::env::var("CUDA_PATH").ok().map(PathBuf::from);
    let cuda_bin = cuda_path.as_ref().map(|p| p.join("bin"));
    let cuda_bin_x64 = cuda_path.as_ref().map(|p| p.join("bin").join("x64"));

    dlls.into_iter()
        .filter(|dll| {
            let in_exe_dir = exe.join(dll).exists();
            let in_cuda_bin = cuda_bin.as_ref().map(|b| b.join(dll).exists()).unwrap_or(false);
            let in_cuda_bin_x64 = cuda_bin_x64.as_ref().map(|b| b.join(dll).exists()).unwrap_or(false);
            !in_exe_dir && !in_cuda_bin && !in_cuda_bin_x64
        })
        .collect()
}

/// Check whether all CUDA DLLs are available.
pub fn are_cuda_dlls_available() -> bool {
    missing_cuda_dlls().is_empty()
}

/// Try to copy CUDA DLLs from installed CUDA Toolkit.
pub fn copy_cuda_from_toolkit() -> Result<usize, String> {
    let cuda_path = std::env::var("CUDA_PATH")
        .map_err(|_| "CUDA_PATH environment variable not set".to_string())?;

    let cuda_bin = PathBuf::from(&cuda_path).join("bin");
    let cuda_bin_x64 = cuda_bin.join("x64");

    if !cuda_bin.exists() {
        return Err(format!("CUDA bin directory not found: {}", cuda_bin.display()));
    }

    let dest_dir = exe_dir();
    let mut copied = 0;

    for dll in cuda_dll_names() {
        let dst = dest_dir.join(&dll);

        if dst.exists() {
            copied += 1;
            continue;
        }

        let src = if cuda_bin.join(&dll).exists() {
            cuda_bin.join(&dll)
        } else if cuda_bin_x64.join(&dll).exists() {
            cuda_bin_x64.join(&dll)
        } else {
            log::warn!("[CUDA] {} not found in CUDA Toolkit", dll);
            continue;
        };

        std::fs::copy(&src, &dst)
            .map_err(|e| format!("Failed to copy {}: {}", dll, e))?;
        log::info!("[CUDA] Copied {} from {}", dll, src.display());
        copied += 1;
    }

    Ok(copied)
}

/// Download CUDA runtime DLLs from NVIDIA's official CDN.
pub async fn download_cuda_runtime(
    progress_cb: impl Fn(u64, u64) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dest_dir = exe_dir();

    let missing = missing_cuda_dlls();
    if missing.is_empty() {
        log::info!("[CUDA] All DLLs already present");
        return Ok(());
    }

    log::info!("[CUDA] Missing DLLs: {:?} — downloading from NVIDIA", missing);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;

    let need_cudart = missing.iter().any(|d| d.contains("cudart"));
    let need_cublas = missing.iter().any(|d| d.contains("cublas"));

    let total_estimate: u64 = if need_cudart && need_cublas {
        370_000_000
    } else if need_cublas {
        365_000_000
    } else {
        5_000_000
    };

    let mut total_downloaded: u64 = 0;

    if need_cudart {
        log::info!("[CUDA] Downloading cudart package...");
        total_downloaded = download_and_extract_dll_package(
            &client,
            CUDART_REDIST_URL,
            CUDART_REDIST_SHA256,
            "_cuda_cudart.zip",
            &dest_dir,
            &missing,
            &progress_cb,
            total_downloaded,
            total_estimate,
        )
        .await?;
    }

    if need_cublas {
        log::info!("[CUDA] Downloading cublas package...");
        total_downloaded = download_and_extract_dll_package(
            &client,
            CUBLAS_REDIST_URL,
            CUBLAS_REDIST_SHA256,
            "_cuda_cublas.zip",
            &dest_dir,
            &missing,
            &progress_cb,
            total_downloaded,
            total_estimate,
        )
        .await?;
    }

    progress_cb(total_downloaded, total_downloaded);

    let still_missing = missing_cuda_dlls();
    if !still_missing.is_empty() {
        return Err(format!(
            "Some CUDA DLLs could not be obtained: {:?}",
            still_missing
        )
        .into());
    }

    log::info!("[CUDA] All runtime DLLs provisioned successfully");
    Ok(())
}

/// Download a zip archive and extract matching DLLs from it.
async fn download_and_extract_dll_package(
    client: &reqwest::Client,
    url: &str,
    expected_sha256: &str,
    archive_name: &str,
    dest_dir: &std::path::Path,
    needed_dlls: &[String],
    progress_cb: &(impl Fn(u64, u64) + Send),
    mut downloaded_so_far: u64,
    total_estimate: u64,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let archive_path = dest_dir.join(archive_name);
    let previous_downloaded = downloaded_so_far;
    let archive_bytes = crate::download::download_verified(
        client,
        url,
        &archive_path,
        expected_sha256,
        |current, _| progress_cb(previous_downloaded + current, total_estimate),
    )
    .await?;
    downloaded_so_far += archive_bytes;

    let extract_result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let zip_file = std::fs::File::open(&archive_path)?;
        let mut archive = zip::ZipArchive::new(zip_file)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let Some(filename) = entry.name().rsplit('/').next().map(str::to_owned) else {
                continue;
            };
            if !needed_dlls.iter().any(|dll| dll == &filename) {
                continue;
            }

            let output = dest_dir.join(&filename);
            let partial = dest_dir.join(format!("{filename}.part"));
            let _ = std::fs::remove_file(&partial);
            let mut output_file = std::fs::File::create(&partial)?;
            std::io::copy(&mut entry, &mut output_file)?;
            output_file.sync_all()?;
            drop(output_file);
            if output.exists() {
                std::fs::remove_file(&output)?;
            }
            std::fs::rename(&partial, &output)?;
            log::info!("[CUDA] Extracted verified {filename}");
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&archive_path);
    extract_result?;
    Ok(downloaded_so_far)
}

#[cfg(test)]
mod tests {
    use super::AccelerationBackend;

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
            AccelerationBackend::Cuda
        );
        assert_eq!(
            AccelerationBackend::parse(None),
            AccelerationBackend::Cuda
        );
        assert_eq!(
            AccelerationBackend::parse(Some("unknown")),
            AccelerationBackend::Cuda
        );
    }

    #[test]
    fn worker_stems_match_external_bin_names() {
        assert_eq!(
            AccelerationBackend::Cuda.worker_bin_stem(),
            "whisper-worker"
        );
        assert_eq!(
            AccelerationBackend::Vulkan.worker_bin_stem(),
            "whisper-worker-vulkan"
        );
    }
}
