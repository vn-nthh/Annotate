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
  checks 22 interactions. It exercises the real Win32 controls and message
  handlers with a unique isolated profile. This includes keyboard input, edits
  saved before closing, tray reopening, download/retry states, long subtitles
  and summaries, prerequisites, PNG/JPEG profile photos, and 150% DPI scaling.
  Scrolling checks background erasure in both themes and preserves a focused
  edit's text and selection. During-rebuild checks verify the window stays
  visible as an input target, including fractional wheel deltas over controls.
  Native titlebar hit testing and button clicks verify that X opens the close
  dialog and minimize minimizes the window. The close dialog consumes wheel
  input without scrolling its background. Sidebar hover checks delayed sibling
  mouse-leave events against the rendered highlight.
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

The scrolling/hover repaint checks failed before their fixes and pass afterward.
Controls are configured while hidden and shown after the complete cached frame
is ready; background erasure uses that same frame. The parent stays visible
throughout rebuilding. The previous
[WM_SETREDRAW](https://learn.microsoft.com/en-us/windows/win32/gdi/wm-setredraw)
path temporarily removed WS_VISIBLE, which can allow Windows to target the
window underneath; the regression
observed a hidden input target in 150 of 156 rebuild samples before the fix.
Ordinary hover updates coalesce redraws. A mouse-leave event only clears the
highlight belonging to its own control. The titlebar controls are exempt from
the scrolling viewport filter, which previously removed both button HWNDs
while leaving their icons painted. All 58 captured screens remain
pixel-identical to the native migration build before these interaction fixes.

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

The native migration baseline's measurements are recorded in
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
The executable hash in the report identifies the build before the scrolling,
sidebar-hover, and titlebar interaction fixes; these timings were not
remeasured afterward.

The extracted portable ZIP is checked by `scripts/test-native-package.ps1`.
It runs the packaged UI capture and Vulkan worker independently of the checkout,
and rejects browser assets, raw fonts, CUDA binaries, and missing runtime files.
The interaction-fix executable passed all 22 interactions and produced 58 PNGs,
including the two settings scroll snapshots. Its SHA-256 is:
`DD2EEDCBECADAB9AFAB5F35716EBFFC9604A027F3019C1E8D8ED520A8C80EA4B`.
The Vulkan worker is unchanged from the tested build above.
The updated portable ZIP also passed all 22 checks in isolation and the worker
reported Vulkan. Archive SHA-256:
`F52FC7E35D5AFFAC44A1D1D8B2048985601E5D79915A82FF58DF7711F6088B2C`.

Live Groq/Azure/OpenRouter requests, Google sign-in/sync against a real account,
and live microphone recording were not exercised. Their preserved backend
flows and relevant deterministic behavior were checked in code and Rust tests.

## Setup guide and resumable model download

Validated on Windows 11 x64, 12 October 2026, for v0.3.0.

New installs open a six-step setup guide: engine, model or API key, microphone,
hotkey, a live "Try it" step, and a feature overview. Profiles with a saved
hotkey, including upgrades from the Tauri build, skip it. Exiting keeps the step
in `settings.json`; the sidebar and Settings offer to resume it. The engine only
switches once it can transcribe. The local model download can be paused and
resumes from its partial file with an HTTP range request, still verified by
SHA-256; a stale partial restarts once.

- All 47 Rust tests pass, including resumed, stale, range-ignoring, complete,
  and stalled-then-cancelled downloads against a local HTTP server, and the
  setup state rules.
- `--capture-ui` renders 76 screen states and passes 31 interaction checks,
  nine of them for the guide: first-run display, waiting for the model, download
  progress, pause and resume, exiting and resuming at the same step, the engine
  switch, hotkey gating, a successful try, finishing into a feature, and pausing
  from Settings. The checks never save API keys, because those would reach the
  live Credential Manager.
- Home, Dictionary, Subtitles, and the close dialog remain pixel-identical to
  the previous build in both themes. Settings differs only by its new Setup
  Guide section and the resulting scrollbar thumb.
- Reference scenes are now parsed when first shown instead of all at window
  creation. With three runs each on a Ryzen 5 5600G, the median open-window
  working set fell from 43.26 to 38.29 MiB (private 25.27 to 16.88 MiB), tray
  from 32.27 to 31.83 MiB, and process start to rendered UI from 114 to 105 ms.

Not exercised: a real Groq or Azure key in the guide, live microphone input in
"Try it", and a full download from the model host. The host was confirmed to
answer range requests with `206 Partial Content`.
