mod audio;
mod download;
mod gec;
mod google_auth;
mod native_app;
mod native_audio;
mod native_benchmark;
mod native_core;
mod native_data;
mod native_migrate;
mod native_picker;
mod native_sync;
mod native_ui;
pub mod subtitle;
mod summary;
mod throbber;
mod transcribe;
mod vad;
mod whisper_local;

use audio::enumerate_capture_devices;
use native_app::AppHandle;
use serde::Serialize;
#[cfg(target_os = "windows")]
use std::ptr::null_mut;
use std::time::Instant;
#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    Foundation::GlobalFree,
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_C, VK_CONTROL,
        VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_V,
    },
};

#[cfg(target_os = "windows")]
const CF_UNICODETEXT_FORMAT: u32 = 13;

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

/// Download the whisper model file, emitting progress events
async fn download_whisper_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    whisper_local::download_model(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("whisper-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("Download failed: {}", e))?;
    Ok(())
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

/// Summarize subtitle entries with OpenRouter GPT-5.4 Mini.
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

#[cfg(target_os = "windows")]
fn paste_text_impl(text: &str) -> Result<(), String> {
    let sanitize_start = Instant::now();
    let sanitized = sanitize_text_for_input(text);
    log::info!(
        "[Timing][paste] sanitize: {:.1}ms chars={}",
        sanitize_start.elapsed().as_secs_f64() * 1000.0,
        sanitized.chars().count()
    );

    let clipboard_start = Instant::now();
    set_clipboard_unicode_text(&sanitized)?;
    log::info!(
        "[Timing][paste] set clipboard: {:.1}ms",
        clipboard_start.elapsed().as_secs_f64() * 1000.0
    );

    let input_start = Instant::now();
    send_ctrl_v()?;
    log::info!(
        "[Timing][paste] send Ctrl+V: {:.1}ms",
        input_start.elapsed().as_secs_f64() * 1000.0
    );

    std::thread::spawn(move || {
        let cleanup_start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(220));
        if let Err(err) = clear_injected_clipboard_if_unchanged(&sanitized) {
            log::warn!("[Clipboard] cleanup failed: {}", err);
        }
        log::info!(
            "[Timing][paste] deferred clipboard cleanup: {:.1}ms",
            cleanup_start.elapsed().as_secs_f64() * 1000.0
        );
    });

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn paste_text_impl(text: &str) -> Result<(), String> {
    let _ = text;
    Err("Pasting text is only supported on Windows".into())
}

#[cfg(target_os = "windows")]
struct ClipboardGuard;

#[cfg(target_os = "windows")]
impl ClipboardGuard {
    fn open() -> Result<Self, String> {
        Self::open_with_owner(null_mut())
    }

    fn open_for_write() -> Result<Self, String> {
        let owner = native_app::clipboard_owner()
            .ok_or_else(|| "The clipboard runtime is not running".to_string())?;
        Self::open_with_owner(owner.0)
    }

    fn open_with_owner(owner: *mut std::ffi::c_void) -> Result<Self, String> {
        let opened = unsafe { OpenClipboard(owner) };
        if opened == 0 {
            return Err("Failed to open clipboard".to_string());
        }
        Ok(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

#[cfg(target_os = "windows")]
fn set_clipboard_unicode_text(text: &str) -> Result<(), String> {
    let mut bytes = Vec::new();
    for unit in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let _guard = ClipboardGuard::open_for_write()?;
    if unsafe { EmptyClipboard() } == 0 {
        return Err("Failed to empty clipboard".to_string());
    }
    write_clipboard_format(CF_UNICODETEXT_FORMAT, &bytes)
}

#[cfg(target_os = "windows")]
fn write_clipboard_format(format: u32, data: &[u8]) -> Result<(), String> {
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
    if handle.is_null() {
        return Err(format!(
            "Failed to allocate clipboard memory for format {}",
            format
        ));
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        unsafe { GlobalFree(handle) };
        return Err(format!(
            "Failed to lock clipboard memory for format {}",
            format
        ));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
        GlobalUnlock(handle);
    }
    if unsafe { SetClipboardData(format, handle) }.is_null() {
        unsafe { GlobalFree(handle) };
        return Err(format!("Failed to set clipboard format {}", format));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn read_clipboard_unicode_text() -> Result<Option<String>, String> {
    let _guard = ClipboardGuard::open()?;
    read_open_clipboard_unicode_text()
}

#[cfg(target_os = "windows")]
fn read_open_clipboard_unicode_text() -> Result<Option<String>, String> {
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT_FORMAT) };
    if handle.is_null() {
        return Ok(None);
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        return Ok(None);
    }

    let size_bytes = unsafe { GlobalSize(handle) };
    if size_bytes < 2 {
        unsafe { GlobalUnlock(handle) };
        return Ok(Some(String::new()));
    }

    let max_units = size_bytes / 2;
    let units = unsafe { std::slice::from_raw_parts(ptr as *const u16, max_units) };
    let len = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..len]);
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(Some(text))
}

#[cfg(target_os = "windows")]
fn clear_injected_clipboard_if_unchanged(injected_text: &str) -> Result<(), String> {
    // Keep comparison and clearing under one open clipboard so another app's
    // newer copy cannot replace the injected text between those operations.
    let _guard = ClipboardGuard::open_for_write()?;
    let current = read_open_clipboard_unicode_text()?;
    if current.as_deref() != Some(injected_text) {
        return Ok(());
    }

    if unsafe { EmptyClipboard() } == 0 {
        return Err("Failed to clear clipboard".to_string());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn send_ctrl_v() -> Result<(), String> {
    send_ctrl_combo(VK_V, "Ctrl+V")
}

#[cfg(target_os = "windows")]
fn send_ctrl_c() -> Result<(), String> {
    send_ctrl_combo(VK_C, "Ctrl+C")
}

/// Force-release modifier keys the user may still be holding from their hotkey,
/// so a following synthetic Ctrl+C isn't polluted (e.g. into Ctrl+Alt+C).
#[cfg(target_os = "windows")]
fn release_modifiers() -> Result<(), String> {
    let mut inputs = [
        keyboard_input(VK_MENU, KEYEVENTF_KEYUP),
        keyboard_input(VK_SHIFT, KEYEVENTF_KEYUP),
        keyboard_input(VK_LWIN, KEYEVENTF_KEYUP),
        keyboard_input(VK_RWIN, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        return Err(format!(
            "Failed to release modifiers: sent {} of {}",
            sent,
            inputs.len()
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn send_ctrl_combo(vk: u16, label: &str) -> Result<(), String> {
    let mut inputs = [
        keyboard_input(VK_CONTROL, 0),
        keyboard_input(vk, 0),
        keyboard_input(vk, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_mut_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        return Err(format!(
            "Failed to send {}: sent {} of {}",
            label,
            sent,
            inputs.len()
        ));
    }
    Ok(())
}

/// Capture the currently selected text in the foreground app by simulating
/// Ctrl+C and reading the clipboard. Restores the prior clipboard text afterwards.
/// Returns an empty string when nothing was copied (no selection).
#[cfg(target_os = "windows")]
fn capture_selected_text(release_delay_ms: Option<u64>) -> Result<String, String> {
    // Let the user's hotkey modifiers release so they don't mix into our Ctrl+C
    std::thread::sleep(std::time::Duration::from_millis(
        release_delay_ms.unwrap_or(120).min(500),
    ));
    // Belt-and-suspenders: force-release any modifiers still held from the hotkey
    if let Err(err) = release_modifiers() {
        log::warn!("[Capture] modifier release failed: {}", err);
    }
    std::thread::sleep(std::time::Duration::from_millis(30));

    // Preserve whatever text the user had on the clipboard
    let previous = read_clipboard_unicode_text().unwrap_or(None);

    // Use a sentinel so we can tell "copy did nothing" apart from "copied same text"
    let sentinel = format!("__annotate_capture_{}__", std::process::id());
    set_clipboard_unicode_text(&sentinel)?;

    send_ctrl_c()?;

    // Poll for the clipboard to change (apps copy asynchronously)
    let mut captured: Option<String> = None;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        match read_clipboard_unicode_text() {
            Ok(Some(text)) if text != sentinel => {
                captured = Some(text);
                break;
            }
            _ => {}
        }
    }

    // Restore the user's original clipboard (or clear our sentinel)
    let restore_result = match &previous {
        Some(text) => set_clipboard_unicode_text(text),
        None => clear_injected_clipboard_if_unchanged(&sentinel),
    };
    if let Err(err) = restore_result {
        log::warn!("[Clipboard] restore after capture failed: {}", err);
    }

    Ok(captured.unwrap_or_default())
}

#[cfg(not(target_os = "windows"))]
fn capture_selected_text(release_delay_ms: Option<u64>) -> Result<String, String> {
    let _ = release_delay_ms;
    Err("Capturing selected text is only supported on Windows".to_string())
}

#[cfg(target_os = "windows")]
fn keyboard_input(vk: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(target_os = "windows")]
fn sanitize_text_for_input(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{00A0}' => sanitized.push(' '),
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => {}
            '\r' => {
                sanitized.push_str("\r\n");
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' => sanitized.push_str("\r\n"),
            _ => sanitized.push(ch),
        }
    }

    sanitized
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::sanitize_text_for_input;

    #[test]
    fn sanitizes_text_for_windows_clipboard_paste() {
        let text = "a\u{00A0}b\u{200B}c\r\nd\re\n\u{FEFF}f";
        assert_eq!(sanitize_text_for_input(text), "a bc\r\nd\r\ne\r\nf");
    }
}

fn register_hotkey(_app: AppHandle, shortcut_str: String) -> Result<(), String> {
    native_app::register(0, &shortcut_str)
}
fn register_dict_hotkey(_app: AppHandle, shortcut_str: String) -> Result<(), String> {
    native_app::register(1, &shortcut_str)
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
        let result = native_app::initialize_runtime()
            .and_then(|_| native_core::initialize(native_app::AppHandle))
            .and_then(|_| native_ui::capture_reference(&directory));
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
    if let Err(error) = native_app::run() {
        eprintln!("Annotate: {error}");
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                None,
                &windows::core::HSTRING::from(error),
                windows::core::w!("Annotate"),
                windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
}
