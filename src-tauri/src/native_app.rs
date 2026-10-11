//! Native lifecycle, tray, global shortcuts, settings, and async runtime.
use serde_json::Value;
use sha2::Digest;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering},
        Arc, Mutex, OnceLock,
    },
};
use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::*,
        System::{LibraryLoader::*, Threading::*},
        UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SETTINGS: OnceLock<Arc<Settings>> = OnceLock::new();
static THREAD: AtomicU32 = AtomicU32::new(0);
static CLIPBOARD_OWNER: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const REGISTER: u32 = WM_APP + 61;
const TRAY: u32 = WM_APP + 62;
const QUIT: u32 = WM_APP + 63;
const RELEASE: u32 = WM_APP + 64;
pub fn clipboard_owner() -> Option<HWND> {
    let owner = CLIPBOARD_OWNER.load(Ordering::Acquire);
    (owner != 0).then_some(HWND(owner as *mut core::ffi::c_void))
}
pub fn initialize_runtime() -> Result<(), String> {
    RUNTIME
        .set(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|_| "Runtime already initialized".into())
}
pub fn spawn<F: std::future::Future + Send + 'static>(
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F::Output: Send + 'static,
{
    RUNTIME.get().unwrap().spawn(future)
}
pub fn spawn_blocking<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(
    f: F,
) -> tokio::task::JoinHandle<R> {
    RUNTIME.get().unwrap().spawn_blocking(f)
}

