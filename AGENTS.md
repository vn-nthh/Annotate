# AGENTS.md

Instructions for coding agents working on Annotate's native Win32 app.
Read this before changing code.
README.md covers building and packaging; `docs/native-validation.md` records
how the current build was verified.

## What this app is

Annotate is a push-to-talk transcription app for Windows: hold a hotkey, speak,
release, and the text is pasted into the active application. This branch
replaces the Tauri/WebView2 app with a single native process built on Win32,
Direct2D, DirectWrite, Windows Composition, and WASAPI. The legacy Tauri app is
the behavioral baseline: feature parity with it is the bar for merging.

## Priorities, in order

1. **Smallest memory and CPU footprint.** The idle baseline is about 31 MiB in
   the tray and 39 MiB with the window open, in one process, with a 95 ms
   startup (`docs/native-benchmark.json`). Do not regress it.
2. **Parity with the Tauri app**, both behavior and appearance.
3. **Native correctness**: keyboard, IME, accessibility, DPI scaling, focus,
   and input targeting behave as Windows controls should.

When these conflict, footprint wins unless the user says otherwise.

### Footprint rules

- Load lazily. Whisper, GECtor, and other models load when the feature is first
  used (for example on hotkey press), never at startup.
- Closing to the tray destroys the settings window, its controls, and its
  Direct2D/DirectWrite resources. Keep it that way; do not cache them.
- Do not add resident threads, processes, timers, or polling loops. Prefer
  message-driven work on the existing UI thread (`annotate-native-ui`) and
  the overlay thread.
- Avoid cloning whole app state (`State`, settings maps) on hot paths such as
  painting, hover, scrolling, or audio callbacks.
- Do not add dependencies casually. Each new crate must justify its size.

### Intentional removals (not parity gaps)

- **CUDA** is removed; local Whisper uses the Vulkan worker only.
- **Web Speech / browser transcription** is removed; there is no browser engine.
- There is no WebView2, Node.js, or frontend runtime. Never reintroduce one,
  even for a single screen.

Older saved settings that select these options migrate to Groq and Vulkan.

## Design specification

The visual design is **"Paperlike Monochromatic"**: warm off-white paper, ink-
grey text, hairline rules, no accent hues except a muted red for danger. It is
calm, typographic, and quiet. New UI must look like it was always part of it.

### Source of truth

- `src/index.html` and `src/index.css` are the **design reference**. They are not
  shipped or loaded at runtime.
- `scripts/capture-native-reference.py` lays them out in headless Chromium and
  freezes every screen state into `src-tauri/assets/native-reference.json`
  (node boxes, colors, font sizes, weights, radii, icons) plus PNG artwork.
- `src-tauri/src/native_ui.rs` embeds that JSON and paints it with Direct2D/
  DirectWrite over real Win32 controls.

To change layout, copy, spacing, or styling, **edit the HTML/CSS reference and
regenerate the reference data**. Do not hand-edit `native-reference.json` or
hard-code new geometry in Rust when the reference can express it. Rust-only
drawing is acceptable for things CSS cannot freeze (animation, live progress,
hover/focus states driven by native messages), and must reuse the tokens below.

### Tokens

Use these names; in Rust they live in `color()` in `native_ui.rs`, and in CSS
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
| `--rule-hover` / `rule-hover` | `#d4d1cb` | `#444448` | Hovered borders                  |
| `--sidebar-bg`                | `#f0eee9` | `#16161a` | Titlebar and sidebar             |
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
- Panel title: 1.2rem, weight 600, letter-spacing −0.01em.
- Body and descriptions: 0.82rem, `ink-light`, line height 1.5.
- Hints: 0.76rem, `ink-faint`. Sidebar tab labels: 0.65rem, weight 500.
- Buttons: 0.82rem, weight 600. Uppercase labels use small positive tracking
  (0.04–0.06em).
- Copy is short, sentence case, and plain. No exclamation marks, emoji, or
  marketing tone. Error text says what failed and what to do next.

### Shape and layout

- Window: 520 × 580 logical px, custom titlebar on `--sidebar-bg`, 68 px
  sidebar of vertical tabs, content panel with a 14 px top-left radius.
- Radii: 6 px (small controls, titlebar buttons), 10 px (buttons, tabs, inputs),
  14 px (cards, modals, content panel). Overlay pill: 16 px.
- Panel padding 24/24/16 px; 16 px between header, description, and groups;
  8 px gaps within groups. Stay on this 4 px grid.
- Active tab: `paper` background with a 3 × 20 px `ink` bar on its left edge.
- Modals: centered 300 px card on a 30% black (55% dark) scrim with a 4 px
  backdrop blur. Primary action is an `ink` button; secondary is `warm` with a
  1.5 px `rule` border; cancel is a text link in `faint`.
- All geometry is in logical pixels and must scale with per-monitor DPI.
  Verify at 100% and 150%.

### Motion

- Transitions are short (150–300 ms) with `ease-out`
  (`cubic-bezier(0.22, 1, 0.36, 1)`); a slight overshoot is reserved for press
  feedback. Avoid long or decorative animation.
- Animation must not run when nothing is visible. Stop timers when the window
  is hidden or the animated element is gone; idle CPU should be zero.

### Recording overlay

The overlay (`src-tauri/src/throbber.rs`) is a borderless, click-through,
non-activating topmost layer drawn with Windows Composition: a near-black
(`#121212`) pill, 32 px tall, 60 px above the bottom of the screen, with four
6 px white dots at 85% opacity. It must never take focus from the target
application. Keep its visual language identical when changing behavior.

### Native behavior requirements

- Use real Win32 controls for anything focusable or editable, so keyboard
  navigation, IME, screen readers, and clipboard work. Direct2D paints around
  and behind them; it does not replace them.
- Every interactive element needs a hover state, a visible keyboard focus
  indicator, and a tooltip where its purpose is not obvious from text.
- Do not flicker. Configure controls while hidden, show them after the cached
  frame is ready, and never hide the parent window during a rebuild
  (`WM_SETREDRAW` removed `WS_VISIBLE` and let input fall through to windows
  underneath).
- Hover and scroll redraws coalesce. A mouse-leave clears only its own
  control's highlight.
- Unsaved edits commit before tab switches, closing, or hiding to the tray.

## Code conventions

- Rust 2021 edition. Run `cargo fmt`. Keep `unsafe` blocks tight around
  Win32 calls.
- Match the surrounding code: short names, sparse comments that explain *why*,
  no speculative abstractions. Native modules are prefixed `native_`.
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

# Render every screen state and run the interaction checks
<path to annotate.exe> --capture-ui <directory outside the repo>
```

`--capture-ui` writes a PNG per screen state (both themes), `controls.json`, and
`checks.json`. For any UI change, inspect the affected PNGs in light and dark,
confirm all checks still pass, and compare against the Chromium reference
screenshots from `capture-native-reference.py`. Unchanged screens should stay
pixel-identical.

For changes that could affect footprint or startup, run
`scripts/benchmark-native.ps1 -NativeExecutable <path>` and compare against
`docs/native-benchmark.json`. For packaging changes, run
`scripts/test-native-package.ps1`.

Report results honestly: say which checks ran, which did not (live Groq/Azure/
OpenRouter calls, real Google sign-in, and live microphone input are not
covered by the harness), and attach numbers when footprint is involved.
