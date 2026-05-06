mod audio;
mod transcribe;
mod whisper_local;
mod google_auth;
mod gec;
mod vad;
pub mod subtitle;

use audio::{enumerate_capture_devices, apply_device_override};
#[cfg(not(target_os = "windows"))]
use enigo::{Enigo, Keyboard, Settings};
use serde::Serialize;
#[cfg(target_os = "windows")]
use std::ptr::null_mut;
#[cfg(not(target_os = "windows"))]
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
#[cfg(target_os = "windows")]
use windows_sys::Win32::{
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData,
            OpenClipboard, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
    },
    UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
    },
};

#[cfg(target_os = "windows")]
const CF_UNICODETEXT_FORMAT: u32 = 13;

/// Shared Enigo instance — creating one per paste leaks Win32 input hooks and COM state.
#[cfg(not(target_os = "windows"))]
static ENIGO: LazyLock<Mutex<Enigo>> = LazyLock::new(|| {
    Mutex::new(Enigo::new(&Settings::default()).expect("Failed to create Enigo"))
});

#[derive(Serialize, Clone)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// List all audio capture devices
#[tauri::command]
fn get_audio_devices() -> Result<Vec<AudioDevice>, String> {
    enumerate_capture_devices().map_err(|e| format!("Failed to enumerate devices: {}", e))
}

/// Set the audio capture device for this process and its WebView2 children
#[tauri::command]
fn set_audio_device(device_id: String) -> Result<(), String> {
    log::info!("set_audio_device called with id='{}'", device_id);
    apply_device_override(&device_id)
}

/// Transcribe audio using Groq Whisper API
#[tauri::command]
async fn transcribe_audio(audio_base64: String, api_key: String, initial_prompt: Option<String>) -> Result<String, String> {
    transcribe::transcribe_with_groq(&audio_base64, &api_key, initial_prompt.as_deref())
        .await
        .map_err(|e| format!("Transcription failed: {}", e))
}

// ── API Key — Windows Credential Manager ─────────────

const CREDENTIAL_SERVICE: &str = "Annotate";
const CREDENTIAL_USER: &str = "GroqApiKey";

/// Save the Groq API key to Windows Credential Manager.
/// The key is never written to disk in plaintext.
#[tauri::command]
fn save_api_key(key: String) -> Result<(), String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    entry.set_password(&key)
        .map_err(|e| format!("Failed to save credential: {}", e))
}

/// Load the Groq API key from Windows Credential Manager.
/// Returns None if no key has been saved yet.
#[tauri::command]
fn load_api_key() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(CREDENTIAL_SERVICE, CREDENTIAL_USER)
        .map_err(|e| format!("Credential entry error: {}", e))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to load credential: {}", e)),
    }
}

// ── Local Whisper Commands ─────────────────────────────

/// Check if the whisper model file exists on disk
#[tauri::command]
fn check_whisper_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(whisper_local::is_model_downloaded(&data_dir))
}

/// Get the model file path (for display in the UI)
#[tauri::command]
fn get_whisper_model_path(app: AppHandle) -> Result<String, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(whisper_local::model_path(&data_dir).to_string_lossy().to_string())
}

/// Download the whisper model file, emitting progress events
#[tauri::command]
async fn download_whisper_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    whisper_local::download_model(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("whisper-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("Download failed: {}", e))?;
    Ok(())
}

/// Load the whisper model — spawns worker process and loads model
#[tauri::command]
async fn load_whisper_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    whisper_local::ensure_loaded(&data_dir).await
}

/// Unload the whisper model — kills worker process, freeing all CUDA memory
#[tauri::command]
async fn unload_whisper_model() -> Result<(), String> {
    whisper_local::unload().await;
    Ok(())
}

/// Transcribe audio locally via the worker process
#[tauri::command]
async fn transcribe_audio_local(
    audio_base64: String,
    initial_prompt: Option<String>,
) -> Result<String, String> {
    whisper_local::transcribe_pcm_b64(&audio_base64, initial_prompt.as_deref()).await
}

// ── GEC (Grammar Correction) Commands ──────────────────

/// Check if the GEC model files exist on disk
#[tauri::command]
fn check_gec_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(gec::is_model_downloaded(&data_dir))
}

/// Download the GEC model bundle, emitting progress events
#[tauri::command]
async fn download_gec_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
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
#[tauri::command]
async fn load_gec_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || gec::ensure_loaded(&data_dir))
        .await
        .map_err(|e| format!("Join error: {}", e))?
}

/// Correct grammar in the given text
#[tauri::command]
async fn correct_grammar(text: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || gec::correct_text(&text))
        .await
        .map_err(|e| format!("Join error: {}", e))?
}

/// Unload the GEC model from memory
#[tauri::command]
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
#[tauri::command]
fn check_ffmpeg(app: AppHandle) -> Result<bool, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(subtitle::is_ffmpeg_available(&data_dir))
}

