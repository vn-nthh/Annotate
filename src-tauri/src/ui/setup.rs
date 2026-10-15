//! The first-run setup guide (engine, model or key, microphone, hotkey, try it,
//! features). It can be exited and resumed at the same step.
use super::components::{self as ui, icon, Kind, RADIUS_LG, RADIUS_MD, RADIUS_SM};
use super::settings::languages;
use super::theme::{rem, MEDIUM, SEMIBOLD};
use super::window::MainWindow;
use crate::native_onboarding::{self as setup, Onboarding};
use gpui::{div, prelude::*, px, Context, Div, SharedString, Window};

#[cfg(target_os = "macos")]
const KEY_STORE: &str = "Paste it below. It's stored in your macOS Keychain.";
#[cfg(not(target_os = "macos"))]
const KEY_STORE: &str = "Paste it below. It's stored in Windows Credential Manager.";

impl MainWindow {
    fn ob_title(&self, text: &'static str) -> Div {
        div()
            .mb(px(6.))
            .text_size(rem(1.2))
            .font_weight(SEMIBOLD)
            .line_height(rem(1.2 * 1.5))
            .text_color(self.theme.ink)
            .child(text)
    }

    fn ob_desc(&self, text: &'static str) -> Div {
        div()
            .mb(px(16.))
            .text_size(rem(0.82))
            .line_height(rem(0.82 * 1.5))
            .text_color(self.theme.light)
            .child(text)
    }

    fn ob_list(&self, items: &[&'static str]) -> Div {
        let t = self.theme;
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .mt(px(6.))
            .mb(px(14.))
            .children(items.iter().enumerate().map(|(index, text)| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .text_size(rem(0.82))
                    .line_height(rem(0.82 * 1.5))
                    .text_color(t.light)
                    .child(
                        div()
                            .flex_none()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(t.warm)
                            .border_1()
                            .border_color(t.rule)
                            .text_size(rem(0.7))
                            .font_weight(SEMIBOLD)
                            .line_height(rem(0.7))
                            .text_color(t.ink)
                            .child((index + 1).to_string()),
                    )
                    .child(*text)
            }))
    }

    fn ob_link(
        &self,
        id: &'static str,
        label: &'static str,
        tip: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = self.theme;
        div().flex().child(
            ui::focusable(
                div()
                    .id(id)
                    .group(id)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .py(px(8.))
                    .px(px(14.))
                    .rounded(px(RADIUS_MD))
                    .bg(t.warm)
                    .border(px(1.5))
                    .border_color(t.rule)
                    .text_size(rem(0.8))
                    .line_height(rem(0.8 * 1.35))
                    .font_weight(SEMIBOLD)
                    .text_color(t.ink)
                    .cursor_pointer()
                    .hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
                    .tooltip(ui::tooltip(tip, &t))
                    .child(label)
                    .child(icon("icons/external.svg", 12., t.ink)),
                &t,
            )
            .on_click(cx.listener(move |view, _, window, cx| view.click(id, window, cx))),
        )
    }

    fn ob_field(
        &self,
        label: &'static str,
        input: gpui::Entity<super::text_input::TextInput>,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let shown = self.show_keys;
        div()
            .pt(px(16.))
            .child(ui::label(label, &self.theme))
            .child(self.text_field(
                &input,
                reveal.then_some(("apikey-toggle", shown)),
                window,
                cx,
            ))
    }

