//! The recording overlay: a near-black pill near the bottom of the screen with
//! four dots that bounce with the microphone level, and a short confirmation
//! after a dictionary capture. It never takes focus or receives clicks.
use super::theme::{FONT, MEDIUM};
use gpui::{
    div, point, prelude::*, px, rgb, rgba, size, svg, App, Bounds, Context, Global, SharedString,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions,
};
use std::{
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant},
};

const CANVAS_WIDTH: f32 = 260.0;
const HEIGHT: f32 = 32.0;
const BOTTOM_MARGIN: f32 = 60.0;
const LISTEN_WIDTH: f32 = 72.0;
// Matches the browser overlay's dictionary confirmation duration.
const CONFIRM_DURATION: Duration = Duration::from_millis(1600);

static LEVEL: AtomicU32 = AtomicU32::new(0);
static LEVEL_AT: AtomicU64 = AtomicU64::new(0);

pub enum Command {
    Listen,
    Hide,
    Confirm { added: bool, text: String },
}

pub fn show_listening() -> Result<(), String> {
    super::send(super::Event::Overlay(Command::Listen));
    Ok(())
}
pub fn hide() -> Result<(), String> {
    super::send(super::Event::Overlay(Command::Hide));
    Ok(())
}
pub fn show_confirmation(kind: &str, text: String) -> Result<(), String> {
    let added = match kind {
        "added" => true,
        "duplicate" => false,
        _ => return Err(format!("unknown confirmation kind: {kind}")),
    };
    super::send(super::Event::Overlay(Command::Confirm { added, text }));
    Ok(())
}
/// Called from the capture thread for every audio packet; read each frame.
pub fn set_level(level: f32) -> Result<(), String> {
    LEVEL.store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    LEVEL_AT.store(now_ms(), Ordering::Relaxed);
    Ok(())
}
fn now_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

enum Mode {
    Hidden,
    Listening { started: Instant },
    Confirm { added: bool, text: SharedString },
}

pub struct Overlay {
    mode: Mode,
    smoothed: f32,
    animations: bool,
    generation: u64,
}

struct OverlayWindow(WindowHandle<Overlay>);
impl Global for OverlayWindow {}

pub fn install(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(CANVAS_WIDTH), px(HEIGHT)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        crate::platform::prepare_overlay(window, CANVAS_WIDTH, HEIGHT, BOTTOM_MARGIN);
        cx.new(|_| Overlay {
            mode: Mode::Hidden,
            smoothed: 0.0,
            animations: true,
            generation: 0,
        })
    });
    match opened {
        Ok(handle) => cx.set_global(OverlayWindow(handle)),
        Err(error) => log::error!("[Overlay] Cannot create the recording overlay: {error}"),
    }
}

pub(super) fn apply(command: Command, cx: &mut App) {
    let Some(handle) = cx.try_global::<OverlayWindow>().map(|w| w.0) else {
        return;
    };
    let _ = handle.update(cx, |overlay, window, cx| {
        overlay.generation += 1;
        match command {
            Command::Listen => {
                overlay.animations = crate::platform::animations_enabled();
                overlay.smoothed = 0.0;
                LEVEL.store(0, Ordering::Relaxed);
                overlay.mode = Mode::Listening {
                    started: Instant::now(),
                };
                crate::platform::show_overlay(window, CANVAS_WIDTH, HEIGHT, BOTTOM_MARGIN);
            }
            Command::Hide => {
                overlay.mode = Mode::Hidden;
                crate::platform::hide_overlay(window);
            }
            Command::Confirm { added, text } => {
                overlay.mode = Mode::Confirm {
                    added,
                    text: text.into(),
                };
                crate::platform::show_overlay(window, CANVAS_WIDTH, HEIGHT, BOTTOM_MARGIN);
                let generation = overlay.generation;
                cx.spawn_in(window, async move |overlay, cx| {
                    cx.background_executor().timer(CONFIRM_DURATION).await;
                    let _ = overlay.update_in(cx, |overlay, window, cx| {
                        if overlay.generation == generation {
                            overlay.mode = Mode::Hidden;
                            crate::platform::hide_overlay(window);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        cx.notify();
    });
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let canvas = div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family(FONT);
        match &self.mode {
            Mode::Hidden => canvas,
            Mode::Listening { started } => {
                let mut level = f32::from_bits(LEVEL.load(Ordering::Relaxed));
                if now_ms().saturating_sub(LEVEL_AT.load(Ordering::Relaxed)) > 250 {
                    level = 0.0;
                }
                self.smoothed += (level - self.smoothed) * 0.22;
                let elapsed = started.elapsed().as_secs_f32();
                let energy = self.smoothed;
                let dots = (0..4).map(|index| {
                    let (bounce, height) = if self.animations {
                        let wave = (elapsed * 3.5 + index as f32 * 0.8).sin();
                        (wave * (0.6 + energy * 2.0), 6.0 * (1.0 + energy * 1.8))
                    } else {
                        (0.0, 6.0)
                    };
                    div()
                        .absolute()
                        .left(px(15.0 + index as f32 * 12.0))
                        .top(px(13.0 + bounce - (height - 6.0) / 2.0))
                        .w(px(6.0))
                        .h(px(height))
                        .rounded(px(3.0f32.min(height / 2.0)))
                        .bg(rgba(0xffffffd9))
                });
                if self.animations {
                    window.request_animation_frame();
                }
                canvas.child(
                    div()
                        .relative()
                        .w(px(LISTEN_WIDTH))
                        .h(px(HEIGHT))
                        .rounded(px(16.0))
                        .bg(rgb(0x121212))
                        .children(dots),
                )
            }
            Mode::Confirm { added, text } => canvas.child(
                div()
                    .h(px(HEIGHT))
                    .min_w(px(72.0))
                    .max_w(px(240.0))
                    .px(px(12.0))
                    .gap(px(6.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(16.0))
                    .bg(rgb(0x121212))
                    .child(
                        svg()
                            .path(if *added {
                                "icons/check.svg"
                            } else {
                                "icons/duplicate.svg"
                            })
                            .size(px(14.0))
                            .flex_none()
                            .text_color(rgb(if *added { 0x4ade80 } else { 0xfbbf24 })),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(12.0))
                            .font_weight(MEDIUM)
                            .text_color(rgb(0xffffff))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(text.clone()),
                    ),
            ),
        }
    }
}
