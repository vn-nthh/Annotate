//! Windows: Win32 shortcuts and tray, WASAPI audio, Shell dialogs, SendInput.
mod audio;
mod data;
mod input;
mod overlay;
mod picker;
mod services;
mod worker;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::PathBuf;
use windows::Win32::Foundation::HWND;

pub use audio::{enumerate_capture_devices, Capture};
pub use data::{compare_terms, legacy_storage_paths, lock_legacy_storage};
pub use input::{capture_selected_text, paste_text, sanitize_text_for_input, set_clipboard_text};
pub use services::{acquire_instance, key_name, register as register_shortcut, Instance};
#[cfg(test)]
pub use worker::test_worker;
pub use worker::{
    configure_whisper_worker, hide_console, WorkerJob, FFMPEG_FILENAME, FFMPEG_SHA256, FFMPEG_URL,
};

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

/// An opaque foreground-window snapshot for the isolated overlay checks.
pub fn overlay_check_foreground() -> DialogOwner {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    DialogOwner(unsafe { GetForegroundWindow() }.0 as isize)
}

/// Probes the actual overlay window. Call off the UI thread because native hit
/// tests synchronously enter its window procedure.
pub fn check_overlay_window(
    owner: DialogOwner,
    foreground: DialogOwner,
    visible: bool,
) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{LPARAM, RECT, WPARAM},
        UI::WindowsAndMessaging::*,
    };
    let hwnd = HWND(owner.0 as *mut _);
    unsafe {
        if IsWindowVisible(hwnd).as_bool() != visible {
            return Err(format!(
                "The recording overlay visibility was not {visible}"
            ));
        }
        let required = WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if style & required.0 as isize != required.0 as isize {
            return Err(format!(
                "The recording overlay has incorrect extended styles: {style:#x}"
            ));
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        if style & WS_POPUP.0 as isize == 0 || style & (WS_CAPTION | WS_THICKFRAME).0 as isize != 0
        {
            return Err(format!(
                "The recording overlay has a native frame: {style:#x}"
            ));
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).map_err(|e| e.to_string())?;
        let mut client = RECT::default();
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
        if rect.right - rect.left != client.right || rect.bottom - rect.top != client.bottom {
            return Err("The recording overlay has borders outside its client area".into());
        }
        let x = ((rect.left + rect.right) / 2) as u16 as isize;
        let y = ((rect.top + rect.bottom) / 2) as u16 as isize;
        let hit = SendMessageW(hwnd, WM_NCHITTEST, None, Some(LPARAM(x | (y << 16))));
        if hit.0 != HTTRANSPARENT as isize {
            return Err(format!("The recording overlay hit test returned {}", hit.0));
        }
        let activate = SendMessageW(
            hwnd,
            WM_MOUSEACTIVATE,
            Some(WPARAM(owner.0 as usize)),
            Some(LPARAM((HTCLIENT | (WM_LBUTTONDOWN << 16)) as isize)),
        );
        if activate.0 != MA_NOACTIVATE as isize {
            return Err(format!(
                "The recording overlay mouse activation returned {}",
                activate.0
            ));
        }
        if GetForegroundWindow().0 as isize != foreground.0 {
            return Err("The recording overlay changed the foreground window".into());
        }
    }
    Ok(())
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
    let fixture = CAPTURE_KEY_DOWN.load(std::sync::atomic::Ordering::Relaxed) as u16;
    let key = if fixture == 0 {
        services::last_key_down()?
    } else {
        fixture
    };
    if matches!(key, 0x10..=0x12 | 0x5b | 0x5c | 0xa0..=0xa5)
        || (fixture == 0 && !down(VIRTUAL_KEY(key)))
    {
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

static CAPTURE_KEY_DOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Supplies a virtual key to the isolated UI harness, without touching the keyboard.
pub fn set_capture_shortcut_key(key: u16) {
    assert!(std::env::var_os("ANNOTATE_UI_TEST").is_some());
    CAPTURE_KEY_DOWN.store(key as u32, std::sync::atomic::Ordering::Relaxed);
}

/// Posts Caps Lock to the capture window without changing the system lock state.
pub fn post_capture_capslock(window: &impl HasWindowHandle) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN},
    };
    assert!(std::env::var_os("ANNOTATE_UI_TEST").is_some());
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(0x14), LPARAM(1));
        }
    }
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
        if rgba.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]) {
            return Err("The captured window is blank".into());
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

/// Exercises GPUI's DPI-change path for reference captures without changing the
/// desktop scale. Call off the UI thread so synchronous WM_SIZE reaches GPUI
/// while its app is not borrowed. SendMessage keeps the suggested RECT alive.
pub fn set_capture_dpi(
    owner: DialogOwner,
    dpi: u32,
    width: f32,
    height: f32,
) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{LPARAM, RECT, WPARAM},
        UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, SendMessageW, WM_DPICHANGED},
    };
    let hwnd = HWND(owner.0 as *mut _);
    let scale = dpi as f32 / 96.;
    let (width, height) = (
        (width * scale).round() as i32,
        (height * scale).round() as i32,
    );
    unsafe {
        let mut outer = RECT::default();
        let mut client = RECT::default();
        GetWindowRect(hwnd, &mut outer).map_err(|e| e.to_string())?;
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
        outer.right += width - (client.right - client.left);
        outer.bottom += height - (client.bottom - client.top);
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            Some(WPARAM((dpi | (dpi << 16)) as usize)),
            Some(LPARAM((&outer as *const RECT) as isize)),
        );
        GetClientRect(hwnd, &mut client).map_err(|e| e.to_string())?;
        let actual = (client.right - client.left, client.bottom - client.top);
        if actual != (width, height) {
            return Err(format!(
                "DPI capture client is {}x{}, expected {width}x{height}",
                actual.0, actual.1
            ));
        }
    }
    Ok(())
}

