//! Building blocks shared by the panels. Sizes follow `src/index.css`.
use super::theme::{rem, Theme, MEDIUM, NORMAL, SEMIBOLD};
use gpui::{
    div, percentage, prelude::*, px, svg, Animation, AnimationExt, AnyView, App, Div, ElementId,
    Hsla, SharedString, Stateful, Transformation, Window,
};
use std::time::Duration;

pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 10.0;
pub const RADIUS_LG: f32 = 14.0;

/// Visible keyboard focus: the CSS `0 0 0 3px var(--focus-ring)` halo. Bordered
/// controls also darken their border, as `:focus` does in the reference.
pub fn focusable(element: Stateful<Div>, theme: &Theme) -> Stateful<Div> {
    let (ring, border) = (theme.focus_ring, theme.faint);
    element
        .tab_index(0)
        .focus(move |s| s.border_color(border).shadow(focus_ring(ring)))
}

pub fn focus_ring(color: Hsla) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow {
        color,
        offset: gpui::point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(3.),
    }]
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// `ink` fill: Add, Generate, Save, modal Quit.
    Primary,
    /// `warm` fill with a 1.5px rule border: modal tray, setup Back.
    Secondary,
    /// Small `ink` buttons beside model status (`.whisper-action-btn`).
    Action,
    /// Transparent with a hairline border (`.ob-chip`, `.ob-feature-btn`).
    Chip,
    /// Uppercase hairline buttons (`.sync-action-btn`).
    Quiet,
    /// Text-only (`.modal-cancel`, `.history-clear`, `.ob-exit`).
    Link,
    /// The grey Summarize button (`.sub-summarize-btn`).
    Summarize,
    /// New File (`.sub-new-btn`): `warm` with a hairline border.
    Plain,
}

pub struct Button {
    id: ElementId,
    label: SharedString,
    kind: Kind,
    disabled: bool,
    danger_hover: bool,
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind) -> Button {
    Button {
        id: id.into(),
        label: label.into(),
        kind,
        disabled: false,
        danger_hover: false,
    }
}

impl Button {
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    /// Destructive actions turn `danger` on hover (Sign Out, Clear all).
    pub fn danger_hover(mut self) -> Self {
        self.danger_hover = true;
        self
    }
    pub fn render(self, theme: &Theme) -> Stateful<Div> {
        let t = *theme;
        let base = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .whitespace_nowrap()
            // Form controls use the font's normal line height (1.35 for Satoshi).
            .line_height(gpui::relative(1.35))
            .child(self.label);
        let base = match self.kind {
            Kind::Primary => base
                .px(px(18.))
                .py(px(10.))
                .rounded(px(RADIUS_MD))
                .bg(t.primary)
                .text_color(t.on_primary)
                .text_size(rem(0.82))
                .font_weight(SEMIBOLD)
                .when(!self.disabled, |b| b.hover(move |s| s.bg(t.primary_hover))),
            Kind::Secondary => base
                .px(px(14.))
                .py(px(9.))
                .rounded(px(RADIUS_MD))
                .bg(t.warm)
                .border(px(1.5))
                .border_color(t.rule)
                .text_color(t.ink)
                .text_size(rem(0.82))
                .font_weight(SEMIBOLD)
                .when(!self.disabled, |b| {
                    b.hover(move |s| s.bg(t.hover).border_color(t.rule_hover))
                }),
            Kind::Action => base
                .px(px(14.))
                .py(px(6.))
                .rounded(px(RADIUS_SM))
                .bg(t.primary)
                .text_color(t.on_primary)
                .text_size(rem(0.78))
                .font_weight(SEMIBOLD)
                .when(!self.disabled, |b| b.hover(move |s| s.bg(t.primary_hover))),
            Kind::Chip => base
                .px(px(10.))
                .py(px(4.))
                .rounded(px(RADIUS_SM))
                .border_1()
                .border_color(t.rule)
                .text_color(t.light)
                .text_size(rem(0.75))
                .font_weight(MEDIUM)
                .when(!self.disabled, |b| {
                    b.hover(move |s| s.text_color(t.ink).border_color(t.rule_hover).bg(t.warm))
                }),
            Kind::Quiet => base
                .px(px(10.))
                .py(px(4.))
                .rounded(px(RADIUS_SM))
                .border_1()
                .border_color(t.rule)
                .text_color(t.faint)
                .text_size(rem(0.72))
                .font_weight(MEDIUM)
                .when(!self.disabled, |b| {
                    if self.danger_hover {
                        b.hover(move |s| {
                            s.text_color(t.danger)
                                .border_color(t.danger)
                                .bg(t.danger_bg)
                        })
                    } else {
                        b.hover(move |s| s.text_color(t.ink).border_color(t.rule_hover).bg(t.warm))
                    }
                }),
            Kind::Summarize => base
                .px(px(16.))
                .py(px(10.))
                .rounded(px(RADIUS_MD))
                .bg(t.summarize)
                .text_color(t.on_primary)
                .text_size(rem(0.82))
                .font_weight(SEMIBOLD)
                .when(!self.disabled, |b| {
                    b.hover(move |s| s.bg(t.summarize_hover))
                }),
            Kind::Plain => base
                .px(px(18.))
                .py(px(10.))
                .rounded(px(RADIUS_MD))
                .when(!t.dark, |b| b.bg(t.warm))
                .border_1()
                .border_color(t.rule)
                .text_color(t.ink)
                .text_size(rem(0.82))
                .font_weight(MEDIUM)
                .when(!self.disabled, |b| {
                    b.hover(move |s| s.border_color(t.faint))
                }),
            Kind::Link => base
                .px(px(8.))
                .py(px(4.))
                .rounded(px(RADIUS_SM))
                .text_color(t.faint)
                .text_size(rem(0.75))
                .font_weight(MEDIUM)
                .when(!self.disabled, |b| {
                    if self.danger_hover {
                        b.hover(move |s| s.text_color(t.danger).bg(t.danger_bg))
                    } else {
                        b.hover(move |s| s.text_color(t.ink))
                    }
                }),
        };
        let base = if self.disabled {
            base.opacity(0.4).cursor_not_allowed()
        } else {
            base.cursor_pointer()
        };
        focusable(base, theme)
    }
}

