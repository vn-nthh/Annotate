<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="src/annotate_logowithtext_white.svg" />
    <source media="(prefers-color-scheme: light)" srcset="src/annotate_logowithtext_solid.svg" />
    <img src="src/annotate_logowithtext_solid.svg" alt="Annotate" width="200" />
  </picture>
</p>

Push-to-talk voice transcription for Windows. Hold a hotkey, speak, and release;
Annotate pastes the transcription into the active application.

## Features

- Groq Whisper, Azure MAI-Transcribe, and offline local Whisper transcription.
- Vulkan GPU acceleration for local Whisper on supported AMD, Intel, and NVIDIA GPUs.
- Custom dictionary, transcription history, and Google Drive sync.
- Optional on-device grammar cleanup using a GECtor ONNX model.
- Subtitle generation from audio/video with VAD, and OpenRouter subtitle summaries.
- Light/dark settings UI, native recording overlay, global hotkeys, and system tray.
- API keys and Google refresh tokens stored in Windows Credential Manager.

The app uses Win32, Direct2D, DirectWrite, Windows Composition, and WASAPI. The
settings window releases its controls and rendering resources when closed to the
tray. Whisper and grammar models load when requested. Browser transcription and
CUDA are no longer available; older saved selections migrate to Groq and Vulkan.

## Download

Download the Windows x64 ZIP from [Releases](https://github.com/vn-nthh/Annotate/releases),
extract the whole folder, and run `annotate.exe`. Keep `DirectML.dll` and
`whisper-worker-vulkan.exe` beside it. Models download from Settings on demand.

Requirements are Windows 10/11 x64 and the
[Microsoft Visual C++ 2015–2022 x64 Redistributable](https://aka.ms/vc14/vc_redist.x64.exe).
Local Whisper additionally requires a GPU driver with Vulkan support. The Vulkan
SDK is a build dependency only. The app does not require WebView2 or Node.js.

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

# Build a portable release ZIP under dist/.
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
are development references for visual comparisons. `scripts/capture-native-reference.py`
uses them to regenerate embedded drawing data; it requires Playwright and Chromium.
The optional npm commands are shortcuts for the PowerShell build scripts. Browser
assets and Python tools do not ship in the portable package.

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

## License

MIT. Satoshi is covered by the included ITF Free Font License.
