//! Per-process audio device routing for Windows.
//!
//! Adapted from the working CATTbyCatt implementation.
//! Uses the undocumented `Windows.Media.Internal.AudioPolicyConfig` WinRT class
//! (same as EarTrumpet and Windows Settings "App volume and device preferences")
//!
//! Strategy:
//! 1. Apply override to our own process (slot 25)
//! 2. Spawn a background thread that watches for msedgewebview2.exe children
//!    and applies the override to each one (slot 25 first, fallback to slot 24)
//! 3. When user picks a mic at runtime, immediately apply to all current PIDs

use crate::AudioDevice;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::core::{GUID, HRESULT, HSTRING, IInspectable, Interface};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eCapture, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    STGM_READ,
};
use windows::Win32::System::WinRT::RoGetActivationFactory;

// ============================================================================
// COM RAII guard — ensures every CoInitializeEx is paired with CoUninitialize
// ============================================================================

struct ComGuard;

impl ComGuard {
    fn init() -> Self {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        ComGuard
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

// ============================================================================
// AudioPolicyConfig — raw COM wrapper
// ============================================================================

struct AudioPolicyConfig {
    ptr: *mut c_void,
}

unsafe impl Send for AudioPolicyConfig {}

impl Drop for AudioPolicyConfig {
    fn drop(&mut self) {
        unsafe {
            if !self.ptr.is_null() {
                let vtbl_ptr = *(self.ptr as *const *const *const c_void);
                let release_fn: unsafe extern "system" fn(*mut c_void) -> u32 =
                    std::mem::transmute(*vtbl_ptr.add(2));
                release_fn(self.ptr);
            }
        }
    }
}

// ============================================================================
// Global state for the watcher thread
// ============================================================================

pub static ACTIVE_DEVICE: Mutex<Option<String>> = Mutex::new(None);

// ============================================================================
// Public API
// ============================================================================

/// Enumerate all audio capture (microphone) devices
pub fn enumerate_capture_devices() -> Result<Vec<AudioDevice>, Box<dyn std::error::Error>> {
    unsafe {
        let _com = ComGuard::init();

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;

        let collection = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;

        let count = collection.GetCount()?;
        let mut devices = Vec::with_capacity(count as usize);

        for i in 0..count {
            let device = collection.Item(i)?;
            let id = device.GetId()?.to_string()?;

            let store = device.OpenPropertyStore(STGM_READ)?;
            let name_prop = store.GetValue(&PKEY_Device_FriendlyName)?;
            let name = name_prop.to_string();

            devices.push(AudioDevice { id, name });
        }

        Ok(devices)
    }
}

/// Apply the audio override to ALL current PIDs (our process + WebView2 children).
pub fn apply_device_override(device_id: &str) -> Result<(), String> {
    // Update the global so the watcher thread uses the new device
    *ACTIVE_DEVICE.lock().unwrap_or_else(|e| e.into_inner()) = Some(device_id.to_string());

    let pids = get_target_pids();
    log::info!("[AudioPolicy] Applying override to {} PIDs: {:?}", pids.len(), pids);

    for pid in &pids {
        match unsafe { apply_to_pid(*pid, device_id) } {
            Ok(_) => log::info!("[AudioPolicy] PID {} → OK", pid),
            Err(e) => log::error!("[AudioPolicy] PID {} → FAILED: {:?}", pid, e),
        }
    }

    Ok(())
}

/// Apply saved audio device override at startup and spawn watcher thread.
pub fn apply_startup_override_and_watch() {
    let device_id = {
        let guard = ACTIVE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(id) => id.clone(),
            None => return,
        }
    };

    log::info!("[AudioPolicy] Applying startup override: {}", device_id);

    // Apply to our own PID immediately
    let own_pid = std::process::id();
    match unsafe { apply_to_pid(own_pid, &device_id) } {
        Ok(_) => log::info!("[AudioPolicy] Applied to our PID {}", own_pid),
        Err(e) => log::error!("[AudioPolicy] Failed for our PID: {:?}", e),
    }

    // Spawn background thread to watch for WebView2 children
    std::thread::spawn(move || {
        webview_watcher(own_pid);
    });
}

// ============================================================================
// Background watcher
// ============================================================================

/// Polls for new msedgewebview2.exe children every 500ms for 30 seconds
/// and applies the audio override to each new one.
fn webview_watcher(parent_pid: u32) {
    let mut known_pids: HashSet<u32> = HashSet::new();

    // Watch for 30 seconds (WebView2 should spawn within the first few seconds)
    for i in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(500));

