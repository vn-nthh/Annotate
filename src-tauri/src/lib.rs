mod app;
mod download;
mod gec;
mod google_auth;
mod native_audio;
mod native_benchmark;
mod native_core;
mod native_data;
mod native_migrate;
mod native_onboarding;
mod native_sync;
mod platform;
pub mod subtitle;
mod summary;
mod transcribe;
mod ui;
mod vad;
mod whisper_local;

use app::AppHandle;
use platform::enumerate_capture_devices;
use serde::Serialize;
use std::time::Instant;

#[derive(Serialize, Clone)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// List all audio capture devices
fn get_audio_devices() -> Result<Vec<AudioDevice>, String> {
    enumerate_capture_devices().map_err(|e| format!("Failed to enumerate devices: {}", e))
}

/// Transcribe audio using Groq Whisper API
async fn transcribe_audio(
    audio_base64: String,
    api_key: String,
    initial_prompt: Option<String>,
    language: Option<String>,
) -> Result<String, String> {
    transcribe::transcribe_with_groq(
        &audio_base64,
        &api_key,
        initial_prompt.as_deref(),
        language.as_deref(),
    )
    .await
    .map_err(|e| format!("Transcription failed: {}", e))
}

/// Transcribe audio using Microsoft MAI-Transcribe through Azure Speech in Foundry Tools.
async fn transcribe_audio_azure(
    audio_base64: String,
    api_key: String,
    endpoint: String,
    initial_prompt: Option<String>,
    language: Option<String>,
) -> Result<String, String> {
    transcribe::transcribe_with_azure_mai(
        &audio_base64,
        &api_key,
        &endpoint,
        initial_prompt.as_deref(),
        language.as_deref(),
    )
    .await
    .map_err(|e| format!("Azure transcription failed: {}", e))
}

/// Warm the shared Azure HTTP/2 connection before the REST upload is ready.
async fn warm_azure_transcription(endpoint: String) -> Result<(), String> {
    transcribe::warm_azure_connection(&endpoint)
        .await
        .map_err(|e| format!("Azure connection warm-up failed: {}", e))
}

// ── API Key — Windows Credential Manager ─────────────

const CREDENTIAL_SERVICE: &str = "Annotate";
const GROQ_CREDENTIAL_USER: &str = "GroqApiKey";
const AZURE_CREDENTIAL_USER: &str = "AzureFoundryApiKey";
const OPENROUTER_CREDENTIAL_USER: &str = "OpenRouterApiKey";

/// Save the Groq API key to Windows Credential Manager.
/// The key is never written to disk in plaintext.
fn save_api_key(key: String) -> Result<(), String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, GROQ_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    entry
        .set_password(&key)
        .map_err(|e| format!("Failed to save credential: {}", e))
}

/// Load the Groq API key from Windows Credential Manager.
/// Returns None if no key has been saved yet.
fn load_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, GROQ_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to load credential: {}", e)),
    }
}

/// Save the Azure Foundry API key to Windows Credential Manager.
fn save_azure_api_key(key: String) -> Result<(), String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, AZURE_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    entry
        .set_password(&key)
        .map_err(|e| format!("Failed to save credential: {}", e))
}

/// Load the Azure Foundry API key from Windows Credential Manager.
fn load_azure_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, AZURE_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to load credential: {}", e)),
    }
}

/// Save the OpenRouter API key to Windows Credential Manager.
/// Empty input clears the saved credential.
fn save_openrouter_api_key(key: String) -> Result<(), String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, OPENROUTER_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    let key = key.trim();
    if key.is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("Failed to clear credential: {}", e)),
        };
    }

    entry
        .set_password(key)
        .map_err(|e| format!("Failed to save credential: {}", e))
}

/// Load the OpenRouter API key from Windows Credential Manager.
fn load_openrouter_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, OPENROUTER_CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to load credential: {}", e)),
    }
}

// ── Local Whisper Commands ─────────────────────────────

/// Check if the whisper model file exists on disk
fn check_whisper_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    Ok(whisper_local::is_model_downloaded(&data_dir))
}

/// Download the whisper model file, emitting progress events. Resumes a paused
/// download; fails with `download::DOWNLOAD_CANCELLED` when paused.
async fn download_whisper_model(app: AppHandle) -> Result<(), String> {
    let guard = whisper_local::MODEL_DOWNLOAD.begin()?;
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    whisper_local::download_model(
        &data_dir,
        throttled_progress(app, "whisper-download-progress"),
        guard.token(),
    )
    .await
    .map_err(|e| {
        if download::is_cancelled(e.as_ref()) {
            download::DOWNLOAD_CANCELLED.to_owned()
        } else {
            format!("Download failed: {e}")
        }
    })?;
    Ok(())
}

