//! "Paperlike Monochromatic" tokens. Names match `src/index.css` and AGENTS.md.
use gpui::{px, rgb, rgba, App, FontWeight, Hsla, Pixels, Rgba};
use std::borrow::Cow;

pub const FONT: &str = "Satoshi";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(not(target_os = "macos"))]
pub const MONO: &str = "Consolas";

/// The reference CSS uses `html { font-size: 14px }`; sizes are in rem.
pub fn rem(value: f32) -> Pixels {
    px(value * 14.0)
}

pub const LIGHT: FontWeight = FontWeight::LIGHT;
pub const NORMAL: FontWeight = FontWeight::NORMAL;
pub const MEDIUM: FontWeight = FontWeight::MEDIUM;
pub const SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;
pub const BOLD: FontWeight = FontWeight::BOLD;

#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub paper: Hsla,
    pub warm: Hsla,
    pub hover: Hsla,
    pub ink: Hsla,
    pub light: Hsla,
    pub faint: Hsla,
    pub rule: Hsla,
    pub rule_hover: Hsla,
    pub sidebar: Hsla,
    pub danger: Hsla,
    pub danger_bg: Hsla,
    pub focus_ring: Hsla,
    /// Hover wash for titlebar buttons and sidebar tabs.
    pub wash: Hsla,
    pub tab_wash: Hsla,
    /// Primary buttons: `ink` in light mode, a fixed off-white in dark mode.
    pub primary: Hsla,
    pub primary_hover: Hsla,
    pub on_primary: Hsla,
    pub summarize: Hsla,
    pub summarize_hover: Hsla,
    pub scrim: Hsla,
    pub active_dot: Hsla,
    pub active_ring: Hsla,
    pub scrollbar: Hsla,
    pub scrollbar_hover: Hsla,
}

fn c(color: u32) -> Hsla {
    rgb(color).into()
}
fn a(color: u32) -> Hsla {
    rgba(color).into()
}

impl Theme {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                dark,
                paper: c(0x1a1a1e),
                warm: c(0x222226),
                hover: c(0x2a2a2e),
                ink: c(0xe8e6e1),
                light: c(0xa0a0a0),
                faint: c(0x6b6b6b),
                rule: c(0x333338),
                rule_hover: c(0x444448),
                sidebar: c(0x16161a),
                danger: c(0xc06060),
                danger_bg: a(0xc0606019),
                focus_ring: a(0xffffff14),
                wash: a(0xffffff14),
                tab_wash: a(0xffffff0d),
                primary: c(0xe8e6e1),
                primary_hover: c(0xffffff),
                on_primary: c(0x1a1a1e),
                summarize: c(0x7f7f87),
                summarize_hover: c(0xa8a8b0),
                scrim: a(0x0000008c),
                active_dot: c(0x90cdf4),
                active_ring: a(0x90cdf433),
                scrollbar: c(0x444444),
                scrollbar_hover: c(0x555555),
            }
        } else {
            Self {
                dark,
                paper: c(0xfaf9f7),
                warm: c(0xf5f3ef),
                hover: c(0xffffff),
                ink: c(0x2c2c2c),
                light: c(0x6b6b6b),
                faint: c(0xa0a09c),
                rule: c(0xe8e6e1),
                rule_hover: c(0xd4d1cb),
                sidebar: c(0xf0eee9),
                danger: c(0x8b4444),
                danger_bg: a(0x8b44440f),
                focus_ring: a(0x2c2c2c1f),
                wash: a(0x2c2c2c14),
                tab_wash: a(0x2c2c2c0f),
                primary: c(0x2c2c2c),
                primary_hover: c(0x1a1a1a),
                on_primary: c(0xfaf9f7),
                summarize: c(0x5b5b63),
                summarize_hover: c(0x5b5b63),
                scrim: a(0x0000004d),
                active_dot: c(0x2c2c2c),
                active_ring: a(0x2c2c2c26),
                scrollbar: c(0xe8e6e1),
                scrollbar_hover: c(0xd4d1cb),
            }
        }
    }
}

/// Satoshi is embedded so the app never falls back to a system font.
pub fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../../assets/fonts/Satoshi-300.otf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Satoshi-400.otf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Satoshi-500.otf")),
        Cow::Borrowed(include_bytes!("../../assets/fonts/Satoshi-700.otf")),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        log::error!("Cannot load Satoshi: {error}");
    }
}

#[allow(dead_code)]
pub fn rgba_of(color: Hsla) -> Rgba {
    color.into()
}
