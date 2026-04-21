# Per-Process Audio Device Routing on Windows 11

## Implementation Guide for Tauri / WebView2 Apps

> Set a per-process default microphone for your Tauri app and its WebView2 child processes using the undocumented `Windows.Media.Internal.AudioPolicyConfig` WinRT class — the same API used by EarTrumpet and Windows Settings' "App volume and device preferences."

---

## Why You Need This

WebView2 (`msedgewebview2.exe`) ignores per-process audio routing and calls `IMMDeviceEnumerator::GetDefaultAudioEndpoint()` directly. Without this override:
- `navigator.mediaDevices.getUserMedia({ audio: true })` uses the system default mic
- The Web Speech API uses the system default mic
- There is **no browser-level API** to select a mic for Web Speech

This guide uses the same undocumented API that Windows Settings uses internally.

---

## Dependencies

```toml
# Cargo.toml
[dependencies]
windows = { version = "0.58", features = [
    "Win32_Media_Audio",
    "Win32_System_Com",
    "Win32_System_WinRT",
    "Win32_System_Diagnostics_ToolHelp",
    "Win32_Foundation",
    "Win32_Devices_FunctionDiscovery",
    "Win32_UI_Shell_PropertiesSystem",
] }
```

> **Note:** Tested with `windows` crate `0.58`. The API surface changed in `0.61+` — `IInspectable` moved from `windows::Foundation` to `windows::core`, and `RoGetActivationFactory` may require different feature flags (`Win32_System_WinRT` still works). Both versions are viable; `0.58` is proven.

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│  Startup                                        │
│  1. Load saved device preference                │
│  2. Apply override to own PID (slot 25)         │
│  3. Spawn watcher thread                        │
│     └─ Poll every 500ms for 30s                 │
│        └─ Apply override to new WebView2 PIDs   │
├─────────────────────────────────────────────────┤
│  User changes mic at runtime                    │
│  1. Save preference                             │
│  2. Collect all PIDs (own + WebView2 children)  │
│  3. Apply override to each PID                  │
└─────────────────────────────────────────────────┘
```

### Critical: Startup Order

The watcher thread **MUST** start **before** `tauri::Builder` runs. WebView2 processes spawn during Tauri initialization — if the watcher starts after, it misses them.

```rust
pub fn run() {
    // ← HERE, before Builder
    apply_saved_audio_override();

    tauri::Builder::default()
        // ...
        .run(tauri::generate_context!())
        .expect("error");
}
```

---

## The AudioPolicyConfig Interface

### Activation

```rust
use windows::core::{GUID, HRESULT, HSTRING, IInspectable, Interface};
use windows::Win32::System::WinRT::RoGetActivationFactory;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

unsafe fn create_policy_config() -> windows::core::Result<*mut c_void> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

    // 1. Activate the WinRT factory
    let class_name = HSTRING::from("Windows.Media.Internal.AudioPolicyConfig");
    let factory: IInspectable = RoGetActivationFactory(&class_name)?;

    // 2. QI for the 21H2+ interface
    let factory_raw = factory.as_raw() as *mut c_void;
    let vtbl = &**(factory_raw as *const *const RawIUnknownVtbl);

    let iid = GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324);
    let mut iface_ptr: *mut c_void = std::ptr::null_mut();
    (vtbl.qi)(factory_raw, &iid, &mut iface_ptr).ok()?;

    Ok(iface_ptr)
}
```

### Interface IIDs

| Windows Version | IID |
|----------------|-----|
| **21H2+ (Windows 11)** | `{ab3d4648-e242-459f-b02f-541c70306324}` |
| Downlevel (Windows 10) | `{2a59116d-6c4f-45e0-a74f-707e3fef9258}` |

### VTable Slots (after IInspectable)

Only two slots matter:

| Slot | Name | Signature | Use |
|------|------|-----------|-----|
| **25** | `SetPersistedDefaultAudioEndpoint` | `(this, pid: u32, flow: i32, role: i32, deviceId: HSTRING) → HRESULT` | Own process ✅ / Cross-process ❌ |
| **24** | (3-param fallback) | `(this, pid: u32, role: i32, deviceId: HSTRING) → HRESULT` | Own process ✅ / Cross-process ✅ |

---

## Device ID Format

**This is the critical piece.** The device ID is NOT the raw `IMMDevice::GetId()` string. It must be wrapped in the SWD (Software Device) path format with the correct interface GUID.

| Direction | GUID Suffix |
|-----------|------------|
| Render (speakers) | `#{e6327cad-dcec-4949-ae8a-991e976a79d2}` |
| **Capture (microphone)** | **`#{2eef81be-33fa-4800-9670-1cd474972c3f}`** |

### Format

```
\\?\SWD#MMDEVAPI#{raw_device_id}#{2eef81be-33fa-4800-9670-1cd474972c3f}
```

Where `raw_device_id` comes from `IMMDevice::GetId()`, e.g. `{0.0.1.00000000}.{8fa6dfb5-...}`.

### In Rust

```rust
let capture_id = format!(
    "\\\\?\\SWD#MMDEVAPI#{}#{{2eef81be-33fa-4800-9670-1cd474972c3f}}",
    device_id
);
let hstr = HSTRING::from(capture_id.as_str());
let handle: *const c_void = std::mem::transmute_copy(&hstr);
```

