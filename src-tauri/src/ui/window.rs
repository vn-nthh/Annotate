//! The settings window: titlebar, sidebar, tab panels, setup guide, and the
//! close prompt. Layout follows `src/index.html` and `src/index.css`.
use super::components::{self as ui, icon, Kind, RADIUS_LG, RADIUS_MD, RADIUS_SM};
use super::text_input::{InputEvent, TextInput};
use super::theme::{rem, Theme, FONT, MEDIUM, NORMAL, SEMIBOLD};
use super::UiState;
use crate::native_core::{get, State};
use crate::native_onboarding::{self as setup, Onboarding};
use gpui::{
    actions, anchored, deferred, div, img, point, prelude::*, px, size, AnimationExt, App, Bounds,
    Context, Corner, Div, Entity, FocusHandle, Focusable, Global, KeyBinding, KeyDownEvent,
    KeyUpEvent, MouseButton, ScrollHandle, SharedString, Stateful, TitlebarOptions, Window,
    WindowBounds, WindowControlArea, WindowHandle, WindowKind, WindowOptions,
};
use std::{collections::HashMap, time::Instant};

actions!(annotate, [Cancel, FocusNext, FocusPrevious]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, None),
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
    ]);
}

/// The theme for widgets (text fields) that render outside `MainWindow`.
pub struct CurrentTheme(pub Theme);
impl Global for CurrentTheme {}

pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 580.0;

/// Editable fields. Settings save when a field loses focus; the dictionary and
/// "Try it" fields keep a local draft.
pub struct Inputs {
    pub dict: Entity<TextInput>,
    pub api_key: Entity<TextInput>,
    pub endpoint: Entity<TextInput>,
    pub openrouter: Entity<TextInput>,
    pub ob_groq: Entity<TextInput>,
    pub ob_azure: Entity<TextInput>,
    pub ob_endpoint: Entity<TextInput>,
    pub ob_try: Entity<TextInput>,
}

pub struct MainWindow {
    pub state: State,
    pub theme: Theme,
    pub tab: String,
    pub modal: bool,
    modal_return_focus: Option<FocusHandle>,
    pub open_select: Option<&'static str>,
    pub show_keys: bool,
    pub show_summary_key: bool,
    pub copied: Option<(String, Instant)>,
    pub try_listening: bool,
    pub try_done: bool,
    pub inputs: Inputs,
    /// The engine whose key the Settings API key field is showing.
    api_key_mode: String,
    /// Unsaved API key edits for the engines not currently shown.
    api_drafts: HashMap<String, String>,
    recorded_key: Option<String>,
    pub scroll: HashMap<&'static str, ScrollHandle>,
    pub drag: Option<(&'static str, f32, f32)>,
    /// The decoded profile photo, kept while the same bytes are in state.
    pub avatar_cache:
        std::cell::RefCell<Option<(std::sync::Arc<Vec<u8>>, std::sync::Arc<gpui::Image>)>>,
    root_focus: FocusHandle,
    /// Tray, Quit, and Cancel in the close prompt; focus stays among them.
    pub(super) modal_focus: [FocusHandle; 3],
}

fn should_close(window: &mut Window, cx: &mut App) -> bool {
    // The callback already holds the window; only lease its root view.
    if let Some(Some(view)) = window.root::<MainWindow>() {
        view.update(cx, |view, cx| view.open_modal(window, cx));
    }
    false
}

/// Opens an unfocused window at `origin` for reference captures.
pub fn open_at(
    cx: &mut App,
    origin: gpui::Point<gpui::Pixels>,
) -> Option<WindowHandle<MainWindow>> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            origin,
            size(px(WIDTH), px(HEIGHT)),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Annotate capture".into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        focus: false,
        is_resizable: false,
        ..Default::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            crate::platform::move_offscreen(window);
            window.on_window_should_close(cx, should_close);
            cx.new(|cx| MainWindow::new(window, cx))
        })
        .ok()?;
    cx.global_mut::<UiState>().main = Some(handle);
    Some(handle)
}

pub fn open(cx: &mut App, settings: bool) {
    let main = cx.global::<UiState>().main;
    if let Some(handle) = main {
        let shown = handle.update(cx, |view, window, cx| {
            if settings && view.tab != "settings" {
                view.select_tab("settings", window, cx);
            }
            window.activate_window();
        });
        if shown.is_ok() {
            return;
        }
    }
    if settings {
        cx.global_mut::<UiState>().memory.tab = "settings".into();
    }
    let bounds = Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Annotate".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(17.))),
        }),
        focus: true,
        show: true,
        kind: WindowKind::Normal,
        is_movable: true,
        is_resizable: false,
        is_minimizable: true,
        app_id: Some("com.annotate.app".into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        crate::platform::style_main_window(window);
        window.on_window_should_close(cx, should_close);
        cx.new(|cx| MainWindow::new(window, cx))
    });
    match opened {
        Ok(handle) => {
            cx.global_mut::<UiState>().main = Some(handle);
            cx.activate(true);
            crate::native_benchmark::shown();
        }
        Err(error) => {
            log::error!("Cannot open the Annotate window: {error}");
            get().status(&error.to_string(), true);
        }
    }
}

/// Closes to the tray. The window, its views, and GPU resources are released.
pub fn close(cx: &mut App) {
    let Some(handle) = cx.global_mut::<UiState>().main.take() else {
        return;
    };
    let _ = handle.update(cx, |view, window, cx| {
        view.remember(cx);
        window.remove_window();
    });
    crate::native_benchmark::hidden();
}

