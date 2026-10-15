//! Settings: devices, engines, keys, local models, appearance, sync, hotkeys.
use super::components::{self as ui, Kind, RADIUS_MD};
use super::home::{header, lift};
use super::theme::{rem, Theme, MEDIUM, NORMAL, SEMIBOLD};
use super::window::MainWindow;
use crate::native_onboarding::{self as setup, Status};
use gpui::{div, img, prelude::*, px, Context, Div, Image, ImageFormat, SharedString, Window};
use std::sync::Arc;

pub const LANGUAGES: [(&str, &str); 21] = [
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("ar", "Arabic"),
    ("hi", "Hindi"),
    ("tr", "Turkish"),
    ("pl", "Polish"),
    ("sv", "Swedish"),
    ("vi", "Vietnamese"),
    ("th", "Thai"),
    ("id", "Indonesian"),
    ("uk", "Ukrainian"),
];

pub fn languages() -> Vec<(String, String)> {
    LANGUAGES
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

pub fn group() -> Div {
    div().py(px(14.)).flex().flex_col()
}

/// `.theme-toggle-row`: a labelled switch.
fn switch_row(
    id: &'static str,
    title: &'static str,
    description: &'static str,
    checked: bool,
    theme: &Theme,
    cx: &mut Context<MainWindow>,
) -> Div {
    let t = *theme;
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(rem(0.72))
                        .font_weight(SEMIBOLD)
                        .line_height(rem(0.72 * 1.5))
                        .text_color(t.light)
                        .child(title.to_uppercase()),
                )
                .child(
                    div()
                        .text_size(rem(0.72))
                        .line_height(rem(0.72 * 1.5))
                        .text_color(t.faint)
                        .child(description),
                ),
        )
        .child(
            ui::toggle(id, checked, &t)
                .tooltip(ui::tooltip(title, &t))
                .on_click(cx.listener(move |view, _, window, cx| view.click(id, window, cx))),
        )
}