> **⚠️ Using the render GUID (`e6327cad-...`) for a capture device produces `E_INVALIDARG` every time.** This was the single biggest pitfall — the error gives zero diagnostic info.

---

## Applying the Override

### Per-PID Function

```rust
unsafe fn apply_to_pid(pid: u32, device_id: &str) -> windows::core::Result<()> {
    let policy_ptr = create_policy_config()?;
    let vtbl_ptr = *(policy_ptr as *const *const *const c_void);

    // Build the SWD capture path
    let capture_id = format!(
        "\\\\?\\SWD#MMDEVAPI#{}#{{2eef81be-33fa-4800-9670-1cd474972c3f}}",
        device_id
    );
    let hstr = HSTRING::from(capture_id.as_str());
    let handle: *const c_void = std::mem::transmute_copy(&hstr);

    // Try slot 25 first (4-param) — works for own process
    type Fn4 = unsafe extern "system" fn(*mut c_void, u32, i32, i32, *const c_void) -> HRESULT;
    let fn4: Fn4 = std::mem::transmute(*vtbl_ptr.add(25));

    let r1 = fn4(policy_ptr, pid, 1, 0, handle); // eCapture, eConsole
    let r2 = fn4(policy_ptr, pid, 1, 1, handle); // eCapture, eMultimedia

    if r1.is_ok() {
        return Ok(()); // Slot 25 worked (own process)
    }

    // Fallback: slot 24 (3-param) — works cross-process
    type Fn3 = unsafe extern "system" fn(*mut c_void, u32, i32, *const c_void) -> HRESULT;
    let fn3: Fn3 = std::mem::transmute(*vtbl_ptr.add(24));

    let r3 = fn3(policy_ptr, pid, 0, handle); // eConsole
    let _  = fn3(policy_ptr, pid, 1, handle); // eMultimedia

    r3.ok()
}
```

### Cross-Process Behavior

| Target | Slot 25 (4-param) | Slot 24 (3-param) |
|--------|-------------------|-------------------|
| **Own process** | ✅ S_OK | ✅ S_OK |
| **WebView2 child** | ❌ E_INVALIDARG | ✅ S_OK |

Always try slot 25 first. If it returns `E_INVALIDARG`, fall back to slot 24. This way own-process gets the 4-param version and children get the 3-param version automatically.

---

## Finding WebView2 Child Processes

Use `CreateToolhelp32Snapshot` to enumerate processes by parent PID. **Do NOT use `sysinfo` or scan all processes by name** — you need children of YOUR process specifically.

```rust
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

fn find_webview2_child_pids(parent_pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).unwrap();
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..std::mem::zeroed()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if entry.th32ParentProcessID == parent_pid {
                    let name = String::from_utf16_lossy(
                        &entry.szExeFile[..entry.szExeFile.iter()
                            .position(|&c| c == 0).unwrap_or(entry.szExeFile.len())]
                    );
                    if name.to_lowercase().contains("msedgewebview2") {
                        pids.push(entry.th32ProcessID);
                    }
                }
                entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
                if Process32NextW(snapshot, &mut entry).is_err() { break; }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
    pids
}
```

---

## The Watcher Thread

WebView2 spawns multiple child processes asynchronously. A single scan at startup misses late-spawning ones. Poll for 30 seconds:

```rust
fn webview_watcher(parent_pid: u32, device_id: String) {
    let mut known: HashSet<u32> = HashSet::new();

    for _ in 0..60 {  // 60 × 500ms = 30 seconds
        std::thread::sleep(std::time::Duration::from_millis(500));

        for pid in find_webview2_child_pids(parent_pid) {
            if known.insert(pid) {
                // New child — apply override
                let _ = unsafe { apply_to_pid(pid, &device_id) };
            }
        }
    }
}
```

---

## Pitfalls

| Pitfall | Consequence | Fix |
|---------|-------------|-----|
| Using render GUID for capture | `E_INVALIDARG` with zero diagnostics | Use `#{2eef81be-33fa-4800-9670-1cd474972c3f}` |
| Starting watcher after `tauri::Builder` | Misses WebView2 processes | Call before Builder |
| Using `COINIT_MULTITHREADED` | Potential COM threading issues | Use `COINIT_APARTMENTTHREADED` |
| Using `sysinfo` to scan all processes | Finds unrelated WebView2 instances | Use `CreateToolhelp32Snapshot` by parent PID |
| Using `LoadLibrary` for `RoGetActivationFactory` | Fragile, linker issues | Use `Win32_System_WinRT` feature |
| Trusting `S_OK` from slot 24 | May be a no-op if device ID format is wrong | Verify with correct capture GUID |
| Only scanning once for WebView2 children | Late-spawning children get no override | Poll for 30 seconds |

---

## Reference Implementation

See [`F:\CATTbyCatt Final\src-tauri\src\audio_policy.rs`](file:///F:/CATTbyCatt%20Final/src-tauri/src/audio_policy.rs) for the complete, production-proven implementation (~400 lines) including config persistence, startup application, runtime switching, and the watcher thread.

**Source of the device ID format:** [EarTrumpet's `AudioPolicyConfigService.cs`](https://github.com/File-New-Project/EarTrumpet/blob/master/EarTrumpet/DataModel/WindowsAudio/Internal/AudioPolicyConfigService.cs)
