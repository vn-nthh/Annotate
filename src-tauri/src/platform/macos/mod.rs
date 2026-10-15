//! AppKit/Carbon services and CoreAudio capture, behind the shared native core.
mod audio;
mod data;
mod input;
mod services;
mod util;
mod worker;

use cocoa::{
    base::{id, nil, BOOL, NO, YES},
    foundation::{NSPoint, NSRect, NSSize},
};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::PathBuf;
use util::{nsstr, on_main, post_main, string};

pub use audio::{enumerate_capture_devices, Capture};
pub use data::{compare_terms, legacy_storage_paths, lock_legacy_storage};
pub use input::{capture_selected_text, paste_text, sanitize_text_for_input, set_clipboard_text};
pub use services::{
    acquire_instance, key_name, pressed_shortcut, register as register_shortcut, Instance,
};
#[cfg(test)]
pub use worker::test_worker;
pub use worker::{
    configure_whisper_worker, hide_console, WorkerJob, FFMPEG_FILENAME, FFMPEG_SHA256, FFMPEG_URL,
};

pub fn local_data_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join("Library/Application Support"))
        .ok_or("The home directory is unavailable".into())
}
pub fn roaming_data_dir() -> Result<PathBuf, String> {
    local_data_dir()
}
pub fn mark_executable(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())
}
pub fn start_services(instance: &Instance) -> Result<(), String> {
    services::start(instance)
}
pub fn stop_services() {
    services::stop();
}

unsafe fn ns_window(window: &impl HasWindowHandle) -> Option<id> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => {
            let view = handle.ns_view.as_ptr() as id;
            let window: id = msg_send![view, window];
            (window != nil).then_some(window)
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub struct DialogOwner(isize);
pub fn dialog_owner(window: &impl HasWindowHandle) -> Option<DialogOwner> {
    unsafe { ns_window(window).map(|w| DialogOwner(msg_send![w, windowNumber])) }
}
unsafe fn window_number(number: isize) -> id {
    let app: id = msg_send![class!(NSApplication), sharedApplication];
    msg_send![app, windowWithWindowNumber: number]
}

pub fn prepare_overlay(window: &impl HasWindowHandle, _: f32, _: f32, _: f32) {
    let Some(owner) = dialog_owner(window) else {
        return;
    };
    post_main(move || unsafe {
        let window = window_number(owner.0);
        if window == nil {
            return;
        }
        let _: () = msg_send![window, setStyleMask: 0usize];
        let _: () = msg_send![window, setIgnoresMouseEvents: YES];
        let _: () = msg_send![window, setHasShadow: NO];
        let _: () = msg_send![window, setOpaque: NO];
        let clear: id = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![window, setBackgroundColor: clear];
        let _: () = msg_send![window, setLevel: 25isize];
        let _: () = msg_send![window, setCollectionBehavior: (1usize | (1 << 8))];
    });
}
pub fn show_overlay(window: &impl HasWindowHandle, width: f32, height: f32, bottom: f32) {
    let Some(owner) = dialog_owner(window) else {
        return;
    };
    post_main(move || unsafe {
        let window = window_number(owner.0);
        if window == nil {
            return;
        }
        let screen: id = msg_send![class!(NSScreen), mainScreen];
        if screen == nil {
            return;
        }
        let frame: NSRect = msg_send![screen, frame];
        let rect = NSRect::new(
            NSPoint::new(
                frame.origin.x + (frame.size.width - width as f64) / 2.,
                frame.origin.y + bottom as f64,
            ),
            NSSize::new(width as f64, height as f64),
        );
        let _: () = msg_send![window, setFrame: rect display: YES];
        let _: () = msg_send![window, orderFrontRegardless];
    });
}
pub fn hide_overlay(window: &impl HasWindowHandle) {
    let Some(owner) = dialog_owner(window) else {
        return;
    };
    post_main(move || unsafe {
        let window = window_number(owner.0);
        if window != nil {
            let _: () = msg_send![window, orderOut: nil];
        }
    });
}
pub fn animations_enabled() -> bool {
    on_main(|| unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let reduced: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduced == NO
    })
}
pub fn style_main_window(window: &impl HasWindowHandle) {
    unsafe {
        if let Some(window) = ns_window(window) {
            let _: () = msg_send![window, setMovableByWindowBackground: YES];
        }
    }
}
pub fn move_offscreen(window: &impl HasWindowHandle) {
    let Some(owner) = dialog_owner(window) else {
        return;
    };
    post_main(move || unsafe {
        let window = window_number(owner.0);
        if window != nil {
            let _: () = msg_send![window, setFrameOrigin: NSPoint::new(-10000., -10000.)];
        }
    });
}

