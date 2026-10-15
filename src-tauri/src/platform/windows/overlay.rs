//! Makes a GPUI window behave as the recording overlay: topmost, click-through,
//! never activated, and absent from the taskbar and Alt+Tab.
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        HiDpi::GetDpiForWindow,
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const MA_NOACTIVATE: isize = 3;
// Placement runs from the window's own message rather than inside GPUI code:
// SetWindowPos sends WM_SIZE synchronously, and GPUI drops resize callbacks
// that arrive while the app is already borrowed.
const PLACE: u32 = WM_APP + 70;
const HIDE: u32 = WM_APP + 71;

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        PLACE => {
            let show = wparam.0 >> 16 != 0;
            let bottom = (wparam.0 & 0xffff) as f32;
            let width = (lparam.0 & 0xffff) as f32;
            let height = ((lparam.0 >> 16) & 0xffff) as f32;
            place(hwnd, width, height, bottom, show);
            LRESULT(0)
        }
        HIDE => {
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
        }
        _ => DefSubclassProc(hwnd, msg, wparam, lparam),
    }
}

pub fn prepare(hwnd: HWND, width: f32, height: f32, bottom: f32) {
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            style
                | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT).0
                    as isize,
        );
        let _ = SetWindowSubclass(hwnd, Some(overlay_proc), 1, 0);
    }
    // Size it while hidden so the first show already has its final layout.
    post(hwnd, PLACE, width, height, bottom, false);
}

fn post(hwnd: HWND, msg: u32, width: f32, height: f32, bottom: f32, show: bool) {
    let wparam = (bottom as usize & 0xffff) | (usize::from(show) << 16);
    let lparam = (width as isize & 0xffff) | ((height as isize & 0xffff) << 16);
    unsafe {
        let _ = PostMessageW(Some(hwnd), msg, WPARAM(wparam), LPARAM(lparam));
    }
}

/// Shows the window centered `bottom` logical px above the primary display's
/// bottom edge, matching the Tauri overlay's position.
pub fn show(hwnd: HWND, width: f32, height: f32, bottom: f32) {
    post(hwnd, PLACE, width, height, bottom, true);
}

pub fn hide(hwnd: HWND) {
    post(hwnd, HIDE, 0., 0., 0., false);
}

unsafe fn place(hwnd: HWND, width: f32, height: f32, bottom: f32, show: bool) {
    let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
    let width = (width * scale).round() as i32;
    let height = (height * scale).round() as i32;
    let margin = (bottom * scale).round() as i32;
    let screen_width = GetSystemMetrics(SM_CXSCREEN);
    let screen_height = GetSystemMetrics(SM_CYSCREEN);
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        (screen_width - width) / 2,
        screen_height - height - margin,
        width,
        height,
        SWP_NOACTIVATE | if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW },
    );
}

/// "Show animations in Windows" (Settings > Accessibility > Visual effects).
pub fn animations_enabled() -> bool {
    use windows::core::BOOL;
    let mut enabled = BOOL(1);
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .is_ok()
            && enabled.as_bool()
    }
}
