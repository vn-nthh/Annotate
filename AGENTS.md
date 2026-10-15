# AGENTS.md

Instructions for coding agents working on Annotate's native app.
Read this before changing code.
README.md covers building and packaging; `docs/native-validation.md` records
GPUI verification and the historical Win32 build (v0.3.0).

## What this app is

Annotate is a push-to-talk transcription app: hold a hotkey, speak, release,
and the text is pasted into the active application. It is one native process.
The UI is built with GPUI 0.2 (Zed's GPU-accelerated framework); operating-system
services sit behind `src-tauri/src/platform`, so the UI and app core can be
shared with a macOS port. Windows is the only implemented platform today.

- `src/ui/`: GPUI views (window, panels, setup guide, recording overlay,
  text field, controls) and the reference-capture mode.
- `src/platform/windows/`: global shortcuts and keyboard hook, tray icon,
  single instance, clipboard and synthetic paste/copy, file dialogs, and the
  overlay's click-through window styles, WASAPI capture, locale collation,
  legacy profile access, and child-process lifetime.
- `src/native_core.rs`: app state and actions; `native_data`, `native_sync`,
  `native_migrate`, `native_onboarding`: storage, Drive sync, Tauri data import,
  setup-guide state. These are UI-independent.
- `src/native_audio.rs`: portable PCM level, speech detection, and WAV encoding.

The legacy Tauri app is the behavioral baseline; the Win32 app (tag v0.3.0) is
the most recent reference implementation of the same behavior.

## Priorities, in order

1. **Parity with the Tauri/Win32 app**, both behavior and appearance.
2. **Native correctness**: keyboard, IME, focus, DPI scaling, and input
   targeting behave as desktop controls should.
3. **Small memory and CPU footprint.** GPU rendering has a fixed cost (creating
   the Direct3D device alone is ~270 ms and ~48 MiB private on an AMD APU);
   the user accepted this in exchange for GPUI. Do not add cost beyond it.
   Current release numbers (`scripts/benchmark-native.ps1`, Ryzen 5 5600G):
   ~523 ms from process creation to the first view render (before GPU
   presentation), 59.33 MiB working set / 123.18 MiB private with the window
   open, 59.26 / 76.63 MiB in the tray. Results are in
   `docs/gpui-benchmark.json`; the Win32 baseline is in
   `docs/native-benchmark.json`.

### Footprint rules

- Load lazily. Whisper, GECtor, and other models load when the feature is first
  used (for example on hotkey press), never at startup.
- Closing to the tray removes the settings window and its views. Keep it that
  way; do not keep hidden windows or cache views.
- Do not add resident threads, processes, timers, or polling loops. Work runs
  on GPUI's main thread, the shortcut thread (`annotate-shortcuts`), or the
  two-worker tokio runtime. Animation requests frames only while visible.
- Avoid cloning whole app state (`State`) on hot paths such as painting,
  hover, scrolling, or audio callbacks. UI refreshes are coalesced
  (`ui::refresh`); the mic level reaches the overlay through an atomic.
- Do not add dependencies casually. Each new crate must justify its size.

### Intentional removals (not parity gaps)

- **CUDA** is removed; local Whisper uses the Vulkan worker only.
- **Web Speech / browser transcription** is removed; there is no browser engine.
- There is no WebView2, Node.js, or frontend runtime. Never reintroduce one,
  even for a single screen.
- GPUI has no backdrop blur or letter-spacing; the close prompt's scrim is not
  blurred and uppercase labels are untracked.

Older saved settings that select removed options migrate to Groq and Vulkan.

## Design specification

The visual design is **"Paperlike Monochromatic"**: warm off-white paper, ink-
grey text, hairline rules, no accent hues except a muted red for danger. It is
calm, typographic, and quiet. New UI must look like it was always part of it.

### Source of truth

- `src/index.html` and `src/index.css` are the **design reference**. They are not
  shipped or loaded at runtime.
- `scripts/capture-native-reference.py --output <dir>` renders them in headless
  Chromium to one PNG per screen state.
- `annotate.exe --capture-ui <dir>` renders the GPUI app with the same fixtures
  and file names, off-screen, without stealing focus.

To change layout, copy, spacing, or styling, change the HTML/CSS reference and
the GPUI view together, then compare the two capture sets. GPUI layout is
flexbox (Taffy), so CSS rules translate directly. Two details matter:

- Body text inherits `line-height: 1.5` (set on the root as a relative value).
  Chromium gives buttons `line-height: normal` (1.35 for Satoshi) and selects
  and inputs a 15 px line at 0.9rem; components follow those measurements.
- Bare `<hr>` dividers in the scrolling Settings panel collapse to zero height
  in Chromium; only the dividers inside the API key and local model sections
  render, and the GPUI Settings view matches that.

### Tokens

Use these names; in Rust they live in `Theme` in `src/ui/theme.rs`, and in CSS
as custom properties. Never introduce an ad-hoc hex value without adding it to
both.

| Token (CSS / Rust)            | Light     | Dark      | Use                              |
| ----------------------------- | --------- | --------- | -------------------------------- |
| `--paper` / `paper`           | `#faf9f7` | `#1a1a1e` | Content background, cards        |
| `--paper-warm` / `warm`       | `#f5f3ef` | `#222226` | Inputs, secondary buttons        |
| `--paper-hover` / `hover`     | `#ffffff` | `#2a2a2e` | Hovered surfaces                 |
| `--ink` / `ink`               | `#2c2c2c` | `#e8e6e1` | Primary text, primary buttons    |
| `--ink-light` / `light`       | `#6b6b6b` | `#a0a0a0` | Descriptions, secondary text     |
| `--ink-faint` / `faint`       | `#a0a09c` | `#6b6b6b` | Hints, placeholders, idle icons  |
| `--rule` / `rule`             | `#e8e6e1` | `#333338` | Borders, dividers                |
| `--rule-hover` / `rule_hover` | `#d4d1cb` | `#444448` | Hovered borders                  |
| `--sidebar-bg` / `sidebar`    | `#f0eee9` | `#16161a` | Titlebar and sidebar             |
| `--danger` / `danger`         | `#8b4444` | `#c06060` | Destructive actions, errors only |

- Colour is monochrome. Hierarchy comes from ink weight (`ink` → `light` →
  `faint`), not hue. The only exceptions are `danger` and the dark-mode
  active status dot (`#90cdf4`). Do not add blues, greens, or brand accents.
- Every change must work in **both light and dark themes**. Check both.
- Shadows are soft and rare (`0 2px 8px rgba(0,0,0,.05)` lift, modal
  `0 8px 32px rgba(0,0,0,.12)`). Avoid heavy elevation.

### Typography

- Font: **Satoshi** (weights 300/400/500/700, embedded; see
  `src-tauri/assets/fonts/`). Do not substitute Segoe UI or system fonts.
- Panel title: 1.2rem, weight 600. Body and descriptions: 0.82rem,
  `ink-light`, line height 1.5. Hints: 0.76rem, `ink-faint`. Sidebar tab
  labels: 0.65rem, weight 500. Buttons: 0.82rem, weight 600.
- `rem` is 14 px (`theme::rem`).
- Copy is short, sentence case, and plain. No exclamation marks, emoji, or
  marketing tone. Error text says what failed and what to do next.

### Shape and layout

- Window: 520 × 580 logical px, custom titlebar on `sidebar`, 68 px sidebar of
  vertical tabs, content panel with a 14 px top-left radius.
- Radii: 6 px (small controls, titlebar buttons), 10 px (buttons, tabs, inputs),
  14 px (cards, modals, content panel). Overlay pill: 16 px.
- Panel padding 24/24/16 px; 16 px between header, description, and groups;
  8 px gaps within groups. Stay on this 4 px grid.
- Active tab: `paper` background with a 3 × 20 px `ink` bar on its left edge.
- Modals: centered 300 px card on a 30% black (55% dark) scrim. Primary action
  is an `ink` button; secondary is `warm` with a 1.5 px `rule` border; cancel is
  a text link in `faint`. Focus stays inside the modal while it is open.
- All geometry is in logical pixels and scales with per-monitor DPI.
  Verify at 100% and 150%.

### Motion

- Transitions are short (150–300 ms). Avoid long or decorative animation.
- Animation must not run when nothing is visible; idle CPU should be zero.

### Recording overlay

The overlay (`src/ui/overlay.rs`, window styles in
`src/platform/windows/overlay.rs`) is a borderless, click-through,
non-activating topmost GPUI window: a near-black (`#121212`) pill, 32 px tall,
60 px above the bottom of the screen, with four 6 px white dots at 85% opacity.
It must never take focus from the target application. Show, hide, and move it
by posting to its window (never call `SetWindowPos` from inside GPUI code: the
resulting `WM_SIZE` arrives while the app is borrowed and GPUI drops it).

### Native behavior requirements

- GPUI draws every control. Focusable controls use `tab_index` and the shared
  focus ring (`components::focusable`); Enter and Space activate buttons; Tab
  and Shift+Tab move focus; Escape closes popups, the close prompt, and hotkey
  recording.
- Text fields (`src/ui/text_input.rs`) implement GPUI's input handler for IME.
  Never slice strings at offsets that did not come from char boundaries.
- Every interactive element needs a hover state, a visible keyboard focus
  indicator, and a tooltip where its purpose is not obvious from text.
- GPUI panics if an element sets `.hover()` twice; give variants their own
  `components::Kind` instead of overriding a button's hover.
- Unsaved edits commit when a field loses focus, before tab switches, closing,
  or hiding to the tray. Rejected values stay as drafts, per engine for API keys.
- Hotkeys are recorded by virtual key (the keyboard hook's last key), not by
  the layout character GPUI reports, so they match `RegisterHotKey`.
- Modal file dialogs run off the UI thread with the window as owner.

## Code conventions

- Rust 2021 edition. Run `cargo fmt`. Keep `unsafe` blocks tight around
  Win32 calls, inside `src/platform`.
- Match the surrounding code: short names, sparse comments that explain *why*,
  no speculative abstractions.
- Anything OS-specific belongs in `src/platform/<os>/` behind the functions
  `src/platform/mod.rs` re-exports; UI and core code must not call Win32.
- Secrets (API keys, Google refresh tokens) live in Windows Credential Manager,
  never in settings files or logs. `src/oauth.config.json` is gitignored.
- Settings stay in `%APPDATA%\com.annotate.app` and data/models in
  `%LOCALAPPDATA%\com.annotate.app`. Keep setting names and credential names
  compatible with the Tauri app so upgrades are seamless.
- Commit messages use Conventional Commits with a scope:
  `fix(ui): ...`, `fix(native): ...`, `perf(whisper): ...`, `feat(overlay): ...`.

## Verifying changes

Run what applies before reporting work as done:

```powershell
# Rust tests
cargo test --package annotate --locked --manifest-path src-tauri/Cargo.toml

# Build (add -ReuseWorker for UI-only changes)
./scripts/build-native.ps1

# Render every screen state, both themes, and the Chromium reference
<path to annotate.exe> --capture-ui <directory outside the repo>
python scripts/capture-native-reference.py --output <another directory>
python scripts/capture-native-reference.py --scale 1.5 --output <another directory>/150
```

For any UI change, inspect the affected PNGs in light and dark and compare them
with the Chromium reference of the same name. Text anti-aliasing differs
(DirectWrite vs. Chromium); layout and colour should not. The setup-guide
reference is static, so it shows Back and hints that the app hides on step 1.
The native capture includes eight representative 150% scenes under `150/`,
using simulated DPI-change messages, plus `checks.json`. It exits nonzero on a
failed capture or interaction check.

For changes that could affect footprint or startup, run
`scripts/benchmark-native.ps1 -NativeExecutable <path>` and compare against the
numbers above. For packaging changes, run `scripts/test-native-package.ps1`.

Report results honestly: say which checks ran, which did not (live Groq/Azure/
OpenRouter calls, real Google sign-in, and live microphone input are not
covered by the harness), and attach numbers when footprint is involved.
