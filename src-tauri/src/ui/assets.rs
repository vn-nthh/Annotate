//! Icons and artwork compiled into the executable; nothing is read from disk.
use gpui::{AssetSource, Image, ImageFormat, Result, SharedString};
use std::{borrow::Cow, sync::Arc};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        fn icon(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $name, ".svg") => {
                    Some(include_bytes!(concat!("../../assets/icons/", $name, ".svg")))
                })*
                _ => None,
            }
        }
        const ICONS: &[&str] = &[$(concat!("icons/", $name, ".svg")),*];
    };
}
icons!(
    "home", "dictionary", "subtitles", "settings", "resume", "minimize", "close", "file",
    "chevron", "eye", "external", "check", "duplicate", "ring", "arc",
);

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(icon(path).map(Cow::Borrowed))
    }
    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|icon| icon.starts_with(path))
            .map(|icon| SharedString::from(*icon))
            .collect())
    }
}

/// Full-color artwork is drawn as images rather than tinted icon masks.
pub fn logo(dark: bool) -> Arc<Image> {
    static LIGHT: std::sync::OnceLock<Arc<Image>> = std::sync::OnceLock::new();
    static DARK: std::sync::OnceLock<Arc<Image>> = std::sync::OnceLock::new();
    let (slot, bytes): (_, &'static [u8]) = if dark {
        (&DARK, include_bytes!("../../assets/logo-dark.svg"))
    } else {
        (&LIGHT, include_bytes!("../../assets/logo-light.svg"))
    };
    slot.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Svg, bytes.to_vec())))
        .clone()
}

pub fn google() -> Arc<Image> {
    static GOOGLE: std::sync::OnceLock<Arc<Image>> = std::sync::OnceLock::new();
    GOOGLE
        .get_or_init(|| {
            Arc::new(Image::from_bytes(
                ImageFormat::Svg,
                include_bytes!("../../assets/icons/google.svg").to_vec(),
            ))
        })
        .clone()
}
