//! Clipboard text, synthetic paste/copy, and selection capture.
use std::ptr::null_mut;
use std::time::Instant;
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

const CF_UNICODETEXT_FORMAT: u32 = 13;

pub fn paste_text(text: &str) -> Result<(), String> {
    let sanitize_start = Instant::now();
    let sanitized = sanitize_text_for_input(text);
    log::info!(
        "[Timing][paste] sanitize: {:.1}ms chars={}",
        sanitize_start.elapsed().as_secs_f64() * 1000.0,
        sanitized.chars().count()
    );

    let clipboard_start = Instant::now();
    set_clipboard_text(&sanitized)?;
    log::info!(
        "[Timing][paste] set clipboard: {:.1}ms",
        clipboard_start.elapsed().as_secs_f64() * 1000.0
    );

    let input_start = Instant::now();
    send_ctrl_combo(VK_V, "Ctrl+V")?;
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

struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self, String> {
        Self::open_with_owner(null_mut())
    }

    fn open_for_write() -> Result<Self, String> {
        let owner = super::services::clipboard_owner()
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

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

pub fn set_clipboard_text(text: &str) -> Result<(), String> {
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

fn read_clipboard_unicode_text() -> Result<Option<String>, String> {
    let _guard = ClipboardGuard::open()?;
    read_open_clipboard_unicode_text()
}

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

/// Force-release modifier keys the user may still be holding from their hotkey,
/// so a following synthetic Ctrl+C isn't polluted (e.g. into Ctrl+Alt+C).
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
pub fn capture_selected_text(release_delay_ms: Option<u64>) -> Result<String, String> {
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
    set_clipboard_text(&sentinel)?;

    send_ctrl_combo(VK_C, "Ctrl+C")?;

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
        Some(text) => set_clipboard_text(text),
        None => clear_injected_clipboard_if_unchanged(&sentinel),
    };
    if let Err(err) = restore_result {
        log::warn!("[Clipboard] restore after capture failed: {}", err);
    }

    Ok(captured.unwrap_or_default())
}

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

/// Windows edit controls expect CRLF and choke on some invisible characters.
pub fn sanitize_text_for_input(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::sanitize_text_for_input;

    #[test]
    fn sanitizes_text_for_windows_clipboard_paste() {
        let text = "a\u{00A0}b\u{200B}c\r\nd\re\n\u{FEFF}f";
        assert_eq!(sanitize_text_for_input(text), "a bc\r\nd\r\ne\r\nf");
    }
}
