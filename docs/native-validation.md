# Native Windows validation

Validated on Windows 11 x64, 12 October 2026, on branch
`codex/native-runtime-ui-parity`.

The settings window uses Win32 controls, Direct2D, and DirectWrite; recording
uses WASAPI and the overlay uses Windows Composition. Closing to the tray
destroys the settings window and its drawing resources. The package contains
no browser runtime or frontend assets. Local Whisper uses the Vulkan worker.

## Behavior and UI

- All 34 Rust tests pass, covering settings rollback, Unicode output, audio
  capture formatting and cancellation, shortcut parsing, OAuth/PKCE, sync races,
  legacy browser-data migration, subtitle timestamps, and worker cleanup.
- The app-owned `--capture-ui <directory>` harness renders 56 screen states and
  checks 16 interactions. It exercises the real Win32 controls and message
  handlers with a unique isolated profile. This includes keyboard input, edits
  saved before closing, tray reopening, download/retry states, long subtitles
  and summaries, prerequisites, PNG/JPEG profile photos, and 150% DPI scaling.
- Light/dark layouts, fonts, icons, copy, and modal geometry are retained from
  the HTML/CSS reference. Browser transcription and CUDA options are removed.
  Native text antialiasing and focus indicators can differ slightly.
- Existing dictionary/history records and tombstones migrate through a
  read-only copy of the old browser database; settings and credential names
  remain compatible.

Regenerate the reference with `scripts/capture-native-reference.py`; capture
the native build with `annotate.exe --capture-ui <directory>`. The capture
produces PNG screens, `controls.json`, and the passed checks in `checks.json`.
No real credentials, microphone, clipboard, or cloud account are used by the
capture harness.

## Offline transcription

A clean worker build passed. `scripts/test-vulkan-transcription.ps1` then
transcribed synthetic offline SAPI speech through the existing
`ggml-large-v3-turbo-q5_0.bin` model. The transcript matched both input sentences;
two valid segments covered 0–2.54 and 2.54–6.86 seconds. Worker logs confirmed
Vulkan0 on an NVIDIA RTX 3060 Ti. Model loading took 1.278 seconds, transcription
2.212 seconds, and segmented transcription 1.175 seconds. The worker exited
with code 0.

Tested worker SHA-256:
`F5C44A5A22EEEB8870D481E627891842F26C7E8618E620AF39B744C65DEBAC26`.

## Performance and package

The final executable's measurements are recorded in
[`native-benchmark.json`](native-benchmark.json). Median working set was
39.11 MiB with the native window open and 31.00 MiB in the tray, compared with
364.90 and 364.63 MiB for Tauri: reductions of 89.28% and 91.50%. Startup from
process creation to rendered UI fell from 596 to 95 ms (6.27 times faster).
The native runtime used one process; Tauri used seven. The benchmark uses three
interleaved measured runs after warmup, with warm filesystem caches and isolated
profiles. Both versions use the same offline fonts and empty Home screen, with
credentials, microphone, models, hotkeys, and network bootstrap disabled.

Startup is OS process creation to the first rendered native frame, compared with
DOM/font readiness and two animation frames for the isolated Tauri baseline.
Memory is sampled after settling and includes all owned processes. Working-set
sums include shared pages; private bytes measure committed private memory.
These measurements describe the idle UI/runtime, not inference with models
loaded or cold-cache launches.

The extracted portable ZIP is checked by `scripts/test-native-package.ps1`.
It runs the packaged UI capture and Vulkan worker independently of the checkout,
and rejects browser assets, raw fonts, CUDA binaries, and missing runtime files.
The final ZIP passed all 16 interactions and produced 58 PNGs, including the two
settings scroll snapshots. Its app/worker hashes match the measured/tested
executables. Archive SHA-256:
`4AEC12FE68FEC632F264895147291039E329468E0297FE759CC93F05288311A0`.

Live Groq/Azure/OpenRouter requests, Google sign-in/sync against a real account,
and live microphone recording were not exercised. Their preserved backend
flows and relevant deterministic behavior were checked in code and Rust tests.
