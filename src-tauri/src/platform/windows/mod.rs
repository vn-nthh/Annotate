//! Windows: Win32 shortcuts and tray, WASAPI audio, Shell dialogs, SendInput.
mod input;
mod overlay;
mod picker;
mod services;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::PathBuf;
use windows::Win32::Foundation::HWND;

pub use input::{capture_selected_text, paste_text, sanitize_text_for_input, set_clipboard_text};
pub use services::{acquire_instance, key_name, register as register_shortcut, Instance};

pub fn local_data_dir() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("LOCALAPPDATA is not available".into())
}

pub fn roaming_data_dir() -> Result<PathBuf, String> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or("APPDATA is not available".into())
}

pub fn start_services(instance: &Instance) -> Result<(), String> {
    services::start(instance)
}

pub fn stop_services() {
    services::stop();
}

fn hwnd(window: &impl HasWindowHandle) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}

pub fn prepare_overlay(window: &impl HasWindowHandle) {
    if let Some(hwnd) = hwnd(window) {
        overlay::prepare(hwnd);
    }
}

pub fn show_overlay(window: &impl HasWindowHandle, width: f32, height: f32, bottom: f32) {
    if let Some(hwnd) = hwnd(window) {
        overlay::show(hwnd, width, height, bottom);
    }
}

pub fn hide_overlay(window: &impl HasWindowHandle) {
    if let Some(hwnd) = hwnd(window) {
        overlay::hide(hwnd);
    }
}

pub fn animations_enabled() -> bool {
    overlay::animations_enabled()
}

/// A raw window handle that can cross threads for an owned modal dialog.
#[derive(Clone, Copy)]
pub struct DialogOwner(isize);
unsafe impl Send for DialogOwner {}

pub fn dialog_owner(window: &impl HasWindowHandle) -> Option<DialogOwner> {
    hwnd(window).map(|hwnd| DialogOwner(hwnd.0 as isize))
}

/// Shows the media file picker. Blocks; call off the UI thread.
pub fn pick_media_file(owner: DialogOwner) -> Result<Option<PathBuf>, String> {
    picker::open(HWND(owner.0 as *mut _))
}

/// Shows the SRT save dialog. Blocks; call off the UI thread.
pub fn pick_srt_destination(owner: DialogOwner, name: &str) -> Result<Option<PathBuf>, String> {
    picker::save(HWND(owner.0 as *mut _), name)
}

/// The shortcut held right now, as stored in settings ("Ctrl+Shift+Space").
/// Read during a key-down so it reflects that key, by virtual-key code rather
/// than the layout's character, matching what `RegisterHotKey` will use.
pub fn pressed_shortcut() -> Option<String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(vk.0 as i32) < 0 };
    let modifiers = [
        0x10, 0x11, 0x12, 0x5b, 0x5c, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5,
    ];
    let key = (0x08u16..=0xfe)
        .filter(|vk| !modifiers.contains(vk))
        .find(|vk| down(VIRTUAL_KEY(*vk)))?;
    let mut keys = vec![];
    if down(VK_CONTROL) {
        keys.push("Ctrl".to_string());
    }
    if down(VK_MENU) {
        keys.push("Alt".into());
    }
    if down(VK_SHIFT) {
        keys.push("Shift".into());
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        keys.push("Super".into());
    }
    keys.push(key_name(key));
    Some(keys.join("+"))
}

/// Rounded corners (Windows 11) for the borderless main window.
pub fn style_main_window(window: &impl HasWindowHandle) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let preference = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&preference as *const windows::Win32::Graphics::Dwm::DWM_WINDOW_CORNER_PREFERENCE).cast(),
                std::mem::size_of_val(&preference) as u32,
            );
        }
    }
}

pub fn show_fatal_error(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR};
    unsafe {
        let _ = MessageBoxW(
            None,
            &windows::core::HSTRING::from(message),
            windows::core::w!("Annotate"),
            MB_ICONERROR,
        );
    }
}

/// History timestamps: the locale's short time today, "MMM d" plus time before.
pub fn format_time(timestamp: u64) -> String {
    use windows::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        Globalization::{GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS},
        System::{
            SystemInformation::GetLocalTime,
            Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
        },
    };
    unsafe {
        let ticks = timestamp * 10000 + 116444736000000000;
        let utc = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut universal = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        let _ = FileTimeToSystemTime(&utc, &mut universal);
        let _ = SystemTimeToTzSpecificLocalTime(None, &universal, &mut local);
        let current = GetLocalTime();
        let mut time = [0u16; 100];
        let length = GetTimeFormatEx(
            windows::core::PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&local),
            None,
            Some(&mut time),
        );
        let mut time = String::from_utf16_lossy(&time[..length.saturating_sub(1) as usize]);
        if time.as_bytes().get(1) == Some(&b':') && time.as_bytes()[0].is_ascii_digit() {
            time.insert(0, '0');
        }
        if current.wYear == local.wYear
            && current.wMonth == local.wMonth
            && current.wDay == local.wDay
        {
            return time;
        }
        let mut date = [0u16; 100];
        let length = GetDateFormatEx(
            windows::core::PCWSTR::null(),
            ENUM_DATE_FORMATS_FLAGS(0),
            Some(&local),
            windows::core::w!("MMM d"),
            Some(&mut date),
            None,
        );
        format!(
            "{} {}",
            String::from_utf16_lossy(&date[..length.saturating_sub(1) as usize]),
            time
        )
    }
}