#[derive(Clone)]
pub struct AppHandle;
impl AppHandle {
    pub fn path(&self) -> &Self {
        self
    }
    pub fn app_local_data_dir(&self) -> Result<PathBuf, String> {
        std::env::var_os("ANNOTATE_DATA_DIR")
            .or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(|p| PathBuf::from(p).join("com.annotate.app").into_os_string())
            })
            .map(PathBuf::from)
            .ok_or("LOCALAPPDATA is not available".into())
    }
    pub fn store(&self, _name: &str) -> Result<Arc<Settings>, String> {
        if let Some(settings) = SETTINGS.get() {
            return Ok(settings.clone());
        }
        let directory = std::env::var_os("ANNOTATE_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("com.annotate.app"))
            })
            .ok_or("APPDATA is not available")?;
        let path = directory.join("settings.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Cannot read settings; original file preserved: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        let settings = Arc::new(Settings {
            path,
            data: Mutex::new(data),
        });
        let _ = SETTINGS.set(settings);
        Ok(SETTINGS.get().unwrap().clone())
    }
    pub fn emit<T: serde::Serialize>(&self, event: &str, payload: T) -> Result<(), String> {
        let value = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        if event == "subtitle-progress" {
            let total = value["total"].as_f64().unwrap_or(1.0).max(1.0);
            crate::native_core::get().change(|s| {
                s.progress.insert(
                    "sub".into(),
                    (
                        (value["current"].as_f64().unwrap_or(0.0) / total) as f32,
                        value["message"].as_str().unwrap_or("Working...").into(),
                    ),
                );
            });
        } else if let Some(key) = event.strip_suffix("-download-progress") {
            let current = value[0].as_f64().unwrap_or(0.0);
            let total = value[1].as_f64().unwrap_or(1.0).max(1.0);
            crate::native_core::get().change(|s| {
                s.progress.insert(
                    key.into(),
                    (
                        (current / total) as f32,
                        format!("{:.0}%", current / total * 100.0),
                    ),
                );
            });
        }
        Ok(())
    }
    pub fn exit(&self, _code: i32) {
        unsafe {
            let thread = THREAD.load(Ordering::Acquire);
            if thread != 0 {
                let _ = PostThreadMessageW(thread, QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}
pub struct Settings {
    path: PathBuf,
    data: Mutex<BTreeMap<String, Value>>,
}
impl Settings {
    pub fn get(&self, key: &str) -> Option<Value> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }
    fn save_locked(&self, data: &BTreeMap<String, Value>) -> Result<(), String> {
        crate::native_data::atomic_write(
            &self.path,
            &serde_json::to_vec(data).map_err(|e| e.to_string())?,
        )
    }
    pub fn update_and_save(
        &self,
        update: impl FnOnce(&mut BTreeMap<String, Value>),
    ) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let previous = data.clone();
        update(&mut data);
        if let Err(error) = self.save_locked(&data) {
            *data = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn set_and_save(&self, key: &str, value: Value) -> Result<(), String> {
        self.update_and_save(|data| {
            data.insert(key.into(), value);
        })
    }
}

#[derive(Clone)]
struct Shortcut {
    mods: HOT_KEY_MODIFIERS,
    key: u32,
    pressed: bool,
    injected: bool,
}
static SHORTCUTS: Mutex<[Option<Shortcut>; 2]> = Mutex::new([None, None]);
struct RegisterRequest {
    slot: usize,
    combo: String,
    cancelled: Arc<AtomicBool>,
    reply: std::sync::mpsc::SyncSender<Result<(), String>>,
}
pub fn register(slot: usize, combo: &str) -> Result<(), String> {
    if slot >= 2 {
        return Err("Unknown hotkey slot".into());
    }
    let thread = THREAD.load(Ordering::Acquire);
    if thread == 0 {
        return Err("Native shortcut runtime is not running".into());
    }
    if unsafe { GetCurrentThreadId() } == thread {
        return unsafe { register_on_thread(slot, combo) };
    }
    let (send, recv) = std::sync::mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let request = Box::new(RegisterRequest {
        slot,
        combo: combo.into(),
        cancelled: cancelled.clone(),
        reply: send,
    });
    let pointer = Box::into_raw(request);
    if let Err(error) =
        unsafe { PostThreadMessageW(thread, REGISTER, WPARAM(0), LPARAM(pointer as isize)) }
    {
        unsafe {
            drop(Box::from_raw(pointer));
        }
        return Err(error.to_string());
    }
    match recv.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(result) => result,
        Err(error) => {
            cancelled.store(true, Ordering::Release);
            Err(format!("Cannot register shortcut: {error}"))
        }
    }
}

// Stable names preserve existing settings and also avoid localized key labels
// that cannot be parsed when a user records the same shortcut on another PC.
pub fn key_name(vk: u16) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5a => return char::from_u32(vk as u32).unwrap().to_string(),
        0x60..=0x69 => return format!("Numpad{}", vk - 0x60),
        0x70..=0x87 => return format!("F{}", vk - 0x70 + 1),
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0c => "Clear",
        0x0d => "Enter",
        0x13 => "Pause",
        0x14 => "CapsLock",
        0x1b => "Escape",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "ArrowLeft",
        0x26 => "ArrowUp",
        0x27 => "ArrowRight",
        0x28 => "ArrowDown",
        0x2c => "PrintScreen",
        0x2d => "Insert",
        0x2e => "Delete",
        0x5d => "ContextMenu",
        0x6a => "NumpadMultiply",
        0x6b => "NumpadAdd",
        0x6c => "NumpadComma",
        0x6d => "NumpadSubtract",
        0x6e => "NumpadDecimal",
        0x6f => "NumpadDivide",
        0x90 => "NumLock",
        0x91 => "ScrollLock",
        0xba => ";",
        0xbb => "=",
        0xbc => ",",
        0xbd => "-",
        0xbe => ".",
        0xbf => "/",
        0xc0 => "`",
        0xdb => "[",
        0xdc => "\\",
        0xdd => "]",
        0xde => "'",
        0xe2 => "IntlBackslash",
        _ => return format!("VK{vk:02X}"),
    }
    .into()
}
fn parse_shortcut(combo: &str) -> Result<Shortcut, String> {
    let mut mods = MOD_NOREPEAT;
    let mut key = None;
    for part in combo.split('+') {
        let part = part.trim().to_ascii_lowercase();
        let modifier = match part.as_str() {
            "ctrl" | "control" => Some(MOD_CONTROL),
            "alt" => Some(MOD_ALT),
            "shift" => Some(MOD_SHIFT),
            "super" | "win" | "meta" => Some(MOD_WIN),
            _ => None,
        };
        if let Some(modifier) = modifier {
            mods |= modifier;
            continue;
        }
        let parsed = match part.as_str() {
            "left" => Some(0x25),
            "up" => Some(0x26),
            "right" => Some(0x27),
            "down" => Some(0x28),
            "numpadenter" => Some(0x0d),
            "backquote" => Some(0xc0),
            "minus" => Some(0xbd),
            "equal" => Some(0xbb),
            "bracketleft" => Some(0xdb),
            "bracketright" => Some(0xdd),
            "backslash" => Some(0xdc),
            "semicolon" => Some(0xba),
            "quote" => Some(0xde),
            "comma" => Some(0xbc),
            "period" => Some(0xbe),
            "slash" => Some(0xbf),
            _ => (1..=254u16)
                .find(|vk| key_name(*vk).eq_ignore_ascii_case(&part))
                .map(u32::from),
        }
        .ok_or_else(|| format!("Unsupported shortcut: {combo}"))?;
        if key.replace(parsed).is_some() {
            return Err("A shortcut must contain one non-modifier key".into());
        }
    }
    let key = key.ok_or_else(|| format!("Invalid shortcut: {combo}"))?;
    if matches!(key,0x10..=0x12|0x5b|0x5c|0xa0..=0xa5) {
        return Err("Choose a non-modifier key".into());
    }
    Ok(Shortcut {
        mods,
        key,
        pressed: false,
        injected: false,
    })
}
unsafe fn register_on_thread(slot: usize, combo: &str) -> Result<(), String> {
    let shortcut = if combo.is_empty() {
        None
    } else {
        Some(parse_shortcut(combo)?)
    };
    let mut slots = SHORTCUTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(new) = &shortcut {
        if slots.iter().enumerate().any(|(i, s)| {
            i != slot
                && s.as_ref()
                    .is_some_and(|s| s.key == new.key && s.mods == new.mods)
        }) {
            return Err("Shortcut is already used by another hotkey".into());
        }
        if slots[slot]
            .as_ref()
            .is_some_and(|old| old.key == new.key && old.mods == new.mods)
        {
            return Ok(());
        }
    }
    let old = slots[slot].take();
    let _ = UnregisterHotKey(None, slot as i32 + 1);
    if let Some(new) = &shortcut {
        if let Err(error) = RegisterHotKey(None, slot as i32 + 1, new.mods, new.key) {
            if let Some(old) = &old {
                if let Err(restore) = RegisterHotKey(None, slot as i32 + 1, old.mods, old.key) {
                    let release = old.pressed;
                    drop(slots);
                    if release {
                        crate::native_core::get().hotkey(prefix(slot), false);
                    }
                    return Err(format!("Failed to register shortcut: {error}; restoring previous shortcut failed: {restore}"));
                }
            }
            slots[slot] = old;
            return Err(format!("Failed to register shortcut: {error}"));
        }
    }
    let release = old.as_ref().is_some_and(|s| s.pressed);
    slots[slot] = shortcut;
    drop(slots);
    if release {
        crate::native_core::get().hotkey(prefix(slot), false);
    }
    Ok(())
}
fn prefix(slot: usize) -> &'static str {
    if slot == 0 {
        "hotkey"
    } else {
        "dict-hotkey"
    }
}
fn released_modifier(mods: HOT_KEY_MODIFIERS, key: u32) -> bool {
    match key {
        0x10 | 0xa0 | 0xa1 => mods.contains(MOD_SHIFT),
        0x11 | 0xa2 | 0xa3 => mods.contains(MOD_CONTROL),
        0x12 | 0xa4 | 0xa5 => mods.contains(MOD_ALT),
        0x5b | 0x5c => mods.contains(MOD_WIN),
        _ => false,
    }
}
unsafe fn opposite_modifier_down(key: u32) -> bool {
    let opposite = match key {
        0xa0 => 0xa1,
        0xa1 => 0xa0,
        0xa2 => 0xa3,
        0xa3 => 0xa2,
        0xa4 => 0xa5,
        0xa5 => 0xa4,
        0x5b => 0x5c,
        0x5c => 0x5b,
        _ => return false,
    };
    GetAsyncKeyState(opposite) < 0
}
unsafe fn still_down(shortcut: &Shortcut) -> bool {
    GetAsyncKeyState(shortcut.key as i32) < 0
        && (!shortcut.mods.contains(MOD_CONTROL) || GetAsyncKeyState(VK_CONTROL.0 as i32) < 0)
        && (!shortcut.mods.contains(MOD_ALT) || GetAsyncKeyState(VK_MENU.0 as i32) < 0)
        && (!shortcut.mods.contains(MOD_SHIFT) || GetAsyncKeyState(VK_SHIFT.0 as i32) < 0)
        && (!shortcut.mods.contains(MOD_WIN)
            || GetAsyncKeyState(VK_LWIN.0 as i32) < 0
            || GetAsyncKeyState(VK_RWIN.0 as i32) < 0)
}
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let keyboard = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let injected = keyboard.flags.contains(LLKHF_INJECTED);
        let mut slots = SHORTCUTS.lock().unwrap_or_else(|e| e.into_inner());
        for (slot, shortcut) in slots.iter_mut().enumerate() {
            let Some(shortcut) = shortcut else {
                continue;
            };
            if matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN)
                && keyboard.vkCode == shortcut.key
                && !shortcut.pressed
            {
                shortcut.injected = injected;
            } else if matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP)
                && shortcut.pressed
                && (!injected || shortcut.injected)
                && (keyboard.vkCode == shortcut.key
                    || (released_modifier(shortcut.mods, keyboard.vkCode)
                        && !opposite_modifier_down(keyboard.vkCode)))
            {
                shortcut.pressed = false;
                // Never do file I/O or app-state work inside a low-level hook.
                // Windows removes hooks that exceed the callback time limit.
                let _ = PostThreadMessageW(
                    THREAD.load(Ordering::Acquire),
                    RELEASE,
                    WPARAM(slot),
                    LPARAM(0),
                );
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
unsafe fn hotkey_message(msg: &MSG) {
    if crate::native_core::get()
        .snapshot()
        .recording_hotkey
        .is_some()
    {
        return;
    }
    let slot = msg.wParam.0.saturating_sub(1);
    let held = {
        let mut slots = SHORTCUTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(Some(shortcut)) = slots.get_mut(slot) else {
            return;
        };
        let key = (msg.lParam.0 as u32 >> 16) & 0xffff;
        let mods = msg.lParam.0 as u32 & 0xffff;
        if shortcut.pressed || key != shortcut.key || mods != shortcut.mods.0 & !MOD_NOREPEAT.0 {
            return;
        }
        let held = still_down(shortcut);
        shortcut.pressed = held;
        held
    };
    crate::native_core::get().hotkey(prefix(slot), true);
    // WM_HOTKEY can still be queued when the user has already released a quick
    // tap. Pair it immediately rather than waiting for a release that has passed.
    if !held {
        crate::native_core::get().hotkey(prefix(slot), false);
    }
}

fn tray_data(hwnd: HWND) -> Result<NOTIFYICONDATAW, String> {
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let icon = LoadIconW(
            Some(HINSTANCE(instance.0)),
            windows::core::PCWSTR(1usize as *const u16),
        )
        .or_else(|_| LoadIconW(None, IDI_APPLICATION))
        .map_err(|e| e.to_string())?;
        let mut tray = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY,
            hIcon: icon,
            ..Default::default()
        };
        for (i, c) in "Annotate".encode_utf16().enumerate() {
            tray.szTip[i] = c;
        }
        Ok(tray)
    }
}
unsafe extern "system" fn runtime_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg != 0 && msg == TASKBAR_CREATED.load(Ordering::Acquire) {
        if let Ok(tray) = tray_data(hwnd) {
            let _ = Shell_NotifyIconW(NIM_ADD, &tray);
        }
        return LRESULT(0);
    }
    match msg {
        TRAY => {
            let event = lparam.0 as u32 & 0xffff;
            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                crate::native_ui::show();
            } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                if let Ok(menu) = CreatePopupMenu() {
                    let _ = AppendMenuW(menu, MF_STRING, 1, w!("Show Annotate"));
                    let _ = AppendMenuW(menu, MF_STRING, 2, w!("Quit"));
                    let mut point = POINT::default();
                    let _ = GetCursorPos(&mut point);
                    let _ = SetForegroundWindow(hwnd);
                    let selected = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        point.x,
                        point.y,
                        Some(0),
                        hwnd,
                        None,
                    );
                    let _ = DestroyMenu(menu);
                    // Required after a notification menu so the next menu also
                    // dismisses correctly when the user clicks elsewhere.
                    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                    if selected.0 == 1 {
                        crate::native_ui::show();
                    }
                    if selected.0 == 2 {
                        AppHandle.exit(0);
                    }
                }
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            crate::native_ui::show();
            LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        CLIPBOARD_OWNER.store(0, Ordering::Release);
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}
struct Hook(HHOOK);
impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.0);
        }
    }
}
struct Tray(NOTIFYICONDATAW);
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.0);
        }
    }
}

