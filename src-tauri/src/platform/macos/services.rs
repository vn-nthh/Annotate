//! Carbon shortcut events and AppKit menu actions share GPUI's main run loop.
use super::util::{nsstr, on_main, post_main};
use cocoa::{
    base::{id, nil, YES},
    foundation::NSSize,
};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{Class, Object, Sel},
    sel, sel_impl,
};
use sha2::Digest;
use std::{
    cell::RefCell,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::Path,
    sync::OnceLock,
};

pub struct Instance {
    file: File,
    notification: String,
}
pub fn acquire_instance(directory: &Path) -> Result<Option<Instance>, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let identity = std::fs::canonicalize(directory).map_err(|e| e.to_string())?;
    let hash = sha2::Sha256::digest(identity.as_os_str().as_encoded_bytes());
    let notification = format!("com.annotate.app.activate.{hash:x}");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("instance.lock"))
        .map_err(|e| e.to_string())?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        file.set_len(0).map_err(|e| e.to_string())?;
        return Ok(Some(Instance { file, notification }));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(error.to_string());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut ready = String::new();
        file.read_to_string(&mut ready).map_err(|e| e.to_string())?;
        if let Ok(pid) = ready.parse::<i32>() {
            on_main(move || unsafe {
                let center: id = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
                let _: () = msg_send![center, postNotificationName: nsstr(&notification) object: nil userInfo: nil deliverImmediately: YES];
                let app: id = msg_send![class!(NSRunningApplication), runningApplicationWithProcessIdentifier: pid];
                if app != nil {
                    let _: bool = msg_send![app, activateWithOptions: 2usize];
                }
            });
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            return Err("The running Annotate instance has not finished starting".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

struct Services {
    manager: GlobalHotKeyManager,
    shortcuts: [Option<HotKey>; 2],
    pressed: [bool; 2],
    status: id,
    target: id,
    observer: id,
}
thread_local! { static SERVICES: RefCell<Option<Services>> = const { RefCell::new(None) }; }
extern "C" fn menu_action(_: &Object, _: Sel, sender: id) {
    let tag: isize = unsafe { msg_send![sender, tag] };
    match tag {
        1 => crate::ui::show_settings(),
        2 => crate::ui::quit(),
        _ => crate::ui::show(),
    }
}
pub fn start(instance: &Instance) -> Result<(), String> {
    let notification = instance.notification.clone();
    on_main(move || unsafe {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
            post_main(move || {
                let action = SERVICES.with(|state| {
                    let mut state = state.borrow_mut();
                    let state = state.as_mut()?;
                    let slot = state
                        .shortcuts
                        .iter()
                        .position(|s| s.is_some_and(|key| key.id() == event.id()))?;
                    let down = event.state() == HotKeyState::Pressed;
                    if state.pressed[slot] == down
                        || (down && crate::native_core::get().recording_hotkey_active())
                    {
                        return None;
                    }
                    state.pressed[slot] = down;
                    Some((slot, down))
                });
                if let Some((slot, down)) = action {
                    crate::native_core::get()
                        .hotkey(if slot == 0 { "hotkey" } else { "dict-hotkey" }, down);
                }
            });
        }));
        static MENU_CLASS: OnceLock<usize> = OnceLock::new();
        let class = *MENU_CLASS.get_or_init(|| {
            let mut class = ClassDecl::new("AnnotateMenuActions", class!(NSObject)).unwrap();
            class.add_method(
                sel!(perform:),
                menu_action as extern "C" fn(&Object, Sel, id),
            );
            class.register() as *const Class as usize
        }) as *const Class;
        let target: id = msg_send![class, new];
        let bar: id = msg_send![class!(NSStatusBar), systemStatusBar];
        let status: id = msg_send![bar, statusItemWithLength: -1.0f64];
        let _: id = msg_send![status, retain];
        let button: id = msg_send![status, button];
        let icon: id = msg_send![class!(NSImage), imageNamed: nsstr("NSApplicationIcon")];
        if icon != nil {
            let image: id = msg_send![icon, copy];
            let _: () = msg_send![image, setSize: NSSize::new(18., 18.)];
            let _: () = msg_send![image, setTemplate: YES];
            let _: () = msg_send![button, setImage: image];
            let _: () = msg_send![image, release];
        } else {
            let _: () = msg_send![button, setTitle: nsstr("Annotate")];
        }
        let menu: id = msg_send![class!(NSMenu), new];
        for (tag, title) in ["Open Annotate", "Settings", "Quit"].iter().enumerate() {
            let item: id = msg_send![class!(NSMenuItem), alloc];
            let item: id = msg_send![item, initWithTitle: nsstr(title) action: sel!(perform:) keyEquivalent: nsstr("")];
            let _: () = msg_send![item, setTarget: target];
            let _: () = msg_send![item, setTag: tag as isize];
            let _: () = msg_send![menu, addItem: item];
            let _: () = msg_send![item, release];
        }
        let _: () = msg_send![status, setMenu: menu];
        let _: () = msg_send![menu, release];
        let callback = block::ConcreteBlock::new(|_: id| crate::ui::show()).copy();
        let center: id = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let queue: id = msg_send![class!(NSOperationQueue), mainQueue];
        let observer: id = msg_send![center, addObserverForName: nsstr(&notification) object: nil queue: queue usingBlock: &*callback];
        let _: id = msg_send![observer, retain];
        SERVICES.with(|state| {
            *state.borrow_mut() = Some(Services {
                manager,
                shortcuts: [None, None],
                pressed: [false; 2],
                status,
                target,
                observer,
            })
        });
        Ok(())
    })?;
    let mut file = &instance.file;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| write!(file, "{}", std::process::id()))
        .and_then(|_| file.flush())
        .map_err(|e| e.to_string())
}
pub fn stop() {
    on_main(|| {
        SERVICES.with(|state| {
            if let Some(state) = state.borrow_mut().take() {
                unsafe {
                    let center: id =
                        msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
                    let _: () = msg_send![center, removeObserver: state.observer];
                    let _: () = msg_send![state.observer, release];
                    let bar: id = msg_send![class!(NSStatusBar), systemStatusBar];
                    let _: () = msg_send![bar, removeStatusItem: state.status];
                    let _: () = msg_send![state.status, release];
                    let _: () = msg_send![state.target, release];
                }
            }
        })
    });
}
pub fn register(slot: usize, combo: &str) -> Result<(), String> {
    if slot >= 2 {
        return Err("Unknown hotkey slot".into());
    }
    let hotkey = if combo.trim().is_empty() {
        None
    } else {
        Some(parse(combo)?)
    };
    on_main(move || {
        SERVICES.with(|state| {
            let mut state = state.borrow_mut();
            let state = state
                .as_mut()
                .ok_or("The shortcut runtime is not running")?;
            if state.shortcuts[slot] == hotkey {
                return Ok(());
            }
            if hotkey.is_some() && state.shortcuts[1 - slot] == hotkey {
                return Err("This shortcut is already used by Annotate".into());
            }
            if let Some(new) = hotkey {
                state
                    .manager
                    .register(new)
                    .map_err(|e| format!("This shortcut is unavailable: {e}"))?;
            }
            if let Some(old) = state.shortcuts[slot] {
                if let Err(error) = state.manager.unregister(old) {
                    if let Some(new) = hotkey {
                        let _ = state.manager.unregister(new);
                    }
                    return Err(error.to_string());
                }
            }
            state.shortcuts[slot] = hotkey;
            state.pressed[slot] = false;
            Ok(())
        })
    })
}
fn parse(combo: &str) -> Result<HotKey, String> {
    let combo = combo.replace("Win+", "Super+").replace("Cmd+", "Super+");
    let key = combo.parse::<HotKey>().map_err(|e| e.to_string())?;
    if matches!(key.key, Code::CapsLock | Code::Fn) {
        return Err("Choose a letter, number, or function key for hold-to-talk on macOS.".into());
    }
    if key.mods == Modifiers::SUPER && matches!(key.key, Code::KeyC | Code::KeyV) {
        return Err(
            "Command+C and Command+V are reserved for copy and paste. Choose another shortcut."
                .into(),
        );
    }
    Ok(key)
}
pub fn pressed_shortcut() -> Option<String> {
    on_main(|| unsafe {
        let app: id = msg_send![class!(NSApplication), sharedApplication];
        let event: id = msg_send![app, currentEvent];
        if event == nil {
            return None;
        }
        let code: u16 = msg_send![event, keyCode];
        if matches!(code, 54..=63) {
            return None;
        }
        let flags: u64 = msg_send![event, modifierFlags];
        let mut parts = Vec::new();
        for (flag, name) in [
            (1 << 18, "Ctrl"),
            (1 << 19, "Alt"),
            (1 << 17, "Shift"),
            (1 << 20, "Super"),
        ] {
            if flags & flag != 0 {
                parts.push(name.to_owned());
            }
        }
        parts.push(key_name(code));
        Some(parts.join("+"))
    })
}
pub fn key_name(code: u16) -> String {
    let name = match code {
        0 => "A",
        1 => "S",
        2 => "D",
        3 => "F",
        4 => "H",
        5 => "G",
        6 => "Z",
        7 => "X",
        8 => "C",
        9 => "V",
        11 => "B",
        12 => "Q",
        13 => "W",
        14 => "E",
        15 => "R",
        16 => "Y",
        17 => "T",
        18 => "1",
        19 => "2",
        20 => "3",
        21 => "4",
        22 => "6",
        23 => "5",
        24 => "Equal",
        25 => "9",
        26 => "7",
        27 => "Minus",
        28 => "8",
        29 => "0",
        30 => "BracketRight",
        31 => "O",
        32 => "U",
        33 => "BracketLeft",
        34 => "I",
        35 => "P",
        36 => "Enter",
        37 => "L",
        38 => "J",
        39 => "Quote",
        40 => "K",
        41 => "Semicolon",
        42 => "Backslash",
        43 => "Comma",
        44 => "Slash",
        45 => "N",
        46 => "M",
        47 => "Period",
        48 => "Tab",
        49 => "Space",
        50 => "Backquote",
        51 => "Backspace",
        53 => "Escape",
        64 => "F17",
        79 => "F18",
        80 => "F19",
        90 => "F20",
        96 => "F5",
        97 => "F6",
        98 => "F7",
        99 => "F3",
        100 => "F8",
        101 => "F9",
        103 => "F11",
        105 => "F13",
        106 => "F16",
        107 => "F14",
        109 => "F10",
        111 => "F12",
        113 => "F15",
        114 => "Insert",
        115 => "Home",
        116 => "PageUp",
        117 => "Delete",
        118 => "F4",
        119 => "End",
        120 => "F2",
        121 => "PageDown",
        122 => "F1",
        123 => "ArrowLeft",
        124 => "ArrowRight",
        125 => "ArrowDown",
        126 => "ArrowUp",
        _ => return format!("Key{code}"),
    };
    name.to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortcuts_keep_physical_key_names_and_reject_copy_paste() {
        for code in [0, 12, 49, 100, 122, 126] {
            assert!(super::parse(&format!("Ctrl+Alt+{}", super::key_name(code))).is_ok());
        }
        assert!(super::parse("Cmd+C").is_err());
        assert!(super::parse("Super+V").is_err());
        assert!(super::parse("CapsLock").is_err());
    }
}
