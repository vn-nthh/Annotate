<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="src/annotate_logowithtext_white.svg" />
    <source media="(prefers-color-scheme: light)" srcset="src/annotate_logowithtext_solid.svg" />
    <img src="src/annotate_logowithtext_solid.svg" alt="Annotate" width="200" />
  </picture>
</p>

Push-to-talk voice transcription for Windows, with an Apple silicon macOS preview.
Hold a hotkey, speak, and release;
Annotate pastes the transcription into the active application.

## Features

- Groq Whisper, Azure MAI-Transcribe, and offline local Whisper transcription.
- Vulkan GPU acceleration for local Whisper on supported AMD, Intel, and NVIDIA GPUs.
- Metal acceleration for local Whisper in the Apple silicon preview.
- Custom dictionary, transcription history, and Google Drive sync.
- Optional on-device grammar cleanup using a GECtor ONNX model.
- Subtitle generation from audio/video with VAD, and OpenRouter subtitle summaries.
- Light/dark settings UI, native recording overlay, global hotkeys, and system tray.
- A first-run setup guide for the engine, model or API key, microphone, and hotkey.
  It can be exited at any step and resumed later from the sidebar or Settings.
- The local model download can be paused and resumes where it stopped.
- API keys and Google refresh tokens stored in Windows Credential Manager.

