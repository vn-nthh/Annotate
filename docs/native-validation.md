# Native Windows validation

## GPUI migration

Validated on Windows 11 x64, 16 October 2026, on branch `feat/gpui-native`.
The settings window, setup guide, controls, text input, and recording overlay
are drawn by GPUI 0.2.2. Shortcuts, tray, synthetic input, dialogs, overlay window
styles, WASAPI capture, locale collation, legacy profile access, and child-process
lifetime are behind `src/platform/windows`. Windows remains the only
implemented platform; macOS still needs its OS services, audio capture,
permissions, model backend, and packaging.

### Build and behavior

- `cargo test --package annotate --locked --manifest-path src-tauri/Cargo.toml`
  passes all 52 tests, including UTF-16 IME composition and masked Unicode
  offset checks. `cargo fmt --check` and the release build with `-ReuseWorker`
  pass without Rust warnings. The existing Vulkan worker is reused and its
  protocol is verified by the build script.
- The release `--capture-ui` harness renders 80 scenes at 100% and eight
  representative light/dark scenes at 150%. The latter use a simulated
  `WM_DPICHANGED` on the capture window, checking 520 x 580 logical and
  780 x 870 physical pixels before restoring the original scale. This does
  not test dragging between real monitors or changing desktop settings. Eight
  additional large-transcript captures cover the preview's top and bottom in
  both themes at both scales, under `stress/`.
- All 33 harness checks pass. They cover engine migration, titlebar and native
  Windows close requests (the Alt+F4/taskbar Close path), titlebar mouse-down
  preserving the native drag action,
  modal focus trapping and keyboard activation, dropdown keys, wheel input,
  shortcut cancellation and key release, field padding hit testing, real
  dictionary typing, edits saved on blur, setup gating/resume/completion,
  simulated recording status, long summaries, and tray teardown/reopening.
  The real overlay HWND is checked during queued show/confirmation/hide for
  visibility, borderless popup and topmost/tool-window/transparent/nonactivating
  styles, matching window/client bounds, `HTTRANSPARENT`, `MA_NOACTIVATE`, and an unchanged foreground HWND. Text checks
  include padding caret placement, drags beyond the field, and UTF-16
  preedit/update/commit/replacement of combining marks, CJK, and surrogate pairs
  through the live GPUI input handler. A click after text replacement but before
  repaint reproduced a byte-index panic before the fix; the regression check
  now verifies valid caret positions for both shorter ASCII and multibyte text.
  Shortcut completion and recording status use fixtures; no real shortcut is
  registered and no microphone or clipboard is used.
- The capture exits nonzero on a failed render or check, rejects blank black
  frames, and writes passed descriptions to `checks.json`. The old Win32
  `controls.json` is no longer produced or required.
- Chromium reference captures were regenerated at both scales. Light/dark
  comparisons confirm the affected layouts and reachable subtitle actions.
  Font rasterization, border rounding, and some font metrics still differ;
  the captures are not pixel-identical. GPUI also omits backdrop blur and
  letter spacing. The static setup reference retains its initial step count,
  Back button, and hint while the app reflects the current setup step.
- Settings and Subtitles borrow the view's state during painting instead of
  copying history, dictionary, and subtitle results on every hover or scroll.
  The subtitle preview prepares its text only when entries change, retains one
  wrapped layout, caches bounded measurement sizes, and paints visible lines.
  The 3,000-entry stress check scrolls through 14,999 source lines without
  rebuilding the layout on repaint, wheel input, or jumping to the last entry.
  It records visible ranges and layout counts in `stress/metrics.json`.
  Only 18-19 source lines were submitted for painting in the measured viewports;
  these work counts are retained in `gpui-benchmark.json`, not frame timings.
- A second independent review used GPT-6 Astra with maximum reasoning and
  Fast service. Its findings led to three keyboard/lifetime fixes: Tab and
  Shift+Tab scroll focused controls fully into view, including input padding
  and borders; Caps Lock reaches shortcut recording through GPUI's modifier
  event; and both tray-close paths cancel shortcut recording before removing
  the window. The focus regression failed on the previous implementation.
  The harness now checks each focus step in both directions, posts a Caps Lock
  event with an isolated virtual-key fixture, and checks both close paths.
  It does not change the physical Caps Lock state or register a global hotkey.
  Astra's focused follow-up found no further actionable issues.
- Subtitle summaries use `openai/gpt-6-luna` through OpenRouter, with reasoning
  effort set to `none` to reserve the existing 512-token budget for the result.
  The response must finish with `stop` and contain a nonempty JSON `summary`
  string; truncated, filtered, malformed, and unstructured responses are
  rejected. Unit tests cover those cases. OpenRouter's public catalog and
  provider documentation were checked; no authenticated failure or successful
  live summary request was reproduced.
- The recording overlay explicitly uses `WS_POPUP` without caption/resize
  styles and overrides `WM_NCCALCSIZE`, since GPUI otherwise reserves a native
  frame around its transparent canvas. Placement recalculates that frame from
  its queued window message; showing and confirming remain nonactivating.
  A native full-window capture shows only the pill and its dots. Windows reports
  no non-client rendering, with both outer and client bounds at 260 x 32 pixels.
- Automatic focus handling belongs to the content area, leaving the custom
  titlebar outside it. A titlebar mouse-down must preserve the native default
  action so Windows can move the window; controls and keyboard focus remain
  handled by GPUI. A real held-button drag of the running release moved the
  window by the expected 80 x 40 physical pixels; its original position and
  the pointer were restored afterward.

