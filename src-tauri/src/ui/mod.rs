//! GPUI front end. Everything here runs on the main thread; other threads reach
//! it through the functions below, which post to a channel the UI drains.
mod assets;
mod capture;
mod checks;
mod components;
mod dictionary;
mod home;
pub mod overlay;
mod settings;
mod setup;
mod subtitle_preview;
mod subtitles;
mod text_input;
mod theme;
mod window;

use futures_util::StreamExt;
use gpui::{App, Application, Global};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

enum Event {
    Refresh,
    Show { settings: bool },
    Hide,
    Pick { save: bool },
    Quit,
    Overlay(overlay::Command),
}

static SENDER: OnceLock<futures_channel::mpsc::UnboundedSender<Event>> = OnceLock::new();
// Collapses bursts of state changes (download progress, sync) into one repaint.
static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);

fn send(event: Event) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.unbounded_send(event);
    }
}

pub fn refresh() {
    if !REFRESH_PENDING.swap(true, Ordering::AcqRel) {
        send(Event::Refresh);
    }
}
pub fn show() {
    send(Event::Show { settings: false });
}
/// Shows the window on the Settings tab, e.g. when no engine is configured yet.
pub fn show_settings() {
    send(Event::Show { settings: true });
}
/// Closes the window to the tray. Its views and GPU resources are released.
pub fn hide() {
    send(Event::Hide);
}
pub fn pick_file(save: bool) {
    send(Event::Pick { save });
}
pub fn quit() {
    send(Event::Quit);
}

/// UI state that outlives the window, so reopening from the tray resumes the
/// same tab and unsaved edits.
#[derive(Default)]
pub(crate) struct Memory {
    pub tab: String,
    pub drafts: std::collections::HashMap<String, String>,
}

pub(crate) struct UiState {
    pub main: Option<gpui::WindowHandle<window::MainWindow>>,
    pub memory: Memory,
}
impl Global for UiState {}

/// Runs GPUI until the app quits. `start` initializes the core and platform
/// services once the UI can receive their events.
pub fn run(start: impl FnOnce() -> Result<(), String> + 'static) -> Result<(), String> {
    run_with(move |cx| {
        start()?;
        window::open(cx, false);
        Ok(())
    })
}

pub fn capture(directory: std::path::PathBuf) -> Result<(), String> {
    capture::run(directory)
}

fn run_with(start: impl FnOnce(&mut App) -> Result<(), String> + 'static) -> Result<(), String> {
    let (sender, mut receiver) = futures_channel::mpsc::unbounded();
    SENDER
        .set(sender)
        .map_err(|_| "The UI is already running".to_string())?;
    let outcome = Rc::new(RefCell::new(Ok(())));
    let result = outcome.clone();
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            theme::load_fonts(cx);
            text_input::bind_keys(cx);
            window::bind_keys(cx);
            cx.set_global(UiState {
                main: None,
                memory: Memory {
                    tab: "home".into(),
                    ..Default::default()
                },
            });
            overlay::install(cx);
            cx.spawn(async move |cx| {
                while let Some(event) = receiver.next().await {
                    if cx.update(|cx| handle(event, cx)).is_err() {
                        break;
                    }
                }
            })
            .detach();
            if let Err(error) = start(cx) {
                *result.borrow_mut() = Err(error);
                cx.quit();
            }
        });
    outcome.replace(Ok(()))
}

fn handle(event: Event, cx: &mut App) {
    match event {
        Event::Refresh => {
            REFRESH_PENDING.store(false, Ordering::Release);
            window::refresh(cx);
        }
        Event::Show { settings } => window::open(cx, settings),
        Event::Hide => window::close(cx),
        Event::Pick { save } => window::pick_file(cx, save),
        Event::Quit => {
            window::commit(cx);
            cx.quit();
        }
        Event::Overlay(command) => overlay::apply(command, cx),
    }
}
