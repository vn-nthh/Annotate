//! Win32 controls with Direct2D/DirectWrite painting of the original UI.
//! The reference JSON is measured at build/development time, never by Chromium
//! at runtime. Controls retain native keyboard, IME, and accessibility behavior.
use crate::native_core::{get, State};
use crate::native_onboarding::{self as setup, Onboarding, Status};
use serde::Deserialize;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
};
use windows::Win32::{
    System::{SystemInformation::*, Time::*},
    UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
use windows::{
    core::{w, Interface, HSTRING},
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::Common::*, Direct2D::*, DirectWrite::*, Dxgi::Common::*, Gdi::*, Imaging::*,
        },
        System::{Com::*, LibraryLoader::*, Threading::*},
        UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};
use windows_numerics::Vector2;

include!(concat!(env!("OUT_DIR"), "/native-assets.rs"));
static THREAD: AtomicU32 = AtomicU32::new(0);
static UI_THREAD: OnceLock<Mutex<Option<std::thread::JoinHandle<()>>>> = OnceLock::new();
const REFRESH: u32 = WM_APP + 43;
const SHOW: u32 = WM_APP + 44;
const HIDE: u32 = WM_APP + 45;
const PICK: u32 = WM_APP + 46;
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct Node {
    kind: String,
    id: String,
    owner: String,
    #[serde(rename = "box")]
    rect: [f32; 4],
    classes: String,
    text: String,
    placeholder: String,
    password: bool,
    color: String,
    background: String,
    border: String,
    width: f32,
    radius: f32,
    dashed: bool,
    size: f32,
    weight: i32,
    line: f32,
    spacing: f32,
    align: String,
    opacity: f32,
    padding: [f32; 4],
    options: Vec<[String; 2]>,
    src: String,
    clip: [f32; 4],
    disabled: bool,
    title: String,
    wrap: f32,
    nowrap: bool,
}
#[derive(Clone, Deserialize)]
struct Scene {
    nodes: Vec<Node>,
    height: f32,
    #[serde(default)]
    fixture: serde_json::Value,
}
/// Reference scenes, parsed on first use. Most windows show only a few of
/// them, so parsing every scene up front would cost memory and startup time.
struct Scenes {
    raw: HashMap<&'static str, &'static serde_json::value::RawValue>,
    parsed: std::cell::RefCell<HashMap<&'static str, std::rc::Rc<Scene>>>,
}
impl Scenes {
    fn load() -> Result<Self, String> {
        Ok(Self {
            raw: serde_json::from_str(include_str!("../assets/native-reference.json"))
                .map_err(|e| e.to_string())?,
            parsed: Default::default(),
        })
    }
    fn get(&self, key: &str) -> Option<std::rc::Rc<Scene>> {
        let (&key, raw) = self.raw.get_key_value(key)?;
        if let Some(scene) = self.parsed.borrow().get(key) {
            return Some(scene.clone());
        }
        let scene = std::rc::Rc::new(serde_json::from_str::<Scene>(raw.get()).ok()?);
        self.parsed.borrow_mut().insert(key, scene.clone());
        Some(scene)
    }
    /// A scene that the reference always contains.
    fn at(&self, key: &str) -> std::rc::Rc<Scene> {
        self.get(key)
            .unwrap_or_else(|| panic!("Missing native reference scene: {key}"))
    }
    fn keys(&self) -> Vec<String> {
        self.raw.keys().map(|k| k.to_string()).collect()
    }
}
#[derive(Default)]
struct Memory {
    tab: String,
    scroll: HashMap<String, f32>,
    drafts: HashMap<String, String>,
}
struct Control {
    hwnd: HWND,
    id: String,
    draft: String,
    dirty: bool,
    kind: String,
    rect: [f32; 4],
    inner: [i32; 4],
    options: Vec<[String; 2]>,
    font: HFONT,
}
struct Window {
    hwnd: HWND,
    memory: Memory,
    state: State,
    scenes: Scenes,
    renderer: Renderer,
    nodes: Vec<Node>,
    controls: Vec<Control>,
    pixels: Vec<u8>,
    height: f32,
    dpi: f32,
    modal: bool,
    hover: String,
    mouse: [f32; 2],
    drag_scroll: bool,
    show_keys: bool,
    show_summary_key: bool,
    rebuilding: bool,
    tooltip: HWND,
    tooltip_texts: Vec<HSTRING>,
    copied: Option<(String, std::time::Instant)>,
    preview_scroll: f32,
    modal_focus: Option<String>,
    /// The "Try it" step saw the hotkey start a recording, then a paste.
    try_listening: bool,
    try_done: bool,
}

pub fn initialize() -> Result<(), String> {
    let (send, recv) = mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name("annotate-native-ui".into())
        .spawn(move || unsafe {
            let result = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
            if let Err(e) = result {
                let _ = send.send(Err(e.to_string()));
                return;
            }
            let thread = GetCurrentThreadId();
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            THREAD.store(thread, Ordering::Release);
            let _ = send.send(Ok(()));
            let mut window: Option<Box<Window>> = None;
            let mut memory = Memory {
                tab: "home".into(),
                ..Default::default()
            };
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.hwnd.0.is_null() {
                    match msg.message {
                        SHOW => {
                            // wParam 1 asks for the Settings tab (first-run setup).
                            let settings = msg.wParam.0 == 1;
                            if let Some(w) = window.as_mut() {
                                if settings && w.memory.tab != "settings" {
                                    w.commit_edits();
                                    w.memory.tab = "settings".into();
                                    w.update(true);
                                }
                                let _ = ShowWindow(w.hwnd, SW_RESTORE);
                                let _ = SetForegroundWindow(w.hwnd);
                            } else {
                                if settings {
                                    memory.tab = "settings".into();
                                }
                                match Window::create(std::mem::take(&mut memory)) {
                                    Ok(mut w) => {
                                        SetWindowLongPtrW(
                                            w.hwnd,
                                            GWLP_USERDATA,
                                            (&mut *w as *mut Window) as isize,
                                        );
                                        w.update(true);
                                        let _ = ShowWindow(w.hwnd, SW_SHOW);
                                        crate::native_benchmark::shown(w.hwnd);
                                        let _ = SetForegroundWindow(w.hwnd);
                                        window = Some(w);
                                    }
                                    Err(e) => {
                                        log::error!("Native UI: {e}");
                                        get().status(&e, true);
                                    }
                                }
                            }
                        }
                        REFRESH => {
                            if let Some(w) = window.as_mut() {
                                w.state = get().snapshot();
                                w.update(false);
                            }
                        }
                        PICK => {
                            if let Some(w) = window.as_mut() {
                                w.choose_file(msg.wParam.0 != 0);
                            }
                        }
                        HIDE => {
                            if let Some(mut w) = window.take() {
                                memory = std::mem::take(&mut w.memory);
                                drop(w);
                            }
                            crate::native_benchmark::hidden();
                        }
                        _ => {}
                    }
                } else {
                    if let Some(w) = window.as_mut() {
                        if w.pretranslate(&msg) || IsDialogMessageW(w.hwnd, &msg).as_bool() {
                            continue;
                        }
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if window.as_ref().is_some_and(|w| w.hwnd.0.is_null()) {
                    let mut w = window.take().unwrap();
                    memory = std::mem::take(&mut w.memory);
                }
            }
            if let Some(w) = window.as_mut() {
                w.commit_edits();
            }
            drop(window);
            THREAD.store(0, Ordering::Release);
            CoUninitialize();
        })
        .map_err(|e| e.to_string())?;
    let _ = UI_THREAD.set(Mutex::new(Some(handle)));
    recv.recv().map_err(|e| e.to_string())?
}
pub fn refresh() {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}
pub fn show() {
    post_show(0);
}
/// Shows the window on the Settings tab, e.g. when no engine is configured yet.
pub fn show_settings() {
    post_show(1);
}
fn post_show(tab: usize) {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, SHOW, WPARAM(tab), LPARAM(0));
        }
    }
}
pub fn hide() {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, HIDE, WPARAM(0), LPARAM(0));
        }
    }
}
pub fn shutdown() {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(handle) = UI_THREAD
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|mut h| h.take()))
    {
        let _ = handle.join();
    }
}
pub fn pick_file(save: bool) {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        unsafe {
            let _ = PostThreadMessageW(thread, PICK, WPARAM(usize::from(save)), LPARAM(0));
        }
    }
}

/// Render all supported native screens through the real DirectWrite/Win32 path.
/// Uses an isolated data directory and never reads credentials or contacts APIs.
pub fn capture_reference(directory: &std::path::Path) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let mut window = Window::create(Memory {
            tab: "home".into(),
            ..Default::default()
        })?;
        SetWindowLongPtrW(
            window.hwnd,
            GWLP_USERDATA,
            (&mut *window as *mut Window) as isize,
        );
        // The isolated profile is a new install; only the setup scenes show the guide.
        get().set_setting(setup::KEY, Onboarding::initial(true, "groq").to_value())?;
        let initial = get().snapshot();
        let mut keys = window.scenes.keys();
        keys.sort();
        let mut controls = HashMap::new();
        for key in keys {
            window.state = initial.clone();
            window.state.mics = vec![crate::AudioDevice {
                id: String::new(),
                name: "System Default".into(),
            }];
            window.memory.drafts.clear();
            window.preview_scroll = 0.0;
            let parts: Vec<_> = key.split(':').collect();
            window.modal = parts[1] == "modal";
            window.memory.tab = if window.modal || parts[1] == "onboarding" {
                "home"
            } else {
                parts[1].split('-').next().unwrap_or("home")
            }
            .into();
            window.memory.scroll.clear();
            window.mouse = [-1.0, -1.0];
            window.hover.clear();
            window
                .state
                .settings
                .insert("theme".into(), serde_json::json!(parts[0]));
            if parts[1] == "onboarding" {
                let mut guide = Onboarding::initial(false, parts.get(3).copied().unwrap_or("groq"));
                guide.step = setup::STEPS
                    .iter()
                    .position(|(id, _)| *id == parts[2])
                    .unwrap_or(0);
                window
                    .state
                    .settings
                    .insert(setup::KEY.into(), guide.to_value());
            }
            if parts[1] == "settings" {
                window
                    .state
                    .settings
                    .insert("transcriptionMode".into(), serde_json::json!(parts[2]));
                window.state.settings.insert(
                    "grammarCleanupEnabled".into(),
                    serde_json::json!(parts[4] == "1"),
                );
                window.state.user = (parts[5] == "in").then(
                    || serde_json::json!({"name":"Example User","email":"example@example.com","picture":"fixture"}),
                );
                window.state.avatar = (parts[5] == "in").then(|| {
                    Arc::new(include_bytes!("../assets/profile-avatar-fixture.png").to_vec())
                });
                window.state.auth_waiting = parts[5] == "waiting";
            }
            let fixture = window.scenes.at(&key).fixture.clone();
            window.apply_fixture(&fixture)?;
            window.update(true);
            save_png(
                &directory.join(format!("{}.png", key.replace(':', "-"))),
                window.renderer.width,
                window.renderer.height,
                &window.capture_pixels(WM_PRINT)?,
            )?;
            controls.insert(
                key.clone(),
                window
                    .controls
                    .iter()
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>(),
            );
            if key.ends_with(":settings:local:vulkan:1:in") {
                window.wheel(-10000.0);
                window.update(true);
                save_png(
                    &directory.join(format!("{}-settings-bottom.png", window.theme())),
                    window.renderer.width,
                    window.renderer.height,
                    &window.capture_pixels(WM_PRINT)?,
                )?;
            }
        }
        std::fs::write(
            directory.join("controls.json"),
            serde_json::to_vec_pretty(&controls).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let checks = window.check_interactions(directory)?;
        std::fs::write(
            directory.join("checks.json"),
            serde_json::to_vec_pretty(&checks).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        drop(window);
        CoUninitialize();
        Ok(())
    }
}
fn save_png(path: &std::path::Path, width: u32, height: u32, bgra: &[u8]) -> Result<(), String> {
    let mut rgba = bgra.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| e.to_string())?
        .write_image_data(&rgba)
        .map_err(|e| e.to_string())
}