impl MainWindow {
    /// Status, button, spinner, and progress for a downloadable model.
    pub(super) fn model_row(
        &self,
        action_id: &'static str,
        key: &'static str,
        loaded_key: &'static str,
        missing: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = self.theme;
        let s = &self.state;
        let progress = s.progress.get(key).cloned();
        let downloaded = s.ready.contains(key);
        let loaded = s.ready.contains(loaded_key);
        let loading = key == "whisper" && s.busy.contains("whisper-load-btn");
        let downloading = s.busy.contains(action_id);
        let pausable = key == "whisper";
        let partial = if pausable && !downloaded && !downloading {
            self.paused_fraction()
        } else {
            None
        };
        let error = s.label(&format!("{key}-status-error"), "");
        let text = if !error.is_empty() {
            if downloaded && key == "whisper" {
                format!("✗ Load failed: {error}")
            } else {
                "✗ Download failed".into()
            }
        } else if let Some(fraction) = partial {
            format!("Download paused at {:.0}%", fraction * 100.0)
        } else if downloading {
            if key == "whisper" {
                "Downloading model…"
            } else {
                "Downloading…"
            }
            .into()
        } else if loading {
            "Loading model (Vulkan)…".into()
        } else if loaded {
            if key == "whisper" {
                "Model ready (Vulkan)"
            } else {
                "Model ready"
            }
            .into()
        } else if downloaded {
            "Model downloaded".into()
        } else {
            missing.to_owned()
        };
        let action: &'static str = if downloaded && key == "whisper" {
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
        let has_button =
            (!downloading || pausable) && !loading && !loaded && (key == "whisper" || !downloaded);
        let color = if !error.is_empty() {
            t.danger
        } else if loaded {
            t.ink
        } else {
            t.light
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(27.))
                    .when(downloading || loading, |row| {
                        row.child(ui::spinner(
                            SharedString::from(format!("{key}-spinner")),
                            &t,
                        ))
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("{key}-status")))
                            .min_w_0()
                            .flex_shrink()
                            .text_size(rem(0.85))
                            .font_weight(MEDIUM)
                            .text_color(color)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .tooltip(ui::tooltip(text.clone(), &t))
                            .child(text),
                    )
                    .when(has_button, |row| {
                        row.child(
                            ui::button(action, label, Kind::Action)
                                .disabled(loading || downloading && !pausable)
                                .render(&t)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.click(action, window, cx)
                                })),
                        )
                    }),
            )
            .when_some(progress, |column, (fraction, label)| {
                column.child(
                    div()
                        .mt(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().flex_1().child(ui::progress_bar(fraction, t.warm, &t)))
                        .child(
                            div()
                                .min_w(px(36.))
                                .text_right()
                                .text_size(rem(0.72))
                                .font_weight(MEDIUM)
                                .text_color(t.faint)
                                .child(label),
                        ),
                )
            })
    }

    /// Share of a paused model download that is already on disk.
    pub(super) fn paused_fraction(&self) -> Option<f32> {
        let bytes = self
            .state
            .label("whisper-partial", "0")
            .parse::<u64>()
            .ok()?;
        (bytes > 0).then(|| {
            (bytes as f64 / crate::whisper_local::model_size_bytes() as f64).min(1.0) as f32
        })
    }

    fn avatar(&self) -> Option<Arc<Image>> {
        let bytes = self.state.avatar.as_ref()?;
        let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            ImageFormat::Png
        } else if bytes.starts_with(&[0xff, 0xd8]) {
            ImageFormat::Jpeg
        } else if bytes.starts_with(b"RIFF") {
            ImageFormat::Webp
        } else if bytes.starts_with(b"GIF8") {
            ImageFormat::Gif
        } else {
            return None;
        };
        let mut cache = self.avatar_cache.borrow_mut();
        if let Some((source, image)) = cache.as_ref() {
            if Arc::ptr_eq(source, bytes) {
                return Some(image.clone());
            }
        }
        let image = Arc::new(Image::from_bytes(format, bytes.to_vec()));
        *cache = Some((bytes.clone(), image.clone()));
        Some(image)
    }

    fn sync_section(&self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let s = &self.state;
        let description = |text: &'static str| {
            div()
                .mb(px(10.))
                .text_size(rem(0.82))
                .line_height(rem(0.82 * 1.5))
                .text_color(t.faint)
                .child(text)
        };
        let quiet = |id: &'static str, label: String, danger: bool| {
            let button = ui::button(id, label.to_uppercase(), Kind::Quiet);
            let button = if danger {
                button.danger_hover()
            } else {
                button
            };
            button
                .render(&t)
                .on_click(cx.listener(move |view, _, window, cx| view.click(id, window, cx)))
        };
        let body =
            if s.auth_waiting {
                div()
                    .flex()
                    .flex_col()
                    .child(description("Finish signing in using your browser…"))
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(quiet("sync-reopen-btn", "Open Again".into(), false))
                            .child(quiet("sync-cancel-btn", "Cancel".into(), true)),
                    )
            } else if let Some(user) = &s.user {
                let name = user["name"].as_str().unwrap_or("User").to_owned();
                let email = user["email"].as_str().unwrap_or("");
                let email = email.split_once('@').map_or_else(
                    || email.to_owned(),
                    |(name, domain)| {
                        format!("{}***@{domain}", name.chars().take(3).collect::<String>())
                    },
                );
                let busy = s.busy.contains("sync-now-btn");
                let sync_label = s.label("sync-now-text", "Sync Now");
                let synced = sync_label.eq_ignore_ascii_case("synced");
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .id("sync-user")
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .py(px(10.))
                            .px(px(12.))
                            .rounded(px(RADIUS_MD))
                            .bg(t.warm)
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
                            .when_some(self.avatar(), |row, avatar| {
                                row.child(
                                    img(avatar)
                                        .size(px(28.))
                                        .min_w(px(28.))
                                        .max_w(px(28.))
                                        .flex_none()
                                        .rounded_full()
                                        .object_fit(gpui::ObjectFit::Cover),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_size(rem(0.82))
                                            .font_weight(SEMIBOLD)
                                            .line_height(rem(0.82 * 1.3))
                                            .text_color(t.ink)
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .text_size(rem(0.68))
                                            .line_height(rem(0.68 * 1.3))
                                            .text_color(t.faint)
                                            .whitespace_nowrap()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(email),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                ui::button("sync-now-btn", sync_label.to_uppercase(), Kind::Quiet)
                                    .disabled(busy)
                                    .render(&t)
                                    .gap(px(5.))
                                    .when(busy, |b| {
                                        b.opacity(1.).child(ui::spinner("sync-spinner", &t))
                                    })
                                    .when(synced, |b| b.text_color(t.ink).border_color(t.ink))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.click("sync-now-btn", window, cx)
                                    })),
                            )
                            .child(quiet("sync-signout-btn", "Sign Out".into(), true)),
                    )
            } else {
                div()
                    .flex()
                    .flex_col()
                    .child(description(
                        "Sync your dictionary and history across devices with Google Drive.",
                    ))
                    .child(
                        ui::focusable(
                            div()
                                .id("sync-signin-btn")
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap(px(8.))
                                .py(px(10.))
                                .px(px(14.))
                                .rounded(px(RADIUS_MD))
                                .bg(t.warm)
                                .border(px(1.5))
                                .border_color(t.rule)
                                .text_size(rem(0.88))
                                .line_height(rem(0.88 * 1.35))
                                .font_weight(MEDIUM)
                                .text_color(t.ink)
                                .cursor_pointer()
                                .hover(move |s| {
                                    s.bg(t.hover).border_color(t.rule_hover).shadow(lift(&t))
                                })
                                .child(img(super::assets::google()).size(px(18.)).flex_none())
                                .child("Sign in with Google"),
                            &t,
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.click("sync-signin-btn", window, cx)
                        })),
                    )
            };
        group()
            .child(ui::label("Cloud Sync", &t))
            .child(div().mt(px(2.)).child(body))
    }

    /// `.hotkey-recorder`: click, then press a shortcut.
    pub(super) fn hotkey_recorder(
        &self,
        id: &'static str,
        button: &'static str,
        setting: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let t = self.theme;
        let recording = self.state.recording_hotkey.as_deref() == Some(button);
        let value = self.state.setting(setting, "");
        let text = if recording {
            "Press a shortcut…".to_owned()
        } else if value.is_empty() {
            "Click to set".to_owned()
        } else {
            value.clone()
        };
        let accent = if t.dark { t.active_dot } else { t.ink };
        let base = div()
            .id(id)
            .w_full()
            .flex()
            .justify_center()
            .py(px(12.))
            .px(px(14.))
            .rounded(px(RADIUS_MD))
            .bg(t.warm)
            .border(px(1.5))
            .border_color(t.rule)
            .text_size(rem(0.9))
            .font_weight(if value.is_empty() || recording {
                MEDIUM
            } else {
                SEMIBOLD
            })
            .line_height(px(16.))
            .text_color(if value.is_empty() || recording {
                t.light
            } else {
                t.ink
            })
            .cursor_pointer()
            .tooltip(ui::tooltip(
                if recording {
                    "Press the keys to use. Esc cancels."
                } else {
                    "Click, then press a shortcut"
                },
                &t,
            ))
            .when(!recording, |b| {
                b.border_dashed().hover(move |s| {
                    s.border_color(t.faint)
                        .text_color(t.ink)
                        .bg(t.hover)
                        .shadow(lift(&t))
                })
            })
            .when(recording, |b| {
                b.bg(t.hover).border_color(accent).text_color(t.ink)
            })
            .child(text);
        ui::focusable(base, &t)
            .on_click(cx.listener(move |view, _, window, cx| view.click(button, window, cx)))
    }

    pub(super) fn settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let scroll = self.scroll_handle("settings");
        let s = &self.state;
        let mode = s.mode();
        let mut mics: Vec<(String, String)> = s
            .mics
            .iter()
            .map(|m| (m.id.clone(), m.name.clone()))
            .collect();
        if mics.is_empty() {
            mics.push((String::new(), "No devices found".into()));
        }
        let engines = vec![
            ("groq".to_owned(), "Groq Whisper (cloud)".to_owned()),
            (
                "azure-mai".to_owned(),
                "Microsoft MAI Transcribe (cloud, preview)".to_owned(),
            ),
            ("local".to_owned(), "Local Whisper (offline)".to_owned()),
        ];
        let guide = self.setup();
        let paused = guide.status == Status::Paused;
        let (guide_desc, guide_label) = if paused {
            (
                format!(
                    "Paused at step {} of {}: {}.",
                    guide.step + 1,
                    setup::STEPS.len(),
                    guide.step_label()
                ),
                "Resume setup",
            )
        } else {
            (
                "Revisit the engine, microphone, and hotkey steps.".to_owned(),
                "Run setup again",
            )
        };
        let dict_hotkey_set = !s.setting("dictHotkey", "").is_empty();
        let api_key = self.inputs.api_key.clone();
        let endpoint = self.inputs.endpoint.clone();
        let openrouter = self.inputs.openrouter.clone();
        // Only the dividers that open the API key and local model sections show:
        // in the CSS reference the others are bare <hr>s in a scrolling flex
        // column and shrink to nothing.
        let column = div()
            .flex()
            .flex_col()
            .pt(px(24.))
            .px(px(24.))
            .pb(px(16.))
            .child(header("Settings", None, &t))
            .child(group().child(ui::label("Microphone", &t)).child(self.select(
                "mic-select",
                mics,
                &s.setting("audioDeviceId", ""),
                cx,
            )))
            .child(
                group()
                    .child(ui::label("Transcription Engine", &t))
                    .child(self.select("mode-select", engines, &mode, cx)),
            )
            .child(group().child(ui::label("Spoken Language", &t)).child(self.select(
                "live-lang-select",
                languages(),
                &s.setting("liveLanguage", "auto"),
                cx,
            )))
            .when(mode != "local", |column| {
                column
                    .child(ui::divider(&t))
                    .child(
                        group()
                            .child(ui::label(
                                if mode == "azure-mai" {
                                    "Foundry API Key"
                                } else {
                                    "Groq API Key"
                                },
                                &t,
                            ))
                            .child(self.text_field(
                                &api_key,
                                Some(("apikey-toggle", self.show_keys)),
                                window,
                                cx,
                            )),
                    )
                    .when(mode == "azure-mai", |column| {
                        column.child(
                            group()
                                .child(ui::label("Foundry Project Endpoint", &t))
                                .child(self.text_field(&endpoint, None, window, cx)),
                        )
                    })
            })
            .child(
                group()
                    .child(ui::label("OpenRouter API Key", &t))
                    .child(self.text_field(
                        &openrouter,
                        Some(("openrouter-key-toggle", self.show_summary_key)),
                        window,
                        cx,
                    ))
                    .child(ui::hint(
                        "Optional. Used only to summarize subtitles with GPT-6 Luna.",
                        &t,
                    )),
            )
            .when(mode == "local", |column| {
                column
                    .child(ui::divider(&t))
                    .child(
                        group()
                            .child(ui::label("GPU Acceleration", &t))
                            .child(self.select(
                                "acceleration-select",
                                vec![(
                                    "vulkan".into(),
                                    "Vulkan (AMD / Intel / NVIDIA)".into(),
                                )],
                                "vulkan",
                                cx,
                            ))
                            .child(ui::hint(
                                "Runs on any GPU with a Vulkan 1.2+ driver. No extra download needed.",
                                &t,
                            )),
                    )
                    .child(group().child(ui::label("Model Status", &t)).child(self.model_row(
                        "whisper-download-btn",
                        "whisper",
                        "whisper-loaded",
                        "Model not downloaded (~600 MB)",
                        cx,
                    )))
            })
            .child(group().child(switch_row(
                "theme-toggle",
                "Dark Mode",
                "Use a darker color scheme",
                s.dark(),
                &t,
                cx,
            )))
            .child(
                group()
                    .child(switch_row(
                        "grammar-toggle",
                        "Grammar Cleanup",
                        "Fix grammar on this device before pasting",
                        s.checked("grammarCleanupEnabled"),
                        &t,
                        cx,
                    ))
                    .when(s.checked("grammarCleanupEnabled"), |group| {
                        group.child(div().mt(px(10.)).pt(px(10.)).child(self.model_row(
                            "gec-download-btn",
                            "gec",
                            "gec-loaded",
                            "Model not downloaded (~105 MB)",
                            cx,
                        )))
                    }),
            )
            .child(self.sync_section(cx))
            .child(
                group()
                    .child(ui::label("Push-to-Talk Hotkey", &t))
                    .child(self.hotkey_recorder("hotkey-recorder", "hotkey-btn", "hotkey", cx)),
            )
            .child(
                group()
                    .child(ui::label("Dictionary Hotkey", &t))
                    .child(self.hotkey_recorder(
                        "dict-hotkey-recorder",
                        "dict-hotkey-btn",
                        "dictHotkey",
                        cx,
                    ))
                    .when(dict_hotkey_set, |group| {
                        group.child(
                            div().mt(px(8.)).flex().child(
                                ui::focusable(
                                    div()
                                        .id("dict-hotkey-clear")
                                        .py(px(6.))
                                        .px(px(12.))
                                        .rounded(px(RADIUS_MD))
                                        .border_1()
                                        .border_color(t.rule)
                                        .text_size(rem(0.8))
                                        .line_height(rem(0.8 * 1.35))
                                        .font_weight(MEDIUM)
                                        .text_color(t.light)
                                        .cursor_pointer()
                                        .hover(move |s| {
                                            s.text_color(t.ink).border_color(t.faint).bg(t.hover)
                                        })
                                        .tooltip(ui::tooltip("Remove the dictionary hotkey", &t))
                                        .child("Clear"),
                                    &t,
                                )
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("dict-hotkey-clear", window, cx)
                                })),
                            ),
                        )
                    })
                    .child(ui::hint(
                        "Select text in any app and press this hotkey to add it to your dictionary.",
                        &t,
                    )),
            )
            .child(
                group().child(ui::label("Setup Guide", &t)).child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            div()
                                .id("setup-guide-desc")
                                .min_w_0()
                                .text_size(rem(0.82))
                                .font_weight(NORMAL)
                                .text_color(t.light)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .tooltip(ui::tooltip(guide_desc.clone(), &t))
                                .child(guide_desc),
                        )
                        .child(
                            ui::button("setup-guide-btn", guide_label, Kind::Action)
                                .render(&t)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("setup-guide-btn", window, cx)
                                })),
                        ),
                ),
            );
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("settings-panel")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .child(column),
            )
            .children(self.scrollbar("settings", cx))
    }
}