pub fn refresh(cx: &mut App) {
    let main = cx.global::<UiState>().main;
    if let Some(handle) = main {
        let _ = handle.update(cx, |view, window, cx| view.sync(window, cx));
    }
}

/// Saves edits that are still focused, before quitting.
pub fn commit(cx: &mut App) {
    let main = cx.global::<UiState>().main;
    if let Some(handle) = main {
        let _ = handle.update(cx, |view, _, cx| view.commit_edits(cx));
    }
}

pub fn pick_file(cx: &mut App, save: bool) {
    let Some(handle) = cx.global::<UiState>().main else {
        return;
    };
    let owner = handle
        .update(cx, |_, window, _| crate::platform::dialog_owner(window))
        .ok()
        .flatten();
    let Some(owner) = owner else {
        return;
    };
    crate::app::spawn_blocking(move || choose_file(owner, save));
}

/// Runs a modal file dialog off the UI thread, then applies its result unless
/// the subtitle job changed meanwhile.
fn choose_file(owner: crate::platform::DialogOwner, save: bool) {
    let core = get();
    let state = core.snapshot();
    if state.busy.contains("sub-generate-btn") || state.busy.contains("sub-summarize-btn") {
        return;
    }
    let name = state
        .file
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_stem())
        .and_then(|p| p.to_str())
        .unwrap_or("subtitles")
        .to_owned();
    let result = if save {
        crate::platform::pick_srt_destination(owner, &format!("{name}.srt"))
    } else {
        crate::platform::pick_media_file(owner)
    };
    let current = core.snapshot();
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
                    core.status(&e, true);
                }
            } else {
                core.change(|s| {
                    s.file = Some(path.to_string_lossy().into_owned());
                    s.entries.clear();
                    s.summary.clear();
                    s.progress.remove("sub");
                });
            }
        }
        Ok(None) => {}
        Err(e) => core.status(&e, true),
    }
}