/// Pause the model download, keeping the partial file. Returns false when idle.
fn cancel_whisper_model_download() -> bool {
    whisper_local::MODEL_DOWNLOAD.cancel()
}

/// Bytes of a paused model download, if any.
fn partial_whisper_model_bytes(app: AppHandle) -> u64 {
    app.path()
        .app_local_data_dir()
        .map(|dir| whisper_local::partial_model_bytes(&dir))
        .unwrap_or(0)
}

/// Each progress event repaints the window, so report at most once per MiB.
fn throttled_progress(app: AppHandle, event: &'static str) -> impl Fn(u64, u64) + Send + 'static {
    const STEP: u64 = 1024 * 1024;
    let last = std::sync::atomic::AtomicU64::new(u64::MAX);
    move |downloaded, total| {
        let previous = last.load(std::sync::atomic::Ordering::Relaxed);
        if previous == u64::MAX
            || downloaded < previous
            || downloaded >= previous + STEP
            || downloaded == total
        {
            last.store(downloaded, std::sync::atomic::Ordering::Relaxed);
            let _ = app.emit(event, (downloaded, total));
        }
    }
}

/// Load the whisper model — spawns worker process and loads model.
/// The Vulkan worker owns GPU resources until the model is unloaded.
async fn load_whisper_model(app: AppHandle, acceleration: Option<String>) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let backend = whisper_local::AccelerationBackend::parse(acceleration.as_deref());
    whisper_local::ensure_loaded(&data_dir, backend).await
}

/// Unload the whisper model — kills worker process, freeing all GPU memory
async fn unload_whisper_model() -> Result<(), String> {
    whisper_local::unload().await;
    Ok(())
}

/// Transcribe audio locally via the worker process
async fn transcribe_audio_local(
    audio_base64: String,
    initial_prompt: Option<String>,
    language: Option<String>,
) -> Result<String, String> {
    whisper_local::transcribe_pcm_b64(
        &audio_base64,
        initial_prompt.as_deref(),
        language.as_deref(),
    )
    .await
}

// ── GEC (Grammar Correction) Commands ──────────────────

/// Check if the GEC model files exist on disk
fn check_gec_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    Ok(gec::is_model_downloaded(&data_dir))
}

/// Download the GEC model bundle, emitting progress events
async fn download_gec_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    gec::download_model(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("gec-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("GEC download failed: {}", e))?;
    Ok(())
}

/// Load the GEC model into memory
async fn load_gec_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || gec::ensure_loaded(&data_dir))
        .await
        .map_err(|e| format!("Join error: {}", e))?
}

