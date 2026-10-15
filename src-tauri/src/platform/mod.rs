//! Operating-system services behind one interface: global shortcuts, tray,
//! single instance, audio capture, synthetic input, clipboard, dialogs, data
//! access, child-process lifetime, and overlay windows.
//! The UI and app core are shared; only these modules differ per platform.
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::*;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
