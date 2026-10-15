# UI reference files

`index.html`, `index.css`, and the artwork describe the settings UI for development
and visual comparisons. The application renders the generated assets in
`src-tauri/assets` with Win32, Direct2D, and DirectWrite. It does not load these
reference files or a browser engine.

To regenerate drawing data and reference screenshots, prepare the fonts and run
`scripts/capture-native-reference.py --output <directory outside this repository>`
with Python, Playwright, and Chromium installed. The script supplies its own test
fixtures; no JavaScript app, server, API key, or OAuth session is needed.

`oauth.config.json.example` is optional public build configuration for Google Drive
sync. It is independent of the reference UI.