Regenerate the screenshots outside the checkout:

```powershell
<path to annotate.exe> --capture-ui <native-directory>
python scripts/capture-native-reference.py --output <reference-directory>
python scripts/capture-native-reference.py --scale 1.5 --output <reference-directory>/150
```

### Performance

[`gpui-benchmark.json`](gpui-benchmark.json) records three measured runs after
warmup on a Ryzen 5 5600G. Median process creation to first view render was
518 ms (496 ms from the app's own timing origin). The GPUI timing hook runs
at entry into the first render, before GPU presentation; it is not a measure
of a completed frame on the display. The benchmark metadata states this.
This benchmark includes the titlebar and both reviews' fixes at `22ca735`;
its measured executable hash is retained in the JSON.

Median open-window working set was 59.25 MiB, with 125.45 MiB private bytes.
In the tray it was 59.17 MiB / 76.14 MiB, with one process in both phases.
The previous recorded build measured 523 ms, 59.33 / 123.18 MiB open, and
59.26 / 76.63 MiB in the tray; this run used 2.27 MiB more private memory
with the window open and 0.49 MiB less in the tray.
Closing to the tray removes the settings window and its views; GPUI's shared
GPU device remains resident. Models are not loaded during the benchmark.
`ANNOTATE_UI_TEST` also skips device discovery, credentials, legacy migration,
and background artifact checks. These timings measure the UI/runtime, not
completion of the normal bootstrap.

The short benchmark samples measured median CPU use of 2.2% of one logical
core with the window open and 0% in the tray, including its polling thread.
A separate isolated normal launch of the same executable, without that
thread, measured CPU over ten seconds without input: 46.875 ms with the
window open and 15.625 ms in the tray, or 0.4682% and 0.1561% of one logical
core. These samples are recorded as `normalIdleValidation` in the same JSON;
they do not establish zero CPU use. Open-window CPU was similar to the
earlier pre-overlay-fix sample (0.468%); tray CPU was lower than that sample
(0.3122%). This probe used the normal shortcut/tray services
with bootstrap disabled and verified native close, destruction of the settings
window in the tray, reopening, and normal quit (exit code 0, no stderr).
The historical Win32/Tauri measurements below remain unchanged in
[`native-benchmark.json`](native-benchmark.json).

### Offline inference and package

`scripts/test-vulkan-transcription.ps1` passed using the existing local model
and synthetic SAPI speech, without opening a microphone. The transcript
matched both sentences; valid segments covered 0-2.54 and 2.54-6.86 seconds.
Logs confirmed Vulkan0 on an NVIDIA RTX 3060 Ti. Loading took 1.337 seconds,
transcription 0.603 seconds, and segmented transcription 0.542 seconds. The
worker exited with code 0.

`scripts/package-native.ps1 -SkipBuild` built the portable ZIP and NSIS setup.
`scripts/test-native-package.ps1` then extracted the ZIP outside the checkout,
rendered all 96 images, passed all 33 checks, verified the isolated Vulkan
worker, and rejected browser assets, raw fonts, CUDA, and missing runtime files.
An earlier package attempt logged a ZIP error while the build continued.
The archive command now requests terminating errors explicitly with
`Compress-Archive -ErrorAction Stop`; a locked-file check confirms it throws.
The installer was built but installation and uninstallation were not run.

Release executable SHA-256:
`DFEA36FFCC279A18C5A96548BD89418931DC35053E6CCC95854509BF316A9CE9`.
Vulkan worker SHA-256:
`939238541825C28A9DC23025C4D270D312CE29A104D55C3E99428BB10B13625F`.
Portable ZIP SHA-256:
`EAC7FBF1B06FBC0E94B32AED8FA5F02639AD72BCEAA83C0D7524E8EB4127FD91`.
Setup executable SHA-256:
`DCD1E14ACA1C1CF798989CCE7DF50627EE34B5F1DE09E0D25569DE8E636F7A99`.

Live Groq/Azure/OpenRouter calls, real Google sign-in/sync, live microphone
input and paste into another app, physical Caps Lock combinations and global
hotkey registration, a real IME session, and macOS were not exercised by this
validation.

## Historical Win32 validation

Validated on Windows 11 x64, 12 October 2026, on branch
`codex/native-runtime-ui-parity`.

The settings window uses Win32 controls, Direct2D, and DirectWrite; recording
uses WASAPI and the overlay uses Windows Composition. Closing to the tray
destroys the settings window and its drawing resources. The package contains
no browser runtime or frontend assets. Local Whisper uses the Vulkan worker.

### Behavior and UI

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

### Offline transcription

A clean worker build passed. `scripts/test-vulkan-transcription.ps1` then
transcribed synthetic offline SAPI speech through the existing
`ggml-large-v3-turbo-q5_0.bin` model. The transcript matched both input sentences;
two valid segments covered 0–2.54 and 2.54–6.86 seconds. Worker logs confirmed
Vulkan0 on an NVIDIA RTX 3060 Ti. Model loading took 1.278 seconds, transcription
2.212 seconds, and segmented transcription 1.175 seconds. The worker exited
with code 0.

Tested worker SHA-256:
`F5C44A5A22EEEB8870D481E627891842F26C7E8618E620AF39B744C65DEBAC26`.

### Performance and package

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

### Setup guide and resumable model download

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