/// Download FFmpeg, emitting progress events
#[tauri::command]
async fn download_ffmpeg(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    let app_handle = app.clone();
    subtitle::ensure_ffmpeg(&data_dir, move |downloaded, total| {
        let _ = app_handle.emit("ffmpeg-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("FFmpeg download failed: {}", e))
}

/// Check if the VAD model is downloaded
#[tauri::command]
fn check_vad_model(app: AppHandle) -> Result<bool, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(vad::is_model_downloaded(&data_dir))
}

/// Download the VAD model, emitting progress events
#[tauri::command]
async fn download_vad_model(app: AppHandle) -> Result<(), String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
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
#[tauri::command]
async fn generate_subtitles(
    app: AppHandle,
    file_path: String,
    engine: String,
    api_key: Option<String>,
    prompt: Option<String>,
    language: Option<String>,
) -> Result<Vec<subtitle::SrtEntry>, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;

    let app_handle = app.clone();
    subtitle::generate_subtitles(
        &file_path,
        &engine,
        api_key.as_deref(),
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
#[tauri::command]
fn save_srt_file(entries: Vec<subtitle::SrtEntry>, output_path: String) -> Result<(), String> {
    let srt_content = subtitle::format_srt(&entries);
    std::fs::write(&output_path, srt_content)
        .map_err(|e| format!("Failed to write SRT file: {}", e))
}

/// Get the SRT text content from entries (for preview).
#[tauri::command]
fn format_srt_preview(entries: Vec<subtitle::SrtEntry>) -> String {
    subtitle::format_srt(&entries)
}

// ── CUDA Runtime Commands ──────────────────────────────

/// Check if CUDA runtime DLLs are available.
/// Returns: { available: bool, missing: ["name.dll", ...], has_toolkit: bool }
#[tauri::command]
fn check_cuda_runtime() -> Result<serde_json::Value, String> {
    let missing = whisper_local::missing_cuda_dlls();
    let available = missing.is_empty();
    let has_toolkit = std::env::var("CUDA_PATH").is_ok();

    // If DLLs are missing but toolkit is installed, try copying automatically
    if !available && has_toolkit {
        match whisper_local::copy_cuda_from_toolkit() {
            Ok(count) => {
                log::info!("[CUDA] Copied {} DLLs from toolkit", count);
                let still_missing = whisper_local::missing_cuda_dlls();
                return Ok(serde_json::json!({
                    "available": still_missing.is_empty(),
                    "missing": still_missing,
                    "has_toolkit": true,
                    "copied_from_toolkit": count
                }));
            }
            Err(e) => {
                log::warn!("[CUDA] Could not copy from toolkit: {}", e);
            }
        }
    }

    Ok(serde_json::json!({
        "available": available,
        "missing": missing,
        "has_toolkit": has_toolkit
    }))
}

/// Download CUDA runtime DLLs from NVIDIA's redistribution CDN
#[tauri::command]
async fn download_cuda_runtime(app: AppHandle) -> Result<(), String> {
    let app_handle = app.clone();
    whisper_local::download_cuda_runtime(move |downloaded, total| {
        let _ = app_handle.emit("cuda-download-progress", (downloaded, total));
    })
    .await
    .map_err(|e| format!("CUDA download failed: {}", e))?;
    Ok(())
}

/// Paste text by writing to clipboard and simulating Ctrl+V
#[tauri::command]
fn paste_text(text: String) -> Result<(), String> {
    // Small delay to let the user's hotkey release propagate
    std::thread::sleep(std::time::Duration::from_millis(100));

    paste_text_impl(&text)
}

#[cfg(target_os = "windows")]
fn paste_text_impl(text: &str) -> Result<(), String> {
    let sanitized = sanitize_text_for_input(text);
    set_clipboard_unicode_text(&sanitized)?;
    send_ctrl_v()?;
    std::thread::sleep(std::time::Duration::from_millis(220));
    if let Err(err) = clear_injected_clipboard_if_unchanged(&sanitized) {
        log::warn!("[Clipboard] cleanup failed: {}", err);
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn paste_text_impl(text: &str) -> Result<(), String> {
    let mut enigo = ENIGO
        .lock()
        .map_err(|e| format!("Enigo lock poisoned: {}", e))?;

    enigo.text(text)
        .map_err(|e| format!("Failed to type text: {}", e))?;

    Ok(())
}

#[cfg(target_os = "windows")]
struct ClipboardGuard;

#[cfg(target_os = "windows")]
impl ClipboardGuard {
    fn open() -> Result<Self, String> {
        let opened = unsafe { OpenClipboard(null_mut()) };
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

    let _guard = ClipboardGuard::open()?;
    if unsafe { EmptyClipboard() } == 0 {
        return Err("Failed to empty clipboard".to_string());
    }
    write_clipboard_format(CF_UNICODETEXT_FORMAT, &bytes)
}

#[cfg(target_os = "windows")]
fn write_clipboard_format(format: u32, data: &[u8]) -> Result<(), String> {
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
    if handle.is_null() {
        return Err(format!("Failed to allocate clipboard memory for format {}", format));
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        return Err(format!("Failed to lock clipboard memory for format {}", format));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
        GlobalUnlock(handle);
    }
    if unsafe { SetClipboardData(format, handle) }.is_null() {
        return Err(format!("Failed to set clipboard format {}", format));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn read_clipboard_unicode_text() -> Result<Option<String>, String> {
    let _guard = ClipboardGuard::open()?;
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
    let current = read_clipboard_unicode_text()?;
    if current.as_deref() != Some(injected_text) {
        return Ok(());
    }

    let _guard = ClipboardGuard::open()?;
    if unsafe { EmptyClipboard() } == 0 {
        return Err("Failed to clear clipboard".to_string());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn send_ctrl_v() -> Result<(), String> {
    let mut inputs = [
        keyboard_input(VK_CONTROL, 0),
        keyboard_input(VK_V, 0),
        keyboard_input(VK_V, KEYEVENTF_KEYUP),
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
        return Err(format!("Failed to send Ctrl+V: sent {} of {}", sent, inputs.len()));
    }
    Ok(())
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

/// Register a global hotkey that emits press/release events
#[tauri::command]
fn register_hotkey(app: AppHandle, shortcut_str: String) -> Result<(), String> {
    let manager = app.global_shortcut();

    // Unregister all existing shortcuts first
    manager.unregister_all().map_err(|e| format!("Failed to unregister: {}", e))?;

    let shortcut: Shortcut = shortcut_str
        .parse()
        .map_err(|e| format!("Invalid shortcut '{}': {}", shortcut_str, e))?;

    let app_handle = app.clone();
    manager
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            match event.state() {
                ShortcutState::Pressed => {
                    let _ = app_handle.emit("hotkey-down", ());
                }
                ShortcutState::Released => {
                    let _ = app_handle.emit("hotkey-up", ());
                }
            }
        })
        .map_err(|e| format!("Failed to register shortcut: {}", e))?;

    Ok(())
}

/// Show the throbber overlay window at bottom-center of the screen
#[tauri::command]
fn show_throbber(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("throbber") {
        // Position at bottom-center of the primary monitor
        if let Ok(Some(monitor)) = window.primary_monitor() {
            let screen = monitor.size();
            let mon_pos = monitor.position();
            let scale = monitor.scale_factor();

            // Get the window's actual physical size (may differ from config due to OS minimums)
            let win_size = window.outer_size().unwrap_or(tauri::PhysicalSize::new(
                (72.0 * scale) as u32,
                (32.0 * scale) as u32,
            ));
            let margin_bottom = (60.0 * scale) as i32;

            let x = mon_pos.x + (screen.width as i32 - win_size.width as i32) / 2;
            let y = mon_pos.y + screen.height as i32 - win_size.height as i32 - margin_bottom;

            use tauri::PhysicalPosition;
            let _ = window.set_position(PhysicalPosition::new(x, y));
        }
        window.show().map_err(|e| format!("Failed to show throbber: {}", e))?;
    }
    Ok(())
}

/// Hide the throbber overlay window
#[tauri::command]
fn hide_throbber(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("throbber") {
        window.hide().map_err(|e| format!("Failed to hide throbber: {}", e))?;
    }
    Ok(())
}

/// Hide main window to system tray
#[tauri::command]
fn hide_to_tray(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| format!("Failed to hide: {}", e))?;
    }
    Ok(())
}

/// Quit the app entirely
#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Start the WebView2 watcher thread BEFORE Tauri builder
    // so it catches WebView2 child processes as they spawn
    audio::apply_startup_override_and_watch();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second instance was launched — show + focus the existing main window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .build(),
        )
        .setup(|app| {
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;

            // Build system tray
            use tauri::menu::{MenuBuilder, MenuItemBuilder};
            use tauri::tray::TrayIconBuilder;

            let show_item = MenuItemBuilder::with_id("show", "Show Annotate").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[&show_item, &quit_item])
                .build()?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Annotate")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    use tauri::tray::{TrayIconEvent, MouseButton, MouseButtonState};
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        if let Some(window) = tray.app_handle().get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_audio_devices,
            set_audio_device,
            transcribe_audio,
            paste_text,
            register_hotkey,
            show_throbber,
            hide_throbber,
            hide_to_tray,
            quit_app,
            save_api_key,
            load_api_key,
            check_whisper_model,
            get_whisper_model_path,
            download_whisper_model,
            load_whisper_model,
            transcribe_audio_local,
            check_cuda_runtime,
            download_cuda_runtime,
            google_auth::google_oauth,
            google_auth::cancel_google_oauth,
            check_gec_model,
            download_gec_model,
            load_gec_model,
            unload_gec_model,
            correct_grammar,
            unload_whisper_model,
            check_ffmpeg,
            download_ffmpeg,
            check_vad_model,
            download_vad_model,
            generate_subtitles,
            save_srt_file,
            format_srt_preview,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
