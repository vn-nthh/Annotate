# UI reference files

`index.html`, `index.css`, and the artwork describe the settings UI for development
and visual comparisons. The application draws the UI itself with GPUI
(`src-tauri/src/ui`). It does not load these reference files or a browser engine.

To render reference screenshots, prepare the fonts and run
`scripts/capture-native-reference.py --output <directory outside this repository>`
with Python, Playwright, and Chromium installed. The script supplies its own test
fixtures; no JavaScript app, server, API key, or OAuth session is needed. Compare
them with `annotate.exe --capture-ui <directory>`, which writes the same file names.
The native harness also writes interaction results to `checks.json` and renders
eight 150% DPI scenes under `150/`. Use `--scale 1.5 --output <directory>/150`
for the corresponding Chromium reference.

`oauth.config.json.example` is optional public build configuration for Google Drive
sync. It is independent of the reference UI.
