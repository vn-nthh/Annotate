//! Home: transcription history.
use super::components::{self as ui, Kind, RADIUS_MD};
use super::theme::{rem, Theme, MEDIUM, SEMIBOLD};
use super::window::MainWindow;
use gpui::{div, point, prelude::*, px, Context, Div, SharedString, Window};

/// `.panel-header`: the tab title with an optional count badge.
pub fn header(title: &'static str, count: Option<usize>, theme: &Theme) -> Div {
    let t = *theme;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .mb(px(16.))
        .child(
            div()
                .text_size(rem(1.2))
                .font_weight(SEMIBOLD)
                .line_height(rem(1.2 * 1.3))
                .text_color(t.ink)
                .child(title),
        )
        .when_some(count.filter(|count| *count > 0), |row, count| {
            row.child(
                div()
                    .min_w(px(20.))
                    .px(px(8.))
                    .py(px(1.))
                    .rounded(px(10.))
                    .bg(t.warm)
                    .border_1()
                    .border_color(t.rule)
                    .text_center()
                    .text_size(rem(0.7))
                    .font_weight(MEDIUM)
                    .line_height(rem(0.7 * 1.5))
                    .text_color(t.faint)
                    .child(count.to_string()),
            )
        })
}

/// `.tab-panel`: the padded column inside the content area.
pub fn panel() -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .pt(px(24.))
        .px(px(24.))
        .pb(px(16.))
}

pub fn empty(text: &'static str, theme: &Theme) -> Div {
    div()
        .py(px(40.))
        .px(px(18.))
        .text_center()
        .text_size(rem(0.85))
        .text_color(theme.faint)
        .child(text)
}

/// The lift shadow on hovered cards (`--lift-shadow`).
pub fn lift(theme: &Theme) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color: gpui::hsla(0., 0., 0., if theme.dark { 0.3 } else { 0.05 }),
        offset: point(px(0.), px(2.)),
        blur_radius: px(if theme.dark { 12. } else { 8. }),
        spread_radius: px(0.),
    }]
}

/// Small uppercase row actions (COPY / DELETE / REMOVE) shown on hover or focus.
pub fn row_action(
    id: SharedString,
    label: &'static str,
    tip: String,
    danger: bool,
    filled: bool,
    group: SharedString,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let t = *theme;
    ui::focusable(
        div()
            .id(id)
            .px(px(8.))
            .py(px(2.))
            .rounded(px(4.))
            .border_1()
            .border_color(t.rule)
            .when(filled, |b| b.bg(t.paper))
            .text_size(rem(0.62))
            .font_weight(MEDIUM)
            .line_height(rem(0.62 * 1.5))
            .text_color(t.faint)
            .cursor_pointer()
            .opacity(0.)
            .group_hover(group, |s| s.opacity(1.))
            .hover(move |s| {
                if danger {
                    s.text_color(t.danger).border_color(t.danger)
                } else {
                    s.text_color(t.ink).border_color(t.faint)
                }
            })
            .tooltip(ui::tooltip(tip, &t))
            .child(label),
        theme,
    )
    .focus(|s| s.opacity(1.))
}

impl MainWindow {
    pub(super) fn home(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let scroll = self.scroll_handle("home");
        let entries = &self.state.data.data.history.entries;
        let count = entries.len();
        let rows: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let group = SharedString::from(format!("history-{index}"));
                let copy_id = format!("history-copy:{index}");
                let copied = self.copied(&copy_id);
                div()
                    .id(group.clone())
                    .group(group.clone())
                    .relative()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .py(px(10.))
                    .px(px(12.))
                    .rounded(px(RADIUS_MD))
                    .bg(t.warm)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .hover(move |s| s.bg(t.hover).border_color(t.rule_hover).shadow(lift(&t)))
                    .child(
                        div()
                            .text_size(rem(0.68))
                            .font_weight(MEDIUM)
                            .line_height(rem(0.68))
                            .text_color(t.faint)
                            .whitespace_nowrap()
                            .child(crate::platform::format_time(entry.time)),
                    )
                    .child(
                        div()
                            .text_size(rem(0.85))
                            .line_height(rem(0.85 * 1.45))
                            .text_color(t.ink)
                            .line_clamp(3)
                            .child(entry.text.clone()),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .right(px(8.))
                            .flex()
                            .gap(px(4.))
                            .child(
                                row_action(
                                    copy_id.clone().into(),
                                    if copied { "COPIED!" } else { "COPY" },
                                    "Copy transcription".into(),
                                    false,
                                    true,
                                    group.clone(),
                                    &t,
                                )
                                .when(copied, |b| b.opacity(1.))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.click(&copy_id, window, cx)
                                })),
                            )
                            .child({
                                let id = format!("history-remove:{index}");
                                row_action(
                                    id.clone().into(),
                                    "DELETE",
                                    "Delete transcription".into(),
                                    true,
                                    true,
                                    group,
                                    &t,
                                )
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.click(&id, window, cx)
                                }))
                            }),
                    )
            })
            .collect();
        panel()
            .child(header("Transcription History", Some(count), &t))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .mb(px(8.))
                    .child(
                        div()
                            .id("history-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .child(if rows.is_empty() {
                                empty("No transcriptions yet", &t)
                            } else {
                                div().flex().flex_col().gap(px(6.)).pr(px(6.)).children(rows)
                            }),
                    )
                    .children(self.scrollbar("home", cx)),
            )
            .when(count > 0, |panel| {
                panel.child(
                    div().flex().child(
                        ui::button("history-clear", "Clear all", Kind::Link)
                            .danger_hover()
                            .render(&t)
                            .px(px(12.))
                            .py(px(8.))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.click("history-clear", window, cx)
                            })),
                    ),
                )
            })
    }
}