    fn engine_card(
        &self,
        engine: &'static str,
        name: &'static str,
        tag: &'static str,
        description: &'static str,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let id = SharedString::from(format!("ob-engine-{engine}"));
        let click = format!("ob-engine-{engine}");
        ui::focusable(
            div()
                .id(id)
                .relative()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(3.))
                .py(px(12.))
                .pl(px(14.))
                .pr(px(44.))
                .rounded(px(RADIUS_LG))
                .line_height(gpui::relative(1.35))
                .bg(if selected { t.hover } else { t.warm })
                .border(px(1.5))
                .border_color(if selected { t.ink } else { t.rule })
                .cursor_pointer()
                .when(!selected, |c| {
                    c.hover(move |s| s.border_color(t.rule_hover).bg(t.hover))
                })
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(rem(0.9))
                                .font_weight(SEMIBOLD)
                                .text_color(t.ink)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(rem(0.66))
                                .font_weight(MEDIUM)
                                .text_color(t.faint)
                                .child(tag.to_uppercase()),
                        ),
                )
                .child(
                    div()
                        .text_size(rem(0.78))
                        .line_height(rem(0.78 * 1.45))
                        .text_color(t.light)
                        .child(description),
                )
                .when(selected, |c| {
                    c.child(
                        div()
                            .absolute()
                            .right(px(14.))
                            .top_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .child(icon("icons/check.svg", 18., t.ink)),
                    )
                }),
            &t,
        )
        .on_click(cx.listener(move |view, _, window, cx| view.click(&click, window, cx)))
    }

    fn model_step(&self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let s = &self.state;
        let total = crate::whisper_local::model_size_bytes();
        let downloading = s.busy.contains("whisper-download-btn");
        let downloaded = s.ready.contains("whisper");
        let failed = !s.label("whisper-status-error", "").is_empty();
        let fraction = if downloading {
            Some(s.progress.get("whisper").map_or(0.0, |p| p.0))
        } else if downloaded {
            None
        } else {
            self.paused_fraction()
        };
        let percent = fraction.map_or(0.0, |f| f * 100.0);
        let (status, button) = if downloaded {
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
        let status_color = if failed && !downloading {
            t.danger
        } else if downloaded {
            t.ink
        } else {
            t.light
        };
        div()
            .p(px(14.))
            .rounded(px(RADIUS_LG))
            .bg(t.warm)
            .border(px(1.5))
            .border_color(t.rule)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(rem(0.9))
                                    .font_weight(SEMIBOLD)
                                    .text_color(t.ink)
                                    .child("Whisper Large v3 Turbo"),
                            )
                            .child(
                                div()
                                    .text_size(rem(0.78))
                                    .text_color(status_color)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(status),
                            ),
                    )
                    .when(!downloaded, |row| {
                        row.child(
                            ui::button("ob-model-btn", button, Kind::Action)
                                .render(&t)
                                .w(px(96.))
                                .px(px(0.))
                                .py(px(8.))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("ob-model-btn", window, cx)
                                })),
                        )
                    }),
            )
            .when_some(fraction, |card, fraction| {
                card.child(
                    div()
                        .mt(px(12.))
                        .child(ui::progress_bar(fraction, t.paper, &t)),
                )
                .child(
                    div()
                        .mt(px(6.))
                        .text_size(rem(0.72))
                        .font_weight(MEDIUM)
                        .text_color(t.faint)
                        .child(format!(
                            "{} of {}",
                            setup::megabytes((fraction as f64 * total as f64) as u64),
                            setup::megabytes(total)
                        )),
                )
            })
    }

    fn try_status(&self) -> (String, gpui::Hsla) {
        let t = self.theme;
        let status = self.state.label("status-text", "Ready");
        let text = match status.as_str() {
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
        let color = if self.state.ready.contains("status-error") {
            t.danger
        } else if self.try_done {
            t.ink
        } else {
            t.light
        };
        (text, color)
    }

    fn step(&self, guide: &Onboarding, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        match guide.step_id() {
            "model" => div()
                .flex()
                .flex_col()
                .child(self.ob_title("Choose a transcription engine"))
                .child(self.ob_desc(
                    "Annotate turns your speech into text with one of these engines. You can switch later in Settings.",
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(self.engine_card(
                            "groq",
                            "Groq Whisper",
                            "Cloud · Free tier",
                            "Fast and accurate. Needs a free Groq API key and an internet connection.",
                            guide.engine == "groq",
                            cx,
                        ))
                        .child(self.engine_card(
                            "azure-mai",
                            "Microsoft MAI Transcribe",
                            "Cloud · Preview",
                            "Uses your Azure AI Foundry project. Needs its endpoint and an API key.",
                            guide.engine == "azure-mai",
                            cx,
                        ))
                        .child(self.engine_card(
                            "local",
                            "Local Whisper",
                            "Offline · Private",
                            "Runs on your graphics card. One 547 MB download, no account needed.",
                            guide.engine == "local",
                            cx,
                        )),
                ),
            "install" => match guide.engine.as_str() {
                "azure-mai" => div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("Connect Microsoft MAI Transcribe"))
                    .child(self.ob_list(&[
                        "Open your project in Azure AI Foundry.",
                        "Copy the project endpoint and API key from its overview.",
                    ]))
                    .child(self.ob_link(
                        "ob-open-azure",
                        "Open Azure AI Foundry",
                        "Opens ai.azure.com in your browser",
                        cx,
                    ))
                    .child(self.ob_field(
                        "Foundry Project Endpoint",
                        self.inputs.ob_endpoint.clone(),
                        false,
                        window,
                        cx,
                    ))
                    .child(self.ob_field(
                        "Foundry API Key",
                        self.inputs.ob_azure.clone(),
                        true,
                        window,
                        cx,
                    )),
                "local" => div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("Download the local model"))
                    .child(self.ob_desc(
                        if cfg!(target_os = "macos") {
                            "Whisper Large v3 Turbo runs on the Apple GPU through Metal, so dictation works offline and stays on this Mac."
                        } else {
                            "Whisper Large v3 Turbo runs on your graphics card through Vulkan, so dictation works offline and stays on this PC."
                        },
                    ))
                    .child(self.model_step(cx))
                    .child(ui::hint(
                        if cfg!(target_os = "macos") {
                            "Uses the Apple GPU. You can pause the download and resume it later."
                        } else {
                            "Needs a graphics card with a current Vulkan driver (AMD, Intel, or NVIDIA). You can pause the download and resume it later."
                        },
                        &t,
                    )),
                _ => div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("Get a Groq API key"))
                    .child(self.ob_list(&[
                        "Open the Groq console and sign in. It's free.",
                        "Create a key under API Keys and copy it.",
                        KEY_STORE,
                    ]))
                    .child(self.ob_link(
                        "ob-open-groq",
                        "Open Groq console",
                        "Opens console.groq.com in your browser",
                        cx,
                    ))
                    .child(self.ob_field(
                        "Groq API Key",
                        self.inputs.ob_groq.clone(),
                        true,
                        window,
                        cx,
                    )),
            },
            "microphone" => {
                let mut mics: Vec<(String, String)> = self
                    .state
                    .mics
                    .iter()
                    .map(|m| (m.id.clone(), m.name.clone()))
                    .collect();
                if mics.is_empty() {
                    mics.push((String::new(), "No devices found".into()));
                }
                div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("Pick your microphone"))
                    .child(self.ob_desc(if cfg!(target_os = "macos") {
                        "Annotate only listens while you hold the hotkey. Allow microphone access when macOS asks."
                    } else { "Annotate only listens while you hold the hotkey." }))
                    .child(ui::label("Microphone", &t))
                    .child(self.select(
                        "ob-mic-select",
                        mics,
                        &self.state.setting("audioDeviceId", ""),
                        cx,
                    ))
                    .child(
                        div()
                            .pt(px(16.))
                            .child(ui::label("Spoken Language", &t))
                            .child(self.select(
                                "ob-lang-select",
                                languages(),
                                &self.state.setting("liveLanguage", "auto"),
                                cx,
                            ))
                            .child(ui::hint(
                                "Auto-detect works for most people. Choosing your language can improve accuracy.",
                                &t,
                            )),
                    )
            }
            "hotkey" => div()
                .flex()
                .flex_col()
                .child(self.ob_title("Set your push-to-talk hotkey"))
                .child(self.ob_desc(
                    "Hold it to record. Let go, and the text is pasted where your cursor is.",
                ))
                .child(ui::label("Push-to-Talk Hotkey", &t))
                .child(self.hotkey_recorder("ob-hotkey-btn", "hotkey-btn", "hotkey", cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .mt(px(12.))
                        .child(
                            div()
                                .mr(px(2.))
                                .text_size(rem(0.76))
                                .text_color(t.faint)
                                .child("Suggestions"),
                        )
                        .child(
                            ui::button("ob-chip-0", "Ctrl+Shift+Space", Kind::Chip)
                                .render(&t)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("ob-chip-0", window, cx)
                                })),
                        )
                        .child(
                            ui::button("ob-chip-1", "Ctrl+Alt+Space", Kind::Chip)
                                .render(&t)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("ob-chip-1", window, cx)
                                })),
                        ),
                )
                .child(ui::hint(
                    if cfg!(target_os = "macos") {
                        "Choose a letter, number, or function key. Caps Lock is unavailable in this preview. Press Esc to cancel."
                    } else { "Pick keys you don't use for anything else. Press Esc to cancel recording." },
                    &t,
                )),
            "try" => {
                let hotkey = self.state.setting("hotkey", "");
                let (status, color) = self.try_status();
                div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("Try it"))
                    .child(self.ob_desc(
                        "Click the box below, hold your hotkey, and say a sentence. Let go to paste the text.",
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .mb(px(12.))
                            .child(
                                div()
                                    .text_size(rem(0.72))
                                    .font_weight(SEMIBOLD)
                                    .text_color(t.light)
                                    .child("YOUR HOTKEY"),
                            )
                            .child(
                                div()
                                    .py(px(3.))
                                    .px(px(8.))
                                    .rounded(px(RADIUS_SM))
                                    .bg(t.warm)
                                    .border_1()
                                    .border_color(t.rule)
                                    .text_size(rem(0.78))
                                    .font_weight(SEMIBOLD)
                                    .text_color(t.ink)
                                    .whitespace_nowrap()
                                    .child(if hotkey.is_empty() {
                                        "Not set".to_owned()
                                    } else {
                                        hotkey
                                    }),
                            ),
                    )
                    .child(self.text_field(&self.inputs.ob_try.clone(), None, window, cx))
                    .child(
                        div()
                            .id("ob-try-status")
                            .mt(px(10.))
                            .text_size(rem(0.8))
                            .text_color(color)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .tooltip(ui::tooltip(status.clone(), &t))
                            .child(status),
                    )
            }
            _ => {
                let feature = |id: &'static str, name: &'static str, description: &'static str| {
                    let tip = format!("Finish setup and open {name}");
                    let click = format!("ob-feature-{id}");
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .py(px(9.))
                        .pl(px(14.))
                        .pr(px(12.))
                        .rounded(px(RADIUS_MD))
                        .bg(t.warm)
                        .border_1()
                        .border_color(t.rule)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .text_size(rem(0.82))
                                        .font_weight(SEMIBOLD)
                                        .text_color(t.ink)
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .text_size(rem(0.74))
                                        .line_height(rem(0.74 * 1.4))
                                        .text_color(t.light)
                                        .child(description),
                                ),
                        )
                        .child(
                            ui::button(SharedString::from(click.clone()), "Open", Kind::Chip)
                                .render(&t)
                                .tooltip(ui::tooltip(tip, &t))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.click(&click, window, cx)
                                })),
                        )
                };
                div()
                    .flex()
                    .flex_col()
                    .child(self.ob_title("You're ready"))
                    .child(self.ob_desc(
                        "A few more things Annotate can do. Everything is in the sidebar.",
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(feature(
                                "home",
                                "History",
                                "Every transcription is kept on Home, ready to copy again.",
                            ))
                            .child(feature(
                                "dictionary",
                                "Dictionary",
                                "Add names and jargon so they're spelled right, even from selected text in any app.",
                            ))
                            .child(feature(
                                "subtitling",
                                "Subtitles",
                                "Make SRT subtitles from audio and video files.",
                            ))
                            .child(feature(
                                "settings",
                                "Settings",
                                "Grammar cleanup, Google Drive sync, and dark mode.",
                            )),
                    )
                    .child(ui::hint(
                        "Closing the window keeps Annotate in the tray, so your hotkey keeps working.",
                        &t,
                    ))
            }
        }
    }

    pub(super) fn onboarding(
        &mut self,
        guide: &Onboarding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let blocker = self.setup_blocker(guide, cx);
        let next_label = if guide.is_last() {
            "Start using Annotate"
        } else if guide.step_id() == "try" && !self.try_done {
            "Skip"
        } else {
            "Continue"
        };
        let count = format!(
            "Step {} of {} · {}",
            guide.step + 1,
            setup::STEPS.len(),
            guide.step_label()
        )
        .to_uppercase();
        let step = guide.step;
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .pt(px(18.))
            .px(px(24.))
            .pb(px(16.))
            .bg(t.paper)
            .rounded_t(px(RADIUS_LG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb(px(10.))
                    .child(
                        div()
                            .text_size(rem(0.72))
                            .font_weight(SEMIBOLD)
                            .text_color(t.light)
                            .whitespace_nowrap()
                            .child(count),
                    )
                    .child(
                        ui::button("ob-exit", "Exit setup", Kind::Link)
                            .render(&t)
                            .mr(px(-8.))
                            .tooltip(ui::tooltip(
                                "Exit setup. Resume it any time from Settings.",
                                &t,
                            ))
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.click("ob-exit", window, cx)
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.))
                    .mb(px(20.))
                    .children((0..setup::STEPS.len()).map(|index| {
                        div()
                            .flex_1()
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(if index <= step { t.ink } else { t.rule })
                    })),
            )
            .child(div().flex_1().min_h_0().child(self.step(guide, window, cx)))
            .child(ui::divider(&t).mb(px(12.)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .when(step > 0, |foot| {
                        foot.child(
                            ui::button("ob-back", "Back", Kind::Secondary)
                                .render(&t)
                                .px(px(18.))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.click("ob-back", window, cx)
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_right()
                            .text_size(rem(0.76))
                            .text_color(t.faint)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(blocker.unwrap_or("")),
                    )
                    .child(
                        ui::button("ob-next", next_label, Kind::Primary)
                            .disabled(blocker.is_some())
                            .render(&t)
                            .py(px(9.))
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.click("ob-next", window, cx)
                                }),
                            ),
                    ),
            )
    }
}