        let device_id = {
            let guard = ACTIVE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(id) => id.clone(),
                None => return, // cleared, stop watching
            }
        };

        if let Ok(child_pids) = find_webview2_child_pids(parent_pid) {
            for pid in child_pids {
                if known_pids.insert(pid) {
                    // New PID found!
                    log::info!("[AudioPolicy] New WebView2 child PID {} detected (iteration {})", pid, i);
                    match unsafe { apply_to_pid(pid, &device_id) } {
                        Ok(_) => log::info!("[AudioPolicy] Applied override to WebView2 PID {}", pid),
                        Err(e) => log::error!("[AudioPolicy] Failed for WebView2 PID {}: {:?}", pid, e),
                    }
                }
            }
        }
    }

    log::info!("[AudioPolicy] Watcher thread done. Found {} WebView2 children.", known_pids.len());
}

// ============================================================================
// Implementation
// ============================================================================

unsafe fn create_policy_config() -> windows::core::Result<AudioPolicyConfig> {

    let class_name = HSTRING::from("Windows.Media.Internal.AudioPolicyConfig");
    let factory: IInspectable = RoGetActivationFactory(&class_name)?;

    #[repr(C)]
    struct RawIUnknownVtbl {
        qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        _add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        _release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    let factory_raw = factory.as_raw() as *mut c_void;
    let iunknown_vtbl = &**(factory_raw as *const *const RawIUnknownVtbl);

    let mut iface_ptr: *mut c_void = std::ptr::null_mut();
    let hr = (iunknown_vtbl.qi)(
        factory_raw,
        &GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324),
        &mut iface_ptr,
    );

    hr.ok()?;

    if iface_ptr.is_null() {
        return Err(windows::core::Error::from(HRESULT(-1)));
    }

    Ok(AudioPolicyConfig { ptr: iface_ptr })
}

/// Apply the audio override to a single PID.
/// EarTrumpet capture SWD format:
/// \\?\SWD#MMDEVAPI#{deviceId}#{2eef81be-33fa-4800-9670-1cd474972c3f}
unsafe fn apply_to_pid(pid: u32, device_id: &str) -> windows::core::Result<()> {
    // Keep COM alive for the entire function — outlives the AudioPolicyConfig pointer
    let _com = ComGuard::init();
    let policy = create_policy_config()?;
    let vtbl_ptr = *(policy.ptr as *const *const *const c_void);

    let capture_id = format!(
        "\\\\?\\SWD#MMDEVAPI#{}#{{2eef81be-33fa-4800-9670-1cd474972c3f}}",
        device_id
    );
    log::info!("[AudioPolicy] SWD path: {}", capture_id);
    let hstr = HSTRING::from(capture_id.as_str());
    let handle: *const c_void = std::mem::transmute_copy(&hstr);

    // Try slot 25 (4-param: pid, flow, role, hstr) — works for own process
    let fn4: unsafe extern "system" fn(
        *mut c_void, u32, i32, i32, *const c_void,
    ) -> HRESULT = std::mem::transmute(*vtbl_ptr.add(25));

    let r1 = fn4(policy.ptr, pid, 1, 0, handle); // eCapture, eConsole
    let r2 = fn4(policy.ptr, pid, 1, 1, handle); // eCapture, eMultimedia

    if r1.is_ok() {
        log::info!("[AudioPolicy] PID {} → slot25 eConsole=OK, eMultimedia={:?}", pid, r2);
        return Ok(());
    }

    // Fallback: slot 24 (3-param: pid, role, hstr) — works for cross-process
    log::info!("[AudioPolicy] PID {} → slot25 failed ({:?}), trying slot24 3-param", pid, r1);
    let fn3: unsafe extern "system" fn(
        *mut c_void, u32, i32, *const c_void,
    ) -> HRESULT = std::mem::transmute(*vtbl_ptr.add(24));

    let r3 = fn3(policy.ptr, pid, 0, handle); // eConsole
    let r4 = fn3(policy.ptr, pid, 1, handle); // eMultimedia
    log::info!("[AudioPolicy] PID {} → slot24 eConsole={:?}, eMultimedia={:?}", pid, r3, r4);

    r3.ok()
}

// ============================================================================
// Process enumeration
// ============================================================================

fn get_target_pids() -> Vec<u32> {
    let our_pid = std::process::id();
    let mut pids = vec![our_pid];

    if let Ok(child_pids) = find_webview2_child_pids(our_pid) {
        pids.extend(child_pids);
    }

    pids
}

fn find_webview2_child_pids(parent_pid: u32) -> Result<Vec<u32>, String> {
    use std::mem::size_of;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut child_pids = Vec::new();

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot failed: {e}"))?;

        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ParentProcessID == parent_pid {
                    let exe_name = String::from_utf16_lossy(
                        &entry.szExeFile[..entry
                            .szExeFile
                            .iter()
                            .position(|&c| c == 0)
                            .unwrap_or(entry.szExeFile.len())],
                    );
                    if exe_name.to_lowercase().contains("msedgewebview2") {
                        child_pids.push(entry.th32ProcessID);
                    }
                }

                entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }

    Ok(child_pids)
}