The interface is built with [GPUI](https://gpui.rs), Zed's GPU-accelerated UI
framework, which renders through Direct3D 11 on Windows and Metal on macOS.
Global shortcuts, the tray/menu bar, synthetic input, and audio capture use
platform services: Win32/WASAPI on Windows and AppKit/Carbon/CoreAudio on macOS.
The settings window is
released when closed to the tray. Whisper and grammar models load when
requested. Browser transcription and CUDA are no longer available; older saved
selections migrate to Groq and Vulkan.

## Download

Download the Windows x64 setup from [Releases](https://github.com/vn-nthh/Annotate/releases)
and run it. It installs per user without administrator rights and upgrades an
existing installation in place, keeping settings, dictionary, history and models.
If you installed an older release with the `.msi`, remove that entry from
Settings > Apps afterwards. A portable ZIP is also available: extract the whole
folder and run `annotate.exe`, keeping `DirectML.dll` and
`whisper-worker-vulkan.exe` beside it. On first launch, the setup guide helps you
pick an engine and get its model or API key; models also download from Settings.

Requirements are Windows 10/11 x64, a Direct3D 11 capable graphics driver, and the
[Microsoft Visual C++ 2015–2022 x64 Redistributable](https://aka.ms/vc14/vc_redist.x64.exe).
Local Whisper additionally requires a GPU driver with Vulkan support. The Vulkan
SDK is a build dependency only. The app does not require WebView2 or Node.js.

### Apple silicon preview

The [macOS preview](https://github.com/vn-nthh/Annotate/releases/tag/v0.5.0-macos-preview.1)
targets Apple silicon (arm64) and macOS 13 or later. Open the DMG and drag
Annotate into Applications, or extract the ZIP. Intel Macs are not included.

The app is ad-hoc signed and is not notarized. If Gatekeeper blocks this preview,
use System Settings > Privacy & Security > Open Anyway after attempting to
open it. Allow Microphone access when requested. Pasting and dictionary capture
also need Annotate enabled under Privacy & Security > Accessibility.

Choose a shortcut with a letter, number, or function key; Caps Lock is not
supported for hold-to-talk in this preview. Command+C and Command+V are reserved
for clipboard operations. The menu-bar icon reopens settings or quits the app;
Command+Q also quits and Command+, opens Settings.

CI checks the arm64 build, Rust tests, app bundle, code signature, command-line
startup, and Metal worker protocol/lifetime. The hosted runner has no Metal
device, so it cannot render GPUI or validate GPU inference. Microphone, hotkeys,
paste, Keychain prompts, real Google sign-in, and multi-monitor behavior still
need hands-on testing on a Mac. This is a preview, not a stable macOS release.

## Build from source

Install [Rust](https://rustup.rs/),
[Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/),
[CMake](https://cmake.org/download/), and the
[Vulkan SDK](https://vulkan.lunarg.com/). Set `VULKAN_SDK` to the installed SDK.

Run these commands in PowerShell from the repository root:

```powershell
# Build the native executable and the Vulkan worker.
./scripts/build-native.ps1

# Build and run a debug executable.
./scripts/build-native.ps1 -Configuration Debug -Run

# Build the portable ZIP and the setup .exe under dist/ (requires NSIS).
./scripts/package-native.ps1

# Check Rust behavior.
cargo test --package annotate --locked --manifest-path src-tauri/Cargo.toml
```

The build fetches unmodified Satoshi fonts directly from Fontshare and verifies
their hashes before embedding them. Font files are ignored by Git; their license
and provenance are in `src-tauri/assets/fonts/`. Downloaded ONNX Runtime binaries
are verified by the `ort` build dependency; `DirectML.dll` is copied into the
build output automatically.

The worker builds into a short cache at the root of the checkout's drive, such
as `C:\aw-target`, to avoid MSVC file-tracker failures in long paths. Set
`WHISPER_WORKER_TARGET_DIR` to choose a different short, writable path.
`CARGO_TARGET_DIR` controls the native app's output independently. For repeated
UI builds, pass `-ReuseWorker` to reuse an existing Vulkan worker.
For packaging an already built release, use `package-native.ps1 -SkipBuild`.

`scripts/benchmark-native.ps1 -NativeExecutable <path to annotate.exe>` measures
startup and process memory with an isolated test profile. Its JSON report records
the measurement conditions and executable hashes.
The migration's UI checks, offline inference test, and performance results are
documented in [native validation](docs/native-validation.md).

The executable, worker, and DLL are the runtime. The retained HTML/CSS and artwork
are the design reference. `annotate.exe --capture-ui <directory>` renders every
screen state, in both themes, to PNGs off-screen, and
`scripts/capture-native-reference.py --output <directory>` renders the HTML/CSS
reference in Chromium to identically named PNGs for comparison (it requires
Playwright and Chromium). The native capture also writes `checks.json` and
eight representative 150% DPI scenes under `150/`. Render the matching reference
with `--scale 1.5 --output <directory>/150`.
The optional npm commands are shortcuts for the PowerShell build scripts. Browser
assets and Python tools do not ship in the portable package.

On an Apple silicon Mac, install the Xcode command-line tools, Rust, CMake, and
Python 3, then run:

```sh
bash scripts/build-macos.sh
cargo test --package annotate --locked --manifest-path src-tauri/Cargo.toml --target aarch64-apple-darwin
python3 scripts/package-macos.py
python3 scripts/test-macos-package.py dist/Annotate-*-macos-arm64.zip
```

The macOS build uses Metal rather than the Vulkan SDK. Packaging writes an
ad-hoc signed app in a ZIP and DMG under `dist/`; Developer ID signing and
notarization are not configured.

## Google Drive sync for local builds

Create a Google OAuth client with the **Desktop app** application type, then
create `src/oauth.config.json`:

```json
{
  "client_id": "YOUR_GOOGLE_DESKTOP_CLIENT_ID.apps.googleusercontent.com"
}
```

Desktop client IDs are public configuration. Annotate uses PKCE and keeps the
refresh token in Windows Credential Manager. No client secret is needed. See
`src/oauth.config.json.example` for reference. Without this optional file, the
remaining app features work normally and sync reports that it is unconfigured.

## Data and upgrades

Settings remain in `%APPDATA%\com.annotate.app`; models and native dictionary and
history data remain in `%LOCALAPPDATA%\com.annotate.app`. Existing dictionary and
history data are imported from the previous browser profile without opening a
browser. Replacing the extracted app folder preserves the data in your profile.
The macOS preview stores settings and data in
`~/Library/Application Support/com.annotate.app`, with secrets in Keychain.

## License

MIT. Satoshi is covered by the included ITF Free Font License.
