//! Opt-in, app-owned timing and tray lifecycle probe. Never used in normal runs.
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

struct Probe {
    directory: PathBuf,
    start: Instant,
    start_ms: u128,
    ready: AtomicBool,
}
static PROBE: OnceLock<Probe> = OnceLock::new();
pub fn initialize(directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    PROBE
        .set(Probe {
            directory: directory.into(),
            start: Instant::now(),
            start_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            ready: AtomicBool::new(false),
        })
        .map_err(|_| "Benchmark already initialized")?;
    std::thread::spawn(|| {
        let probe = PROBE.get().unwrap();
        while probe.start.elapsed().as_secs() < 120 {
            if probe.ready.load(Ordering::Acquire) && probe.directory.join("quit.request").exists()
            {
                crate::native_core::get().app.exit(0);
                return;
            }
            if probe.ready.load(Ordering::Acquire)
                && probe.directory.join("hide.request").exists()
                && !probe.directory.join("hidden.json").exists()
            {
                crate::native_ui::hide();
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        crate::native_core::get().app.exit(0);
    });
    Ok(())
}
pub fn ready() {
    if let Some(probe) = PROBE.get() {
        if !probe.ready.swap(true, Ordering::AcqRel) {
            let elapsed = probe.start.elapsed().as_millis();
            let _=std::fs::write(probe.directory.join("ready.json"),serde_json::json!({"processId":std::process::id(),"startMs":probe.start_ms,"readyMs":probe.start_ms+elapsed,"uiReadyMs":elapsed}).to_string());
        }
    }
}
pub fn hidden() {
    if let Some(probe) = PROBE.get() {
        let _ = std::fs::write(probe.directory.join("hidden.json"), "{}");
    }
}
pub fn shown(hwnd: windows::Win32::Foundation::HWND) {
    if let Some(probe) = PROBE.get() {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOW,
            );
            let _=std::fs::write(probe.directory.join("shown.json"),serde_json::json!({"visible":windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd).as_bool()}).to_string());
        }
    }
}