impl MainWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = get().snapshot();
        let theme = Theme::new(state.dark());
        cx.set_global(CurrentTheme(theme));
        let mut make = |placeholder: &str, masked: bool| {
            let input = cx.new(|cx| TextInput::new(placeholder.to_owned(), masked, window, cx));
            cx.subscribe_in(&input, window, Self::input_event).detach();
            input
        };
        let inputs = Inputs {
            dict: make("Add a term…", false),
            api_key: make("Paste Groq API key", true),
            endpoint: make(
                "https://resource.services.ai.azure.com/api/projects/project",
                false,
            ),
            openrouter: make("Paste OpenRouter API key", true),
            ob_groq: make("Paste Groq API key", true),
            ob_azure: make("Paste Foundry API key", true),
            ob_endpoint: make(
                "https://resource.services.ai.azure.com/api/projects/project",
                false,
            ),
            ob_try: make("Your words appear here", false),
        };
        let memory = std::mem::take(&mut cx.global_mut::<UiState>().memory);
        let mut view = Self {
            api_key_mode: state.mode(),
            api_drafts: HashMap::new(),
            recorded_key: None,
            state,
            theme,
            tab: if memory.tab.is_empty() {
                "home".into()
            } else {
                memory.tab
            },
            modal: false,
            modal_return_focus: None,
            open_select: None,
            show_keys: false,
            show_summary_key: false,
            copied: None,
            try_listening: false,
            try_done: false,
            inputs,
            scroll: HashMap::new(),
            drag: None,
            avatar_cache: Default::default(),
            root_focus: cx.focus_handle(),
            modal_focus: [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()],
        };
        view.fill_inputs(cx);
        for (id, draft) in memory.drafts {
            if let Some(mode) = id.strip_prefix("api_key:") {
                if mode != view.api_key_mode {
                    view.api_drafts.insert(mode.to_owned(), draft);
                    continue;
                }
            }
            let id = if id.starts_with("api_key:") {
                "api_key"
            } else {
                &id
            };
            if let Some(input) = view.input_by_id(id) {
                input.update(cx, |input, cx| {
                    input.set_text(draft, cx);
                    input.dirty = true;
                });
            }
        }
        window.focus(&view.root_focus);
        if view.setup().is_open() {
            view.focus_setup(window, cx);
        }
        view
    }

    /// Shows a fixed state without reading the core (reference captures).
    pub fn show_scene(
        &mut self,
        state: State,
        tab: &str,
        modal: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme = Theme::new(state.dark());
        cx.set_global(CurrentTheme(self.theme));
        self.api_key_mode = state.mode();
        self.state = state;
        self.tab = tab.into();
        self.modal = modal;
        self.open_select = None;
        self.scroll.clear();
        for input in [
            &self.inputs.api_key,
            &self.inputs.endpoint,
            &self.inputs.openrouter,
            &self.inputs.ob_groq,
            &self.inputs.ob_azure,
            &self.inputs.ob_endpoint,
            &self.inputs.dict,
            &self.inputs.ob_try,
        ] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.fill_inputs(cx);
        cx.notify();
    }

    pub fn scroll_to_bottom(&mut self, id: &'static str, cx: &mut Context<Self>) {
        if let Some(handle) = self.scroll.get(id) {
            let max = handle.max_offset().height;
            handle.set_offset(point(px(0.), -max));
            cx.notify();
        }
    }

    pub fn setup(&self) -> Onboarding {
        Onboarding::from_value(self.state.settings.get(setup::KEY), &self.state.mode())
    }

    /// Takes the latest core state and refreshes fields that are not being edited.
    fn sync(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.state = get().snapshot();
        let theme = Theme::new(self.state.dark());
        if theme.dark != self.theme.dark {
            self.theme = theme;
            cx.set_global(CurrentTheme(theme));
        }
        let mode = self.state.mode();
        if mode != self.api_key_mode {
            // A draft belongs to the engine it was typed for; keep it if the
            // save failed, and bring back the new engine's own draft.
            self.commit_input("api_key", cx);
            let (dirty, text) = {
                let input = self.inputs.api_key.read(cx);
                (input.dirty, input.text().to_owned())
            };
            if dirty {
                self.api_drafts.insert(self.api_key_mode.clone(), text);
            }
            let draft = self.api_drafts.remove(&mode);
            self.api_key_mode = mode;
            self.inputs.api_key.update(cx, |input, cx| match draft {
                Some(draft) => {
                    input.set_text(draft, cx);
                    input.dirty = true;
                }
                None => input.dirty = false,
            });
        }
        if self
            .copied
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed().as_millis() >= 1200)
        {
            self.copied = None;
        }
        self.fill_inputs(cx);
        let status = self.state.label("status-text", "Ready");
        if self.setup().step_id() == "try" {
            self.try_listening |= status == "Listening…";
            self.try_done |= self.try_listening && status == "Done";
        }
        cx.notify();
    }

    fn field(&self, id: &str) -> String {
        let s = &self.state;
        match id {
            "api_key" => s.label(
                if s.mode() == "azure-mai" {
                    "azure-key"
                } else {
                    "groq-key"
                },
                "",
            ),
            "endpoint" | "ob_endpoint" => s.setting("azureMaiEndpoint", ""),
            "openrouter" => s.label("openrouter-key-input", ""),
            "ob_groq" => s.label("groq-key", ""),
            "ob_azure" => s.label("azure-key", ""),
            _ => String::new(),
        }
    }

    fn input_by_id(&self, id: &str) -> Option<&Entity<TextInput>> {
        Some(match id {
            "dict" => &self.inputs.dict,
            "api_key" => &self.inputs.api_key,
            "endpoint" => &self.inputs.endpoint,
            "openrouter" => &self.inputs.openrouter,
            "ob_groq" => &self.inputs.ob_groq,
            "ob_azure" => &self.inputs.ob_azure,
            "ob_endpoint" => &self.inputs.ob_endpoint,
            "ob_try" => &self.inputs.ob_try,
            _ => return None,
        })
    }

    fn input_id(&self, input: &Entity<TextInput>) -> &'static str {
        let i = &self.inputs;
        [
            (&i.dict, "dict"),
            (&i.api_key, "api_key"),
            (&i.endpoint, "endpoint"),
            (&i.openrouter, "openrouter"),
            (&i.ob_groq, "ob_groq"),
            (&i.ob_azure, "ob_azure"),
            (&i.ob_endpoint, "ob_endpoint"),
            (&i.ob_try, "ob_try"),
        ]
        .into_iter()
        .find(|(entity, _)| *entity == input)
        .map_or("", |(_, id)| id)
    }

    /// Shows saved values in settings fields that hold no unsaved draft.
    fn fill_inputs(&mut self, cx: &mut Context<Self>) {
        let placeholder = if self.state.mode() == "azure-mai" {
            "Paste Foundry API key"
        } else {
            "Paste Groq API key"
        };
        self.inputs
            .api_key
            .update(cx, |input, _| input.set_placeholder(placeholder));
        for id in [
            "api_key",
            "endpoint",
            "openrouter",
            "ob_groq",
            "ob_azure",
            "ob_endpoint",
        ] {
            let value = self.field(id);
            if let Some(input) = self.input_by_id(id).cloned() {
                input.update(cx, |input, cx| {
                    if !input.dirty {
                        input.set_text(value, cx);
                    }
                });
            }
        }
    }

    fn input_event(
        &mut self,
        input: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.input_id(input);
        match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Blurred => self.commit_input(id, cx),
            InputEvent::Submitted => match id {
                "dict" => self.add_term(cx),
                "ob_groq" | "ob_azure" | "ob_endpoint" => self.setup_step("ob-next", window, cx),
                _ => {}
            },
        }
    }

    /// Saves one settings field if it was edited. A value the core rejects
    /// stays as a draft so the user's typing is not lost.
    fn commit_input(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(input) = self.input_by_id(id).cloned() else {
            return;
        };
        let (dirty, value) = {
            let input = input.read(cx);
            (input.dirty, input.text().to_owned())
        };
        let action = match id {
            "api_key" => format!("apikey-input:{}", self.api_key_mode),
            "endpoint" | "ob_endpoint" => "azure-endpoint-input".into(),
            "openrouter" => "openrouter-key-input".into(),
            "ob_groq" => "apikey-input:groq".into(),
            "ob_azure" => "apikey-input:azure-mai".into(),
            _ => return,
        };
        if !dirty {
            return;
        }
        get().action(&action, value.clone());
        let state = get().snapshot();
        let saved = match action.as_str() {
            "azure-endpoint-input" => state.setting("azureMaiEndpoint", "") == value,
            "openrouter-key-input" => state.label("openrouter-key-input", "") == value.trim(),
            action => {
                state.label(
                    if action.ends_with("azure-mai") {
                        "azure-key"
                    } else {
                        "groq-key"
                    },
                    "",
                ) == value.trim()
            }
        };
        if saved {
            input.update(cx, |input, _| input.dirty = false);
        }
        self.state = state;
        cx.notify();
    }

    pub fn commit_edits(&mut self, cx: &mut Context<Self>) {
        for id in [
            "api_key",
            "endpoint",
            "openrouter",
            "ob_groq",
            "ob_azure",
            "ob_endpoint",
        ] {
            self.commit_input(id, cx);
        }
    }

    /// Keeps the tab and unsaved drafts for the next time the window opens.
    fn remember(&mut self, cx: &mut Context<Self>) {
        self.commit_edits(cx);
        let mut drafts: HashMap<String, String> = self
            .api_drafts
            .iter()
            .map(|(mode, text)| (format!("api_key:{mode}"), text.clone()))
            .collect();
        for id in [
            "dict",
            "ob_try",
            "api_key",
            "endpoint",
            "openrouter",
            "ob_groq",
            "ob_azure",
            "ob_endpoint",
        ] {
            if let Some(input) = self.input_by_id(id) {
                let input = input.read(cx);
                // An empty settings draft is a rejected attempt to clear it.
                let local = matches!(id, "dict" | "ob_try");
                if input.dirty && !(local && input.text().is_empty()) {
                    let key = if id == "api_key" {
                        format!("api_key:{}", self.api_key_mode)
                    } else {
                        id.to_owned()
                    };
                    drafts.insert(key, input.text().to_owned());
                }
            }
        }
        cx.global_mut::<UiState>().memory = super::Memory {
            tab: self.tab.clone(),
            drafts,
        };
    }

    pub fn select_tab(&mut self, tab: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edits(cx);
        self.open_select = None;
        self.tab = tab.into();
        window.focus(&self.root_focus);
        cx.notify();
    }

    pub fn add_term(&mut self, cx: &mut Context<Self>) {
        let value = self.inputs.dict.read(cx).text().to_owned();
        get().action("dict-add-btn", value);
        self.inputs
            .dict
            .update(cx, |input, cx| input.set_text("", cx));
        self.state = get().snapshot();
        cx.notify();
    }

    /// Clicks that need window state first; everything else goes to the core.
    pub fn click(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "titlebar-minimize" => window.minimize_window(),
            "titlebar-close" => self.open_modal(window, cx),
            "modal-cancel" => self.close_modal(window, cx),
            "modal-quit" => {
                self.commit_edits(cx);
                cx.quit();
            }
            "modal-tray" => {
                self.modal = false;
                self.remember(cx);
                cx.global_mut::<UiState>().main = None;
                window.remove_window();
                crate::native_benchmark::hidden();
            }
            "apikey-toggle" => {
                self.show_keys = !self.show_keys;
                for input in [
                    &self.inputs.api_key,
                    &self.inputs.ob_groq,
                    &self.inputs.ob_azure,
                ] {
                    let masked = !self.show_keys;
                    input.update(cx, |input, cx| {
                        input.masked = masked;
                        cx.notify();
                    });
                }
            }
            "openrouter-key-toggle" => {
                self.show_summary_key = !self.show_summary_key;
                let masked = !self.show_summary_key;
                self.inputs.openrouter.update(cx, |input, cx| {
                    input.masked = masked;
                    cx.notify();
                });
            }
            "ob-back" | "ob-next" | "ob-exit" => self.setup_step(id, window, cx),
            "setup-guide-btn" | "setup-resume-btn" => {
                self.commit_edits(cx);
                let mut guide = self.setup();
                guide.resume(&self.state.mode());
                self.enter_setup(guide, window, cx);
            }
            id if id.starts_with("ob-engine-") => {
                let mut guide = self.setup();
                guide.choose(&id["ob-engine-".len()..]);
                self.enter_setup(guide, window, cx);
            }
            id if id.starts_with("ob-feature-") => {
                let mut guide = self.setup();
                guide.complete();
                self.tab = id["ob-feature-".len()..].into();
                self.enter_setup(guide, window, cx);
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
            "dict-add-btn" => self.add_term(cx),
            "sub-summary-copy-btn" => {
                if get().copy_text(&self.state.summary).is_ok() {
                    self.copied = Some((id.into(), Instant::now()));
                    self.schedule_copied_reset(window, cx);
                }
            }
            id if id.starts_with("history-") || id.starts_with("dict-remove:") => {
                let (action, index) = id.split_once(':').unwrap_or((id, ""));
                let index = index.parse::<usize>().unwrap_or(usize::MAX);
                let data = &self.state.data.data;
                let text = if action == "dict-remove" {
                    data.dictionary.terms.get(index).map(|t| t.value.clone())
                } else {
                    data.history.entries.get(index).map(|t| t.text.clone())
                };
                if action == "history-copy" {
                    if get().copy_text(&text.unwrap_or_default()).is_ok() {
                        self.copied = Some((id.into(), Instant::now()));
                        self.schedule_copied_reset(window, cx);
                    }
                } else {
                    get().action(action, text.unwrap_or_default());
                }
            }
            _ => {
                self.commit_edits(cx);
                get().action(id, String::new());
            }
        }
        cx.notify();
    }

    fn schedule_copied_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(1250))
                .await;
            let _ = view.update(cx, |view, cx| {
                if view
                    .copied
                    .as_ref()
                    .is_some_and(|(_, at)| at.elapsed().as_millis() >= 1200)
                {
                    view.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn copied(&self, id: &str) -> bool {
        self.copied
            .as_ref()
            .is_some_and(|(copied, at)| copied == id && at.elapsed().as_millis() < 1200)
    }

    pub fn open_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edits(cx);
        self.open_select = None;
        self.modal_return_focus = window.focused(cx);
        self.modal = true;
        window.focus(&self.modal_focus[0]);
        cx.notify();
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modal = false;
        if let Some(focus) = self.modal_return_focus.take() {
            window.focus(&focus);
        }
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_select.take().is_some() {
            cx.notify();
            return;
        }
        if self.modal {
            self.close_modal(window, cx);
        }
        if self.state.recording_hotkey.is_some() {
            get().change(|s| s.recording_hotkey = None);
        }
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, _: &mut Context<Self>) {
        if self.modal {
            self.cycle_modal_focus(1, window);
        } else {
            window.focus_next();
        }
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, _: &mut Context<Self>) {
        if self.modal {
            self.cycle_modal_focus(2, window);
        } else {
            window.focus_prev();
        }
    }

    /// The close prompt is modal: Tab never reaches the controls behind it.
    fn cycle_modal_focus(&self, step: usize, window: &mut Window) {
        let current = self
            .modal_focus
            .iter()
            .position(|handle| handle.is_focused(window))
            .unwrap_or(2);
        window.focus(&self.modal_focus[(current + step) % 3]);
    }

    /// While a hotkey button is recording, the next non-modifier key becomes
    /// the shortcut. Escape cancels.
    fn record_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.state.recording_hotkey.clone() else {
            return;
        };
        self.recorded_key = Some(event.keystroke.key.clone());
        cx.stop_propagation();
        if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
            get().change(|s| s.recording_hotkey = None);
            return;
        }
        if let Some(combo) = crate::platform::pressed_shortcut() {
            if let Err(e) = get().register_shortcut(&id, &combo) {
                get().status(&e, true);
            }
        }
    }

    fn recorded_key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.recorded_key.as_deref() == Some(event.keystroke.key.as_str()) {
            self.recorded_key = None;
            // GPUI activates focused buttons on Enter/Space release.
            cx.stop_propagation();
        }
    }

    pub fn setup_blocker(&self, guide: &Onboarding, cx: &App) -> Option<&'static str> {
        match guide.step_id() {
            "install" => {
                let i = &self.inputs;
                setup::install_blocker(
                    &guide.engine,
                    &setup::Readiness {
                        groq_key: i.ob_groq.read(cx).text(),
                        azure_key: i.ob_azure.read(cx).text(),
                        endpoint: i.ob_endpoint.read(cx).text(),
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

    /// Back, Continue, and Exit setup.
    pub fn setup_step(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edits(cx);
        self.state = get().snapshot();
        let mut guide = self.setup();
        match id {
            "ob-exit" => {
                get().change(|s| s.recording_hotkey = None);
                guide.pause();
            }
            "ob-back" => guide.back(),
            _ => {
                if self.setup_blocker(&guide, cx).is_some() {
                    return;
                }
                // The engine switches only once it can transcribe, so leaving
                // setup halfway never leaves a broken engine selected.
                if guide.step_id() == "install" && self.state.mode() != guide.engine {
                    get().action("mode-select", guide.engine.clone());
                }
                if guide.is_last() {
                    self.tab = "home".into();
                }
                guide.next();
            }
        }
        let exited = guide.status == setup::Status::Paused;
        self.enter_setup(guide, window, cx);
        if exited {
            get().status("Setup paused", false);
        }
    }

    /// Saves the guide's state and shows its current step (or the app).
    fn enter_setup(&mut self, guide: Onboarding, window: &mut Window, cx: &mut Context<Self>) {
        let open = guide.is_open();
        if open && guide.step_id() == "try" {
            self.try_listening = false;
            self.try_done = false;
        }
        if let Err(error) = get().set_setting(setup::KEY, guide.to_value()) {
            get().status(&error, true);
        }
        self.state = get().snapshot();
        if open {
            self.focus_setup(window, cx);
        } else {
            window.focus(&self.root_focus);
        }
        cx.notify();
    }

    fn focus_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup().step_id() == "try" {
            window.focus(&self.inputs.ob_try.read(cx).focus_handle(cx));
        } else {
            window.focus(&self.root_focus);
        }
    }

    pub fn scroll_handle(&mut self, id: &'static str) -> ScrollHandle {
        self.scroll.entry(id).or_default().clone()
    }

    pub fn set_select(&mut self, id: Option<&'static str>, cx: &mut Context<Self>) {
        self.open_select = id;
        cx.notify();
    }

    fn titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let mac = cfg!(target_os = "macos");
        let control = |id: &'static str, path: &'static str, tip: &'static str, close: bool| {
            let hover_bg = if close { t.danger_bg } else { t.wash };
            let hover_fg = if close { t.danger } else { t.ink };
            ui::focusable(
                div()
                    .id(id)
                    .group(id)
                    .size(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover_bg))
                    .tooltip(ui::tooltip(tip, &t))
                    .child(
                        icon(path, 14., t.faint).group_hover(id, move |s| s.text_color(hover_fg)),
                    ),
                &t,
            )
            .on_click(cx.listener(move |view, _, window, cx| view.click(id, window, cx)))
        };
        div()
            .h(px(48.))
            .flex_none()
            .flex()
            .items_center()
            .bg(t.sidebar)
            .child(
                div()
                    .id("titlebar-drag")
                    .h_full()
                    .flex_1()
                    .flex()
                    .items_center()
                    .pl(px(if mac { 84. } else { 20. }))
                    .window_control_area(WindowControlArea::Drag)
                    .child(
                        img(super::assets::logo(t.dark))
                            .h(px(25.))
                            .w(px(25. * 124. / 32.)),
                    ),
            )
            .when(!mac, |bar| {
                bar.child(
                    div()
                        .flex()
                        .gap(px(2.))
                        .pr(px(10.))
                        .child(control(
                            "titlebar-minimize",
                            "icons/minimize.svg",
                            "Minimize",
                            false,
                        ))
                        .child(control("titlebar-close", "icons/close.svg", "Close", true)),
                )
            })
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let guide = self.setup();
        let tab = |id: &'static str, path: &'static str, label: &'static str, active: bool| {
            let tip = if id == "setup-resume" {
                format!(
                    "Resume setup at step {} of {}: {}",
                    guide.step + 1,
                    setup::STEPS.len(),
                    guide.step_label()
                )
            } else {
                label.to_owned()
            };
            let color = if active { t.ink } else { t.faint };
            ui::focusable(
                div()
                    .id(SharedString::from(format!("tab:{id}")))
                    .group(SharedString::from(format!("tab:{id}")))
                    .relative()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .pt(px(10.))
                    .pb(px(8.))
                    .px(px(4.))
                    .rounded(px(RADIUS_MD))
                    .cursor_pointer()
                    .text_color(color)
                    .when(active, |b| {
                        b.bg(t.paper).shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(0., 0., 0., if t.dark { 0.3 } else { 0.06 }),
                            offset: point(px(0.), px(1.)),
                            blur_radius: px(if t.dark { 4. } else { 3. }),
                            spread_radius: px(0.),
                        }])
                    })
                    .when(!active, |b| {
                        b.hover(move |s| s.bg(t.tab_wash).text_color(t.light))
                    })
                    .tooltip(ui::tooltip(tip, &t))
                    .when(active, |b| {
                        b.child(
                            div()
                                .absolute()
                                .left(px(-8.))
                                .top(px(18.))
                                .w(px(3.))
                                .h(px(20.))
                                .rounded_r(px(3.))
                                .bg(t.ink),
                        )
                    })
                    .child(icon(path, 20., color).when(!active, |i| {
                        i.group_hover(SharedString::from(format!("tab:{id}")), move |s| {
                            s.text_color(t.light)
                        })
                    }))
                    .child(
                        div()
                            .text_size(rem(0.65))
                            .font_weight(MEDIUM)
                            .line_height(rem(0.65 * 1.35))
                            .child(label),
                    ),
                &t,
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                if id == "setup-resume" {
                    view.click("setup-resume-btn", window, cx);
                } else {
                    view.select_tab(id, window, cx);
                }
            }))
        };
        let status = self.state.label("status-text", "Ready");
        let error = self.state.ready.contains("status-error");
        let active = self.state.ready.contains("status-active");
        let dot = if error {
            t.danger
        } else if active {
            t.active_dot
        } else {
            t.faint
        };
        div()
            .w(px(68.))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .py(px(12.))
            .bg(t.sidebar)
            .child(
                div()
                    .w_full()
                    .px(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(tab("home", "icons/home.svg", "Home", self.tab == "home"))
                    .child(tab(
                        "dictionary",
                        "icons/dictionary.svg",
                        "Dictionary",
                        self.tab == "dictionary",
                    ))
                    .child(tab(
                        "subtitling",
                        "icons/subtitles.svg",
                        "Subtitles",
                        self.tab == "subtitling",
                    ))
                    .child(tab(
                        "settings",
                        "icons/settings.svg",
                        "Settings",
                        self.tab == "settings",
                    )),
            )
            .child(
                div()
                    .mt_auto()
                    .w_full()
                    .px(px(8.))
                    .flex()
                    .flex_col()
                    .when(guide.status == setup::Status::Paused, |b| {
                        b.child(div().mb(px(4.)).child(tab(
                            "setup-resume",
                            "icons/resume.svg",
                            "Setup",
                            false,
                        )))
                    })
                    .child(
                        div()
                            .id("status")
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(4.))
                            .py(px(8.))
                            .tooltip(ui::tooltip(status.clone(), &t))
                            .child(
                                div()
                                    .size(px(14.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .when(active && !error, |b| {
                                        b.child(
                                            div()
                                                .absolute()
                                                .size(px(14.))
                                                .rounded_full()
                                                .bg(t.active_ring)
                                                .with_animation(
                                                    "status-pulse",
                                                    gpui::Animation::new(
                                                        std::time::Duration::from_millis(1500),
                                                    )
                                                    .repeat(),
                                                    |ring, delta| {
                                                        ring.opacity(
                                                            0.7 + 0.3
                                                                * (delta * std::f32::consts::TAU)
                                                                    .cos(),
                                                        )
                                                    },
                                                ),
                                        )
                                    })
                                    .child(div().size(px(8.)).rounded_full().bg(dot)),
                            )
                            .child(
                                div()
                                    .w(px(52.))
                                    .text_center()
                                    .text_size(rem(0.6))
                                    .line_height(rem(0.6 * 1.2))
                                    .text_color(t.faint)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(status),
                            ),
                    ),
            )
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let panel: gpui::AnyElement = match self.tab.as_str() {
            "dictionary" => self.dictionary(window, cx).into_any_element(),
            "subtitling" => self.subtitles(window, cx).into_any_element(),
            "settings" => self.settings(window, cx).into_any_element(),
            _ => self.home(window, cx).into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(t.paper)
            .rounded_tl(px(RADIUS_LG))
            .child(panel)
    }

    fn modal(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div()
            .id("close-modal")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.scrim)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _, window, cx| view.close_modal(window, cx)),
            )
            .child(
                div()
                    .id("modal-card")
                    .w(px(300.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt(px(24.))
                    .px(px(28.))
                    .pb(px(20.))
                    .bg(t.paper)
                    .border_1()
                    .border_color(t.rule)
                    .rounded(px(RADIUS_LG))
                    .shadow(vec![gpui::BoxShadow {
                        color: gpui::hsla(0., 0., 0., 0.12),
                        offset: point(px(0.), px(8.)),
                        blur_radius: px(32.),
                        spread_radius: px(0.),
                    }])
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .mb(px(4.))
                            .text_size(rem(1.05))
                            .font_weight(SEMIBOLD)
                            .text_color(t.ink)
                            .child("Close Annotate?"),
                    )
                    .child(
                        div()
                            .mb(px(20.))
                            .text_size(rem(0.8))
                            .text_color(t.light)
                            .child("Choose how to exit the application."),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .gap(px(8.))
                            .mb(px(12.))
                            .child(
                                ui::button("modal-tray", "Minimize to Tray", Kind::Secondary)
                                    .render(&t)
                                    .track_focus(&self.modal_focus[0])
                                    .flex_1()
                                    .py(px(10.))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.click("modal-tray", window, cx)
                                    })),
                            )
                            .child(
                                ui::button("modal-quit", "Quit", Kind::Primary)
                                    .render(&t)
                                    .track_focus(&self.modal_focus[1])
                                    .flex_1()
                                    .px(px(14.))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.click("modal-quit", window, cx)
                                    })),
                            ),
                    )
                    .child(
                        ui::button("modal-cancel", "Cancel", Kind::Link)
                            .render(&t)
                            .track_focus(&self.modal_focus[2])
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.click("modal-cancel", window, cx)
                            })),
                    ),
            )
    }

    /// A native-feeling dropdown (`.select-wrap select`). The list opens below
    /// the field; Up/Down change the value directly, as a Windows combobox does.
    pub fn select(
        &self,
        id: &'static str,
        options: Vec<(String, String)>,
        value: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let selected = options
            .iter()
            .position(|(key, _)| key == value)
            .unwrap_or(0);
        let label = options
            .get(selected)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let open = self.open_select == Some(id);
        let values: Vec<String> = options.iter().map(|(key, _)| key.clone()).collect();
        let field = ui::focusable(
            div()
                .id(id)
                .group(id)
                .w_full()
                .flex()
                .items_center()
                .pl(px(14.))
                .pr(px(36.))
                .py(px(10.))
                .bg(t.warm)
                .border(px(1.5))
                .border_color(if open { t.faint } else { t.rule })
                .rounded(px(RADIUS_MD))
                .cursor_pointer()
                .hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
                .text_size(rem(0.9))
                .font_weight(NORMAL)
                .line_height(px(15.))
                .text_color(t.ink)
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(12.))
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .child(
                            icon("icons/chevron.svg", 12., t.faint)
                                .group_hover(id, move |s| s.text_color(t.light)),
                        ),
                ),
            &t,
        )
        .relative()
        .on_click(cx.listener(move |view, _, _, cx| {
            let next = if view.open_select == Some(id) {
                None
            } else {
                Some(id)
            };
            view.set_select(next, cx);
        }))
        .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
            let step: isize = match event.keystroke.key.as_str() {
                "down" => 1,
                "up" => -1,
                _ => return,
            };
            cx.stop_propagation();
            let index = (selected as isize + step).clamp(0, values.len() as isize - 1) as usize;
            if index != selected {
                view.choose(id, values[index].clone(), cx);
            }
        }));
        div().relative().w_full().child(field).when(open, |wrap| {
            let list = options.into_iter().enumerate().map(|(index, (key, text))| {
                let chosen = index == selected;
                div()
                    .id(SharedString::from(format!("{id}-option-{index}")))
                    .px(px(12.))
                    .py(px(7.))
                    .rounded(px(RADIUS_SM))
                    .cursor_pointer()
                    .text_size(rem(0.9))
                    .text_color(t.ink)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .when(chosen, |o| o.bg(t.rule))
                    .hover(move |s| s.bg(if chosen { t.rule } else { t.warm }))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.choose(id, key.clone(), cx);
                    }))
                    .child(text)
            });
            wrap.child(deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id(SharedString::from(format!("{id}-list")))
                            .occlude()
                            .mt(px(4.))
                            .w(px(404.))
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .p(px(4.))
                            .bg(t.paper)
                            .border_1()
                            .border_color(t.rule)
                            .rounded(px(RADIUS_MD))
                            .shadow(vec![gpui::BoxShadow {
                                color: gpui::hsla(0., 0., 0., if t.dark { 0.3 } else { 0.08 }),
                                offset: point(px(0.), px(4.)),
                                blur_radius: px(16.),
                                spread_radius: px(0.),
                            }])
                            .font_family(FONT)
                            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                                view.set_select(None, cx);
                            }))
                            .children(list),
                    ),
            ))
        })
    }

    fn choose(&mut self, id: &'static str, value: String, cx: &mut Context<Self>) {
        self.open_select = None;
        self.commit_edits(cx);
        let action = match id {
            "ob-mic-select" => "mic-select",
            "ob-lang-select" => "live-lang-select",
            id => id,
        };
        get().action(action, value);
        self.state = get().snapshot();
        cx.notify();
    }

    /// A text field inside `.input-wrap`, with an optional show/hide button.
    pub fn text_field(
        &self,
        input: &Entity<TextInput>,
        reveal: Option<(&'static str, bool)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let t = self.theme;
        let focused = input.read(cx).focus_handle(cx);
        let input_id = self.input_id(input);
        let pointer_input = input.clone();
        div()
            .id(SharedString::from(format!("field-{input_id}")))
            .relative()
            .w_full()
            .flex()
            .items_center()
            .pl(px(14.))
            .pr(px(if reveal.is_some() { 40. } else { 14. }))
            .py(px(10.))
            .bg(t.warm)
            .border(px(1.5))
            .border_color(t.rule)
            .rounded(px(RADIUS_MD))
            .hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
            .when(focused.is_focused(window), move |f| f.border_color(t.faint))
            .cursor(gpui::CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if !window.default_prevented() {
                    pointer_input.update(cx, |input, cx| input.on_mouse_down(event, window, cx));
                    window.prevent_default();
                }
            })
            .text_size(rem(0.9))
            .line_height(px(15.))
            .text_color(t.ink)
            .child(input.clone())
            .when_some(reveal, |field, (id, shown)| {
                field.child(
                    ui::focusable(
                        div()
                            .id(id)
                            .group(id)
                            .absolute()
                            .right(px(8.))
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(RADIUS_SM))
                            .cursor_pointer()
                            .hover(move |s| s.bg(t.focus_ring))
                            .tooltip(ui::tooltip(if shown { "Hide key" } else { "Show key" }, &t))
                            .child(
                                icon("icons/eye.svg", 16., if shown { t.ink } else { t.faint })
                                    .group_hover(id, move |s| s.text_color(t.ink)),
                            ),
                        &t,
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |view, _, window, cx| view.click(id, window, cx))),
                )
            })
    }

    /// A thin draggable scrollbar for a scroll container (`::-webkit-scrollbar`).
    pub fn scrollbar(&self, id: &'static str, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let Some(handle) = self.scroll.get(id).cloned() else {
            return vec![];
        };
        let viewport = handle.bounds().size.height;
        let max = handle.max_offset().height;
        // The thumb is sized from the last layout. If this frame's layout differs
        // (first open, content changed), draw once more with the new size.
        let watched = handle.clone();
        let sentinel = gpui::canvas(
            move |_, window, _| {
                if watched.bounds().size.height != viewport || watched.max_offset().height != max {
                    window.request_animation_frame();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
        .into_any_element();
        if max <= px(0.5) || viewport <= px(0.) {
            return vec![sentinel];
        }
        let t = self.theme;
        let offset = -handle.offset().y;
        let thumb = (viewport * (viewport / (viewport + max))).max(px(24.));
        let top = (viewport - thumb) * (offset / max).clamp(0., 1.);
        let dragging = self.drag.is_some_and(|(drag, _, _)| drag == id);
        vec![
            sentinel,
            div()
                .id(SharedString::from(format!("{id}-scrollbar")))
                .absolute()
                .top_0()
                .right_0()
                .h(viewport)
                .w(px(8.))
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-thumb")))
                        .absolute()
                        .right_0()
                        .top(top)
                        .w(px(4.))
                        .h(thumb)
                        .rounded(px(3.))
                        .bg(if dragging {
                            t.scrollbar_hover
                        } else {
                            t.scrollbar
                        })
                        .hover(move |s| s.bg(t.scrollbar_hover))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                view.drag =
                                    Some((id, f32::from(event.position.y), f32::from(offset)));
                                cx.notify();
                            }),
                        ),
                )
                .into_any_element(),
        ]
    }

    fn drag_scroll(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((id, start_y, start_offset)) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let Some(handle) = self.scroll.get(id) else {
            return;
        };
        let viewport = f32::from(handle.bounds().size.height);
        let max = f32::from(handle.max_offset().height);
        let thumb = (viewport * (viewport / (viewport + max))).max(24.);
        let travel = (viewport - thumb).max(1.);
        let offset =
            (start_offset + (f32::from(event.position.y) - start_y) * max / travel).clamp(0., max);
        handle.set_offset(point(px(0.), px(-offset)));
        cx.notify();
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Benchmark probe: the first frame is about to paint.
        crate::native_benchmark::ready();
        let t = self.theme;
        let guide = self.setup();
        let body = if guide.is_open() {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.onboarding(&guide, window, cx))
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .bg(t.sidebar)
                .child(self.sidebar(cx))
                .child(self.content(window, cx))
        };
        // Focusing a GPUI element consumes mouse-down's native default action.
        // Keep that handler off the titlebar so Windows can start a drag.
        let body = body.track_focus(&self.root_focus);
        div()
            .id("root")
            .key_context("Annotate")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::focus_next))
            .on_action(cx.listener(Self::focus_previous))
            .capture_key_down(cx.listener(Self::record_key))
            .capture_key_up(cx.listener(Self::recorded_key_up))
            .on_mouse_move(cx.listener(|view, event, _, cx| view.drag_scroll(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if view.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.sidebar)
            .font_family(FONT)
            .text_size(rem(1.))
            .line_height(gpui::relative(1.5))
            .text_color(t.ink)
            .child(self.titlebar(cx))
            .child(body)
            .when(self.modal, |root| root.child(self.modal(cx)))
    }
}
