//! Operating-system services behind one interface: global shortcuts, tray,
//! single instance, synthetic input, clipboard, dialogs, and overlay windows.
//! The UI and app core are shared; only these modules differ per platform.
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::*;