/// Correct grammar in the given text
async fn correct_grammar(text: String) -> Result<String, String> {
    let total_start = Instant::now();
    let input_chars = text.chars().count();
    let result = tokio::task::spawn_blocking(move || {
        let blocking_start = Instant::now();
        let result = gec::correct_text(&text);
        log::info!(
            "[Timing][gec] rust correct_text blocking: {:.1}ms input_chars={}",
            blocking_start.elapsed().as_secs_f64() * 1000.0,
            input_chars
        );
        result
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?;

    match &result {
        Ok(corrected) => log::info!(
            "[Timing][gec] rust correct_grammar total: {:.1}ms status=ok output_chars={}",
            total_start.elapsed().as_secs_f64() * 1000.0,
            corrected.chars().count()
        ),
        Err(err) => log::info!(
            "[Timing][gec] rust correct_grammar total: {:.1}ms status=err error={}",
            total_start.elapsed().as_secs_f64() * 1000.0,
            err
        ),
    }

    result
}

/// Unload the GEC model from memory
async fn unload_gec_model() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        gec::unload();
        Ok(())
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

// ── Subtitle Commands ──────────────────────────────────

/// Check if FFmpeg is available
fn check_ffmpeg(app: AppHandle) -> Result<bool, String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    Ok(subtitle::is_ffmpeg_available(&data_dir))
}

/// Download FFmpeg, emitting progress events
async fn download_ffmpeg(app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    subtitle::ensure_ffmpeg(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("ffmpeg-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("FFmpeg download failed: {}", e))
}

/// Check if the VAD model is downloaded
fn check_vad_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    Ok(vad::is_model_downloaded(&data_dir))
}

/// Download the VAD model, emitting progress events
async fn download_vad_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    vad::download_model(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("vad-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("VAD download failed: {}", e))
}

/// Generate subtitles from an audio/video file.
/// Emits 'subtitle-progress' events during processing.
async fn generate_subtitles(
    app: AppHandle,
    file_path: String,
    engine: String,
    api_key: Option<String>,
    azure_endpoint: Option<String>,
    prompt: Option<String>,
    language: Option<String>,
) -> Result<Vec<subtitle::SrtEntry>, String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;

    let app_handle = app.clone();
    subtitle::generate_subtitles(
        &file_path,
        &engine,
        api_key.as_deref(),
        azure_endpoint.as_deref(),
        prompt.as_deref(),
        language.as_deref(),
        &data_dir,
        move |progress| {
            let _ = app_handle.emit("subtitle-progress", &progress);
        },
    )
    .await
}

/// Save SRT entries to a file.
fn save_srt_file(entries: Vec<subtitle::SrtEntry>, output_path: String) -> Result<(), String> {
    let srt_content = subtitle::format_srt(&entries);
    std::fs::write(&output_path, srt_content)
        .map_err(|e| format!("Failed to write SRT file: {}", e))
}

/// Summarize subtitle entries with OpenRouter GPT-6 Luna.
async fn summarize_subtitles(entries: Vec<subtitle::SrtEntry>) -> Result<String, String> {
    let api_key = load_openrouter_api_key()?
        .ok_or_else(|| "OpenRouter API key not configured. Add it in Settings.".to_string())?;

    summary::summarize_entries(&api_key, &entries)
        .await
        .map_err(|e| format!("Subtitle summary failed: {}", e))
}

/// Paste text by writing to clipboard and simulating Ctrl+V
fn paste_text(text: String, release_delay_ms: Option<u64>) -> Result<(), String> {
    let total_start = Instant::now();
    let chars = text.chars().count();

    // Small delay to let the user's hotkey release propagate
    let release_delay_start = Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(
        release_delay_ms.unwrap_or(100).min(500),
    ));
    log::info!(
        "[Timing][paste] hotkey release delay: {:.1}ms",
        release_delay_start.elapsed().as_secs_f64() * 1000.0
    );

    let result = paste_text_impl(&text);
    log::info!(
        "[Timing][paste] total: {:.1}ms chars={} status={}",
        total_start.elapsed().as_secs_f64() * 1000.0,
        chars,
        if result.is_ok() { "ok" } else { "err" }
    );

    result
}

fn paste_text_impl(text: &str) -> Result<(), String> {
    platform::paste_text(text)
}

fn capture_selected_text(release_delay_ms: Option<u64>) -> Result<String, String> {
    platform::capture_selected_text(release_delay_ms)
}

fn register_hotkey(_app: AppHandle, shortcut_str: String) -> Result<(), String> {
    app::register(0, &shortcut_str)
}
fn register_dict_hotkey(_app: AppHandle, shortcut_str: String) -> Result<(), String> {
    app::register(1, &shortcut_str)
}
pub fn run() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "--capture-ui") {
        let Some(directory) = args.get(2) else {
            eprintln!("--capture-ui requires an output directory");
            std::process::exit(1);
        };
        let directory = std::path::PathBuf::from(directory);
        std::env::set_var("ANNOTATE_UI_TEST", "1");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::set_var(
            "ANNOTATE_DATA_DIR",
            directory.join(format!("test-data-{}-{stamp}", std::process::id())),
        );
        let result = app::initialize_runtime().and_then(|_| ui::capture(directory));
        if let Err(error) = result {
            eprintln!("Native UI capture: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).is_some_and(|s| s == "--benchmark") {
        let Some(directory) = args.get(2) else {
            eprintln!("--benchmark requires a report directory");
            std::process::exit(1);
        };
        let directory = std::path::PathBuf::from(directory);
        std::env::set_var("ANNOTATE_UI_TEST", "1");
        std::env::set_var("ANNOTATE_DATA_DIR", directory.join("data"));
        if let Err(error) = native_benchmark::initialize(&directory) {
            eprintln!("Benchmark: {error}");
            std::process::exit(1);
        }
    }
    if let Err(error) = app::run() {
        eprintln!("Annotate: {error}");
        platform::show_fatal_error(&error);
    }
}