fn instance_identity(directory: &std::path::Path) -> String {
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(directory)
    };
    let resolved = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let path = resolved.to_string_lossy();
    let path = if let Some(unc) = path.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{unc}")
    } else {
        path.strip_prefix("\\\\?\\").unwrap_or(&path).into()
    };
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}
pub fn run() -> Result<(), String> {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let directory = AppHandle.app_local_data_dir()?;
        let normalized = instance_identity(&directory);
        let hash = sha2::Sha256::digest(normalized.as_bytes());
        let name = HSTRING::from(format!("Local\\AnnotateNative-{hash:x}"));
        let mutex = CreateMutexW(None, false, &name).map_err(|e| e.to_string())?;
        let duplicate = GetLastError() == ERROR_ALREADY_EXISTS;
        let _mutex = Handle(mutex);
        let title = HSTRING::from(format!("Annotate runtime {hash:x}"));
        if duplicate {
            // The primary process may still be between creating its mutex and HWND.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if let Ok(existing) = FindWindowW(w!("AnnotateNativeRuntime"), &title) {
                    let mut process = 0;
                    GetWindowThreadProcessId(existing, Some(&mut process));
                    let _ = AllowSetForegroundWindow(process);
                    if SendMessageTimeoutW(
                        existing,
                        WM_COPYDATA,
                        WPARAM(0),
                        LPARAM(0),
                        SMTO_ABORTIFHUNG,
                        1000,
                        None,
                    )
                    .0 != 0
                    {
                        return Ok(());
                    }
                    return Err("The running Annotate instance is not responding".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("The running Annotate instance has not finished starting".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        if RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(runtime_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: w!("AnnotateNativeRuntime"),
            ..Default::default()
        }) == 0
        {
            return Err(windows::core::Error::from_win32().to_string());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("AnnotateNativeRuntime"),
            &title,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )
        .map_err(|e| e.to_string())?;
        let _window = Window(hwnd);
        CLIPBOARD_OWNER.store(hwnd.0 as isize, Ordering::Release);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Release,
        );
        initialize_runtime()?;
        THREAD.store(GetCurrentThreadId(), Ordering::Release);
        let _ = crate::throbber::initialize();
        crate::native_core::initialize(AppHandle)?;
        crate::native_ui::initialize()?;
        let _hook = Hook(
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook),
                Some(HINSTANCE(instance.0)),
                0,
            )
            .map_err(|e| e.to_string())?,
        );
        let tray = tray_data(hwnd)?;
        if !Shell_NotifyIconW(NIM_ADD, &tray).as_bool() {
            crate::native_core::get().status("Could not add the system tray icon", true);
        }
        let _tray = Tray(tray);
        crate::native_ui::show();
        let result = loop {
            let received = GetMessageW(&mut msg, None, 0, 0).0;
            if received == 0 {
                break Ok(());
            }
            if received < 0 {
                break Err(windows::core::Error::from_win32().to_string());
            }
            match msg.message {
                QUIT => break Ok(()),
                REGISTER => {
                    let request = Box::from_raw(msg.lParam.0 as *mut RegisterRequest);
                    if !request.cancelled.load(Ordering::Acquire) {
                        let _ = request
                            .reply
                            .send(register_on_thread(request.slot, &request.combo));
                    }
                }
                RELEASE => crate::native_core::get().hotkey(prefix(msg.wParam.0), false),
                WM_HOTKEY => hotkey_message(&msg),
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        };
        THREAD.store(0, Ordering::Release);
        for slot in 0..2 {
            let _ = UnregisterHotKey(None, slot + 1);
        }
        crate::native_core::get().shutdown();
        crate::google_auth::cancel_google_oauth();
        let _ = crate::throbber::hide();
        crate::native_ui::shutdown();
        RUNTIME.get().unwrap().block_on(async {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                crate::whisper_local::unload(),
            )
            .await;
        });
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_names_round_trip_and_only_relevant_modifiers_release() {
        for key in [0x20, 0x25, 0x69, 0x87, 0xba, 0xbb, 0xc0, 0xe2] {
            let shortcut = parse_shortcut(&format!("Control+Alt+{}", key_name(key))).unwrap();
            assert_eq!(shortcut.key, key as u32);
            assert!(released_modifier(shortcut.mods, VK_LCONTROL.0 as u32));
            assert!(!released_modifier(shortcut.mods, VK_LSHIFT.0 as u32));
        }
        assert_eq!(parse_shortcut("Ctrl+`").unwrap().key, VK_OEM_3.0 as u32);
        assert_eq!(parse_shortcut("ArrowLeft").unwrap().key, VK_LEFT.0 as u32);
        assert!(parse_shortcut("Ctrl").is_err());
        assert!(parse_shortcut("Ctrl+A+B").is_err());
        assert!(parse_shortcut("F25").is_err());
    }
    #[test]
    fn settings_updates_are_serialized_and_rollback_when_save_fails() {
        let directory = std::env::temp_dir().join(format!(
            "annotate-settings-test-{}-{}",
            std::process::id(),
            crate::native_data::now()
        ));
        let settings = Arc::new(Settings {
            path: directory.join("settings.json"),
            data: Mutex::new(BTreeMap::new()),
        });
        let identity = instance_identity(&directory);
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let settings = settings.clone();
                std::thread::spawn(move || {
                    settings
                        .set_and_save(&format!("setting-{index}"), serde_json::json!(index))
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(instance_identity(&directory), identity);
        let data: Value = serde_json::from_slice(&std::fs::read(&settings.path).unwrap()).unwrap();
        assert_eq!(data.as_object().unwrap().len(), 4);
        std::fs::remove_file(&settings.path).unwrap();
        std::fs::create_dir(&settings.path).unwrap();
        assert!(settings
            .set_and_save("must-not-survive", Value::Bool(true))
            .is_err());
        assert!(settings.get("must-not-survive").is_none());
        std::fs::remove_dir(&settings.path).unwrap();
        std::fs::remove_file(settings.path.with_extension("tmp")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
