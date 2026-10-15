//! AppKit clipboard access and targeted Command+C / Command+V events.
use super::util::{nsstr, on_main, string};
use cocoa::base::{id, nil, BOOL, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::{
    ffi::c_void,
    sync::Mutex,
    time::{Duration, Instant},
};

static INPUT: Mutex<()> = Mutex::new(());
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: id) -> bool;
    static kAXTrustedCheckOptionPrompt: id;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *const c_void;
    fn CGEventSetFlags(event: *const c_void, flags: u64);
    fn CGEventPostToPid(pid: i32, event: *const c_void);
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const c_void);
}

fn command(key: u16) -> Result<(), String> {
    on_main(move || unsafe {
        if !AXIsProcessTrusted() {
            let yes: id = msg_send![class!(NSNumber), numberWithBool: YES];
            let options: id = msg_send![class!(NSDictionary), dictionaryWithObject: yes forKey: kAXTrustedCheckOptionPrompt];
            AXIsProcessTrustedWithOptions(options);
            return Err("Allow Annotate in System Settings > Privacy & Security > Accessibility, then try again.".into());
        }
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let application: id = msg_send![workspace, frontmostApplication];
        if application == nil {
            return Err("No application is available for keyboard input".into());
        }
        let pid: i32 = msg_send![application, processIdentifier];
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(std::ptr::null(), key, down);
            if event.is_null() {
                return Err("macOS could not create a keyboard event".into());
            }
            CGEventSetFlags(event, if down { 1 << 20 } else { 0 });
            CGEventPostToPid(pid, event);
            CFRelease(event);
        }
        Ok(())
    })
}
pub fn sanitize_text_for_input(text: &str) -> String {
    text.replace(['\0', '\u{200b}', '\u{feff}'], "")
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{00a0}', " ")
}
fn clipboard() -> id {
    unsafe { msg_send![class!(NSPasteboard), generalPasteboard] }
}
fn write(text: &str) -> Result<isize, String> {
    let text = text.to_owned();
    on_main(move || unsafe {
        let board = clipboard();
        let _: isize = msg_send![board, clearContents];
        let ok: BOOL =
            msg_send![board, setString: nsstr(&text) forType: nsstr("public.utf8-plain-text")];
        if ok != YES {
            return Err("macOS could not write to the clipboard".into());
        }
        Ok(msg_send![board, changeCount])
    })
}
pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    let _guard = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    write(text).map(|_| ())
}
pub fn paste_text(text: &str) -> Result<(), String> {
    let _guard = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    let count = write(&sanitize_text_for_input(text))?;
    command(9)?;
    crate::app::spawn(async move {
        tokio::time::sleep(Duration::from_millis(220)).await;
        on_main(move || unsafe {
            let board = clipboard();
            let current: isize = msg_send![board, changeCount];
            if current == count {
                let _: isize = msg_send![board, clearContents];
            }
        });
    });
    Ok(())
}

type Contents = Vec<Vec<(String, Vec<u8>)>>;
fn snapshot() -> Contents {
    on_main(|| unsafe {
        let board = clipboard();
        let items: id = msg_send![board, pasteboardItems];
        let count: usize = msg_send![items, count];
        let mut contents = Vec::with_capacity(count);
        for i in 0..count {
            let item: id = msg_send![items, objectAtIndex: i];
            let types: id = msg_send![item, types];
            let count: usize = msg_send![types, count];
            let mut formats = Vec::with_capacity(count);
            for j in 0..count {
                let kind: id = msg_send![types, objectAtIndex: j];
                let data: id = msg_send![item, dataForType: kind];
                if data == nil {
                    continue;
                }
                let size: usize = msg_send![data, length];
                let bytes: *const u8 = msg_send![data, bytes];
                let bytes = if size == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(bytes, size).to_vec()
                };
                formats.push((string(kind), bytes));
            }
            contents.push(formats);
        }
        contents
    })
}
fn restore(contents: Contents, expected: isize) {
    on_main(move || unsafe {
        let board = clipboard();
        let count: isize = msg_send![board, changeCount];
        if count != expected {
            return;
        }
        let _: isize = msg_send![board, clearContents];
        let items: id = msg_send![class!(NSMutableArray), array];
        for formats in contents {
            let item: id = msg_send![class!(NSPasteboardItem), new];
            for (kind, bytes) in formats {
                let data: id =
                    msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() length: bytes.len()];
                let _: BOOL = msg_send![item, setData: data forType: nsstr(&kind)];
            }
            let _: () = msg_send![items, addObject: item];
            let _: () = msg_send![item, release];
        }
        let _: BOOL = msg_send![board, writeObjects: items];
    });
}
pub fn capture_selected_text(release_delay_ms: Option<u64>) -> Result<String, String> {
    let _guard = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::sleep(Duration::from_millis(
        release_delay_ms.unwrap_or(100).min(500),
    ));
    let before = snapshot();
    let marker = write("")?;
    if let Err(error) = command(8) {
        restore(before, marker);
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_millis(1000);
    loop {
        let (count, selected) = on_main(|| unsafe {
            let board = clipboard();
            let count: isize = msg_send![board, changeCount];
            let text: id = msg_send![board, stringForType: nsstr("public.utf8-plain-text")];
            (count, string(text))
        });
        if count != marker || Instant::now() >= deadline {
            restore(before, count);
            return if count != marker && !selected.trim().is_empty() {
                Ok(selected)
            } else {
                Err(
                    "No text was copied. Select text in the target application and try again."
                        .into(),
                )
            };
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
