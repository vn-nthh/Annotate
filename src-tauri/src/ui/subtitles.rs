//! Subtitles: SRT generation from media files, with optional summaries.
use super::components::{self as ui, icon, Kind, RADIUS_MD, RADIUS_SM};
use super::home::{header, panel};
use super::settings::{group, languages};
use super::theme::{rem, MEDIUM, MONO, SEMIBOLD};
use super::window::MainWindow;
use gpui::{div, prelude::*, px, Context, Div, SharedString, Window};

impl MainWindow {
    fn sub_progress(&self) -> Option<Div> {
        let t = self.theme;
        let (fraction, message) = self.state.progress.get("sub").cloned()?;
        Some(
            div()
                .mb(px(16.))
                .flex()
                .flex_col()
                .child(ui::progress_bar(fraction, t.warm, &t))
                .child(
                    div()
                        .mt(px(6.))
                        .text_center()
                        .text_size(rem(0.75))
                        .text_color(t.light)
                        .child(message),
                ),
        )
    }

    fn dependency(
        &self,
        key: &'static str,
        name: &'static str,
        action: &'static str,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = self.theme;
        let s = &self.state;
        let downloading = s.busy.contains(action);
        let ready = s.ready.contains(key);
        let status = if downloading {
            s.progress
                .get(key)
                .map(|(fraction, _)| format!("Downloading... {:.0}%", fraction * 100.0))
                .unwrap_or_else(|| "Downloading...".into())
        } else if ready {
            "Ready".into()
        } else if s.labels.contains_key(&format!("{key}-status-error")) {
            "Download failed".into()
        } else {
            "Not installed".into()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .py(px(8.))
            .px(px(12.))
            .rounded(px(RADIUS_SM))
            .bg(t.warm)
            .border_1()
            .border_color(t.rule)
            .child(
                div()
                    .min_w(px(80.))
                    .text_size(rem(0.78))
                    .font_weight(SEMIBOLD)
                    .text_color(t.ink)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(rem(0.75))
                    .font_weight(if ready {
                        MEDIUM
                    } else {
                        gpui::FontWeight::NORMAL
                    })
                    .text_color(if ready { t.ink } else { t.faint })
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(status),
            )
            .when(!ready && !downloading, |row| {
                row.child(
                    ui::button(action, "Download", Kind::Action)
                        .render(&t)
                        .on_click(
                            cx.listener(move |view, _, window, cx| view.click(action, window, cx)),
                        ),
                )
            })
    }

    pub(super) fn subtitles(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let s = self.state.clone();
        let description = div()
            .mb(px(16.))
            .text_size(rem(0.82))
            .line_height(rem(0.82 * 1.5))
            .text_color(t.light)
            .child("Generate SRT subtitles from any audio or video file using VAD + Whisper.");
        if !s.entries.is_empty() {
            return self.subtitle_result(&s, cx).into_any_element();
        }
        let busy = s.busy.contains("sub-generate-btn");
        let locked = busy || s.busy.contains("sub-summarize-btn");
        let deps_ready = s.ready.contains("ffmpeg") && s.ready.contains("vad");
        let enabled = s.file.is_some() && deps_ready && !busy;
        let file_name = s.file.as_deref().map(|file| {
            std::path::Path::new(file)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(file)
                .to_owned()
        });
        let has_file = file_name.is_some();
        let scroll = self.scroll_handle("subtitling");
        let zone = ui::focusable(
            div()
                .id("sub-file-zone")
                .group("sub-file-zone")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .py(px(24.))
                .px(px(16.))
                .mb(px(16.))
                .rounded(px(RADIUS_MD))
                .bg(t.warm)
                .border(px(2.))
                .border_color(t.rule)
                .when(!has_file, |z| z.border_dashed())
                .when(!locked, |z| {
                    z.cursor_pointer()
                        .hover(move |s| s.border_color(t.faint).bg(t.hover))
                })
                .tooltip(ui::tooltip(
                    file_name
                        .clone()
                        .unwrap_or_else(|| "Choose an audio or video file".into()),
                    &t,
                ))
                .child(
                    icon("icons/file.svg", 32., t.faint)
                        .group_hover("sub-file-zone", move |s| s.text_color(t.light)),
                )
                .when(!has_file, |z| {
                    z.child(
                        div()
                            .text_size(rem(0.82))
                            .font_weight(MEDIUM)
                            .text_color(t.faint)
                            .child("Choose a file..."),
                    )
                })
                .when_some(file_name, |z, name| {
                    z.child(
                        div()
                            .max_w_full()
                            .text_center()
                            .text_size(rem(0.78))
                            .font_weight(SEMIBOLD)
                            .text_color(t.ink)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(name),
                    )
                }),
            &t,
        )
        .on_click(cx.listener(move |view, _, window, cx| {
            if !locked {
                view.click("sub-file-zone", window, cx)
            }
        }));
        let column = panel()
            .size_auto()
            .child(header("Subtitling", None, &t))
            .child(description)
            .child(zone)
            .child(
                group()
                    .mb(px(16.))
                    .child(ui::label("Language", &t))
                    .child(self.select(
                        "sub-lang-select",
                        languages(),
                        &s.setting("subtitleLanguage", "auto"),
                        cx,
                    )),
            )
            .when(!deps_ready, |column| {
                column.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .mb(px(16.))
                        .child(self.dependency("ffmpeg", "FFmpeg", "sub-ffmpeg-download-btn", cx))
                        .child(self.dependency("vad", "VAD Model", "sub-vad-download-btn", cx)),
                )
            })
            .child(
                ui::button(
                    "sub-generate-btn",
                    if busy {
                        "Generating..."
                    } else {
                        "Generate Subtitles"
                    },
                    Kind::Primary,
                )
                .disabled(!enabled)
                .render(&t)
                .w_full()
                .py(px(12.))
                .mb(px(16.))
                .text_size(rem(0.88))
                .on_click(cx.listener(move |view, _, window, cx| {
                    if enabled {
                        view.click("sub-generate-btn", window, cx)
                    }
                })),
            )
            .children(self.sub_progress());
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("subtitling-panel")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .child(column),
            )
            .children(self.scrollbar("subtitling", cx))
            .into_any_element()
    }

    fn subtitle_result(&mut self, s: &crate::native_core::State, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let title = s
            .file
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .and_then(|p| p.to_str())
            .unwrap_or("Subtitles")
            .to_owned();
        let preview = crate::subtitle::format_srt(&s.entries);
        let busy = s.busy.contains("sub-summarize-btn");
        let key_missing = s.label("openrouter-key-input", "").is_empty();
        let locked = s.busy.contains("sub-generate-btn") || busy;
        let copied = self.copied("sub-summary-copy-btn");
        let scroll = self.scroll_handle("preview");
        let page = self.scroll_handle("subtitling");
        // The page scrolls when a long summary pushes the actions down; the
        // preview fills the remaining height and scrolls on its own.
        let column = panel()
            .h_auto()
            .min_h(px(super::window::HEIGHT - 48.))
            .child(header("Subtitling", None, &t))
            .children(self.sub_progress())
            .child(
                div()
                    .id("sub-result-title")
                    .mb(px(2.))
                    .text_size(rem(0.92))
                    .font_weight(SEMIBOLD)
                    .text_color(t.ink)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .tooltip(ui::tooltip(title.clone(), &t))
                    .child(title),
            )
            .child(
                div()
                    .mb(px(10.))
                    .text_size(rem(0.7))
                    .font_weight(MEDIUM)
                    .text_color(t.faint)
                    .child(format!("{} entries", s.entries.len())),
            )
            .when(!s.summary.is_empty(), |column| {
                column.child(
                    div()
                        .flex_none()
                        .mb(px(12.))
                        .py(px(12.))
                        .px(px(14.))
                        .rounded(px(RADIUS_MD))
                        .bg(t.warm)
                        .border_1()
                        .border_color(t.rule)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(8.))
                                .mb(px(8.))
                                .child(
                                    div()
                                        .text_size(rem(0.72))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(t.light)
                                        .child("SUMMARY"),
                                )
                                .child(
                                    ui::focusable(
                                        div()
                                            .id("sub-summary-copy-btn")
                                            .py(px(3.))
                                            .px(px(8.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(t.rule)
                                            .when(!t.dark, |b| b.bg(t.paper))
                                            .text_size(rem(0.68))
                                            .line_height(rem(0.68 * 1.35))
                                            .font_weight(SEMIBOLD)
                                            .text_color(t.light)
                                            .cursor_pointer()
                                            .hover(move |s| {
                                                s.text_color(t.ink)
                                                    .border_color(t.faint)
                                                    .bg(if t.dark { t.tab_wash } else { t.hover })
                                            })
                                            .tooltip(ui::tooltip("Copy summary", &t))
                                            .child(if copied { "Copied!" } else { "Copy" }),
                                        &t,
                                    )
                                    .on_click(cx.listener(
                                        |view, _, window, cx| {
                                            view.click("sub-summary-copy-btn", window, cx)
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .text_size(rem(0.84))
                                .line_height(rem(0.84 * 1.55))
                                .text_color(t.ink)
                                .child(s.summary.clone()),
                        ),
                )
            })
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(60.))
                    .max_h(px(350.))
                    .mb(px(12.))
                    .rounded(px(RADIUS_MD))
                    .bg(t.warm)
                    .border_1()
                    .border_color(t.rule)
                    .child(
                        div()
                            .id("sub-preview")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .py(px(12.))
                            .px(px(14.))
                            .font_family(MONO)
                            .text_size(rem(0.72))
                            .line_height(rem(0.72 * 1.55))
                            .text_color(t.ink)
                            .children(
                                preview
                                    .lines()
                                    .map(|line| {
                                        div()
                                            .min_h(rem(0.72 * 1.55))
                                            .child(SharedString::from(line.to_owned()))
                                    })
                                    .collect::<Vec<_>>(),
                            ),
                    )
                    .children(self.scrollbar("preview", cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(8.))
                    .child(
                        ui::button("sub-save-btn", "Save .srt File", Kind::Primary)
                            .render(&t)
                            .flex_1()
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.click("sub-save-btn", window, cx)
                            })),
                    )
                    .child({
                        let disabled = busy || key_missing;
                        let button = ui::button(
                            "sub-summarize-btn",
                            if busy { "Summarizing..." } else { "Summarize" },
                            Kind::Summarize,
                        )
                        .disabled(disabled)
                        .render(&t)
                        .flex_1();
                        let button = if key_missing {
                            button.tooltip(ui::tooltip(
                                "Set an OpenRouter API key in Settings to enable summaries.",
                                &t,
                            ))
                        } else {
                            button
                        };
                        button.on_click(cx.listener(move |view, _, window, cx| {
                            if !disabled {
                                view.click("sub-summarize-btn", window, cx)
                            }
                        }))
                    })
                    .child(
                        ui::button("sub-new-btn", "New File", Kind::Plain)
                            .disabled(locked)
                            .render(&t)
                            .flex_1()
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if !locked {
                                    view.click("sub-new-btn", window, cx)
                                }
                            })),
                    ),
            );
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("subtitling-result")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&page)
                    .child(column),
            )
            .children(self.scrollbar("subtitling", cx))
    }
}