/// Posts an operating-system close request (UI interaction checks).
pub fn post_close(window: &impl HasWindowHandle) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE},
    };
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Posts a left click at a logical client position (UI interaction checks).
pub fn post_click(window: &impl HasWindowHandle, x: f32, y: f32) {
    post_mouse_button(window, x, y, true);
    post_mouse_button(window, x, y, false);
}

/// Presses or releases the left mouse button (UI interaction checks).
pub fn post_mouse_button(window: &impl HasWindowHandle, x: f32, y: f32, down: bool) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    };
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            let (x, y) = ((x * scale) as isize, (y * scale) as isize);
            let point = LPARAM((x & 0xffff) | ((y & 0xffff) << 16));
            let _ = PostMessageW(Some(hwnd), WM_MOUSEMOVE, WPARAM(0), point);
            let message = if down { WM_LBUTTONDOWN } else { WM_LBUTTONUP };
            let _ = PostMessageW(Some(hwnd), message, WPARAM(usize::from(down)), point);
        }
    }
}

/// Posts a mouse wheel turn at a logical client position (UI interaction checks).
pub fn post_wheel(window: &impl HasWindowHandle, x: f32, y: f32, notches: i16) {
    use windows::Win32::{
        Foundation::{LPARAM, POINT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    };
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            let mut point = POINT {
                x: (x * scale) as i32,
                y: (y * scale) as i32,
            };
            let _ = ClientToScreen(hwnd, &mut point);
            let delta = (notches as i32 * 120) as u16 as usize;
            let position =
                LPARAM((point.x as i16 as u16 as isize) | ((point.y as i16 as u16 as isize) << 16));
            let _ = PostMessageW(Some(hwnd), WM_MOUSEWHEEL, WPARAM(delta << 16), position);
        }
    }
}

/// GPUI's public keystroke helper sends only key-down; buttons click on key-up.
pub fn post_key_up(window: &impl HasWindowHandle, key: &str) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::{PostMessageW, WM_KEYUP},
    };
    let key = match key {
        "enter" => 0x0d,
        "space" => 0x20,
        "escape" => 0x1b,
        "tab" => 0x09,
        "up" => 0x26,
        "down" => 0x28,
        key if key.len() == 1 && key.is_ascii() => key.as_bytes()[0].to_ascii_uppercase() as usize,
        _ => return,
    };
    if let Some(hwnd) = hwnd(window) {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(key), LPARAM(0xc0000001));
        }
    }
}

/// Drags from one logical client position to another (UI interaction checks).
pub fn post_drag(window: &impl HasWindowHandle, from: (f32, f32), to: (f32, f32)) {
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    };
    if let Some(hwnd) = hwnd(window) {
        let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.;
        for (message, buttons, (x, y)) in [
            (WM_MOUSEMOVE, 0, from),
            (WM_LBUTTONDOWN, 1, from),
            (WM_MOUSEMOVE, 1, to),
            (WM_LBUTTONUP, 0, to),
        ] {
            let (x, y) = ((x * scale) as isize, (y * scale) as isize);
            let position = LPARAM((x & 0xffff) | ((y & 0xffff) << 16));
            unsafe {
                let _ = PostMessageW(Some(hwnd), message, WPARAM(buttons), position);
            }
        }
    }
}