pub fn show_fatal_error(message: &str) {
    let message = message.to_owned();
    on_main(move || unsafe {
        let alert: id = msg_send![class!(NSAlert), new];
        let _: () = msg_send![alert, setMessageText: nsstr("Annotate could not start")];
        let _: () = msg_send![alert, setInformativeText: nsstr(&message)];
        let _: isize = msg_send![alert, runModal];
        let _: () = msg_send![alert, release];
    });
}
pub fn format_time(timestamp: u64) -> String {
    objc::rc::autoreleasepool(move || unsafe {
        let date: id =
            msg_send![class!(NSDate), dateWithTimeIntervalSince1970: timestamp as f64 / 1000.];
        let time: id = msg_send![class!(NSDateFormatter), localizedStringFromDate: date dateStyle: 0usize timeStyle: 1usize];
        let calendar: id = msg_send![class!(NSCalendar), currentCalendar];
        let today: BOOL = msg_send![calendar, isDateInToday: date];
        if today == YES {
            return string(time);
        }
        let formatter: id = msg_send![class!(NSDateFormatter), new];
        let _: () = msg_send![formatter, setDateFormat: nsstr("MMM d")];
        let day: id = msg_send![formatter, stringFromDate: date];
        let result = format!("{} {}", string(day), string(time));
        let _: () = msg_send![formatter, release];
        result
    })
}

pub fn pick_media_file(owner: DialogOwner) -> Result<Option<PathBuf>, String> {
    pick(owner, None)
}
pub fn pick_srt_destination(owner: DialogOwner, name: &str) -> Result<Option<PathBuf>, String> {
    pick(owner, Some(name.to_owned()))
}

fn pick(owner: DialogOwner, save: Option<String>) -> Result<Option<PathBuf>, String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static OPEN: AtomicBool = AtomicBool::new(false);
    if OPEN.swap(true, Ordering::AcqRel) {
        return Err("A file dialog is already open".into());
    }
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            OPEN.store(false, Ordering::Release);
        }
    }
    let _guard = Guard;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    on_main(move || unsafe {
        let window = window_number(owner.0);
        if window == nil {
            return Err("The file dialog's window has closed".to_string());
        }
        let panel: id = if save.is_some() {
            msg_send![class!(NSSavePanel), savePanel]
        } else {
            msg_send![class!(NSOpenPanel), openPanel]
        };
        if let Some(name) = save {
            let _: () = msg_send![panel, setNameFieldStringValue: nsstr(&name)];
        } else {
            let _: () = msg_send![panel, setCanChooseDirectories: NO];
            let _: () = msg_send![panel, setAllowsMultipleSelection: NO];
        }
        let callback = block::ConcreteBlock::new(move |result: isize| {
            let path = if result == 1 {
                let url: id = msg_send![panel, URL];
                let path: id = msg_send![url, path];
                Some(PathBuf::from(string(path)))
            } else {
                None
            };
            let _ = sender.send(path);
        })
        .copy();
        let _: () =
            msg_send![panel, beginSheetModalForWindow: window completionHandler: &*callback];
        Ok(())
    })?;
    receiver
        .recv()
        .map_err(|_| "The file dialog closed without a result".into())
}
