mod audio;
mod transcribe;

use audio::{enumerate_capture_devices, apply_device_override};
use enigo::{Enigo, Keyboard, Settings};
use serde::Serialize;
use std::sync::{LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// Shared Enigo instance — creating one per paste leaks Win32 input hooks and COM state.
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

/// Paste text by writing to clipboard and simulating Ctrl+V
#[tauri::command]
fn paste_text(text: String) -> Result<(), String> {
    // Small delay to let the user's hotkey release propagate
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut enigo = ENIGO
        .lock()
        .map_err(|e| format!("Enigo lock poisoned: {}", e))?;

    // Set clipboard
    enigo.text(&text)
        .map_err(|e| format!("Failed to type text: {}", e))?;

    Ok(())
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
        .plugin(tauri_plugin_store::Builder::default().build())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
