//! Dictionary: terms passed to the transcription engine as a prompt.
use super::components::{self as ui, Kind, RADIUS_MD};
use super::home::{empty, header, panel, row_action};
use super::theme::{rem, MEDIUM, SEMIBOLD};
use super::window::MainWindow;
use gpui::{div, prelude::*, px, Context, SharedString, Window};

impl MainWindow {
    pub(super) fn dictionary(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let scroll = self.scroll_handle("dictionary");
        let terms = &self.state.data.data.dictionary.terms;
        let count = terms.len();
        let rows: Vec<_> = terms
            .iter()
            .enumerate()
            .map(|(index, term)| {
                let group = SharedString::from(format!("term-{index}"));
                let id = format!("dict-remove:{index}");
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .py(px(10.))
                    .px(px(12.))
                    .rounded(px(RADIUS_MD))
                    .bg(t.warm)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
                    .child(
                        div()
                            .min_w_0()
                            .text_size(rem(0.88))
                            .font_weight(MEDIUM)
                            .line_height(rem(0.88 * 1.5))
                            .text_color(t.ink)
                            .child(term.value.clone()),
                    )
                    .child(
                        row_action(
                            id.clone().into(),
                            "REMOVE",
                            format!("Remove {}", term.value),
                            true,
                            false,
                            group,
                            &t,
                        )
                        .flex_none()
                        .on_click(cx.listener(move |view, _, window, cx| view.click(&id, window, cx))),
                    )
            })
            .collect();
        panel()
            .child(header("Dictionary", Some(count), &t))
            .child(
                div()
                    .mb(px(16.))
                    .text_size(rem(0.82))
                    .line_height(rem(0.82 * 1.5))
                    .text_color(t.light)
                    .child("Add key terms to improve transcription accuracy for supported engines."),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap(px(8.))
                    .mb(px(16.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.text_field(&self.inputs.dict.clone(), None, window, cx)),
                    )
                    .child(
                        ui::button("dict-add-btn", "Add", Kind::Primary)
                            .render(&t)
                            .text_size(rem(0.85))
                            .font_weight(SEMIBOLD)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.click("dict-add-btn", window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("dict-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .child(if rows.is_empty() {
                                empty("No terms added yet", &t)
                            } else {
                                div().flex().flex_col().gap(px(4.)).pr(px(6.)).children(rows)
                            }),
                    )
                    .children(self.scrollbar("dictionary", cx)),
            )
    }
}
