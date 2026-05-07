<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="src/annotate_logowithtext_white.svg" />
    <source media="(prefers-color-scheme: light)" srcset="src/annotate_logowithtext_solid.svg" />
    <img src="src/annotate_logowithtext_solid.svg" alt="Annotate" width="200" />
  </picture>
</p>

<p align="center">
  Push-to-talk voice transcription for Windows.
  <br />
  Hold a hotkey, speak, release — your words are typed instantly.
</p>

<p align="center">
  <a href="https://github.com/vn-nthh/Annotate/releases">Download</a> · <a href="#features">Features</a> · <a href="#getting-started">Getting Started</a>
</p>

---

## Features

- **Push-to-talk transcription** — Hold a global hotkey, speak, and release. The transcription is pasted wherever your cursor is.
- **Three transcription engines**
  - **Web Speech** — Built-in, no setup required
  - **Groq Whisper** — Cloud-based, fast and accurate (requires free API key)
  - **Local Whisper** — Fully offline, runs on your GPU via CUDA
- **Custom dictionary** — Add domain-specific terms to improve transcription accuracy
- **Grammar correction** — On-device grammar cleanup powered by a local GEC (GECtor) model
- **Subtitle generation** — Generate `.srt` subtitles from any audio/video file using VAD + Whisper
- **Google Drive sync** — Sync dictionary and transcription history across devices
- **System tray** — Minimize to tray for always-on access
- **Dark mode** — Light and dark themes
- **Privacy-first** — API keys stored in Windows Credential Manager, not on disk

## Tech Stack

| Layer | Technology |
|---|---|
| Framework | [Tauri 2](https://v2.tauri.app/) |
| Backend | Rust |
| Frontend | HTML / CSS / JavaScript |
| Local STT | [whisper.cpp](https://github.com/ggerganov/whisper.cpp) via sidecar process |
| Grammar | GECtor ONNX model via [ort](https://github.com/pykeio/ort) |
| VAD | Silero VAD ONNX |
| Cloud STT | [Groq](https://groq.com/) Whisper API |

## Getting Started

### Download

Grab the latest installer from [Releases](https://github.com/vn-nthh/Annotate/releases).

- **`.exe`** — NSIS installer (recommended)
- **`.msi`** — Windows Installer

> **Note:** Windows SmartScreen may show a warning since the app is not code-signed. Click **"More info" → "Run anyway"** to proceed.

### Build from Source

**Prerequisites:** [Node.js](https://nodejs.org/) (LTS), [Rust](https://rustup.rs/), [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)

```bash
# Install frontend dependencies
npm install

# Run in development mode
npm run dev

# Build release installers
npm run build
```

### Google Drive Sync (Optional)

To enable cloud sync, create a `src/oauth.config.json` file with your Google OAuth credentials:

```json
{
  "client_id": "YOUR_CLIENT_ID.apps.googleusercontent.com",
  "client_secret": "YOUR_CLIENT_SECRET"
}
```

See `src/oauth.config.json.example` for reference.

## License

MIT