impl Window {
    unsafe fn create(memory: Memory) -> Result<Box<Self>, String> {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: w!("AnnotateNativeSettings"),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            hIcon: LoadIconW(
                Some(HINSTANCE(instance.0)),
                windows::core::PCWSTR(1usize as *const u16),
            )
            .unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let dpi = GetDpiForSystem() as f32 / 96.0;
        let width = (520.0 * dpi).round() as i32;
        let height = (580.0 * dpi).round() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW | WS_EX_COMPOSITED,
            w!("AnnotateNativeSettings"),
            w!("Annotate"),
            WS_POPUP | WS_MINIMIZEBOX | WS_CLIPCHILDREN,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )
        .map_err(|e| e.to_string())?;
        let region = CreateRoundRectRgn(
            0,
            0,
            width + 1,
            height + 1,
            (20.0 * dpi) as i32,
            (20.0 * dpi) as i32,
        );
        SetWindowRgn(hwnd, Some(region), true);
        let renderer = match Renderer::new(dpi) {
            Ok(renderer) => renderer,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e.to_string());
            }
        };
        let scenes = Scenes::load()?;
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            None,
            WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP),
            0,
            0,
            0,
            0,
            Some(hwnd),
            None,
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        Ok(Box::new(Self {
            hwnd,
            memory,
            state: get().snapshot(),
            scenes,
            renderer,
            nodes: vec![],
            controls: vec![],
            pixels: vec![],
            height: 532.0,
            dpi,
            modal: false,
            hover: String::new(),
            mouse: [-1.0, -1.0],
            drag_scroll: false,
            show_keys: false,
            show_summary_key: false,
            rebuilding: false,
            tooltip,
            tooltip_texts: vec![],
            copied: None,
            preview_scroll: 0.0,
            modal_focus: None,
            try_listening: false,
            try_done: false,
        }))
    }
    fn theme(&self) -> &str {
        if self.state.dark() {
            "dark"
        } else {
            "light"
        }
    }
    fn apply_fixture(&mut self, value: &serde_json::Value) -> Result<(), String> {
        if let Some(file) = value["file"].as_str() {
            self.state.file = Some(file.into());
        }
        if let Some(entries) = value.get("entries") {
            self.state.entries =
                serde_json::from_value(entries.clone()).map_err(|e| e.to_string())?;
        }
        if let Some(summary) = value["summary"].as_str() {
            self.state.summary = summary.into();
        }
        if value["openrouter"].as_bool() == Some(true) {
            self.state
                .labels
                .insert("openrouter-key-input".into(), "UI fixture only".into());
        }
        if let Some(ready) = value["ready"].as_array() {
            for name in ready.iter().filter_map(|v| v.as_str()) {
                self.state.ready.insert(name.into());
            }
        }
        let time = value["time"].as_u64().unwrap_or(1700000000000);
        if let Some(terms) = value["dictionary"].as_array() {
            self.state.data.data.dictionary.terms = terms
                .iter()
                .filter_map(|v| v.as_str())
                .map(|value| crate::native_data::Term {
                    value: value.into(),
                    updated_at: time,
                })
                .collect();
        }
        if let Some(entries) = value["home"].as_array() {
            self.state.data.data.history.entries = entries
                .iter()
                .filter_map(|v| v.as_str())
                .map(|text| crate::native_data::Entry {
                    text: text.into(),
                    time,
                    updated_at: time,
                })
                .collect();
        }
        Ok(())
    }
    unsafe fn check_interactions(
        &mut self,
        directory: &std::path::Path,
    ) -> Result<Vec<&'static str>, String> {
        let mut checks = vec![];
        self.modal = false;
        self.memory.scroll.clear();
        self.memory.tab = "settings".into();
        self.state = get().snapshot();
        self.state
            .settings
            .insert("transcriptionMode".into(), serde_json::json!("local"));
        self.update(true);
        if self
            .controls
            .iter()
            .find(|c| c.id == "mode-select")
            .unwrap()
            .options
            .iter()
            .map(|o| o[0].as_str())
            .collect::<Vec<_>>()
            != ["groq", "azure-mai", "local"]
        {
            return Err("Engine options mismatch".into());
        }
        if self
            .controls
            .iter()
            .find(|c| c.id == "acceleration-select")
            .unwrap()
            .options
            .iter()
            .map(|o| o[0].as_str())
            .collect::<Vec<_>>()
            != ["vulkan"]
        {
            return Err("Acceleration options mismatch".into());
        }
        checks.push("Only Groq, Azure MAI, and local Vulkan are selectable");
        for theme in ["light", "dark"] {
            self.state
                .settings
                .insert("theme".into(), serde_json::json!(theme));
            self.memory.scroll.clear();
            self.update(true);
            for delta in [-120i16, 120] {
                for _ in 0..20 {
                    SendMessageW(
                        self.hwnd,
                        WM_MOUSEWHEEL,
                        Some(WPARAM((delta as u16 as usize) << 16)),
                        None,
                    );
                    if IsWindowVisible(self.hwnd).as_bool() {
                        return Err("Updating hidden UI unexpectedly showed its window".into());
                    }
                    let erased = self.capture_pixels(WM_ERASEBKGND)?;
                    if erased
                        .chunks_exact(4)
                        .zip(self.pixels.chunks_exact(4))
                        .any(|(actual, expected)| actual[..3] != expected[..3])
                    {
                        return Err(format!("Native controls erase to an unthemed background while scrolling ({theme})"));
                    }
                }
            }
        }
        checks.push("Control background erasure matches the full themed frame throughout scrolling in both themes");
        self.check_wheel_routing()?;
        checks.push("Scrolling keeps its input target visible throughout control rebuilding, including fractional deltas over controls");
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        for id in ["titlebar-close", "titlebar-minimize"] {
            let c = self
                .controls
                .iter()
                .find(|c| c.id == id)
                .ok_or_else(|| format!("Missing native titlebar button: {id}"))?;
            let hwnd = c.hwnd;
            let center = LPARAM((c.inner[2] / 2) as isize | (((c.inner[3] / 2) as isize) << 16));
            let mut point = POINT {
                x: c.inner[0] + c.inner[2] / 2,
                y: c.inner[1] + c.inner[3] / 2,
            };
            SendMessageW(hwnd, WM_MOUSEMOVE, None, Some(center));
            if !self.controls.iter().any(|c| c.id == id && c.hwnd == hwnd)
                || !IsWindowVisible(hwnd).as_bool()
                || !IsWindowEnabled(hwnd).as_bool()
            {
                return Err(format!(
                    "Titlebar hover replaced or disabled its button: {id}"
                ));
            }
            if ChildWindowFromPointEx(self.hwnd, point, CWP_SKIPINVISIBLE | CWP_SKIPDISABLED)
                != hwnd
            {
                return Err(format!(
                    "Titlebar hit testing missed its native button: {id}"
                ));
            }
            let _ = ClientToScreen(self.hwnd, &mut point);
            if SendMessageW(
                self.hwnd,
                WM_NCHITTEST,
                None,
                Some(LPARAM(
                    point.x as u16 as isize | ((point.y as u16 as isize) << 16),
                )),
            )
            .0 != HTCLIENT as isize
            {
                return Err(format!(
                    "Caption dragging intercepted a titlebar button: {id}"
                ));
            }
            // Exercise the stock BUTTON and BN_CLICKED path, rather than click().
            SendMessageW(
                hwnd,
                WM_LBUTTONDOWN,
                Some(WPARAM(1)), // MK_LBUTTON
                Some(center),
            );
            SendMessageW(hwnd, WM_LBUTTONUP, None, Some(center));
            if id == "titlebar-close" {
                if !self.modal || !self.controls.iter().any(|c| c.id == "modal-tray") {
                    return Err("Native X button did not open the close dialog".into());
                }
                let cancel = self
                    .controls
                    .iter()
                    .find(|c| c.id == "modal-cancel")
                    .unwrap()
                    .hwnd;
                SendMessageW(cancel, BM_CLICK, None, None);
                if self.modal {
                    return Err("Native Cancel button did not close the dialog".into());
                }
            } else {
                if !IsIconic(self.hwnd).as_bool() {
                    return Err("Native minimize button did not minimize its window".into());
                }
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
        }
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        self.hover.clear();
        self.update(false);
        checks.push("Native titlebar buttons survive hover; X opens the close dialog and minimize minimizes the window");
        let home = self
            .controls
            .iter()
            .find(|c| c.id == "tab:home")
            .unwrap()
            .hwnd;
        let dictionary = self
            .controls
            .iter()
            .find(|c| c.id == "tab:dictionary")
            .unwrap()
            .hwnd;
        SendMessageW(home, WM_MOUSEMOVE, None, None);
        SendMessageW(dictionary, WM_MOUSEMOVE, None, None);
        let hovered_frame = self.capture_pixels(WM_PRINT)?;
        SendMessageW(home, WM_MOUSELEAVE, None, None);
        if self.hover != "tab:dictionary" || self.capture_pixels(WM_PRINT)? != hovered_frame {
            return Err("A previous menu item cleared the current item's hover highlight".into());
        }
        checks.push("Leaving a previous menu item preserves the current item's hover highlight");
        self.hover.clear();
        self.state.user = Some(serde_json::json!({"name":"Example User","picture":"fixture"}));
        for bytes in [
            include_bytes!("../assets/profile-avatar-fixture.png").as_slice(),
            include_bytes!("../assets/profile-avatar-fixture.jpg").as_slice(),
        ] {
            self.state.avatar = Some(Arc::new(bytes.to_vec()));
            self.update(true);
            if !self.renderer.images.contains_key("native-profile-avatar") {
                return Err("Native profile photo did not decode".into());
            }
        }
        let with_photo = self
            .nodes
            .iter()
            .find(|n| n.id == "sync-user-name")
            .unwrap()
            .rect[0];
        self.state.user = Some(serde_json::json!({"name":"Example User"}));
        self.state.avatar = None;
        self.update(true);
        if self.nodes.iter().any(|n| n.id == "sync-user-avatar")
            || self
                .nodes
                .iter()
                .find(|n| n.id == "sync-user-name")
                .unwrap()
                .rect[0]
                != with_photo - 38.0
        {
            return Err("Missing profile photo retained empty space".into());
        }
        checks
            .push("PNG/JPEG profile photos render natively and missing photos release their space");
        self.memory.tab = "subtitling".into();
        self.state = get().snapshot();
        self.update(true);
        if IsWindowEnabled(
            self.controls
                .iter()
                .find(|c| c.id == "sub-generate-btn")
                .unwrap()
                .hwnd,
        )
        .as_bool()
        {
            return Err("Generate enabled without prerequisites".into());
        }
        checks.push("Generate disabled without file and dependencies");
        self.state.busy.insert("sub-ffmpeg-download-btn".into());
        self.state
            .progress
            .insert("ffmpeg".into(), (0.42, "42%".into()));
        self.update(true);
        if self
            .nodes
            .iter()
            .find(|n| n.id == "sub-dep-ffmpeg-status")
            .unwrap()
            .text
            != "Downloading... 42%"
            || self
                .controls
                .iter()
                .any(|c| c.id == "sub-ffmpeg-download-btn")
        {
            return Err("Dependency download status mismatch".into());
        }
        self.state.busy.remove("sub-ffmpeg-download-btn");
        self.state.progress.remove("ffmpeg");
        self.state
            .labels
            .insert("ffmpeg-status-error".into(), "Test download error".into());
        self.update(true);
        if self
            .nodes
            .iter()
            .find(|n| n.id == "sub-dep-ffmpeg-status")
            .unwrap()
            .text
            != "Download failed"
            || !self
                .controls
                .iter()
                .any(|c| c.id == "sub-ffmpeg-download-btn")
        {
            return Err("Dependency retry action mismatch".into());
        }
        checks.push("Dependency downloads show progress and restore retry after failure");
        self.state = get().snapshot();
        let fixture = self.scenes.at("light:subtitling-summary").fixture.clone();
        self.apply_fixture(&fixture)?;
        self.state.labels.remove("openrouter-key-input");
        self.update(true);
        if IsWindowEnabled(
            self.controls
                .iter()
                .find(|c| c.id == "sub-summarize-btn")
                .unwrap()
                .hwnd,
        )
        .as_bool()
        {
            return Err("Summarize enabled without API key".into());
        }
        checks.push("Summarize disabled without OpenRouter key");
        self.state.entries = (0..60)
            .map(|i| crate::subtitle::SrtEntry {
                index: i + 1,
                start: i as f64 * 3.0,
                end: i as f64 * 3.0 + 2.5,
                text: format!("Long native subtitle entry {i}"),
            })
            .collect();
        self.update(true);
        let before = self
            .nodes
            .iter()
            .find(|n| n.classes.contains("mono preview"))
            .unwrap()
            .rect[1];
        self.mouse = [150.0, 350.0];
        self.wheel(-48.0);
        if self.preview_scroll <= 0.0
            || self
                .nodes
                .iter()
                .find(|n| n.classes.contains("mono preview"))
                .unwrap()
                .rect[1]
                >= before
        {
            return Err("Subtitle preview did not scroll".into());
        }
        checks.push("Long SRT preview scrolls within its clipped viewport");
        self.state.summary = "A long summary that retains every word. ".repeat(100);
        self.update(true);
        if self
            .nodes
            .iter()
            .find(|n| n.id == "sub-summary-text")
            .unwrap()
            .text
            != self.state.summary
        {
            return Err("Summary was truncated".into());
        }
        checks.push("Long summary retains complete text");
        self.mouse = [100.0, 120.0];
        self.wheel(-100000.0);
        if !self.controls.iter().any(|c| c.id == "sub-save-btn") {
            return Err("Long summary hid Save below unreachable content".into());
        }
        if let Some(n) = self
            .nodes
            .iter()
            .find(|n| n.classes.contains("mono preview"))
        {
            if n.clip[1] >= 580.0 {
                return Err("Preview clip did not follow page scrolling".into());
            }
        }
        checks.push("Long summary page scroll keeps result actions and preview reachable");
        self.memory.tab = "dictionary".into();
        self.memory.scroll.clear();
        self.state = get().snapshot();
        self.update(true);
        let input = self
            .controls
            .iter()
            .find(|c| c.id == "dict-input")
            .unwrap()
            .hwnd;
        let _ = SetWindowTextW(input, w!("Native parity résumé"));
        let msg = MSG {
            hwnd: input,
            message: WM_KEYDOWN,
            wParam: WPARAM(VK_RETURN.0 as usize),
            ..Default::default()
        };
        if !self.pretranslate(&msg)
            || !get()
                .snapshot()
                .data
                .data
                .dictionary
                .terms
                .iter()
                .any(|t| t.value == "Native parity résumé")
        {
            return Err("Dictionary Enter was swallowed".into());
        }
        checks.push("Dictionary Enter adds the term through the actual message pretranslation");
        if !self.controls.iter().any(|c| c.id == "dict-remove:0") {
            return Err("Keyboard row action missing while mouse is absent".into());
        }
        checks.push("Row actions exist for keyboard focus before hovering");
        if crate::native_app::clipboard_owner().is_some() {
            return Err("Capture unexpectedly owns the live clipboard".into());
        }
        self.memory.tab = "home".into();
        let fixture = self.scenes.at("light:home-filled").fixture.clone();
        self.apply_fixture(&fixture)?;
        self.update(true);
        self.click("history-copy:0");
        if self.copied.is_some() {
            return Err("Copy failure showed success feedback".into());
        }
        checks.push("Copy failure has no success feedback and never opens the clipboard in the isolated harness");
        get().change(|s| s.recording_hotkey = Some("hotkey-btn".into()));
        self.state = get().snapshot();
        if !self.pretranslate(&MSG {
            hwnd: self.hwnd,
            message: WM_KEYDOWN,
            wParam: WPARAM(VK_ESCAPE.0 as usize),
            ..Default::default()
        }) || get().snapshot().recording_hotkey.is_some()
        {
            return Err("Shortcut Escape was swallowed".into());
        }
        checks.push("Escape cancels shortcut capture before dialog navigation");
        self.memory.tab = "settings".into();
        get().action("mode-select", "azure-mai".into());
        self.state = get().snapshot();
        self.update(true);
        let endpoint = self
            .controls
            .iter()
            .find(|c| c.id == "azure-endpoint-input")
            .unwrap()
            .hwnd;
        let _ = SetWindowTextW(endpoint, w!("https://native-parity.invalid/endpoint"));
        self.update(true);
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        let endpoint = self
            .controls
            .iter()
            .find(|c| c.id == "azure-endpoint-input")
            .unwrap()
            .hwnd;
        let _ = SetFocus(Some(endpoint));
        SendMessageW(endpoint, EM_SETSEL, Some(WPARAM(8)), Some(LPARAM(14)));
        for delta in [-120i16, 120] {
            SendMessageW(
                self.hwnd,
                WM_MOUSEWHEEL,
                Some(WPARAM((delta as u16 as usize) << 16)),
                None,
            );
            let endpoint = self
                .controls
                .iter()
                .find(|c| c.id == "azure-endpoint-input")
                .unwrap()
                .hwnd;
            if !IsWindowVisible(self.hwnd).as_bool()
                || GetFocus() != endpoint
                || SendMessageW(endpoint, EM_GETSEL, None, None).0 != (8 | (14 << 16))
                || read_text(endpoint) != "https://native-parity.invalid/endpoint"
            {
                return Err(
                    "Scrolling lost the active edit, focus, selection, or window visibility".into(),
                );
            }
        }
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        checks.push(
            "Visible scrolling preserves an active edit, focus, selection, and window visibility",
        );
        SendMessageW(self.hwnd, WM_CLOSE, None, None);
        if get().snapshot().setting("azureMaiEndpoint", "")
            != "https://native-parity.invalid/endpoint"
            || !self.modal
        {
            return Err("Close lost the active endpoint edit".into());
        }
        let path = get().app.app_local_data_dir()?.join("settings.json");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if saved["azureMaiEndpoint"] != "https://native-parity.invalid/endpoint" {
            return Err("Active endpoint did not reach the settings file".into());
        }
        checks.push("Alt+F4/close commits an active settings edit before the tray modal");
        let scroll = self.scroll();
        for child in [false, true] {
            let target = if child {
                self.controls
                    .iter()
                    .find(|c| c.id == "modal-cancel")
                    .unwrap()
                    .hwnd
            } else {
                self.hwnd
            };
            SendMessageW(
                target,
                WM_MOUSEWHEEL,
                Some(WPARAM((-120i16 as u16 as usize) << 16)),
                None,
            );
            if !self.modal || self.scroll() != scroll {
                return Err("Wheel input scrolled the page behind the close dialog".into());
            }
        }
        checks.push("The close dialog consumes wheel input without scrolling the page behind it");
        self.click("modal-cancel");
        if self.modal {
            return Err("Modal Cancel failed".into());
        }
        checks.push("Cancel restores the settings screen");
        let bounds = RECT {
            left: 0,
            top: 0,
            right: 780,
            bottom: 870,
        };
        SendMessageW(
            self.hwnd,
            WM_DPICHANGED,
            Some(WPARAM(144 | (144 << 16))),
            Some(LPARAM((&bounds as *const RECT) as isize)),
        );
        if self.renderer.width != 780
            || self.renderer.height != 870
            || self
                .controls
                .iter()
                .find(|c| c.id == "azure-endpoint-input")
                .unwrap()
                .inner[2]
                <= 404
        {
            return Err("DPI change did not scale rendering and controls together".into());
        }
        checks.push("150% DPI change scales native controls and drawing together");
        let bounds = RECT {
            left: 0,
            top: 0,
            right: 520,
            bottom: 580,
        };
        SendMessageW(
            self.hwnd,
            WM_DPICHANGED,
            Some(WPARAM(96 | (96 << 16))),
            Some(LPARAM((&bounds as *const RECT) as isize)),
        );
        self.open_modal();
        self.click("modal-tray");
        let mut memory = std::mem::take(&mut self.memory);
        memory.tab = "settings".into();
        let mut reopened = Self::create(memory)?;
        SetWindowLongPtrW(
            reopened.hwnd,
            GWLP_USERDATA,
            (&mut *reopened as *mut Window) as isize,
        );
        reopened.update(true);
        let endpoint = reopened
            .controls
            .iter()
            .find(|c| c.id == "azure-endpoint-input")
            .unwrap()
            .hwnd;
        if read_text(endpoint) != "https://native-parity.invalid/endpoint" {
            return Err("Reopening from tray lost the setting".into());
        }
        checks.push("Tray close releases the HWND and reopening retains the saved setting");
        // The original HWND was released to the tray; continue in the new one.
        reopened.check_setup(&mut checks, directory)?;
        drop(reopened);
        Ok(checks)
    }
    /// Walks the setup guide through its real controls. Never saves API keys:
    /// those would reach the live Windows Credential Manager.
    unsafe fn check_setup(
        &mut self,
        checks: &mut Vec<&'static str>,
        directory: &std::path::Path,
    ) -> Result<(), String> {
        // Live states the reference scenes cannot show, saved for review.
        let snap = |w: &Self, name: &str| -> Result<(), String> {
            save_png(
                &directory.join(format!("light-setup-{name}.png")),
                w.renderer.width,
                w.renderer.height,
                &w.capture_pixels(WM_PRINT)?,
            )
        };
        let control = |w: &Self, id: &str| w.controls.iter().find(|c| c.id == id).map(|c| c.hwnd);
        let text = |w: &Self, id: &str| {
            w.nodes
                .iter()
                .find(|n| n.id == id && n.kind == "text")
                .map(|n| n.text.clone())
                .unwrap_or_default()
        };
        self.modal = false;
        self.memory.tab = "home".into();
        get().set_setting(setup::KEY, Onboarding::initial(false, "groq").to_value())?;
        self.state = get().snapshot();
        self.update(true);
        if control(self, "ob-engine-groq").is_none()
            || control(self, "tab:home").is_some()
            || control(self, "ob-back").is_some()
        {
            return Err("A new install did not open the setup guide on its first step".into());
        }
        checks.push("A new install opens the setup guide, which replaces the tabs until exited");
        self.click("ob-engine-local");
        self.click("ob-next");
        if self.setup().step_id() != "install"
            || self.state.mode() == "local"
            || control(self, "ob-model-btn").is_none()
            || control(self, "ob-next").is_some_and(|h| IsWindowEnabled(h).as_bool())
        {
            return Err("Local setup did not wait for the model download".into());
        }
        checks.push("Choosing Local Whisper waits for the model before switching engines");
        get().change(|s| {
            s.busy.insert("whisper-download-btn".into());
            s.progress.insert("whisper".into(), (0.5, "50%".into()));
        });
        self.state = get().snapshot();
        self.update(false);
        let half = text(self, "ob-progress-text");
        if text(self, "ob-model-btn") != "Pause"
            || !half.starts_with("274 MB of 547 MB")
            || text(self, "ob-model-status") != "Downloading… 50%"
        {
            return Err(format!("Download row mismatch while downloading: {half}"));
        }
        snap(self, "downloading")?;
        get().change(|s| {
            s.busy.remove("whisper-download-btn");
            s.progress.remove("whisper");
            s.labels
                .insert("whisper-partial".into(), "287020597".into());
        });
        self.state = get().snapshot();
        self.update(false);
        if text(self, "ob-model-btn") != "Resume"
            || text(self, "ob-model-status") != "Paused at 50%"
        {
            return Err("A paused download is not offered for resuming".into());
        }
        snap(self, "paused")?;
        checks.push("The model download shows progress, pauses, and offers to resume");
        self.click("ob-exit");
        if self.setup().status != Status::Paused
            || control(self, "setup-resume-btn").is_none()
            || control(self, "tab:home").is_none()
        {
            return Err("Exit setup did not return to the app with a resume button".into());
        }
        snap(self, "exited")?;
        self.memory.tab = "settings".into();
        self.memory.scroll.insert("settings".into(), 100000.0);
        self.update(true);
        if text(self, "setup-guide-btn") != "Resume setup"
            || !text(self, "setup-guide-desc").contains("step 2 of 6")
        {
            return Err("Settings does not offer to resume a paused setup".into());
        }
        snap(self, "settings-resume")?;
        checks.push("Exiting setup keeps its step and offers Resume in the sidebar and Settings");
        self.click("setup-resume-btn");
        if !self.setup().is_open() || self.setup().step_id() != "install" {
            return Err("Resume did not reopen setup at the same step".into());
        }
        get().change(|s| {
            s.ready.insert("whisper".into());
            s.labels.remove("whisper-partial");
        });
        self.state = get().snapshot();
        self.update(false);
        self.click("ob-next");
        if self.state.mode() != "local" || self.setup().step_id() != "microphone" {
            return Err("Continuing after the download did not switch to Local Whisper".into());
        }
        checks.push(
            "Resuming returns to the same step; continuing switches the engine once it works",
        );
        self.click("ob-next");
        self.click("ob-chip-0");
        self.state = get().snapshot();
        self.update(false);
        if self.setup().step_id() != "hotkey"
            || control(self, "ob-next").is_some_and(|h| IsWindowEnabled(h).as_bool())
        {
            return Err("Setup continued without a working hotkey".into());
        }
        snap(self, "hotkey-failed")?;
        checks.push("Setup waits for a hotkey that registered successfully");
        get().set_setting("hotkey", serde_json::json!("Ctrl+Shift+Space"))?;
        self.state = get().snapshot();
        self.update(false);
        self.click("ob-next");
        if self.setup().step_id() != "try"
            || text(self, "ob-next") != "Skip"
            || text(self, "ob-try-hotkey") != "Ctrl+Shift+Space"
        {
            return Err("The Try it step did not show the hotkey and a Skip option".into());
        }
        get().status("Listening…", false);
        self.state = get().snapshot();
        self.update(false);
        get().status("Done", false);
        self.state = get().snapshot();
        self.update(false);
        if text(self, "ob-next") != "Continue"
            || !text(self, "ob-try-status").starts_with("It works")
        {
            return Err("A successful try did not confirm and allow continuing".into());
        }
        snap(self, "try-done")?;
        checks.push("Try it confirms a real recording and paste before continuing");
        self.click("ob-next");
        self.click("ob-feature-dictionary");
        if self.setup().status != Status::Completed
            || self.memory.tab != "dictionary"
            || control(self, "setup-resume-btn").is_some()
        {
            return Err("Opening a feature did not finish setup".into());
        }
        checks.push("Finishing setup opens the chosen feature and removes the resume button");
        self.memory.tab = "settings".into();
        self.memory.scroll.clear();
        get().change(|s| {
            s.ready.remove("whisper");
            s.busy.insert("whisper-download-btn".into());
            s.progress.insert("whisper".into(), (0.5, "50%".into()));
        });
        self.state = get().snapshot();
        self.update(true);
        // Scroll the model row into view so its button is a live control.
        let row = self
            .nodes
            .iter()
            .find(|n| n.id == "whisper-status-text")
            .map_or(0.0, |n| n.rect[1]);
        self.memory.scroll.insert("settings".into(), (row - 260.0).max(0.0));
        self.update(true);
        if !self.controls.iter().any(|c| c.id == "whisper-pause-btn") {
            return Err("Settings offers no way to pause the model download".into());
        }
        snap(self, "settings-downloading")?;
        get().change(|s| {
            s.busy.remove("whisper-download-btn");
            s.progress.remove("whisper");
            s.labels
                .insert("whisper-partial".into(), "287020597".into());
        });
        self.state = get().snapshot();
        self.update(true);
        if text(self, "whisper-status-text") != "Download paused at 50%"
            || text(self, "whisper-download-btn") != "Resume Download"
        {
            return Err("Settings does not offer to resume a paused model download".into());
        }
        snap(self, "settings-paused")?;
        checks.push("Settings can pause the model download and resume it later");
        Ok(())
    }
    fn scroll(&self) -> f32 {
        *self.memory.scroll.get(&self.memory.tab).unwrap_or(&0.0)
    }
    fn draft_key(&self, id: &str) -> String {
        match id {
            "apikey-input" => format!("{id}:{}", self.state.mode()),
            "ob-groq-key" => "apikey-input:groq".into(),
            "ob-azure-key" => "apikey-input:azure-mai".into(),
            "ob-azure-endpoint" => "azure-endpoint-input".into(),
            _ => id.into(),
        }
    }
    fn key_shown(&self, id: &str) -> bool {
        if id == "openrouter-key-input" {
            self.show_summary_key
        } else {
            self.show_keys
        }
    }
    /// The text a control holds now, including an edit not yet saved.
    fn live(&self, id: &str) -> String {
        self.controls
            .iter()
            .find(|c| c.id == id)
            .map(|c| unsafe { read_text(c.hwnd) })
            .or_else(|| self.memory.drafts.get(&self.draft_key(id)).cloned())
            .unwrap_or_else(|| self.field(id))
    }
    fn setup(&self) -> Onboarding {
        Onboarding::from_value(self.state.settings.get(setup::KEY), &self.state.mode())
    }
    fn list_viewport(&self, dict: bool) -> [f32; 4] {
        if dict {
            [92.0, 201.40625, 404.0, 362.59375]
        } else {
            [
                92.0,
                113.1875,
                404.0,
                if self.state.data.data.history.entries.is_empty() {
                    442.8125
                } else {
                    411.0625
                },
            ]
        }
    }
    fn field(&self, id: &str) -> String {
        let s = &self.state;
        match alias(id) {
            "dict-input" | "ob-try-input" => {
                self.memory.drafts.get(id).cloned().unwrap_or_default()
            }
            "ob-groq-key" => s.label("groq-key", ""),
            "ob-azure-key" => s.label("azure-key", ""),
            "apikey-input" => s.label(
                if s.mode() == "azure-mai" {
                    "azure-key"
                } else {
                    "groq-key"
                },
                "",
            ),
            "openrouter-key-input" => s.label(id, ""),
            "azure-endpoint-input" => s.setting("azureMaiEndpoint", ""),
            "mic-select" => s.setting("audioDeviceId", ""),
            "mode-select" => s.mode(),
            "acceleration-select" => s.setting("whisperAcceleration", "vulkan"),
            "live-lang-select" => s.setting("liveLanguage", "auto"),
            "sub-lang-select" => s.setting("subtitleLanguage", "auto"),
            _ => s.label(id, ""),
        }
    }
    fn build_scene(&mut self) {
        let theme = self.theme();
        let tab = self.memory.tab.clone();
        let has_picture = self
            .state
            .user
            .as_ref()
            .and_then(|user| user["picture"].as_str())
            .is_some_and(|url| !url.is_empty());
        let guide = self.setup();
        let key = if guide.is_open() {
            match guide.step_id() {
                step @ ("model" | "install") => {
                    format!("{theme}:onboarding:{step}:{}", guide.engine)
                }
                step => format!("{theme}:onboarding:{step}"),
            }
        } else if tab == "settings" {
            format!(
                "{theme}:settings:{}:{}:{}:{}",
                self.state.mode(),
                if self.state.mode() == "local" {
                    self.state.setting("whisperAcceleration", "vulkan")
                } else {
                    "vulkan".into()
                },
                u8::from(self.state.checked("grammarCleanupEnabled")),
                if self.state.auth_waiting {
                    "waiting"
                } else if self.state.user.is_some() {
                    "in"
                } else {
                    "out"
                }
            )
        } else if tab == "subtitling" {
            format!(
                "{theme}:subtitling{}",
                if !self.state.entries.is_empty() {
                    if self.state.summary.is_empty() {
                        "-result"
                    } else {
                        "-summary"
                    }
                } else if self.state.file.is_some() {
                    if self.state.ready.contains("ffmpeg") && self.state.ready.contains("vad") {
                        "-ready"
                    } else {
                        "-file"
                    }
                } else {
                    ""
                }
            )
        } else {
            format!("{theme}:{tab}")
        };
        let scene = self
            .scenes
            .get(&key)
            .unwrap_or_else(|| self.scenes.at(&format!("{theme}:home")));
        self.nodes = scene.nodes.clone();
        self.height = scene.height;
        if guide.is_open() {
            // The guide fits the window; it never scrolls.
            self.height = 532.0;
            self.onboarding_state(&guide);
        } else {
            // The reference contains the empty lists. Build their native rows using
            // the same card sizes, font sizes, gaps, and padding as index.css.
            if tab == "home" || tab == "dictionary" {
                self.populate_list(tab == "dictionary");
            }
            if tab == "subtitling" {
                self.subtitles();
            }
            self.settings_state();
            self.setup_entry_points(&guide);
        }
        let scroll = if guide.is_open() {
            0.0
        } else {
            self.scroll().min((self.height - 532.0).max(0.0))
        };
        if !guide.is_open() {
            self.memory.scroll.insert(self.memory.tab.clone(), scroll);
        }
        for n in &mut self.nodes {
            if ((tab == "home" || tab == "dictionary") && n.classes.contains("native-list"))
                || (tab != "home"
                    && tab != "dictionary"
                    && n.rect[0] >= 68.0
                    && (n.rect[1] >= 48.0 || n.clip[2] > 0.0)
                    && n.classes != "content")
            {
                n.rect[1] -= scroll;
                if tab != "home" && tab != "dictionary" && n.clip[2] > 0.0 {
                    n.clip[1] -= scroll;
                }
            }
            match n.id.as_str() {
                "status-text" => {
                    n.text = self.state.label("status-text", "Ready");
                    n.rect[0] = 8.0;
                    n.rect[2] = 52.0;
                    n.align = "center".into();
                    n.title = n.text.clone();
                }
                "status-dot" => {
                    n.background = if self.state.ready.contains("status-error") {
                        color(self.state.dark(), "danger")
                    } else if self.state.ready.contains("status-active") {
                        if self.state.dark() {
                            "#90cdf4".into()
                        } else {
                            color(false, "ink")
                        }
                    } else {
                        color(self.state.dark(), "faint")
                    };
                }
                "hotkey-display" | "dict-hotkey-display" => {
                    let dict = n.id == "dict-hotkey-display";
                    n.text = if self.state.recording_hotkey.as_deref()
                        == Some(if dict {
                            "dict-hotkey-btn"
                        } else {
                            "hotkey-btn"
                        }) {
                        "Press a shortcut…".into()
                    } else {
                        self.state
                            .setting(if dict { "dictHotkey" } else { "hotkey" }, "Click to set")
                    };
                    n.rect[0] = 92.0;
                    n.rect[2] = 404.0;
                    n.wrap = 404.0;
                    n.align = "center".into();
                }
                "sync-user-name" => {
                    n.text = self
                        .state
                        .user
                        .as_ref()
                        .and_then(|v| v["name"].as_str())
                        .unwrap_or("User")
                        .into();
                    if !has_picture {
                        n.rect[0] -= 38.0;
                    }
                    n.rect[2] = if has_picture { 330.0 } else { 368.0 };
                    n.wrap = n.rect[2];
                    n.nowrap = true;
                }
                "sync-user-email" => {
                    let email = self
                        .state
                        .user
                        .as_ref()
                        .and_then(|v| v["email"].as_str())
                        .unwrap_or("");
                    n.text = email.split_once('@').map_or_else(
                        || email.to_owned(),
                        |(name, domain)| {
                            format!("{}***@{domain}", name.chars().take(3).collect::<String>())
                        },
                    );
                    if !has_picture {
                        n.rect[0] -= 38.0;
                    }
                    n.rect[2] = if has_picture { 330.0 } else { 368.0 };
                    n.wrap = n.rect[2];
                    n.nowrap = true;
                }
                "sync-user-avatar" => {
                    n.src = "native-profile-avatar".into();
                }
                _ => {}
            }
        }
        self.nodes
            .retain(|n| n.id != "sync-user-avatar" || has_picture);
        if self.modal {
            let key = format!("{}:modal", self.theme());
            for n in &self.scenes.at(&key).nodes {
                if n.owner == "close-modal"
                    || n.id == "close-modal"
                    || n.owner.starts_with("modal-")
                {
                    self.nodes.push(n.clone());
                }
            }
        }
        if let Some(hit) = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == self.hover && !n.disabled)
            .cloned()
        {
            if !self
                .nodes
                .iter()
                .any(|n| n.kind == "rect" && n.owner == hit.owner)
            {
                let mut bg = self.rect(
                    hit.rect,
                    "warm",
                    "transparent",
                    0.0,
                    if hit.owner.starts_with("tab:") || hit.owner == "setup-resume-btn" {
                        10.0
                    } else if hit.owner == "ob-exit" {
                        6.0
                    } else {
                        0.0
                    },
                );
                bg.owner = hit.owner.clone();
                if hit.owner == "titlebar-close" {
                    bg.background = if self.state.dark() {
                        "rgba(192,96,96,0.1)"
                    } else {
                        "rgba(139,68,68,0.08)"
                    }
                    .into();
                }
                let index = self
                    .nodes
                    .iter()
                    .position(|n| n.owner == hit.owner)
                    .unwrap_or(0);
                self.nodes.insert(index, bg);
            }
        }
        if self.state.ready.contains("status-active") {
            if let Some(dot) = self.nodes.iter().find(|n| n.id == "status-dot").cloned() {
                let mut ring = self.rect(
                    [dot.rect[0] - 3.0, dot.rect[1] - 3.0, 14.0, 14.0],
                    "transparent",
                    "transparent",
                    0.0,
                    7.0,
                );
                ring.background = if self.state.dark() {
                    "rgba(144,205,244,0.2)"
                } else {
                    "rgba(44,44,44,0.15)"
                }
                .into();
                ring.opacity =
                    0.7 + 0.3 * ((animation_time() / 1500.0) * std::f32::consts::TAU).cos();
                self.nodes.push(ring);
            }
        }
    }
    fn populate_list(&mut self, dict: bool) {
        let viewport = self.list_viewport(dict);
        let mut y = viewport[1];
        let count = if dict {
            self.state.data.data.dictionary.terms.len()
        } else {
            self.state.data.data.history.entries.len()
        };
        if count == 0 {
            return;
        }
        self.nodes.retain(|n| {
            !n.classes
                .contains(if dict { "dict-empty" } else { "history-empty" })
        });
        let template = self.scenes.at(&format!(
            "{}:{}-filled",
            self.theme(),
            if dict { "dictionary" } else { "home" }
        ));
        let count_id = if dict { "dict-count" } else { "history-count" };
        let count_text = count.to_string();
        let count_width =
            self.renderer.text_width(&count_text, 9.8, 500) + count_text.len() as f32 * 0.392;
        for mut n in template.nodes.iter().filter(|n| n.id == count_id).cloned() {
            if n.kind == "rect" {
                n.rect[2] = 18.0 + count_width;
            } else {
                n.text = count_text.clone();
                n.rect[2] = count_width + 1.0;
                n.wrap = n.rect[2];
            }
            self.nodes.push(n);
        }
        let terms: Vec<_> = if dict {
            self.state
                .data
                .data
                .dictionary
                .terms
                .iter()
                .map(|t| (t.value.clone(), String::new()))
                .collect()
        } else {
            self.state
                .data
                .data
                .history
                .entries
                .iter()
                .map(|e| (e.text.clone(), format_time(e.time)))
                .collect()
        };
        let text_class = if dict {
            "dict-item-text"
        } else {
            "history-item-text"
        };
        let text_template = template
            .nodes
            .iter()
            .find(|n| n.kind == "text" && n.classes == text_class)
            .unwrap()
            .clone();
        let first_list_node = self.nodes.len();
        let focus = unsafe { GetFocus() };
        let focused = self
            .controls
            .iter()
            .find(|c| c.hwnd == focus)
            .map(|c| c.id.clone())
            .unwrap_or_default();
        for (index, (text, time)) in terms.iter().enumerate() {
            let text_width = if dict { 320.546875 } else { 378.0 };
            let text_height = self.renderer.text_height(
                text,
                text_template.size,
                text_template.weight,
                text_width,
                text_template.line,
            );
            let height = if dict {
                22.0 + text_height.max(18.46875)
            } else {
                35.515625 + text_height.min(51.75)
            };
            let hovered = self.mouse[0] >= 92.0
                && self.mouse[0] < 496.0
                && self.mouse[1] >= y - self.scroll()
                && self.mouse[1] < y + height - self.scroll();
            let active = hovered || focused.ends_with(&format!(":{index}"));
            let mut card = self.rect(
                [92.0, y, 404.0, height],
                if active { "hover" } else { "warm" },
                if active { "rule-hover" } else { "transparent" },
                1.0,
                10.0,
            );
            card.owner = format!("row:{index}");
            self.nodes.push(card);
            if !dict {
                let mut n = template
                    .nodes
                    .iter()
                    .find(|n| n.classes == "history-item-time")
                    .unwrap()
                    .clone();
                n.text = time.clone();
                n.rect = [105.0, y + 9.0, 378.0, 12.0];
                n.wrap = 378.0;
                n.nowrap = true;
                self.nodes.push(n);
            }
            let mut n = text_template.clone();
            n.text = text.clone();
            n.rect = [
                105.0,
                if dict {
                    y + (height - text_height) / 2.0 + 1.0
                } else {
                    y + 25.515625
                },
                text_width,
                if dict {
                    text_height + 2.0
                } else {
                    text_height.min(51.75) + 2.0
                },
            ];
            n.wrap = text_width;
            n.nowrap = false;
            self.nodes.push(n);
            let mut buttons = if dict {
                vec![("dict-remove", "REMOVE", 57.453125, "dict-item-remove")]
            } else {
                vec![
                    ("history-copy", "COPY", 43.28125, "history-item-copy"),
                    ("history-remove", "DELETE", 50.953125, "history-item-remove"),
                ]
            };
            if !dict
                && self.copied.as_ref().is_some_and(|(id, t)| {
                    id == &format!("history-copy:{index}") && t.elapsed().as_millis() < 1200
                })
            {
                buttons[0].2 = self.renderer.text_width("COPIED!", 8.68, 500) + 7.0 * 0.3472 + 18.0;
            }
            let total = buttons.iter().map(|b| b.2).sum::<f32>() + (buttons.len() - 1) as f32 * 4.0;
            let mut x = 487.0 - total;
            for (action, label, width, class) in buttons {
                let action_id = format!("{action}:{index}");
                let copied = self
                    .copied
                    .as_ref()
                    .is_some_and(|(id, t)| id == &action_id && t.elapsed().as_millis() < 1200);
                let label = if copied { "COPIED!" } else { label };
                let r = [
                    x,
                    if dict {
                        y + (height - 18.0) / 2.0
                    } else {
                        y + 9.0
                    },
                    width,
                    18.0,
                ];
                for mut n in template
                    .nodes
                    .iter()
                    .filter(|n| n.classes == class)
                    .take(3)
                    .cloned()
                {
                    n.owner = action_id.clone();
                    n.opacity = if active { 1.0 } else { 0.0 };
                    if n.kind == "text" {
                        n.text = label.into();
                        n.rect = [x + 1.0, r[1] + 3.0, width - 2.0, 12.0];
                        n.wrap = width - 2.0;
                        n.align = "center".into();
                        n.nowrap = true;
                    } else {
                        n.rect = r;
                    }
                    n.title = if action == "dict-remove" {
                        format!("Remove {text}")
                    } else if action == "history-copy" {
                        "Copy transcription".into()
                    } else {
                        "Delete transcription".into()
                    };
                    self.nodes.push(n);
                }
                x += width + 4.0;
            }
            y += height + if dict { 4.0 } else { 6.0 };
        }
        for n in &mut self.nodes[first_list_node..] {
            n.classes.push_str(" native-list");
            n.clip = viewport;
        }
        self.height =
            532.0 + (y - if dict { 4.0 } else { 6.0 } - viewport[1] - viewport[3]).max(0.0);
        if !dict {
            for n in template
                .nodes
                .iter()
                .filter(|n| n.id == "history-clear")
                .cloned()
            {
                self.nodes.push(n);
            }
        }
    }
    fn settings_state(&mut self) {
        let s = &self.state;
        let ready = &s.ready;
        let mode = s.mode();
        for n in &mut self.nodes {
            if n.kind == "select" {
                if alias(&n.id) == "mic-select" {
                    n.options = s
                        .mics
                        .iter()
                        .map(|m| [m.id.clone(), m.name.clone()])
                        .collect();
                    if n.options.is_empty() {
                        n.options.push(["".into(), "No devices found".into()]);
                    }
                }
                let key = match alias(&n.id) {
                    "mic-select" => "audioDeviceId",
                    "mode-select" => "transcriptionMode",
                    "acceleration-select" => "whisperAcceleration",
                    "live-lang-select" => "liveLanguage",
                    _ => "subtitleLanguage",
                };
                let value = s.setting(
                    key,
                    if key.ends_with("Language") {
                        "auto"
                    } else if key == "transcriptionMode" {
                        "groq"
                    } else if key == "whisperAcceleration" {
                        "vulkan"
                    } else {
                        ""
                    },
                );
                n.text = n
                    .options
                    .iter()
                    .find(|o| o[0] == value)
                    .or_else(|| n.options.first())
                    .map(|o| o[1].clone())
                    .unwrap_or_default();
            }
            if n.id == "sub-dep-ffmpeg-status" || n.id == "sub-dep-vad-status" {
                let key = if n.id.contains("ffmpeg") {
                    "ffmpeg"
                } else {
                    "vad"
                };
                let downloading = s.busy.contains(&format!("sub-{key}-download-btn"));
                n.text = if downloading {
                    s.progress
                        .get(key)
                        .map(|(percent, _)| format!("Downloading... {:.0}%", percent * 100.0))
                        .unwrap_or_else(|| "Downloading...".into())
                } else if ready.contains(key) {
                    "Ready".into()
                } else if s.labels.contains_key(&format!("{key}-status-error")) {
                    "Download failed".into()
                } else {
                    "Not installed".into()
                };
                n.color = color(s.dark(), if ready.contains(key) { "ink" } else { "faint" });
                n.weight = if ready.contains(key) { 500 } else { 400 };
                n.rect[2] = if downloading || ready.contains(key) {
                    303.0
                } else {
                    n.wrap
                };
                n.nowrap = true;
            }
            if n.id == "apikey-label" {
                n.text = if mode == "azure-mai" {
                    "FOUNDRY API KEY"
                } else {
                    "GROQ API KEY"
                }
                .into();
            }
            if n.classes == "sync-now-text" {
                n.text = s.label("sync-now-text", "Sync Now").to_uppercase();
                n.rect[2] = self.renderer.text_width(&n.text, n.size, n.weight);
            }
            if n.kind == "hit" {
                n.disabled = match n.owner.as_str() {
                    "sub-generate-btn" => {
                        s.file.is_none()
                            || !ready.contains("ffmpeg")
                            || !ready.contains("vad")
                            || s.busy.contains("sub-generate-btn")
                    }
                    "sub-summarize-btn" => {
                        s.label("openrouter-key-input", "").is_empty()
                            || s.busy.contains("sub-summarize-btn")
                    }
                    "sub-file-zone" | "sub-new-btn" => {
                        s.busy.contains("sub-generate-btn") || s.busy.contains("sub-summarize-btn")
                    }
                    _ => s.busy.contains(&n.owner),
                };
            }
        }
        self.nodes.retain(|n| {
            !(n.owner == "sub-ffmpeg-download-btn"
                && (ready.contains("ffmpeg") || s.busy.contains("sub-ffmpeg-download-btn"))
                || n.owner == "sub-vad-download-btn"
                    && (ready.contains("vad") || s.busy.contains("sub-vad-download-btn")))
        });
        if let Some(button) = self
            .nodes
            .iter()
            .find(|n| n.owner == "sync-now-btn" && n.kind == "hit")
            .cloned()
        {
            let busy = s.busy.contains("sync-now-btn");
            let label = s.label("sync-now-text", "Sync Now").to_uppercase();
            let width = self.renderer.text_width(&label, 10.08, 500)
                + label.chars().count() as f32 * 0.4032;
            let button_width = width + 22.0 + if busy { 17.0 } else { 0.0 };
            for n in &mut self.nodes {
                if n.owner == "sync-now-btn" {
                    if n.kind == "text" {
                        n.rect[0] = button.rect[0] + 11.0 + if busy { 17.0 } else { 0.0 };
                        n.rect[2] = width;
                        n.wrap = width;
                    } else {
                        n.rect[2] = button_width;
                        n.wrap = button_width - 22.0;
                    }
                    if label == "SYNCED" {
                        n.color = color(s.dark(), "ink");
                        n.border = color(s.dark(), "ink");
                    }
                }
                if n.owner == "sync-signout-btn" {
                    n.rect[0] += button_width - button.rect[2];
                }
            }
            if busy {
                self.nodes.push(Node {
                    kind: "spinner".into(),
                    rect: [button.rect[0] + 11.0, button.rect[1] + 5.5, 12.0, 12.0],
                    color: color(s.dark(), "ink"),
                    border: color(s.dark(), "rule"),
                    opacity: 1.0,
                    ..Default::default()
                });
            }
        }
        // Checkbox layout is captured from the existing switch's label. Draw the
        // knob natively, including its theme-specific track color.
        let switches: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| {
                n.kind == "hit" && (n.owner == "theme-toggle" || n.owner == "grammar-toggle")
            })
            .cloned()
            .collect();
        for switch in switches {
            let checked = if switch.owner == "theme-toggle" {
                s.dark()
            } else {
                s.checked("grammarCleanupEnabled")
            };
            let mut track = self.rect(
                switch.rect,
                if checked { "ink" } else { "rule" },
                "transparent",
                0.0,
                12.0,
            );
            track.owner = switch.owner.clone();
            self.nodes.push(track);
            self.nodes.push(self.rect(
                [
                    switch.rect[0] + if checked { 23.0 } else { 3.0 },
                    switch.rect[1] + 3.0,
                    18.0,
                    18.0,
                ],
                "paper",
                "transparent",
                0.0,
                9.0,
            ));
        }
        let dict_hotkey_set = !s.setting("dictHotkey", "").is_empty();
        self.model_row(
            "whisper-status-text",
            "whisper-download-btn",
            "whisper",
            "whisper-loaded",
            "Model not downloaded (~600 MB)",
        );
        self.model_row(
            "gec-status-text",
            "gec-download-btn",
            "gec",
            "gec-loaded",
            "Model not downloaded (~105 MB)",
        );
        if dict_hotkey_set {
            if let Some(node) = self
                .nodes
                .iter()
                .find(|n| n.kind == "hit" && n.owner == "dict-hotkey-btn")
                .cloned()
            {
                let y = node.rect[1] + node.rect[3] + 8.0;
                let width = self.renderer.text_width("Clear", 11.2, 500) + 26.0;
                self.button(
                    [92.0, y, width, 30.8],
                    "Clear",
                    "dict-hotkey-clear",
                    "paper",
                );
                for n in &mut self.nodes {
                    if n.classes == "setting-hint" && n.rect[1] > y {
                        n.rect[1] += 38.8;
                    }
                }
                self.height += 38.8;
            }
        }
    }
    /// Share of a paused model download that is already on disk.
    fn paused_fraction(&self) -> Option<f32> {
        let bytes = self
            .state
            .label("whisper-partial", "0")
            .parse::<u64>()
            .ok()?;
        (bytes > 0).then(|| {
            (bytes as f64 / crate::whisper_local::model_size_bytes() as f64).min(1.0) as f32
        })
    }
    /// Why the current setup step cannot be left yet, if anything.
    fn setup_blocker(&self, guide: &Onboarding) -> Option<&'static str> {
        match guide.step_id() {
            "install" => {
                let (groq_key, azure_key, endpoint) = (
                    self.live("ob-groq-key"),
                    self.live("ob-azure-key"),
                    self.live("ob-azure-endpoint"),
                );
                setup::install_blocker(
                    &guide.engine,
                    &setup::Readiness {
                        groq_key: &groq_key,
                        azure_key: &azure_key,
                        endpoint: &endpoint,
                        model_downloaded: self.state.ready.contains("whisper"),
                    },
                )
            }
            "hotkey" if self.state.setting("hotkey", "").is_empty() => {
                Some("Set a hotkey to continue")
            }
            _ => None,
        }
    }
    /// Sidebar and Settings entry points for resuming or rerunning setup.
    fn setup_entry_points(&mut self, guide: &Onboarding) {
        let paused = guide.status == Status::Paused;
        let progress = format!(
            "step {} of {}: {}",
            guide.step + 1,
            setup::STEPS.len(),
            guide.step_label()
        );
        self.nodes
            .retain(|n| n.owner != "setup-resume-btn" || paused);
        for n in &mut self.nodes {
            if n.owner == "setup-resume-btn" {
                n.title = format!("Resume setup at {progress}");
            }
        }
        let Some(button) = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == "setup-guide-btn")
            .cloned()
        else {
            return;
        };
        let (description, label) = if paused {
            (format!("Paused at {progress}."), "Resume setup")
        } else {
            (
                "Revisit the engine, microphone, and hotkey steps.".to_owned(),
                "Run setup again",
            )
        };
        let width = self.renderer.text_width(label, 10.92, 600) + 28.0;
        let x = button.rect[0] + button.rect[2] - width;
        for n in &mut self.nodes {
            if n.owner == "setup-guide-btn" {
                if n.kind == "text" {
                    n.text = label.into();
                    n.rect[0] = x + 14.0;
                    n.rect[2] = width - 28.0;
                    n.wrap = n.rect[2];
                    n.align = "center".into();
                    n.nowrap = true;
                } else {
                    n.rect[0] = x;
                    n.rect[2] = width;
                }
            }
            if n.id == "setup-guide-desc" {
                n.text = description.clone();
                n.rect[2] = x - 12.0 - n.rect[0];
                n.wrap = n.rect[2];
                n.nowrap = true;
                n.title = description.clone();
            }
        }
    }
    /// Fills the setup guide's live values into its reference scene.
    fn onboarding_state(&mut self, guide: &Onboarding) {
        let dark = self.state.dark();
        let step = guide.step_id();
        let status = self.state.label("status-text", "Ready");
        if step == "try" {
            self.try_listening |= status == "Listening…";
            self.try_done |= self.try_listening && status == "Done";
        }
        let blocker = self.setup_blocker(guide);
        let next_label = if guide.is_last() {
            "Start using Annotate"
        } else if step == "try" && !self.try_done {
            "Skip"
        } else {
            "Continue"
        };
        let hotkey = self.state.setting("hotkey", "");
        let recording = self.state.recording_hotkey.as_deref() == Some("hotkey-btn");
        let count = format!(
            "Step {} of {} · {}",
            guide.step + 1,
            setup::STEPS.len(),
            guide.step_label()
        )
        .to_uppercase();
        if guide.step == 0 {
            self.nodes.retain(|n| n.owner != "ob-back");
        }
        let next = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == "ob-next")
            .map(|n| n.rect)
            .unwrap_or([400.0, 529.0, 96.0, 36.0]);
        let next_width = self.renderer.text_width(next_label, 11.48, 600) + 36.0;
        let next_x = next[0] + next[2] - next_width;
        let hint_left = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == "ob-back")
            .map_or(24.0, |n| n.rect[0] + n.rect[2] + 12.0);
        let try_text = match status.as_str() {
            "Listening…" => "Listening. Let go of the keys when you're done.".to_owned(),
            "Transcribing…" | "Cleaning grammar…" | "Pasting…" => status.clone(),
            "No speech" => "Didn't catch that. Hold the keys while you speak.".into(),
            "Set API key" => "No API key yet. Go back to step 2 to add one.".into(),
            "Set Azure endpoint" => "No endpoint yet. Go back to step 2 to add it.".into(),
            "Microphone failed" => "The microphone didn't start. Check it in step 3.".into(),
            "Transcribe failed" => format!(
                "Transcription failed: {}",
                self.state.label("status-detail", "try again")
            ),
            _ if self.try_done => "It works. Your words were pasted where the cursor was.".into(),
            _ => "Waiting for your hotkey".into(),
        };
        let try_error = self.state.ready.contains("status-error");
        // Local model download row.
        let total = crate::whisper_local::model_size_bytes();
        let downloading = self.state.busy.contains("whisper-download-btn");
        let downloaded = self.state.ready.contains("whisper");
        let failed = !self.state.label("whisper-status-error", "").is_empty();
        let fraction = if downloading {
            Some(self.state.progress.get("whisper").map_or(0.0, |p| p.0))
        } else if downloaded {
            None
        } else {
            self.paused_fraction()
        };
        let percent = fraction.map_or(0.0, |f| f * 100.0);
        let (model_status, model_button) = if downloaded {
            ("Downloaded and ready".to_owned(), "")
        } else if downloading {
            (format!("Downloading… {percent:.0}%"), "Pause")
        } else if fraction.is_some() {
            (format!("Paused at {percent:.0}%"), "Resume")
        } else if failed {
            (
                "Download failed. Check your connection.".to_owned(),
                "Retry",
            )
        } else {
            (
                format!("Not downloaded · {}", setup::megabytes(total)),
                "Download",
            )
        };
        let progress_text = fraction.map(|f| {
            format!(
                "{} of {}",
                setup::megabytes((f as f64 * total as f64) as u64),
                setup::megabytes(total)
            )
        });
        let button_rect = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == "ob-model-btn")
            .map(|n| n.rect);
        let bar = self
            .nodes
            .iter()
            .find(|n| n.id == "ob-progress" && n.kind == "rect")
            .map(|n| n.rect);
        let hotkey_button = self
            .nodes
            .iter()
            .find(|n| n.kind == "hit" && n.owner == "ob-hotkey-btn")
            .map(|n| n.rect);
        let kbd_text = if hotkey.is_empty() {
            "Not set".to_owned()
        } else {
            hotkey.clone()
        };
        let kbd_width = self.renderer.text_width(&kbd_text, 10.92, 600) + 18.0;
        let options: Vec<[String; 2]> = self
            .state
            .mics
            .iter()
            .map(|m| [m.id.clone(), m.name.clone()])
            .collect();
        for i in 0..setup::STEPS.len() {
            let ink = if i <= guide.step { "ink" } else { "rule" };
            let background = color(dark, ink);
            if let Some(n) = self
                .nodes
                .iter_mut()
                .find(|n| n.id == format!("ob-seg-{i}"))
            {
                n.background = background;
            }
        }
        for n in &mut self.nodes {
            match n.id.as_str() {
                "ob-count" => {
                    n.text = count.clone();
                    n.rect[2] = 360.0;
                    n.wrap = 360.0;
                    n.nowrap = true;
                }
                "ob-hint" => {
                    n.text = blocker.unwrap_or("").into();
                    n.rect[0] = hint_left;
                    n.rect[2] = next_x - 12.0 - hint_left;
                    n.wrap = n.rect[2];
                    n.align = "right".into();
                    n.nowrap = true;
                }
                "ob-hotkey-display" => {
                    n.text = if recording {
                        "Press a shortcut…".into()
                    } else if hotkey.is_empty() {
                        "Click to set".into()
                    } else {
                        hotkey.clone()
                    };
                    if let Some(r) = hotkey_button {
                        n.rect[0] = r[0];
                        n.rect[2] = r[2];
                        n.wrap = r[2];
                        n.align = "center".into();
                    }
                    n.color = color(
                        dark,
                        if hotkey.is_empty() || recording {
                            "light"
                        } else {
                            "ink"
                        },
                    );
                    n.weight = if hotkey.is_empty() { 500 } else { 600 };
                }
                "ob-try-hotkey" => {
                    if n.kind == "text" {
                        n.text = kbd_text.clone();
                        n.rect[2] = kbd_width - 16.0;
                        n.wrap = n.rect[2];
                        n.nowrap = true;
                    } else {
                        n.rect[2] = kbd_width;
                    }
                }
                "ob-try-status" => {
                    n.text = try_text.clone();
                    n.rect[2] = 472.0;
                    n.wrap = 472.0;
                    n.nowrap = true;
                    n.title = try_text.clone();
                    n.color = color(
                        dark,
                        if try_error {
                            "danger"
                        } else if self.try_done {
                            "ink"
                        } else {
                            "light"
                        },
                    );
                }
                "ob-model-status" => {
                    n.text = model_status.clone();
                    n.rect[2] = 300.0;
                    n.wrap = 300.0;
                    n.nowrap = true;
                    n.color = color(
                        dark,
                        if failed && !downloading {
                            "danger"
                        } else if downloaded {
                            "ink"
                        } else {
                            "light"
                        },
                    );
                }
                "ob-progress-fill" => {
                    if let Some(bar) = bar {
                        n.rect = [
                            bar[0] + 1.0,
                            bar[1] + 1.0,
                            (bar[2] - 2.0) * fraction.unwrap_or(0.0).clamp(0.0, 1.0),
                            bar[3] - 2.0,
                        ];
                    }
                }
                "ob-progress-text" => {
                    n.text = progress_text.clone().unwrap_or_default();
                    n.rect[2] = 300.0;
                    n.wrap = 300.0;
                    n.nowrap = true;
                }
                _ => {}
            }
            if n.owner == "ob-model-btn" && n.kind == "text" {
                if let Some(r) = button_rect {
                    n.text = model_button.into();
                    n.rect[0] = r[0];
                    n.rect[2] = r[2];
                    n.wrap = r[2];
                    n.align = "center".into();
                    n.nowrap = true;
                }
            }
            if n.owner == "ob-next" {
                n.disabled = blocker.is_some();
                n.opacity = if n.disabled { 0.4 } else { 1.0 };
                n.rect[0] = next_x;
                n.rect[2] = next_width;
                if n.kind == "text" {
                    n.text = next_label.into();
                    n.wrap = next_width;
                    n.align = "center".into();
                    n.nowrap = true;
                }
            }
            if n.kind == "select" {
                if n.id == "ob-mic-select" {
                    n.options = if options.is_empty() {
                        vec![["".into(), "No devices found".into()]]
                    } else {
                        options.clone()
                    };
                }
                let value = self.state.setting(
                    if n.id == "ob-mic-select" {
                        "audioDeviceId"
                    } else {
                        "liveLanguage"
                    },
                    if n.id == "ob-mic-select" { "" } else { "auto" },
                );
                n.text = n
                    .options
                    .iter()
                    .find(|o| o[0] == value)
                    .or_else(|| n.options.first())
                    .map(|o| o[1].clone())
                    .unwrap_or_default();
            }
        }
        if downloaded {
            self.nodes.retain(|n| n.owner != "ob-model-btn");
        }
        if progress_text.is_none() {
            // Close the gap the progress bar leaves in the model card.
            let bottom = self
                .nodes
                .iter()
                .find(|n| n.id == "ob-progress-text")
                .map(|n| n.rect[1] + n.rect[3]);
            if let (Some(bar), Some(bottom)) = (bar, bottom) {
                let gap = bottom - (bar[1] - 12.0);
                for n in &mut self.nodes {
                    if n.classes == "ob-model" {
                        n.rect[3] -= gap;
                    } else if n.classes == "setting-hint" {
                        n.rect[1] -= gap;
                    }
                }
            }
            self.nodes.retain(|n| {
                !matches!(
                    n.id.as_str(),
                    "ob-progress" | "ob-progress-fill" | "ob-progress-text"
                )
            });
        }
    }
    /// Back, Continue, and Exit setup.
    unsafe fn setup_step(&mut self, id: &str) {
        self.commit_edits();
        self.state = get().snapshot();
        let mut guide = self.setup();
        match id {
            "ob-exit" => {
                get().change(|s| s.recording_hotkey = None);
                guide.pause();
            }
            "ob-back" => guide.back(),
            _ => {
                if self.setup_blocker(&guide).is_some() {
                    return;
                }
                // The engine switches only once it can transcribe, so leaving
                // setup halfway never leaves a broken engine selected.
                if guide.step_id() == "install" && self.state.mode() != guide.engine {
                    get().action("mode-select", guide.engine.clone());
                }
                if guide.is_last() {
                    self.memory.tab = "home".into();
                }
                guide.next();
            }
        }
        let exited = guide.status == Status::Paused;
        self.enter_setup(guide);
        if exited {
            get().status("Setup paused", false);
        }
    }
    /// Saves the guide's state and shows its current step (or the app).
    unsafe fn enter_setup(&mut self, guide: Onboarding) {
        let step = guide.step_id();
        let open = guide.is_open();
        if open && step == "try" {
            self.try_listening = false;
            self.try_done = false;
        }
        if let Err(error) = get().set_setting(setup::KEY, guide.to_value()) {
            get().status(&error, true);
        }
        self.state = get().snapshot();
        self.hover.clear();
        self.update(true);
        let focus = if !open {
            None
        } else if step == "try" {
            Some("ob-try-input")
        } else if self.controls.iter().any(|c| c.hwnd == GetFocus()) {
            None
        } else {
            Some("ob-next")
        };
        if let Some(c) = focus.and_then(|id| self.controls.iter().find(|c| c.id == id)) {
            let _ = SetFocus(Some(c.hwnd));
        }
    }
    fn model_row(
        &mut self,
        status_id: &str,
        action_id: &str,
        key: &str,
        loaded_key: &str,
        missing: &str,
    ) {
        let Some(status) = self.nodes.iter().find(|n| n.id == status_id).cloned() else {
            return;
        };
        let progress = self.state.progress.get(key).cloned();
        let downloaded = self.state.ready.contains(key);
        let loaded = self.state.ready.contains(loaded_key);
        let loading = key == "whisper" && self.state.busy.contains("whisper-load-btn");
        let downloading = self.state.busy.contains(action_id);
        let pausable = key == "whisper";
        let partial = if pausable && !downloaded && !downloading {
            self.paused_fraction()
        } else {
            None
        };
        let error = self.state.label(&format!("{key}-status-error"), "");
        let error_text = if downloaded && key == "whisper" {
            format!("✗ Load failed: {error}")
        } else {
            "✗ Download failed".into()
        };
        let paused_text = partial
            .map(|f| format!("Download paused at {:.0}%", f * 100.0))
            .unwrap_or_default();
        let text = if !error.is_empty() {
            error_text.as_str()
        } else if partial.is_some() {
            paused_text.as_str()
        } else if downloading {
            if key == "whisper" {
                "Downloading model…"
            } else {
                "Downloading…"
            }
        } else if loading {
            "Loading model (Vulkan)…"
        } else if loaded {
            if key == "whisper" {
                "Model ready (Vulkan)"
            } else {
                "Model ready"
            }
        } else if downloaded {
            "Model downloaded"
        } else {
            missing
        };
        let width = self.renderer.text_width(text, status.size, status.weight);
        let action = if downloaded && key == "whisper" {
            "whisper-load-btn"
        } else if downloading && pausable {
            "whisper-pause-btn"
        } else {
            action_id
        };
        let label = if downloaded {
            "Load Model"
        } else if downloading {
            "Pause"
        } else if partial.is_some() {
            "Resume Download"
        } else if key == "gec" && !error.is_empty() {
            "Retry Download"
        } else {
            "Download Model"
        };
        let button_width = self.renderer.text_width(label, 10.92, 600) + 28.0;
        let has_button =
            (!downloading || pausable) && !loading && !loaded && (key == "whisper" || !downloaded);
        let base_bottom = status.rect[1] - 5.5625 + 27.0;
        let row_height = if has_button { 27.0 } else { 17.85 };
        let delta = row_height - 27.0 + if progress.is_some() { 23.12 } else { 0.0 };
        for n in &mut self.nodes {
            if n.rect[1] >= base_bottom {
                n.rect[1] += delta;
            }
            if n.id == status_id {
                n.text = text.into();
                n.rect[2] = width.min(404.0 - button_width - 10.0) + 1.0;
                n.wrap = n.rect[2];
                n.nowrap = true;
                n.title = text.into();
                n.color = color(
                    self.state.dark(),
                    if !error.is_empty() {
                        "danger"
                    } else if loaded {
                        "ink"
                    } else {
                        "faint"
                    },
                );
                if !has_button {
                    n.rect[1] -= 4.5625;
                    n.rect[2] = 404.0;
                    n.wrap = 404.0;
                }
                if loading || downloading {
                    n.rect[0] += 16.0;
                    if !has_button {
                        n.rect[2] -= 16.0;
                    }
                    n.wrap = n.rect[2];
                }
            }
            if n.owner == action_id {
                n.owner = action.into();
                n.id = action.into();
                // Leave room for the spinner drawn before the status text.
                n.rect[0] = 92.0
                    + width.min(404.0 - button_width - 10.0)
                    + 10.0
                    + if loading || downloading { 16.0 } else { 0.0 };
                n.rect[2] = button_width;
                n.disabled = loading || downloading && !pausable;
                if n.kind == "text" {
                    n.text = label.into();
                    n.rect[0] += 14.0;
                    n.rect[2] -= 28.0;
                    n.wrap = n.rect[2];
                }
            }
        }
        if downloading || loading {
            self.nodes.push(Node {
                kind: "spinner".into(),
                rect: [
                    92.0,
                    status.rect[1] - 3.0 + if has_button { 4.5625 } else { 0.0 },
                    12.0,
                    12.0,
                ],
                color: color(self.state.dark(), "ink"),
                border: color(self.state.dark(), "rule"),
                opacity: 1.0,
                ..Default::default()
            });
        }
        if !has_button {
            self.nodes.retain(|n| n.owner != action);
        }
        if let Some((percent, label)) = progress {
            let y = status.rect[1] - 5.5625 + row_height + 9.0;
            self.nodes
                .push(self.rect([92.0, y + 4.56, 358.0, 6.0], "warm", "rule", 1.0, 3.0));
            self.nodes.push(self.rect(
                [93.0, y + 5.56, 356.0 * percent.clamp(0.0, 1.0), 4.0],
                "ink",
                "transparent",
                0.0,
                3.0,
            ));
            self.nodes.push(self.text(
                [460.0, y, 36.0, 15.12],
                &label,
                10.08,
                500,
                "faint",
                "right",
            ));
        }
        self.height += delta;
    }
    fn subtitles(&mut self) {
        if self.state.entries.is_empty() {
            let busy = self.state.busy.contains("sub-generate-btn");
            if let Some(file) = &self.state.file {
                let name = std::path::Path::new(file)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(file);
                for n in &mut self.nodes {
                    if n.id == "sub-file-name" {
                        n.text = name.into();
                        n.rect[0] = 107.0;
                        n.rect[2] = 374.0;
                        n.wrap = 374.0;
                        n.align = "center".into();
                        n.nowrap = true;
                        n.title = name.into();
                    }
                }
            }
            if self.state.file.is_none()
                && self.state.ready.contains("ffmpeg")
                && self.state.ready.contains("vad")
            {
                self.nodes.retain(|n| {
                    !n.owner.starts_with("sub-dep-")
                        && !n.owner.starts_with("sub-ffmpeg-")
                        && !n.owner.starts_with("sub-vad-")
                });
                for n in &mut self.nodes {
                    if n.owner == "sub-generate-btn" {
                        n.rect[1] -= 112.0;
                    }
                }
            }
            let enabled = self.state.file.is_some()
                && self.state.ready.contains("ffmpeg")
                && self.state.ready.contains("vad")
                && !busy;
            for n in &mut self.nodes {
                if n.owner == "sub-generate-btn" {
                    n.disabled = !enabled;
                    n.opacity = if enabled { 1.0 } else { 0.4 };
                    if n.kind == "text" {
                        n.text = if busy {
                            "Generating..."
                        } else {
                            "Generate Subtitles"
                        }
                        .into();
                        n.rect[0] = 92.0;
                        n.rect[2] = 404.0;
                        n.wrap = 404.0;
                        n.align = "center".into();
                    }
                }
            }
            if let Some((percent, message)) = self.state.progress.get("sub").cloned() {
                let y = self
                    .nodes
                    .iter()
                    .find(|n| n.id == "sub-generate-btn" && n.kind == "hit")
                    .map(|n| n.rect[1] + n.rect[3] + 16.0)
                    .unwrap_or(560.0);
                self.progress([92.0, y, 404.0, 6.0], percent, &message);
                self.height = self.height.max(y + 40.0 - 48.0);
            }
            return;
        }
        let title = self
            .state
            .file
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .and_then(|p| p.to_str())
            .unwrap_or("Subtitles")
            .to_owned();
        let summary_height = if self.state.summary.is_empty() {
            0.0
        } else {
            55.0 + self
                .renderer
                .text_height(&self.state.summary, 11.76, 400, 374.0, 18.228)
        };
        let progress = self.state.progress.get("sub").cloned();
        let progress_height = if progress.is_some() { 43.75 } else { 0.0 };
        let preview_y = 159.1875
            + progress_height
            + if summary_height > 0.0 {
                summary_height + 12.0
            } else {
                0.0
            };
        let preview_height = (514.0 - preview_y).clamp(60.0, 350.0);
        let buttons_y = preview_y + preview_height + 12.0;
        let preview = crate::subtitle::format_srt(&self.state.entries);
        let content_height = self
            .renderer
            .measure_height(&preview, 10.08, 400, 374.0, 15.624, true);
        self.preview_scroll = self
            .preview_scroll
            .min((content_height - (preview_height - 26.0)).max(0.0));
        let summary_key_missing = self.state.label("openrouter-key-input", "").is_empty();
        let busy = self.state.busy.contains("sub-summarize-btn");
        let summary_copied = self
            .copied
            .as_ref()
            .is_some_and(|(id, t)| id == "sub-summary-copy-btn" && t.elapsed().as_millis() < 1200);
        let copy_width = if summary_copied {
            self.renderer.text_width("Copied!", 9.52, 600) + 18.0
        } else {
            41.65625
        };
        for n in &mut self.nodes {
            match n.id.as_str() {
                "sub-result-title" => {
                    n.text = title.clone();
                    n.rect[0] = 92.0;
                    n.rect[1] = 114.1875 + progress_height;
                    n.rect[2] = 404.0;
                    n.wrap = 404.0;
                    n.nowrap = true;
                    n.title = title.clone();
                }
                "sub-preview-count" => {
                    n.text = format!("{} entries", self.state.entries.len());
                    n.rect[1] = 135.5 + progress_height;
                    n.rect[2] = 404.0;
                    n.wrap = 404.0;
                }
                "sub-summary" => {
                    n.rect[1] += progress_height;
                    if n.kind == "rect" {
                        n.rect[3] = summary_height;
                    }
                }
                "sub-summary-text" => {
                    n.text = self.state.summary.clone();
                    n.rect = [
                        107.0,
                        202.1875 + progress_height,
                        374.0,
                        summary_height - 55.0 + 2.0,
                    ];
                    n.wrap = 374.0;
                }
                "sub-preview" => {
                    if n.kind == "rect" {
                        n.rect = [92.0, preview_y, 404.0, preview_height];
                    } else if n.kind == "text" {
                        n.text = preview.clone();
                        n.classes.push_str(" mono preview");
                        n.rect = [
                            107.0,
                            preview_y + 15.0 - self.preview_scroll,
                            374.0,
                            content_height + 4.0,
                        ];
                        n.wrap = 374.0;
                        n.clip = [107.0, preview_y + 13.0, 374.0, preview_height - 26.0];
                    }
                }
                _ => {}
            }
            if n.owner == "sub-summary-copy-btn" {
                n.rect[1] += progress_height;
                if n.kind == "text" {
                    n.text = if summary_copied { "Copied!" } else { "Copy" }.into();
                    n.rect[0] = 481.0 - copy_width + 9.0;
                    n.rect[2] = copy_width - 18.0;
                    n.wrap = n.rect[2];
                } else {
                    n.rect[0] = 481.0 - copy_width;
                    n.rect[2] = copy_width;
                }
            }
            if ["sub-save-btn", "sub-summarize-btn", "sub-new-btn"].contains(&n.owner.as_str()) {
                let offset = if n.kind == "text" { 11.0 } else { 0.0 };
                n.rect[1] = buttons_y + offset;
            }
            if n.owner == "sub-summarize-btn" {
                n.disabled = busy || summary_key_missing;
                n.opacity = if n.disabled { 0.4 } else { 1.0 };
                n.title = if summary_key_missing {
                    "Set an OpenRouter API key in Settings to enable summaries.".into()
                } else {
                    String::new()
                };
                if n.kind == "text" {
                    n.text = if busy { "Summarizing..." } else { "Summarize" }.into();
                    n.rect[0] = 230.0;
                    n.rect[2] = 126.0;
                    n.wrap = 126.0;
                    n.align = "center".into();
                }
            }
        }
        if let Some((percent, message)) = progress {
            self.progress([92.0, 113.1875, 404.0, 6.0], percent, &message);
        }
        if content_height > preview_height - 26.0 {
            let inner_height = preview_height - 26.0;
            let thumb = (inner_height * inner_height / content_height).max(20.0);
            let y = preview_y
                + 13.0
                + self.preview_scroll / (content_height - inner_height) * (inner_height - thumb);
            self.nodes.push(self.rect(
                [492.0, y, 4.0, thumb],
                "scrollbar",
                "transparent",
                0.0,
                3.0,
            ));
        }
        self.height = 532.0 + (buttons_y + 38.0 - 564.0).max(0.0);
    }
    fn rect(&self, r: [f32; 4], bg: &str, border: &str, width: f32, radius: f32) -> Node {
        Node {
            kind: "rect".into(),
            rect: r,
            background: color(self.state.dark(), bg),
            border: color(self.state.dark(), border),
            width,
            radius,
            opacity: 1.0,
            ..Default::default()
        }
    }
    fn text(
        &self,
        r: [f32; 4],
        text: &str,
        size: f32,
        weight: i32,
        ink: &str,
        align: &str,
    ) -> Node {
        Node {
            kind: "text".into(),
            rect: r,
            text: text.into(),
            size,
            weight,
            line: size * 1.5,
            color: color(self.state.dark(), ink),
            align: align.into(),
            opacity: 1.0,
            wrap: r[2],
            nowrap: r[3] <= size * 1.8,
            ..Default::default()
        }
    }
    fn button(&mut self, r: [f32; 4], label: &str, id: &str, bg: &str) {
        let mut rect = self.rect(
            r,
            bg,
            if bg == "warm" || bg == "paper" {
                "rule"
            } else {
                "transparent"
            },
            if bg == "warm" || bg == "paper" {
                1.0
            } else {
                0.0
            },
            10.0,
        );
        rect.owner = id.into();
        self.nodes.push(rect);
        let mut text = self.text(
            [r[0], r[1] + (r[3] - 14.0) / 2.0, r[2], 14.0],
            label,
            11.48,
            600,
            if bg == "ink" || bg == "summary" {
                "paper"
            } else {
                "ink"
            },
            "center",
        );
        text.owner = id.into();
        self.nodes.push(text);
        self.nodes.push(Node {
            kind: "hit".into(),
            owner: id.into(),
            rect: r,
            text: label.into(),
            ..Default::default()
        });
    }
    fn progress(&mut self, r: [f32; 4], percent: f32, label: &str) {
        self.nodes.push(self.rect(r, "warm", "rule", 1.0, 3.0));
        self.nodes.push(self.rect(
            [r[0], r[1], r[2] * percent.clamp(0.0, 1.0), r[3]],
            "ink",
            "transparent",
            0.0,
            3.0,
        ));
        self.nodes.push(self.text(
            [r[0], r[1] + 12.0, r[2], 30.0],
            label,
            10.5,
            400,
            "light",
            "center",
        ));
    }
    unsafe fn check_wheel_routing(&mut self) -> Result<(), String> {
        #[derive(Default)]
        struct Probe {
            samples: usize,
            hidden: usize,
        }
        unsafe extern "system" fn probe_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
            _id: usize,
            data: usize,
        ) -> LRESULT {
            let result = DefSubclassProc(hwnd, msg, wparam, lparam);
            if msg == WM_PARENTNOTIFY || msg == WM_SETREDRAW {
                let probe = &mut *(data as *mut Probe);
                probe.samples += 1;
                probe.hidden += usize::from(!IsWindowVisible(hwnd).as_bool());
            }
            result
        }
        self.memory.scroll.clear();
        self.update(true);
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        let mut probe = Probe::default();
        // Sample during child creation/destruction: checking after WM_MOUSEWHEEL
        // returns misses a temporarily hidden input target.
        let result = (|| {
            if !SetWindowSubclass(
                self.hwnd,
                Some(probe_proc),
                2,
                (&mut probe as *mut Probe) as usize,
            )
            .as_bool()
            {
                return Err("Cannot attach scroll visibility check".into());
            }
            for child in [false, true] {
                for delta in [-30i16, -90, 120] {
                    let target = if child {
                        self.controls
                            .iter()
                            .find(|c| c.id == "tab:home")
                            .unwrap()
                            .hwnd
                    } else {
                        self.hwnd
                    };
                    let expected = self.scroll() - delta as f32 / 120.0 * 48.0;
                    SendMessageW(
                        target,
                        WM_MOUSEWHEEL,
                        Some(WPARAM((delta as u16 as usize) << 16)),
                        None,
                    );
                    if self.scroll() != expected {
                        return Err(
                            "Parent or child wheel input lost a fractional scroll delta".into()
                        );
                    }
                }
            }
            if probe.samples == 0 || probe.hidden != 0 {
                return Err(format!(
                    "Scrolling hid its input target in {} of {} rebuild samples",
                    probe.hidden, probe.samples
                ));
            }
            Ok(())
        })();
        let _ = RemoveWindowSubclass(self.hwnd, Some(probe_proc), 2);
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        result
    }
    unsafe fn update(&mut self, rebuild: bool) {
        if let Err(error) = self.renderer.set_avatar(self.state.avatar.as_ref()) {
            log::warn!("Cannot decode Google profile photo: {error}");
        }
        self.build_scene();
        let signature: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| {
                (n.kind == "hit" || n.kind == "edit" || n.kind == "select")
                    && (!self.modal || n.owner.starts_with("modal-"))
                    && !(n.rect[1] + n.rect[3] < 48.0
                        && n.rect[0] >= 68.0
                        && !n.owner.starts_with("titlebar-")
                        || n.rect[1] >= 580.0
                        || n.clip[2] > 0.0
                            && (n.rect[1] + n.rect[3] <= n.clip[1]
                                || n.rect[1] >= n.clip[1] + n.clip[3]))
            })
            .map(|n| (n.owner.clone(), n.rect, n.options.clone()))
            .collect();
        let previous: Vec<_> = self
            .controls
            .iter()
            .map(|c| (c.id.clone(), c.rect, c.options.clone()))
            .collect();
        // Keep the parent visible as an input target. WM_SETREDRAW(FALSE) would
        // hide it from Windows while leaving its old pixels on screen.
        let visible = IsWindowVisible(self.hwnd).as_bool();
        let controls_changed = rebuild || signature != previous;
        self.rebuilding = true;
        if controls_changed {
            self.create_controls();
        }
        for c in &self.controls {
            if let Some(n) = self.nodes.iter().find(|n| {
                n.owner == c.id && (n.kind == "hit" || n.kind == "edit" || n.kind == "select")
            }) {
                let _ = EnableWindow(c.hwnd, !n.disabled);
                if c.id == "theme-toggle" || c.id == "grammar-toggle" {
                    SendMessageW(
                        c.hwnd,
                        BM_SETCHECK,
                        Some(WPARAM(
                            if if c.id == "theme-toggle" {
                                self.state.dark()
                            } else {
                                self.state.checked("grammarCleanupEnabled")
                            } {
                                BST_CHECKED.0 as usize
                            } else {
                                BST_UNCHECKED.0 as usize
                            },
                        )),
                        None,
                    );
                }
                if c.kind == "edit" && !c.dirty && !self.memory.drafts.contains_key(&c.draft) {
                    let value = self.field(&c.id);
                    if read_text(c.hwnd) != value {
                        let _ = SetWindowTextW(c.hwnd, &HSTRING::from(value));
                    }
                }
            }
        }
        if !self.state.busy.is_empty()
            || self.state.ready.contains("status-active")
            || self.copied.is_some()
        {
            SetTimer(Some(self.hwnd), 1, 50, None);
        } else {
            let _ = KillTimer(Some(self.hwnd), 1);
        }
        if let Err(e) = self.paint() {
            log::error!("Native paint: {e}");
        }
        if controls_changed {
            // New controls erase and paint from this completed frame.
            for c in &self.controls {
                let _ = ShowWindow(c.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        let _ = RedrawWindow(
            Some(self.hwnd),
            None,
            None,
            RDW_INVALIDATE
                | RDW_ALLCHILDREN
                | RDW_NOERASE
                | if visible && controls_changed {
                    RDW_UPDATENOW
                } else {
                    REDRAW_WINDOW_FLAGS(0)
                },
        );
        self.rebuilding = false;
    }
    unsafe fn create_controls(&mut self) {
        self.rebuilding = true;
        // Preserve an active edit/caret across backend updates. Rebuild only when
        // layout or tab changes; its draft is captured before destroying HWNDs.
        let focused = GetFocus();
        let focus = self
            .controls
            .iter()
            .find(|c| c.hwnd == focused)
            .map(|c| (c.id.clone(), SendMessageW(c.hwnd, EM_GETSEL, None, None).0));
        for c in self.controls.drain(..) {
            if c.kind == "edit" && c.dirty {
                self.memory
                    .drafts
                    .insert(c.draft.clone(), read_text(c.hwnd));
            }
            let info = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND,
                hwnd: self.hwnd,
                uId: c.hwnd.0 as usize,
                ..Default::default()
            };
            SendMessageW(
                self.tooltip,
                TTM_DELTOOLW,
                None,
                Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
            );
            let _ = DestroyWindow(c.hwnd);
            if !c.font.0.is_null() {
                let _ = DeleteObject(c.font.into());
            }
        }
        self.tooltip_texts.clear();
        for n in self.nodes.clone() {
            if n.kind != "hit" && n.kind != "edit" && n.kind != "select" {
                continue;
            }
            if self.modal && !n.owner.starts_with("modal-") {
                continue;
            }
            if n.rect[1] + n.rect[3] < 48.0
                && n.rect[0] >= 68.0
                && !n.owner.starts_with("titlebar-")
                || n.rect[1] >= 580.0
                || n.clip[2] > 0.0
                    && (n.rect[1] + n.rect[3] <= n.clip[1] || n.rect[1] >= n.clip[1] + n.clip[3])
            {
                continue;
            }
            let is_edit = n.kind == "edit";
            let is_select = n.kind == "select";
            // Configure and subclass controls before making them visible.
            let style = WS_CHILD
                | WS_TABSTOP
                | WINDOW_STYLE(if is_edit {
                    ES_AUTOHSCROLL as u32
                        | if n.password && !self.key_shown(&n.id) {
                            ES_PASSWORD as u32
                        } else {
                            0
                        }
                } else if is_select {
                    CBS_DROPDOWNLIST as u32 | CBS_OWNERDRAWFIXED as u32 | CBS_HASSTRINGS as u32
                } else if n.owner == "theme-toggle" || n.owner == "grammar-toggle" {
                    BS_AUTOCHECKBOX as u32
                } else {
                    BS_OWNERDRAW as u32
                });
            let pad = if is_edit { n.padding[0] } else { 0.0 };
            let inset = if is_edit {
                (n.rect[3] - n.size * 1.25) / 2.0
            } else {
                0.0
            };
            let x = ((n.rect[0] + pad) * self.dpi).round() as i32;
            let y = ((n.rect[1] + inset) * self.dpi).round() as i32;
            let width = ((n.rect[2] - pad - if is_edit { n.padding[2].max(14.0) } else { 0.0 })
                * self.dpi)
                .round() as i32;
            let height = (if is_edit {
                n.size * 1.3
            } else if is_select {
                300.0
            } else {
                n.rect[3]
            } * self.dpi)
                .round() as i32;
            let id = self.controls.len() + 100;
            let draft = self.draft_key(&n.id);
            let caption = if is_edit {
                self.memory
                    .drafts
                    .get(&draft)
                    .cloned()
                    .unwrap_or_else(|| self.field(&n.id))
            } else {
                let words: Vec<_> = self
                    .nodes
                    .iter()
                    .filter(|t| t.kind == "text" && t.owner == n.owner)
                    .map(|t| t.text.as_str())
                    .collect();
                if !words.is_empty() {
                    words.join(" ")
                } else if !n.title.is_empty() {
                    n.title.clone()
                } else {
                    match n.owner.as_str() {
                        "theme-toggle" => "Dark theme",
                        "grammar-toggle" => "Grammar cleanup",
                        "sub-file-zone" => "Choose a file",
                        _ => n.owner.strip_prefix("tab:").unwrap_or(&n.owner),
                    }
                    .into()
                }
            };
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                if is_edit {
                    w!("EDIT")
                } else if is_select {
                    w!("COMBOBOX")
                } else {
                    w!("BUTTON")
                },
                &HSTRING::from(caption),
                style,
                x,
                y,
                width,
                height,
                Some(self.hwnd),
                Some(HMENU(id as *mut _)),
                None,
                None,
            )
            .unwrap();
            if n.rect[0] >= 68.0
                && !n.owner.starts_with("titlebar-")
                && !n.owner.starts_with("modal-")
            {
                let bounds = if n.clip[2] > 0.0 {
                    intersect(
                        rect(n.clip),
                        D2D_RECT_F {
                            left: 68.0,
                            top: 48.0,
                            right: 520.0,
                            bottom: 580.0,
                        },
                    )
                } else {
                    D2D_RECT_F {
                        left: 68.0,
                        top: 48.0,
                        right: 520.0,
                        bottom: 580.0,
                    }
                };
                let clip = CreateRectRgn(
                    ((bounds.left * self.dpi).round() as i32 - x).max(0),
                    ((bounds.top * self.dpi).round() as i32 - y).max(0),
                    ((bounds.right * self.dpi).round() as i32 - x).min(width),
                    ((bounds.bottom * self.dpi).round() as i32 - y).min(if is_select {
                        (n.rect[3] * self.dpi).round() as i32
                    } else {
                        height
                    }),
                );
                let _ = SetWindowRgn(hwnd, Some(clip), false);
            }
            let font = if is_edit || is_select {
                let font = CreateFontW(
                    -(n.size * self.dpi).round() as i32,
                    0,
                    0,
                    0,
                    n.weight as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH.0 as u32,
                    w!("Satoshi"),
                );
                SendMessageW(
                    hwnd,
                    WM_SETFONT,
                    Some(WPARAM(font.0 as usize)),
                    Some(LPARAM(0)),
                );
                if is_edit {
                    SendMessageW(
                        hwnd,
                        EM_SETCUEBANNER,
                        Some(WPARAM(1)),
                        Some(LPARAM(HSTRING::from(&n.placeholder).as_ptr() as isize)),
                    );
                }
                font
            } else {
                HFONT::default()
            };
            if is_select {
                SendMessageW(
                    hwnd,
                    CB_SETITEMHEIGHT,
                    Some(WPARAM(usize::MAX)),
                    Some(LPARAM((n.rect[3] * self.dpi).round() as isize - 6)),
                );
                for option in &n.options {
                    SendMessageW(
                        hwnd,
                        CB_ADDSTRING,
                        Some(WPARAM(0)),
                        Some(LPARAM(HSTRING::from(&option[1]).as_ptr() as isize)),
                    );
                }
                let selected = n
                    .options
                    .iter()
                    .position(|o| o[0] == self.field(&n.id))
                    .unwrap_or(0);
                SendMessageW(hwnd, CB_SETCURSEL, Some(WPARAM(selected)), None);
            }
            let _ = EnableWindow(hwnd, !n.disabled);
            let _ = SetWindowSubclass(hwnd, Some(control_proc), 1, 0);
            if !n.title.is_empty() {
                self.tooltip_texts.push(HSTRING::from(&n.title));
                let info = TTTOOLINFOW {
                    cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd: self.hwnd,
                    uId: hwnd.0 as usize,
                    lpszText: windows::core::PWSTR(
                        self.tooltip_texts.last().unwrap().as_ptr() as *mut u16
                    ),
                    ..Default::default()
                };
                SendMessageW(
                    self.tooltip,
                    TTM_ADDTOOLW,
                    None,
                    Some(LPARAM((&info as *const TTTOOLINFOW) as isize)),
                );
            }
            self.controls.push(Control {
                hwnd,
                id: n.owner,
                dirty: is_edit && self.memory.drafts.contains_key(&draft),
                draft,
                kind: n.kind,
                rect: n.rect,
                inner: [
                    x,
                    y,
                    width,
                    if is_select {
                        (n.rect[3] * self.dpi).round() as i32
                    } else {
                        height
                    },
                ],
                options: n.options,
                font,
            });
        }
        if let Some((id, selection)) = focus {
            if let Some(c) = self.controls.iter().find(|c| c.id == id) {
                let _ = SetFocus(Some(c.hwnd));
                if c.kind == "edit" {
                    SendMessageW(
                        c.hwnd,
                        EM_SETSEL,
                        Some(WPARAM(selection as u16 as usize)),
                        Some(LPARAM((selection >> 16) as u16 as isize)),
                    );
                }
            }
        }
    }
    unsafe fn paint(&mut self) -> windows::core::Result<()> {
        self.renderer.target.BeginDraw();
        self.renderer
            .target
            .Clear(Some(&parse_color(&color(self.state.dark(), "paper"))));
        let clip = D2D_RECT_F {
            left: 68.0,
            top: 48.0,
            right: 520.0,
            bottom: 580.0,
        };
        for n in &self.nodes {
            if n.id == "close-modal" && n.kind == "rect" {
                self.renderer.target.EndDraw(None, None)?;
                let mut pixels = self.renderer.pixels()?;
                blur(
                    &mut pixels,
                    self.renderer.width as usize,
                    self.renderer.height as usize,
                );
                self.renderer.target.BeginDraw();
                self.renderer.draw_pixels(&pixels)?;
            }
            let content = n.rect[0] >= 68.0
                && !n.owner.starts_with("titlebar-")
                && n.classes != "content"
                && n.owner != "close-modal"
                && !n.owner.starts_with("modal-");
            if content {
                self.renderer.target.PushAxisAlignedClip(
                    &if n.clip[2] > 0.0 {
                        intersect(rect(n.clip), clip)
                    } else {
                        clip
                    },
                    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                );
            }
            let mut n = n.clone();
            if n.kind == "edit" {
                n.text = self
                    .controls
                    .iter()
                    .find(|c| c.id == n.owner)
                    .map(|c| read_text(c.hwnd))
                    .unwrap_or_else(|| self.field(&n.id));
                n.password &= !self.key_shown(&n.id);
            }
            if self
                .controls
                .iter()
                .any(|c| c.id == n.owner && c.hwnd == GetFocus())
            {
                n.classes.push_str(" focused");
            }
            self.renderer.draw(&n, self.state.dark(), &self.hover)?;
            if content {
                self.renderer.target.PopAxisAlignedClip();
            }
        }
        if self.height > 532.0 {
            let viewport = if self.memory.tab == "home" || self.memory.tab == "dictionary" {
                self.list_viewport(self.memory.tab == "dictionary")
            } else {
                [68.0, 48.0, 452.0, 532.0]
            };
            let limit = self.height - 532.0;
            let height = (viewport[3] * viewport[3] / (viewport[3] + limit)).max(24.0);
            let top = viewport[1] + self.scroll() / limit * (viewport[3] - height);
            let mut n = self.rect(
                [viewport[0] + viewport[2] - 4.0, top, 4.0, height],
                "scrollbar",
                "transparent",
                0.0,
                3.0,
            );
            n.kind = "rect".into();
            self.renderer.draw(&n, self.state.dark(), "")?;
        }
        self.renderer.target.EndDraw(None, None)?;
        self.pixels = self.renderer.pixels()?;
        crate::native_benchmark::ready();
        Ok(())
    }
    unsafe fn capture_pixels(&self, message: u32) -> Result<Vec<u8>, String> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.renderer.width as i32,
                biHeight: -(self.renderer.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| e.to_string())?;
        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, bitmap.into());
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), self.pixels.len());
        pixels.copy_from_slice(&self.pixels);
        for c in &self.controls {
            let saved = SaveDC(dc);
            let _ = SetViewportOrgEx(dc, c.inner[0], c.inner[1], None);
            IntersectClipRect(dc, 0, 0, c.inner[2], c.inner[3]);
            if message == WM_ERASEBKGND {
                // Make a missing/custom-control erasure visible to the scrolling
                // regression check instead of inheriting the settled frame.
                let sentinel = CreateSolidBrush(COLORREF(0x00ff00ff));
                FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: c.inner[2],
                        bottom: c.inner[3],
                    },
                    sentinel,
                );
                let _ = DeleteObject(sentinel.into());
            }
            SendMessageW(
                c.hwnd,
                message,
                Some(WPARAM(dc.0 as usize)),
                Some(LPARAM(
                    (PRF_CLIENT | PRF_CHILDREN | PRF_ERASEBKGND) as isize,
                )),
            );
            let _ = RestoreDC(dc, saved);
        }
        let mut result = pixels.to_vec();
        for p in result.chunks_exact_mut(4) {
            p[3] = 255;
        }
        SelectObject(dc, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        Ok(result)
    }
    unsafe fn click(&mut self, id: &str) {
        if self
            .nodes
            .iter()
            .any(|n| n.kind == "hit" && n.owner == id && n.disabled)
        {
            return;
        }
        match id {
            "titlebar-minimize" => {
                let _ = ShowWindow(self.hwnd, SW_MINIMIZE);
            }
            "titlebar-close" => {
                self.open_modal();
            }
            "modal-cancel" => {
                self.close_modal();
            }
            "modal-quit" => {
                self.commit_edits();
                get().app.exit(0);
            }
            "modal-tray" => {
                self.commit_edits();
                self.modal = false;
                let hwnd = self.hwnd;
                self.hwnd = HWND::default();
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
            }
            "apikey-toggle" => {
                self.show_keys = !self.show_keys;
                self.update(true);
            }
            "openrouter-key-toggle" => {
                self.show_summary_key = !self.show_summary_key;
                self.update(true);
            }
            "ob-back" | "ob-next" | "ob-exit" => self.setup_step(id),
            "setup-guide-btn" | "setup-resume-btn" => {
                self.commit_edits();
                let mut guide = self.setup();
                guide.resume(&self.state.mode());
                self.enter_setup(guide);
            }
            id if id.starts_with("ob-engine-") => {
                let mut guide = self.setup();
                guide.choose(&id["ob-engine-".len()..]);
                self.enter_setup(guide);
            }
            id if id.starts_with("ob-feature-") => {
                let mut guide = self.setup();
                guide.complete();
                self.memory.tab = id["ob-feature-".len()..].into();
                self.enter_setup(guide);
            }
            "ob-model-btn" => get().action(
                if self.state.busy.contains("whisper-download-btn") {
                    "whisper-pause-btn"
                } else {
                    "whisper-download-btn"
                },
                String::new(),
            ),
            "ob-hotkey-btn" => get().action("hotkey-btn", String::new()),
            "ob-chip-0" | "ob-chip-1" => {
                let combo = if id == "ob-chip-0" {
                    "Ctrl+Shift+Space"
                } else {
                    "Ctrl+Alt+Space"
                };
                if let Err(e) = get().register_shortcut("hotkey-btn", combo) {
                    get().status(&e, true);
                }
            }
            id if id.starts_with("tab:") => {
                self.commit_edits();
                self.memory.tab = id[4..].into();
                self.state = get().snapshot();
                self.hover.clear();
                self.update(true);
            }
            "dict-add-btn" => {
                let value = self
                    .controls
                    .iter()
                    .find(|c| c.id == "dict-input")
                    .map(|c| read_text(c.hwnd))
                    .unwrap_or_default();
                get().action(id, value);
                self.memory.drafts.remove("dict-input");
                if let Some(c) = self.controls.iter_mut().find(|c| c.id == "dict-input") {
                    c.dirty = false;
                    self.rebuilding = true;
                    let _ = SetWindowTextW(c.hwnd, w!(""));
                    self.rebuilding = false;
                }
                self.state = get().snapshot();
                self.update(false);
            }
            "sub-summary-copy-btn" => {
                if get().copy_text(&self.state.summary).is_ok() {
                    self.copied = Some((id.into(), std::time::Instant::now()));
                    self.update(false);
                }
            }
            id if id.starts_with("history-") || id.starts_with("dict-remove:") => {
                let (action, index) = id.split_once(':').unwrap_or((id, ""));
                let index = index.parse::<usize>().unwrap_or(usize::MAX);
                let text = if action == "dict-remove" {
                    self.state
                        .data
                        .data
                        .dictionary
                        .terms
                        .get(index)
                        .map(|t| t.value.clone())
                } else {
                    self.state
                        .data
                        .data
                        .history
                        .entries
                        .get(index)
                        .map(|t| t.text.clone())
                };
                if action == "history-copy" {
                    if get().copy_text(&text.unwrap_or_default()).is_ok() {
                        self.copied = Some((id.into(), std::time::Instant::now()));
                        self.update(false);
                    }
                } else {
                    get().action(action, text.unwrap_or_default());
                }
            }
            _ => get().action(id, String::new()),
        }
    }
    unsafe fn open_modal(&mut self) {
        self.commit_edits();
        self.modal_focus = self
            .controls
            .iter()
            .find(|c| c.hwnd == GetFocus())
            .map(|c| c.id.clone());
        self.modal = true;
        self.update(true);
        if let Some(c) = self.controls.iter().find(|c| c.id == "modal-tray") {
            let _ = SetFocus(Some(c.hwnd));
        }
    }
    unsafe fn close_modal(&mut self) {
        self.modal = false;
        self.update(true);
        if let Some(c) = self
            .modal_focus
            .take()
            .and_then(|id| self.controls.iter().find(|c| c.id == id))
        {
            let _ = SetFocus(Some(c.hwnd));
        }
    }
    unsafe fn pretranslate(&mut self, msg: &MSG) -> bool {
        if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
            return false;
        }
        if self.state.recording_hotkey.is_some() || msg.wParam.0 == VK_ESCAPE.0 as usize {
            SendMessageW(self.hwnd, WM_KEYDOWN, Some(msg.wParam), Some(msg.lParam));
            return true;
        }
        if msg.wParam.0 == VK_RETURN.0 as usize
            && self
                .controls
                .iter()
                .any(|c| c.hwnd == msg.hwnd && c.id == "dict-input")
        {
            self.click("dict-add-btn");
            return true;
        }
        if msg.wParam.0 == VK_RETURN.0 as usize
            && self.controls.iter().any(|c| {
                c.hwnd == msg.hwnd
                    && matches!(
                        c.id.as_str(),
                        "ob-groq-key" | "ob-azure-endpoint" | "ob-azure-key"
                    )
            })
        {
            self.click("ob-next");
            return true;
        }
        if msg.wParam.0 == VK_RETURN.0 as usize {
            if let Some(id) = self
                .controls
                .iter()
                .find(|c| {
                    c.hwnd == msg.hwnd && c.kind == "hit" && IsWindowEnabled(c.hwnd).as_bool()
                })
                .map(|c| c.id.clone())
            {
                self.click(&id);
                return true;
            }
        }
        false
    }
    unsafe fn choose_file(&mut self, save: bool) {
        let state = get().snapshot();
        if state.busy.contains("sub-generate-btn") || state.busy.contains("sub-summarize-btn") {
            return;
        }
        let focus = self
            .controls
            .iter()
            .find(|c| c.hwnd == GetFocus())
            .map(|c| c.id.clone());
        let name = state
            .file
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .and_then(|p| p.to_str())
            .unwrap_or("subtitles");
        let result = if save {
            crate::native_picker::save(self.hwnd, &format!("{name}.srt"))
        } else {
            crate::native_picker::open(self.hwnd)
        };
        let current = get().snapshot();
        if current.file != state.file
            || crate::subtitle::format_srt(&current.entries)
                != crate::subtitle::format_srt(&state.entries)
        {
            return;
        }
        match result {
            Ok(Some(path)) => {
                if save {
                    if let Err(e) =
                        crate::save_srt_file(state.entries, path.to_string_lossy().into_owned())
                    {
                        get().status(&e, true);
                    }
                } else {
                    self.preview_scroll = 0.0;
                    get().change(|s| {
                        s.file = Some(path.to_string_lossy().into_owned());
                        s.entries.clear();
                        s.summary.clear();
                        s.progress.remove("sub");
                    });
                }
            }
            Ok(None) => {}
            Err(e) => get().status(&e, true),
        }
        self.state = get().snapshot();
        self.update(false);
        if let Some(c) = focus.and_then(|id| self.controls.iter().find(|c| c.id == id)) {
            let _ = SetFocus(Some(c.hwnd));
        }
    }
    unsafe fn commit_edits(&mut self) {
        let values: Vec<_> = self
            .controls
            .iter()
            .filter(|c| {
                c.kind == "edit" && c.dirty && c.id != "dict-input" && c.id != "ob-try-input"
            })
            .map(|c| (c.draft.clone(), read_text(c.hwnd)))
            .collect();
        for (id, value) in values {
            self.save_edit(&id, value);
        }
    }
    unsafe fn save_edit(&mut self, id: &str, value: String) {
        get().action(id, value.clone());
        let state = get().snapshot();
        let saved = match id {
            "azure-endpoint-input" => state.setting("azureMaiEndpoint", "") == value,
            "openrouter-key-input" => state.label(id, "") == value.trim(),
            id if id.starts_with("apikey-input:") => {
                state.label(
                    if id.ends_with("azure-mai") {
                        "azure-key"
                    } else {
                        "groq-key"
                    },
                    "",
                ) == value.trim()
            }
            _ => false,
        };
        if saved {
            self.memory.drafts.remove(id);
            for c in &mut self.controls {
                if c.draft == id {
                    c.dirty = false;
                }
            }
        } else {
            self.memory.drafts.insert(id.into(), value);
        }
    }
    unsafe fn wheel(&mut self, delta: f32) {
        if self.modal {
            return;
        }
        if self.memory.tab == "subtitling" && !self.state.entries.is_empty() {
            if let Some(n) = self.nodes.iter().find(|n| {
                n.classes.contains("mono preview") && inside(n.clip, self.mouse[0], self.mouse[1])
            }) {
                let content = self
                    .renderer
                    .measure_height(&n.text, n.size, n.weight, n.wrap, n.line, true);
                self.preview_scroll =
                    (self.preview_scroll - delta).clamp(0.0, (content - n.clip[3]).max(0.0));
                self.update(false);
                return;
            }
        }
        let current = self.scroll();
        let next = (current - delta).clamp(0.0, (self.height - 532.0).max(0.0));
        if next != current {
            self.memory.scroll.insert(self.memory.tab.clone(), next);
            self.update(true);
        }
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            self.rebuilding = true;
            if !self.hwnd.0.is_null() {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.hwnd);
            }
            for c in self.controls.drain(..) {
                if !c.font.0.is_null() {
                    let _ = DeleteObject(c.font.into());
                }
            }
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Window;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let w = &mut *pointer;
    match msg {
        WM_COMMAND if w.rebuilding => return LRESULT(0),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            blit(
                dc,
                &w.pixels,
                w.renderer.width,
                w.renderer.height,
                [0, 0, w.renderer.width as i32, w.renderer.height as i32],
            );
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_DRAWITEM => {
            let item = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if let Some(c) = w.controls.iter().find(|c| c.hwnd == item.hwndItem) {
                if c.kind == "select" && item.itemID != u32::MAX {
                    let selected = item.itemState.0 & ODS_SELECTED.0 != 0;
                    let brush = CreateSolidBrush(rgb(&color(
                        w.state.dark(),
                        if selected { "rule" } else { "paper" },
                    )));
                    FillRect(item.hDC, &item.rcItem, brush);
                    let _ = DeleteObject(brush.into());
                    SetTextColor(item.hDC, rgb(&color(w.state.dark(), "ink")));
                    SetBkMode(item.hDC, TRANSPARENT);
                    let text = c
                        .options
                        .get(item.itemID as usize)
                        .map(|o| o[1].as_str())
                        .unwrap_or("");
                    let mut text: Vec<_> = text.encode_utf16().collect();
                    let mut r = item.rcItem;
                    r.left += 12;
                    DrawTextW(item.hDC, &mut text, &mut r, DT_SINGLELINE | DT_VCENTER);
                } else {
                    blit(
                        item.hDC,
                        &w.pixels,
                        w.renderer.width,
                        w.renderer.height,
                        [
                            (c.rect[0] * w.dpi).round() as i32,
                            (c.rect[1] * w.dpi).round() as i32,
                            (c.rect[2] * w.dpi).round() as i32,
                            (c.rect[3] * w.dpi).round() as i32,
                        ],
                    );
                    if item.itemState.0 & ODS_FOCUS.0 != 0 {
                        let mut focus = item.rcItem;
                        focus.left += 3;
                        focus.top += 3;
                        focus.right -= 3;
                        focus.bottom -= 3;
                        let _ = DrawFocusRect(item.hDC, &focus);
                    }
                }
            }
            return LRESULT(1);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
            let dc = HDC(wparam.0 as *mut _);
            SetTextColor(dc, rgb(&color(w.state.dark(), "ink")));
            SetBkColor(dc, rgb(&color(w.state.dark(), "warm")));
            return LRESULT(w.renderer.edit_brush(w.state.dark()).0 as isize);
        }
        WM_COMMAND => {
            let control = HWND(lparam.0 as *mut _);
            let code = (wparam.0 >> 16) as u32;
            if control.0.is_null() && wparam.0 as u16 == IDCANCEL.0 as u16 {
                w.close_modal();
                get().change(|s| s.recording_hotkey = None);
                w.state = get().snapshot();
                w.update(false);
                return LRESULT(0);
            }
            if let Some(index) = w.controls.iter().position(|c| c.hwnd == control) {
                let id = w.controls[index].id.clone();
                let kind = w.controls[index].kind.clone();
                if kind == "select" && code == CBN_SELCHANGE as u32 {
                    let selected = SendMessageW(control, CB_GETCURSEL, None, None).0 as usize;
                    let value = w.controls[index]
                        .options
                        .get(selected)
                        .map(|o| o[0].clone())
                        .unwrap_or_default();
                    w.commit_edits();
                    get().action(alias(&id), value);
                } else if kind == "edit" && code == EN_CHANGE as u32 {
                    w.controls[index].dirty = true;
                    w.update(false);
                } else if kind == "edit" && code == EN_KILLFOCUS as u32 && w.controls[index].dirty {
                    let value = read_text(control);
                    let scoped = w.controls[index].draft.clone();
                    if id == "dict-input" || id == "ob-try-input" {
                        w.memory.drafts.insert(scoped, value);
                    } else {
                        w.save_edit(&scoped, value);
                    }
                } else if kind == "hit" && code == BN_CLICKED as u32 {
                    w.click(&id);
                }
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            w.open_modal();
            return LRESULT(0);
        }
        WM_TIMER => {
            if w.copied
                .as_ref()
                .is_some_and(|(_, t)| t.elapsed().as_millis() >= 1200)
            {
                w.copied = None;
            }
            w.update(false);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 as u16 as f32) / 96.0;
            if let Ok(renderer) = Renderer::new(dpi) {
                w.dpi = dpi;
                w.renderer = renderer;
                let bounds = &*(lparam.0 as *const RECT);
                let width = (520.0 * dpi).round() as i32;
                let height = (580.0 * dpi).round() as i32;
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    bounds.left,
                    bounds.top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let region = CreateRoundRectRgn(
                    0,
                    0,
                    width + 1,
                    height + 1,
                    (20.0 * dpi) as i32,
                    (20.0 * dpi) as i32,
                );
                let _ = SetWindowRgn(hwnd, Some(region), true);
                w.update(true);
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let mut p = POINT {
                x: lparam.0 as i16 as i32,
                y: (lparam.0 >> 16) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut p);
            if !w.modal && (p.y as f32 / w.dpi) < 48.0 && (p.x as f32 / w.dpi) < 452.0 {
                return LRESULT(HTCAPTION as isize);
            }
        }
        WM_MOUSEWHEEL => {
            w.wheel(((wparam.0 >> 16) as i16 as f32) / 120.0 * 48.0);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            w.mouse = [
                lparam.0 as i16 as f32 / w.dpi,
                (lparam.0 >> 16) as i16 as f32 / w.dpi,
            ];
            if w.drag_scroll {
                let viewport = if w.memory.tab == "home" || w.memory.tab == "dictionary" {
                    w.list_viewport(w.memory.tab == "dictionary")
                } else {
                    [68.0, 48.0, 452.0, 532.0]
                };
                let position = (w.mouse[1] - viewport[1]) / viewport[3] * (w.height - 532.0);
                w.memory.scroll.insert(
                    w.memory.tab.clone(),
                    position.clamp(0.0, (w.height - 532.0).max(0.0)),
                );
                w.update(true);
            } else if w.memory.tab == "home" || w.memory.tab == "dictionary" {
                w.hover.clear();
                w.update(false);
            }
        }
        WM_LBUTTONDOWN => {
            let x = lparam.0 as i16 as f32 / w.dpi;
            let y = (lparam.0 >> 16) as i16 as f32 / w.dpi;
            if w.modal {
                if !w
                    .nodes
                    .iter()
                    .any(|n| n.classes == "modal-card" && inside(n.rect, x, y))
                {
                    w.close_modal();
                }
            } else {
                let viewport = if w.memory.tab == "home" || w.memory.tab == "dictionary" {
                    w.list_viewport(w.memory.tab == "dictionary")
                } else {
                    [68.0, 48.0, 452.0, 532.0]
                };
                if w.height > 532.0 && x > viewport[0] + viewport[2] - 6.0 && inside(viewport, x, y)
                {
                    w.drag_scroll = true;
                    SetCapture(hwnd);
                }
            }
        }
        WM_LBUTTONUP => {
            w.drag_scroll = false;
            let _ = ReleaseCapture();
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                w.close_modal();
                get().change(|s| s.recording_hotkey = None);
                w.state = get().snapshot();
                w.update(false);
                return LRESULT(0);
            }
            if let Some(id) = w.state.recording_hotkey.clone() {
                let vk = wparam.0 as u16;
                if ![VK_SHIFT.0, VK_CONTROL.0, VK_MENU.0, VK_LWIN.0, VK_RWIN.0].contains(&vk) {
                    let combo = key_combo(vk);
                    if let Err(e) = get().register_shortcut(&id, &combo) {
                        get().status(&e, true);
                    }
                    return LRESULT(0);
                }
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
unsafe extern "system" fn control_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    let parent = GetParent(hwnd).unwrap_or_default();
    let ptr = GetWindowLongPtrW(parent, GWLP_USERDATA) as *mut Window;
    if !ptr.is_null() {
        let w = &mut *ptr;
        match msg {
            WM_ERASEBKGND => {
                if let Some(c) = w.controls.iter().find(|c| c.hwnd == hwnd) {
                    // Stock controls may erase before WM_PAINT/WM_DRAWITEM.
                    // Use the same frame here rather than their system brush.
                    blit(
                        HDC(wparam.0 as *mut _),
                        &w.pixels,
                        w.renderer.width,
                        w.renderer.height,
                        c.inner,
                    );
                    return LRESULT(1);
                }
            }
            WM_MOUSEMOVE => {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(parent, &mut p);
                w.mouse = [p.x as f32 / w.dpi, p.y as f32 / w.dpi];
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                let _ = TrackMouseEvent(&mut tracking);
                let id = w
                    .controls
                    .iter()
                    .find(|c| c.hwnd == hwnd)
                    .map(|c| c.id.clone())
                    .unwrap_or_default();
                if w.hover != id {
                    w.hover = id;
                    w.update(false);
                }
            }
            WM_MOUSELEAVE => {
                if w.controls.iter().any(|c| c.hwnd == hwnd && c.id == w.hover) {
                    w.hover.clear();
                    w.update(false);
                }
            }
            WM_MOUSEWHEEL => {
                if w.controls
                    .iter()
                    .any(|c| c.hwnd == hwnd && c.kind == "select")
                    && SendMessageW(hwnd, CB_GETDROPPEDSTATE, None, None).0 != 0
                {
                    return DefSubclassProc(hwnd, msg, wparam, lparam);
                }
                w.wheel(((wparam.0 >> 16) as i16 as f32) / 120.0 * 48.0);
                return LRESULT(0);
            }
            WM_GETDLGCODE => {
                if w.state.recording_hotkey.is_some()
                    || wparam.0 == VK_RETURN.0 as usize
                        && w.controls
                            .iter()
                            .any(|c| c.hwnd == hwnd && c.id == "dict-input")
                {
                    return LRESULT(DLGC_WANTALLKEYS as isize);
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                if w.state.recording_hotkey.is_some() {
                    SendMessageW(parent, WM_KEYDOWN, Some(wparam), Some(lparam));
                    return LRESULT(0);
                }
                if wparam.0 == VK_RETURN.0 as usize
                    && w.controls
                        .iter()
                        .any(|c| c.hwnd == hwnd && c.id == "dict-input")
                {
                    w.click("dict-add-btn");
                    return LRESULT(0);
                }
                if wparam.0 == VK_ESCAPE.0 as usize {
                    SendMessageW(parent, WM_KEYDOWN, Some(wparam), Some(lparam));
                    return LRESULT(0);
                }
            }
            WM_SETCURSOR => {
                if let Some(c) = w.controls.iter().find(|c| c.hwnd == hwnd) {
                    if let Ok(cursor) = LoadCursorW(
                        None,
                        if c.kind == "edit" {
                            IDC_IBEAM
                        } else {
                            IDC_HAND
                        },
                    ) {
                        SetCursor(Some(cursor));
                        return LRESULT(1);
                    }
                }
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                if !w.rebuilding {
                    w.update(false);
                }
                return result;
            }
            WM_PAINT | WM_PRINT | WM_PRINTCLIENT => {
                if let Some(c) = w.controls.iter().find(|c| {
                    c.hwnd == hwnd
                        && (c.kind == "select"
                            || c.kind == "edit" && read_text(hwnd).is_empty()
                            || c.id == "theme-toggle"
                            || c.id == "grammar-toggle")
                }) {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = if msg == WM_PAINT {
                        BeginPaint(hwnd, &mut ps)
                    } else {
                        HDC(wparam.0 as *mut _)
                    };
                    blit(dc, &w.pixels, w.renderer.width, w.renderer.height, c.inner);
                    if msg == WM_PAINT {
                        let _ = EndPaint(hwnd, &ps);
                    }
                    return LRESULT(0);
                }
            }
            _ => {}
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}
unsafe fn read_text(hwnd: HWND) -> String {
    let mut text = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
    let length = GetWindowTextW(hwnd, &mut text);
    String::from_utf16_lossy(&text[..length as usize])
}
unsafe fn blit(dc: HDC, pixels: &[u8], width: u32, height: u32, region: [i32; 4]) {
    // A cropped top-down DIB avoids GDI's source-origin/viewport ambiguity when
    // owner-drawn child controls print into a translated DC.
    let [x, y, w, h] = region;
    if pixels.is_empty() || w <= 0 || h <= 0 {
        return;
    }
    let mut crop = vec![0u8; (w as usize) * (h as usize) * 4];
    for row in 0..h {
        let sy = y + row;
        if sy < 0 || sy >= height as i32 {
            continue;
        }
        let left = x.max(0);
        let right = (x + w).min(width as i32);
        if right <= left {
            continue;
        }
        let source = (sy as usize * width as usize + left as usize) * 4;
        let dest = (row as usize * w as usize + (left - x) as usize) * 4;
        let count = (right - left) as usize * 4;
        crop[dest..dest + count].copy_from_slice(&pixels[source..source + count]);
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    StretchDIBits(
        dc,
        0,
        0,
        w,
        h,
        0,
        0,
        w,
        h,
        Some(crop.as_ptr().cast()),
        &info,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
}
/// Setup-guide controls that edit the same values as their Settings twins.
fn alias(id: &str) -> &str {
    match id {
        "ob-mic-select" => "mic-select",
        "ob-lang-select" => "live-lang-select",
        "ob-azure-endpoint" => "azure-endpoint-input",
        _ => id,
    }
}
fn color(dark: bool, name: &str) -> String {
    match (dark, name) {
        (_, "transparent") => "rgba(0, 0, 0, 0)",
        (_, "summary") => {
            if dark {
                "#7f7f87"
            } else {
                "#5b5b63"
            }
        }
        (false, "paper") => "#faf9f7",
        (false, "warm") => "#f5f3ef",
        (false, "hover") => "#ffffff",
        (false, "ink") => "#2c2c2c",
        (false, "light") => "#6b6b6b",
        (false, "faint") => "#a0a09c",
        (false, "rule") => "#e8e6e1",
        (false, "rule-hover") => "#d4d1cb",
        (false, "danger") => "#8b4444",
        (true, "danger") => "#c06060",
        (false, "scrollbar") => "#e8e6e1",
        (true, "scrollbar") => "#444444",
        (true, "paper") => "#1a1a1e",
        (true, "warm") => "#222226",
        (true, "hover") => "#2a2a2e",
        (true, "ink") => "#e8e6e1",
        (true, "light") => "#a0a0a0",
        (true, "faint") => "#6b6b6b",
        (true, "rule") => "#333338",
        (true, "rule-hover") => "#444448",
        _ => "#2c2c2c",
    }
    .into()
}
fn parse_color(text: &str) -> D2D1_COLOR_F {
    if let Some(hex) = text.strip_prefix('#') {
        if let Ok(rgb) = u32::from_str_radix(hex, 16) {
            return D2D1_COLOR_F {
                r: ((rgb >> 16) & 255) as f32 / 255.0,
                g: ((rgb >> 8) & 255) as f32 / 255.0,
                b: (rgb & 255) as f32 / 255.0,
                a: 1.0,
            };
        }
    }
    let values: Vec<f32> = text
        .trim_start_matches("rgba(")
        .trim_start_matches("rgb(")
        .trim_end_matches(')')
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    D2D1_COLOR_F {
        r: values.first().copied().unwrap_or(0.0) / 255.0,
        g: values.get(1).copied().unwrap_or(0.0) / 255.0,
        b: values.get(2).copied().unwrap_or(0.0) / 255.0,
        a: values.get(3).copied().unwrap_or(1.0),
    }
}
fn rgb(text: &str) -> COLORREF {
    let c = parse_color(text);
    COLORREF((c.r * 255.0) as u32 | ((c.g * 255.0) as u32) << 8 | ((c.b * 255.0) as u32) << 16)
}
fn rect(r: [f32; 4]) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r[0],
        top: r[1],
        right: r[0] + r[2],
        bottom: r[1] + r[3],
    }
}
fn inside(r: [f32; 4], x: f32, y: f32) -> bool {
    x >= r[0] && y >= r[1] && x < r[0] + r[2] && y < r[1] + r[3]
}
fn intersect(a: D2D_RECT_F, b: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right).max(a.left.max(b.left)),
        bottom: a.bottom.min(b.bottom).max(a.top.max(b.top)),
    }
}
// Three separable box passes approximate the original 4px backdrop blur.
fn blur(pixels: &mut [u8], width: usize, height: usize) {
    let mut temp = vec![0u8; pixels.len()];
    for radius in [3usize, 4, 4] {
        for horizontal in [true, false] {
            let (rows, columns) = if horizontal {
                (height, width)
            } else {
                (width, height)
            };
            let at = |row: usize, col: usize| {
                if horizontal {
                    (row * width + col) * 4
                } else {
                    (col * width + row) * 4
                }
            };
            for row in 0..rows {
                let mut sums = [0u32; 4];
                for k in 0..=radius * 2 {
                    let p = at(row, k.saturating_sub(radius).min(columns - 1));
                    for c in 0..4 {
                        sums[c] += pixels[p + c] as u32;
                    }
                }
                for col in 0..columns {
                    let p = at(row, col);
                    for c in 0..4 {
                        temp[p + c] = (sums[c] / (radius * 2 + 1) as u32) as u8;
                    }
                    let remove = at(row, col.saturating_sub(radius));
                    let add = at(row, (col + radius + 1).min(columns - 1));
                    for c in 0..4 {
                        sums[c] = sums[c] - pixels[remove + c] as u32 + pixels[add + c] as u32;
                    }
                }
            }
            pixels.copy_from_slice(&temp);
        }
    }
}

struct Renderer {
    wic: IWICImagingFactory,
    avatar: Option<Arc<Vec<u8>>>,
    target: ID2D1RenderTarget,
    bitmap: IWICBitmap,
    write: IDWriteFactory,
    font_collection: IDWriteFontCollection,
    font_loader: IDWriteInMemoryFontFileLoader,
    images: HashMap<String, ID2D1Bitmap>,
    width: u32,
    height: u32,
    light_brush: HBRUSH,
    dark_brush: HBRUSH,
    font_handles: Vec<HANDLE>,
    mono_family: OnceLock<HSTRING>,
}
impl Renderer {
    unsafe fn new(dpi: f32) -> windows::core::Result<Self> {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let width = (520.0 * dpi).round() as u32;
        let height = (580.0 * dpi).round() as u32;
        let bitmap = wic.CreateBitmap(
            width,
            height,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapCacheOnLoad,
        )?;
        let target = factory.CreateWicBitmapRenderTarget(
            &bitmap,
            &D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0 * dpi,
                dpiY: 96.0 * dpi,
                ..Default::default()
            },
        )?;
        target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let write5: IDWriteFactory5 = write.cast()?;
        let builder = write5.CreateFontSetBuilder()?;
        let font_loader = write5.CreateInMemoryFontFileLoader()?;
        write.RegisterFontFileLoader(&font_loader)?;
        let mut handles = vec![];
        for bytes in [
            include_bytes!("../assets/fonts/Satoshi-300.otf").as_slice(),
            include_bytes!("../assets/fonts/Satoshi-400.otf").as_slice(),
            include_bytes!("../assets/fonts/Satoshi-500.otf").as_slice(),
            include_bytes!("../assets/fonts/Satoshi-700.otf").as_slice(),
        ] {
            let file = font_loader.CreateInMemoryFontFileReference(
                &write,
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                None,
            )?;
            builder.AddFontFile(&file)?;
            let mut count = 0;
            handles.push(AddFontMemResourceEx(
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                None,
                &mut count,
            ));
        }
        let collection = write
            .cast::<IDWriteFactory3>()?
            .CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)?
            .cast()?;
        Ok(Self {
            wic,
            avatar: None,
            target,
            bitmap,
            write,
            font_collection: collection,
            font_loader,
            images: HashMap::new(),
            width,
            height,
            light_brush: CreateSolidBrush(rgb("#f5f3ef")),
            dark_brush: CreateSolidBrush(rgb("#222226")),
            font_handles: handles,
            mono_family: OnceLock::new(),
        })
    }
    unsafe fn set_avatar(&mut self, bytes: Option<&Arc<Vec<u8>>>) -> windows::core::Result<()> {
        if match (&self.avatar, bytes) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        } {
            return Ok(());
        }
        self.avatar = bytes.cloned();
        self.images.remove("native-profile-avatar");
        let Some(bytes) = bytes else {
            return Ok(());
        };
        let stream = self.wic.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder = self.wic.CreateDecoderFromStream(
            &stream,
            std::ptr::null(),
            WICDecodeMetadataCacheOnLoad,
        )?;
        let frame = decoder.GetFrame(0)?;
        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(windows::core::Error::from_hresult(E_INVALIDARG));
        }
        let side = width.min(height);
        let crop = self.wic.CreateBitmapClipper()?;
        crop.Initialize(
            &frame,
            &WICRect {
                X: ((width - side) / 2) as i32,
                Y: ((height - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            },
        )?;
        let scaler = self.wic.CreateBitmapScaler()?;
        scaler.Initialize(&crop, 64, 64, WICBitmapInterpolationModeFant)?;
        let converter = self.wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut pixels = vec![0; 64 * 64 * 4];
        converter.CopyPixels(std::ptr::null(), 64 * 4, &mut pixels)?;
        for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            let x = (i % 64) as f32 + 0.5 - 32.0;
            let y = (i / 64) as f32 + 0.5 - 32.0;
            let coverage = (32.5 - (x * x + y * y).sqrt()).clamp(0.0, 1.0);
            for channel in pixel {
                *channel = (*channel as f32 * coverage).round() as u8;
            }
        }
        let bitmap = self.target.CreateBitmap(
            D2D_SIZE_U {
                width: 64,
                height: 64,
            },
            Some(pixels.as_ptr().cast()),
            64 * 4,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            },
        )?;
        self.images.insert("native-profile-avatar".into(), bitmap);
        Ok(())
    }
    unsafe fn format(
        &self,
        size: f32,
        weight: i32,
        mono: bool,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let family = if mono {
            self.mono_family
                .get_or_init(|| {
                    let mut system = None;
                    let _ = self.write.GetSystemFontCollection(&mut system, false);
                    if let Some(system) = system {
                        for family in ["SF Mono", "Cascadia Code", "Fira Code", "Consolas"] {
                            let mut index = 0;
                            let mut exists = false.into();
                            if system
                                .FindFamilyName(&HSTRING::from(family), &mut index, &mut exists)
                                .is_ok()
                                && exists.as_bool()
                            {
                                return HSTRING::from(family);
                            }
                        }
                    }
                    HSTRING::from("Consolas")
                })
                .clone()
        } else {
            HSTRING::from("Satoshi")
        };
        self.write.CreateTextFormat(
            &family,
            if mono {
                None
            } else {
                Some(&self.font_collection)
            },
            DWRITE_FONT_WEIGHT(if weight == 600 { 700 } else { weight }),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-US"),
        )
    }
    fn text_height(&self, text: &str, size: f32, weight: i32, width: f32, line: f32) -> f32 {
        self.measure_height(text, size, weight, width, line, false)
    }
    fn measure_height(
        &self,
        text: &str,
        size: f32,
        weight: i32,
        width: f32,
        line: f32,
        mono: bool,
    ) -> f32 {
        unsafe {
            let Ok(format) = self.format(size, weight, mono) else {
                return line;
            };
            let _ = format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line, size);
            let Ok(layout) = self.write.CreateTextLayout(
                &text.encode_utf16().collect::<Vec<_>>(),
                &format,
                width,
                10000.0,
            ) else {
                return line;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut metrics);
            metrics.height
        }
    }
    fn text_width(&self, text: &str, size: f32, weight: i32) -> f32 {
        unsafe {
            let Ok(format) = self.format(size, weight, false) else {
                return 0.0;
            };
            let Ok(layout) = self.write.CreateTextLayout(
                &text.encode_utf16().collect::<Vec<_>>(),
                &format,
                10000.0,
                1000.0,
            ) else {
                return 0.0;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut metrics);
            metrics.widthIncludingTrailingWhitespace
        }
    }
    unsafe fn draw(&mut self, n: &Node, dark: bool, hover: &str) -> windows::core::Result<()> {
        let mut r = n.rect;
        if r[2] <= 0.0 || r[3] <= 0.0 {
            return Ok(());
        }
        let hovered = !n.owner.is_empty() && n.owner == hover;
        let opacity = n.opacity;
        if opacity == 0.0 {
            return Ok(());
        }
        match n.kind.as_str() {
            "rect" | "select" | "edit" => {
                let mut bg = parse_color(&n.background);
                let mut border = parse_color(&n.border);
                if hovered
                    && (n.kind == "edit"
                        || n.kind == "select"
                        || n.classes.contains("modal-btn-secondary")
                        || n.classes.contains("ob-btn-secondary")
                        || n.classes.contains("ob-link-btn")
                        || n.classes.contains("ob-card") && !n.classes.contains("selected"))
                {
                    bg = parse_color(&color(dark, "hover"));
                    border = parse_color(&color(dark, "rule-hover"));
                }
                if n.classes.contains("focused") && (n.kind == "edit" || n.kind == "select") {
                    border = parse_color(&color(dark, "faint"));
                    let ring = self.target.CreateSolidColorBrush(
                        &D2D1_COLOR_F {
                            r: if dark { 1.0 } else { 0.1725 },
                            g: if dark { 1.0 } else { 0.1725 },
                            b: if dark { 1.0 } else { 0.1725 },
                            a: if dark { 0.08 } else { 0.12 },
                        },
                        None,
                    )?;
                    self.target.DrawRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: rect([r[0] - 1.5, r[1] - 1.5, r[2] + 3.0, r[3] + 3.0]),
                            radiusX: n.radius + 1.5,
                            radiusY: n.radius + 1.5,
                        },
                        &ring,
                        3.0,
                        None,
                    );
                }
                if n.classes == "modal-card" || n.classes.contains("tab-btn active") {
                    let sigma = if n.classes == "modal-card" { 16.0 } else { 2.0 };
                    let offset = if n.classes == "modal-card" { 8.0 } else { 1.0 };
                    for distance in (1..=sigma as i32 * 2).rev() {
                        let d = distance as f32;
                        let a = (if n.classes == "modal-card" {
                            0.12
                        } else if dark {
                            0.3
                        } else {
                            0.06
                        }) * (-d * d / (2.0 * sigma * sigma)).exp()
                            / (sigma * 1.3);
                        let brush = self.target.CreateSolidColorBrush(
                            &D2D1_COLOR_F {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a,
                            },
                            None,
                        )?;
                        self.target.FillRoundedRectangle(
                            &D2D1_ROUNDED_RECT {
                                rect: rect([
                                    r[0] - d,
                                    r[1] - d + offset,
                                    r[2] + d * 2.0,
                                    r[3] + d * 2.0,
                                ]),
                                radiusX: n.radius + d,
                                radiusY: n.radius + d,
                            },
                            &brush,
                        );
                    }
                }
                if hovered && n.owner == "titlebar-close" {
                    bg = parse_color(if dark {
                        "rgba(192, 96, 96, 0.1)"
                    } else {
                        "rgba(139, 68, 68, 0.08)"
                    });
                }
                if hovered
                    && (n.classes.contains("whisper-action-btn")
                        || n.classes.contains("ob-btn-primary"))
                {
                    bg = parse_color(if dark { "#ffffff" } else { "#1a1a1a" });
                }
                if hovered
                    && (n.classes.contains("item-copy")
                        || n.classes.contains("summary-copy")
                        || n.classes.contains("ob-chip")
                        || n.classes.contains("ob-feature-btn"))
                {
                    border = parse_color(&color(dark, "faint"));
                }
                if hovered && n.classes.contains("item-remove") {
                    border = parse_color(&color(dark, "danger"));
                }
                bg.a *= opacity;
                border.a *= opacity;
                if n.classes == "content" {
                    r[2] += 14.0;
                    r[3] += 14.0;
                }
                let round = D2D1_ROUNDED_RECT {
                    rect: rect(r),
                    radiusX: n.radius,
                    radiusY: n.radius,
                };
                if bg.a > 0.0 {
                    let brush = self.target.CreateSolidColorBrush(&bg, None)?;
                    self.target.FillRoundedRectangle(&round, &brush);
                }
                if n.width > 0.0 && border.a > 0.0 {
                    let brush = self.target.CreateSolidColorBrush(&border, None)?;
                    let outline = D2D1_ROUNDED_RECT {
                        rect: rect([
                            r[0] + n.width / 2.0,
                            r[1] + n.width / 2.0,
                            r[2] - n.width,
                            r[3] - n.width,
                        ]),
                        radiusX: n.radius,
                        radiusY: n.radius,
                    };
                    let factory = self.target.GetFactory()?;
                    let stroke = if n.dashed {
                        Some(factory.CreateStrokeStyle(
                            &D2D1_STROKE_STYLE_PROPERTIES {
                                dashStyle: D2D1_DASH_STYLE_DASH,
                                ..Default::default()
                            },
                            None,
                        )?)
                    } else {
                        None
                    };
                    self.target
                        .DrawRoundedRectangle(&outline, &brush, n.width, stroke.as_ref());
                }
                if n.classes.contains("tab-btn active") {
                    let brush = self
                        .target
                        .CreateSolidColorBrush(&parse_color(&color(dark, "ink")), None)?;
                    self.target.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: rect([0.0, r[1] + r[3] / 2.0 - 10.0, 3.0, 20.0]),
                            radiusX: 1.5,
                            radiusY: 1.5,
                        },
                        &brush,
                    );
                }
                if n.kind == "select" {
                    let mut t = n.clone();
                    t.kind = "text".into();
                    t.rect = [r[0] + 14.0, r[1] + (r[3] - 16.0) / 2.0, r[2] - 50.0, 16.0];
                    t.wrap = t.rect[2];
                    t.nowrap = true;
                    self.draw(&t, dark, hover)?;
                }
                if n.kind == "edit" {
                    let mut t = n.clone();
                    t.kind = "text".into();
                    t.rect = [
                        r[0] + n.padding[0],
                        r[1] + (r[3] - 16.0) / 2.0,
                        r[2] - n.padding[0] - n.padding[2],
                        16.0,
                    ];
                    t.wrap = t.rect[2];
                    t.nowrap = true;
                    if t.text.is_empty() {
                        t.text = n.placeholder.clone();
                        t.color = color(dark, "faint");
                    } else if n.password {
                        t.text = "●".repeat(t.text.chars().count());
                    }
                    self.draw(&t, dark, hover)?;
                }
            }
            "text" => {
                let format = self.format(
                    n.size.max(1.0),
                    n.weight.max(300),
                    n.classes.contains("mono"),
                )?;
                if n.align == "center" {
                    format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                }
                if n.align == "right" {
                    format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                }
                let multiline = !n.nowrap && r[3] > n.size * 1.8;
                let maxwidth = if multiline && n.wrap > 0.0 {
                    n.wrap
                } else {
                    r[2] + if n.align == "center" { 0.0 } else { 3.0 }
                };
                if n.nowrap {
                    format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                }
                if n.nowrap || n.classes.contains("history-item-text") {
                    let ellipsis = self.write.CreateEllipsisTrimmingSign(&format)?;
                    format.SetTrimming(
                        &DWRITE_TRIMMING {
                            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            ..Default::default()
                        },
                        &ellipsis,
                    )?;
                }
                if r[3] > n.size * 1.8 {
                    format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, n.line, n.size)?;
                }
                let layout = self.write.CreateTextLayout(
                    &n.text.encode_utf16().collect::<Vec<_>>(),
                    &format,
                    maxwidth,
                    r[3] + 2.0,
                )?;
                if n.spacing != 0.0 {
                    let layout1: IDWriteTextLayout1 = layout.cast()?;
                    layout1.SetCharacterSpacing(
                        n.spacing / 2.0,
                        n.spacing / 2.0,
                        0.0,
                        DWRITE_TEXT_RANGE {
                            startPosition: 0,
                            length: n.text.encode_utf16().count() as u32,
                        },
                    )?;
                }
                let mut color = parse_color(&n.color);
                if hovered
                    && (n.classes.contains("item-copy")
                        || n.classes.contains("summary-copy")
                        || n.classes.contains("ob-chip")
                        || n.classes.contains("ob-feature-btn")
                        || n.classes.contains("ob-exit"))
                {
                    color = parse_color(&crate::native_ui::color(dark, "ink"));
                }
                if hovered && n.classes.contains("item-remove") {
                    color = parse_color(&crate::native_ui::color(dark, "danger"));
                }
                color.a *= opacity;
                let ink = self.target.CreateSolidColorBrush(&color, None)?;
                self.target.DrawTextLayout(
                    Vector2 {
                        X: r[0],
                        Y: r[1]
                            - if n.classes.contains("mono") {
                                0.0
                            } else {
                                n.size * 0.12
                            },
                    },
                    &layout,
                    &ink,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                );
            }
            "spinner" => {
                let track = self
                    .target
                    .CreateSolidColorBrush(&parse_color(&n.border), None)?;
                let ink = self
                    .target
                    .CreateSolidColorBrush(&parse_color(&n.color), None)?;
                let center = Vector2 {
                    X: r[0] + r[2] / 2.0,
                    Y: r[1] + r[3] / 2.0,
                };
                let radius = r[2] / 2.0 - 1.0;
                self.target.DrawEllipse(
                    &D2D1_ELLIPSE {
                        point: center,
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &track,
                    2.0,
                    None,
                );
                let phase = animation_time() / 600.0 * std::f32::consts::TAU;
                for i in 0..12 {
                    let a = phase + i as f32 * std::f32::consts::FRAC_PI_2 / 12.0;
                    let b = phase + (i + 1) as f32 * std::f32::consts::FRAC_PI_2 / 12.0;
                    self.target.DrawLine(
                        Vector2 {
                            X: center.X + radius * a.cos(),
                            Y: center.Y + radius * a.sin(),
                        },
                        Vector2 {
                            X: center.X + radius * b.cos(),
                            Y: center.Y + radius * b.sin(),
                        },
                        &ink,
                        2.0,
                        None,
                    );
                }
            }
            "image" | "icon" => {
                let name = n.src.trim_start_matches("./").replace(".svg", ".png");
                if !self.images.contains_key(&name) {
                    if let Some(bytes) = image(&name) {
                        let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
                        let mut reader = decoder
                            .read_info()
                            .map_err(|_| windows::core::Error::from_hresult(E_FAIL))?;
                        let mut rgba = vec![0; reader.output_buffer_size()];
                        let info = reader
                            .next_frame(&mut rgba)
                            .map_err(|_| windows::core::Error::from_hresult(E_FAIL))?;
                        rgba.truncate(info.buffer_size());
                        for pixel in rgba.chunks_exact_mut(4) {
                            let a = pixel[3] as u16;
                            let red = pixel[0];
                            pixel[0] = (pixel[2] as u16 * a / 255) as u8;
                            pixel[1] = (pixel[1] as u16 * a / 255) as u8;
                            pixel[2] = (red as u16 * a / 255) as u8;
                        }
                        let bitmap = self.target.CreateBitmap(
                            D2D_SIZE_U {
                                width: info.width,
                                height: info.height,
                            },
                            Some(rgba.as_ptr().cast()),
                            info.width * 4,
                            &D2D1_BITMAP_PROPERTIES {
                                pixelFormat: D2D1_PIXEL_FORMAT {
                                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                                },
                                dpiX: 96.0,
                                dpiY: 96.0,
                            },
                        )?;
                        self.images.insert(name.clone(), bitmap);
                    }
                }
                if let Some(bitmap) = self.images.get(&name) {
                    self.target.DrawBitmap(
                        bitmap,
                        Some(&rect(r)),
                        opacity,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }
    unsafe fn pixels(&self) -> windows::core::Result<Vec<u8>> {
        let lock = self
            .bitmap
            .Lock(std::ptr::null(), WICBitmapLockRead.0 as u32)?;
        let mut count = 0;
        let mut pointer = std::ptr::null_mut();
        lock.GetDataPointer(&mut count, &mut pointer)?;
        Ok(std::slice::from_raw_parts(pointer, count as usize).to_vec())
    }
    unsafe fn draw_pixels(&self, pixels: &[u8]) -> windows::core::Result<()> {
        let bitmap = self.target.CreateBitmap(
            D2D_SIZE_U {
                width: self.width,
                height: self.height,
            },
            Some(pixels.as_ptr().cast()),
            self.width * 4,
            &D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
            },
        )?;
        self.target.DrawBitmap(
            &bitmap,
            Some(&rect([0.0, 0.0, 520.0, 580.0])),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        Ok(())
    }
    fn edit_brush(&self, dark: bool) -> HBRUSH {
        if dark {
            self.dark_brush
        } else {
            self.light_brush
        }
    }
}
impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.write.UnregisterFontFileLoader(&self.font_loader);
            let _ = DeleteObject(self.light_brush.into());
            let _ = DeleteObject(self.dark_brush.into());
            for handle in &self.font_handles {
                let _ = RemoveFontMemResourceEx(*handle);
            }
        }
    }
}
unsafe fn key_combo(vk: u16) -> String {
    let mut keys = vec![];
    if GetKeyState(VK_CONTROL.0 as i32) < 0 {
        keys.push("Ctrl".into());
    }
    if GetKeyState(VK_MENU.0 as i32) < 0 {
        keys.push("Alt".into());
    }
    if GetKeyState(VK_SHIFT.0 as i32) < 0 {
        keys.push("Shift".into());
    }
    if GetKeyState(VK_LWIN.0 as i32) < 0 || GetKeyState(VK_RWIN.0 as i32) < 0 {
        keys.push("Super".into());
    }
    let key = crate::native_app::key_name(vk);
    keys.push(key);
    keys.join("+")
}
fn format_time(timestamp: u64) -> String {
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
        let length = windows::Win32::Globalization::GetTimeFormatEx(
            windows::core::PCWSTR::null(),
            windows::Win32::Globalization::TIME_NOSECONDS,
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
        let length = windows::Win32::Globalization::GetDateFormatEx(
            windows::core::PCWSTR::null(),
            windows::Win32::Globalization::ENUM_DATE_FORMATS_FLAGS(0),
            Some(&local),
            w!("MMM d"),
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
fn animation_time() -> f32 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as f32
}