/// Section label (`.setting-label`): small uppercase `ink-light`.
pub fn label(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .mb(px(8.))
        .text_size(rem(0.72))
        .font_weight(SEMIBOLD)
        .text_color(theme.light)
        .child(SharedString::from(text.into().to_uppercase()))
}

/// Supporting copy under a control (`.setting-hint`).
pub fn hint(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .mt(px(8.))
        .text_size(rem(0.76))
        .line_height(rem(0.76 * 1.45))
        .text_color(theme.faint)
        .child(text.into())
}

pub fn divider(theme: &Theme) -> Div {
    div().h(px(1.)).w_full().flex_none().bg(theme.rule)
}

/// The 44×24 switch (`.toggle-switch`).
pub fn toggle(id: impl Into<ElementId>, checked: bool, theme: &Theme) -> Stateful<Div> {
    let t = *theme;
    let knob_left = if checked { 23. } else { 3. };
    focusable(
        div()
            .id(id)
            .relative()
            .flex_none()
            .w(px(44.))
            .h(px(24.))
            .rounded(px(12.))
            .bg(if checked { t.ink } else { t.rule })
            .cursor_pointer()
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(px(knob_left))
                    .size(px(18.))
                    .rounded_full()
                    .bg(t.paper)
                    .shadow(vec![gpui::BoxShadow {
                        color: gpui::hsla(0., 0., 0., 0.15),
                        offset: gpui::point(px(0.), px(1.)),
                        blur_radius: px(3.),
                        spread_radius: px(0.),
                    }]),
            ),
        theme,
    )
}

/// A 12px ring with a rotating `ink` arc (`.spinner-inline`).
pub fn spinner(id: impl Into<ElementId>, theme: &Theme) -> impl IntoElement {
    div()
        .relative()
        .flex_none()
        .size(px(12.))
        .child(
            svg()
                .absolute()
                .size(px(12.))
                .path("icons/ring.svg")
                .text_color(theme.rule),
        )
        .child(
            svg()
                .absolute()
                .size(px(12.))
                .path("icons/arc.svg")
                .text_color(theme.ink)
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(600)).repeat(),
                    |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
                ),
        )
}

/// A thin bar (`.whisper-progress-bar`), 0..=1.
pub fn progress_bar(fraction: f32, background: Hsla, theme: &Theme) -> Div {
    div()
        .h(px(6.))
        .w_full()
        .rounded(px(3.))
        .border_1()
        .border_color(theme.rule)
        .bg(background)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .rounded(px(3.))
                .bg(theme.ink)
                .w(gpui::relative(fraction.clamp(0.0, 1.0))),
        )
}

pub struct Tooltip {
    text: SharedString,
    theme: Theme,
}
impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = self.theme;
        div().pl(px(8.)).pt(px(10.)).child(
            div()
                .max_w(px(280.))
                .px(px(8.))
                .py(px(4.))
                .rounded(px(RADIUS_SM))
                .bg(t.paper)
                .border_1()
                .border_color(t.rule)
                .shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0., 0., 0., if t.dark { 0.3 } else { 0.08 }),
                    offset: gpui::point(px(0.), px(2.)),
                    blur_radius: px(8.),
                    spread_radius: px(0.),
                }])
                .font_family(super::theme::FONT)
                .text_size(rem(0.75))
                .font_weight(NORMAL)
                .text_color(t.ink)
                .child(self.text.clone()),
        )
    }
}

pub fn tooltip(
    text: impl Into<SharedString>,
    theme: &Theme,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let theme = *theme;
    move |_, cx| {
        cx.new(|_| Tooltip {
            text: text.clone(),
            theme,
        })
        .into()
    }
}

/// An icon tinted with `color`.
pub fn icon(path: &'static str, size: f32, color: Hsla) -> gpui::Svg {
    svg()
        .path(path)
        .size(px(size))
        .flex_none()
        .text_color(color)
}
