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

pub fn prepare_overlay(window: &impl HasWindowHandle, width: f32, height: f32, bottom: f32) {
    if let Some(hwnd) = hwnd(window) {
        overlay::prepare(hwnd, width, height, bottom);
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
    // The keyboard hook sees each key before the window does, so this is the
    // key behind the key-down being handled, not just any key held down.
    let key = services::last_key_down()?;
    if matches!(key, 0x10..=0x12 | 0x5b | 0x5c | 0xa0..=0xa5) || !down(VIRTUAL_KEY(key)) {
        return None;
    }
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
                (&preference as *const windows::Win32::Graphics::Dwm::DWM_WINDOW_CORNER_PREFERENCE)
                    .cast(),
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
        Globalization::{
            GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS,
        },
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

/// Saves the window's client area as a PNG (UI reference captures). Works while
/// the window is covered or off-screen because DWM renders the full content.
pub fn capture_window(window: &impl HasWindowHandle, path: &std::path::Path) -> Result<(), String> {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::*,
        Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
        UI::WindowsAndMessaging::GetClientRect,
    };
    let hwnd = hwnd(window).ok_or("No window handle")?;
    unsafe {
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect).map_err(|e| e.to_string())?;
        let (width, height) = (rect.right, rect.bottom);
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        let old = SelectObject(dc, bitmap.into());
        // PW_CLIENTONLY | PW_RENDERFULLCONTENT
        let printed = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(3)).as_bool();
        let pixels = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
        let mut rgba = Vec::with_capacity(pixels.len());
        for p in pixels.chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        if !printed {
            return Err("PrintWindow failed".into());
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder =
            png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgba))
            .map_err(|e| e.to_string())
    }
}

/// Moves a window off-screen without activating it (reference captures).
pub fn move_offscreen(window: &impl HasWindowHandle) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                -8000,
                -8000,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE,
            );
        }
    }
}
