//! App state and actions shared by the native window and global shortcuts.
use crate::app::{self as native_app, AppHandle};
use crate::{native_audio, native_data, subtitle::SrtEntry};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
};

static CORE: OnceLock<Arc<Core>> = OnceLock::new();
const MAX_CAPTURED_TERM_CHARS: usize = 100;
pub fn get() -> &'static Arc<Core> {
    CORE.get().expect("native app initialized")
}
pub fn try_get() -> Option<&'static Arc<Core>> {
    CORE.get()
}

#[derive(Clone)]
pub struct State {
    pub settings: BTreeMap<String, Value>,
    pub data: native_data::Store,
    pub labels: HashMap<String, String>,
    pub progress: HashMap<String, (f32, String)>,
    pub busy: HashSet<String>,
    pub ready: HashSet<String>,
    pub mics: Vec<crate::AudioDevice>,
    pub file: Option<String>,
    pub entries: Vec<SrtEntry>,
    pub summary: String,
    pub user: Option<Value>,
    pub avatar: Option<Arc<Vec<u8>>>,
    pub avatar_url: Option<String>,
    pub auth_waiting: bool,
    pub recording_hotkey: Option<String>,
}
impl State {
    pub fn setting(&self, key: &str, default: &str) -> String {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .into()
    }
    pub fn checked(&self, key: &str) -> bool {
        self.settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    pub fn dark(&self) -> bool {
        self.setting("theme", "light") == "dark"
    }
    pub fn mode(&self) -> String {
        self.setting("transcriptionMode", "groq")
    }
    pub fn label(&self, id: &str, fallback: &str) -> String {
        self.labels
            .get(id)
            .map(String::as_str)
            .unwrap_or(fallback)
            .into()
    }
}

struct RecordingConfig {
    mode: String,
    device: String,
    api_key: String,
    endpoint: String,
    prompt: Option<String>,
    language: Option<String>,
}
impl RecordingConfig {
    fn from_state(state: &State) -> Self {
        let mode = state.mode();
        let language = state.setting("liveLanguage", "auto");
        Self {
            api_key: if mode == "local" {
                String::new()
            } else {
                state.label(
                    if mode == "azure-mai" {
                        "azure-key"
                    } else {
                        "groq-key"
                    },
                    "",
                )
            },
            mode,
            device: state.setting("audioDeviceId", ""),
            endpoint: state.setting("azureMaiEndpoint", ""),
            prompt: state.data.data.dictionary.prompt(),
            language: (language != "auto").then_some(language),
        }
    }
}
struct Session {
    capture: native_audio::Capture,
    config: RecordingConfig,
}
#[derive(Default)]
struct Recording {
    generation: u64,
    active: bool,
    processing: bool,
    shutdown: bool,
    session: Option<Session>,
}
pub struct Core {
    pub app: AppHandle,
    pub state: Mutex<State>,
    capture: Mutex<Recording>,
    pub sync: crate::native_sync::Sync,
    sync_generation: AtomicU64,
    status_generation: AtomicU64,
    whisper_operation: tokio::sync::Mutex<()>,
    migration: tokio::sync::Mutex<()>,
    // One clipboard capture/restore at a time, as in the browser build.
    capturing_selection: AtomicBool,
}

pub fn initialize(app: AppHandle) -> Result<(), String> {
    let directory = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let store = app.store("settings.json").map_err(|e| e.to_string())?;
    let mut settings = BTreeMap::new();
    for key in [
        "audioDeviceId",
        "transcriptionMode",
        "hotkey",
        "dictHotkey",
        "theme",
        "grammarCleanupEnabled",
        "azureMaiEndpoint",
        "liveLanguage",
        "subtitleLanguage",
        crate::native_onboarding::KEY,
    ] {
        if let Some(value) = store.get(key) {
            settings.insert(key.into(), value);
        }
    }
    let mode = settings
        .get("transcriptionMode")
        .and_then(Value::as_str)
        .filter(|mode| ["groq", "azure-mai", "local"].contains(mode))
        .unwrap_or("groq")
        .to_owned();
    settings.insert("transcriptionMode".into(), json!(mode));
    settings.insert("whisperAcceleration".into(), json!("vulkan"));
    // A saved hotkey means the app was already set up (including by the Tauri
    // build), so only new installs open the setup guide. Test and benchmark
    // profiles are new too, but measure the app itself.
    let onboarding = settings
        .get(crate::native_onboarding::KEY)
        .cloned()
        .unwrap_or_else(|| {
            let set_up =
                settings.contains_key("hotkey") || std::env::var_os("ANNOTATE_UI_TEST").is_some();
            crate::native_onboarding::Onboarding::initial(set_up, &mode).to_value()
        });
    settings.insert(crate::native_onboarding::KEY.into(), onboarding.clone());
    store.update_and_save(|values| {
        values.insert("transcriptionMode".into(), json!(mode));
        values.insert("whisperAcceleration".into(), json!("vulkan"));
        values.insert(crate::native_onboarding::KEY.into(), onboarding);
    })?;
    let data = native_data::Store::open(&directory)?;
    let user = store.get("gd_user_info").filter(|v| !v.is_null());
    let core = Arc::new(Core {
        app: app.clone(),
        state: Mutex::new(State {
            settings,
            data,
            user,
            avatar: None,
            avatar_url: None,
            labels: HashMap::new(),
            progress: HashMap::new(),
            busy: HashSet::new(),
            ready: HashSet::new(),
            mics: vec![],
            file: None,
            entries: vec![],
            summary: String::new(),
            auth_waiting: false,
            recording_hotkey: None,
        }),
        capture: Mutex::new(Recording::default()),
        sync: crate::native_sync::Sync::default(),
        sync_generation: AtomicU64::new(0),
        status_generation: AtomicU64::new(0),
        whisper_operation: tokio::sync::Mutex::new(()),
        migration: tokio::sync::Mutex::new(()),
        capturing_selection: AtomicBool::new(false),
    });
    CORE.set(core.clone()).map_err(|_| "Already initialized")?;
    // The main native window paints before device discovery, credentials, or any
    // optional model/network initialization is performed.
    if std::env::var_os("ANNOTATE_UI_TEST").is_none() {
        native_app::spawn(async move {
            core.bootstrap().await;
        });
    }
    Ok(())
}
impl Core {
    pub fn snapshot(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn mode(&self) -> String {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).mode()
    }
    pub fn recording_hotkey_active(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recording_hotkey
            .is_some()
    }
    pub fn change(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()));
        crate::ui::refresh();
    }
    pub fn status(&self, text: &str, error: bool) {
        let generation = self.status_generation.fetch_add(1, Ordering::AcqRel) + 1;
        let active = !error && ["Listening…", "Transcribing…", "Cleaning grammar…"].contains(&text);
        self.change(|s| {
            s.labels.insert("status-text".into(), text.into());
            if error {
                s.ready.insert("status-error".into());
            } else {
                s.ready.remove("status-error");
            }
            if active {
                s.ready.insert("status-active".into());
            } else {
                s.ready.remove("status-active");
            }
        });
        if !active && text != "Ready" {
            native_app::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if get().status_generation.load(Ordering::Acquire) == generation {
                    get().status("Ready", false);
                }
            });
        }
    }
    fn data_change(&self, f: impl FnOnce(&mut native_data::Data)) -> bool {
        let result = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            mutate_data(&mut s.data, f)
        };
        if let Err(error) = result {
            self.status(&error, true);
            false
        } else {
            crate::ui::refresh();
            self.schedule_sync();
            true
        }
    }
    pub fn set_setting(&self, key: &str, value: Value) -> Result<(), String> {
        let store = self.app.store("settings.json").map_err(|e| e.to_string())?;
        store.set_and_save(key, value.clone())?;
        self.change(|s| {
            s.settings.insert(key.into(), value);
        });
        Ok(())
    }

    pub fn copy_text(&self, text: &str) -> Result<(), String> {
        let result = crate::platform::set_clipboard_text(&normalize_output(text));
        if let Err(error) = &result {
            self.status(error, true);
        }
        result
    }

    pub fn save_provider_key(&self, mode: &str, value: String) -> Result<(), String> {
        let value = value.trim().to_owned();
        let key = match mode {
            "groq" => {
                crate::save_api_key(value.clone())?;
                "groq-key"
            }
            "azure-mai" => {
                crate::save_azure_api_key(value.clone())?;
                "azure-key"
            }
            _ => return Err("This transcription engine does not use an API key".into()),
        };
        self.change(|state| {
            state.labels.insert(key.into(), value);
        });
        Ok(())
    }
    async fn bootstrap(self: Arc<Self>) {
        let app = self.app.clone();
        let setup = native_app::spawn_blocking(move || {
            let core = get();
            match crate::get_audio_devices() {
                Ok(mics) => core.change(|s| s.mics = mics),
                Err(e) => core.status(&e, true),
            }
            for (id, result) in [
                ("groq-key", crate::load_api_key()),
                ("azure-key", crate::load_azure_api_key()),
                ("openrouter-key-input", crate::load_openrouter_api_key()),
            ] {
                match result {
                    Ok(Some(key)) => core.change(|s| {
                        s.labels.insert(id.into(), key);
                    }),
                    Err(error) => core.status(&error, true),
                    _ => {}
                }
            }
            // Replaces the browser build's onboarding: Web Speech users migrate
            // to Groq, and new installs default to it, without a key. Open the
            // engine settings instead of failing silently on the first hotkey.
            let needs_setup = {
                let state = core.state.lock().unwrap_or_else(|e| e.into_inner());
                let setup_open = crate::native_onboarding::Onboarding::from_value(
                    state.settings.get(crate::native_onboarding::KEY),
                    &state.mode(),
                )
                .is_open();
                match state.mode().as_str() {
                    // The setup guide is already showing and asks for the key.
                    _ if setup_open => false,
                    "local" => false,
                    "azure-mai" => {
                        state.label("azure-key", "").trim().is_empty()
                            || state.setting("azureMaiEndpoint", "").trim().is_empty()
                    }
                    _ => state.label("groq-key", "").trim().is_empty(),
                }
            };
            if needs_setup {
                crate::ui::show_settings();
                core.status("Add an API key", true);
            }
            core.update_partial_download();
            for (key, check) in [
                ("whisper", crate::check_whisper_model(app.clone())),
                ("gec", crate::check_gec_model(app.clone())),
                ("ffmpeg", crate::check_ffmpeg(app.clone())),
                ("vad", crate::check_vad_model(app.clone())),
            ] {
                if check.unwrap_or(false) {
                    core.change(|s| {
                        s.ready.insert(key.into());
                    });
                }
            }
            if let Err(error) = core.sync.migrate_saved_credentials() {
                core.status(&error, true);
            }
            // Register each shortcut independently so one conflict does not
            // leave the other unbound. Retries cover a previous instance that is
            // still releasing its registrations.
            let (hotkey, dict_hotkey) = {
                let state = core.state.lock().unwrap_or_else(|e| e.into_inner());
                (state.setting("hotkey", ""), state.setting("dictHotkey", ""))
            };
            let mut failure = None;
            for (slot, combo, label) in [
                (0, hotkey, "Hotkey failed"),
                (1, dict_hotkey, "Dictionary hotkey failed"),
            ] {
                if combo.is_empty() {
                    continue;
                }
                let mut result = native_app::register(slot, &combo);
                for _ in 1..3 {
                    if result.is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    result = native_app::register(slot, &combo);
                }
                if let Err(error) = result {
                    log::error!("[Hotkey] {label}: {error}");
                    failure.get_or_insert(label);
                }
            }
            match failure {
                Some(label) => Err(label.to_owned()),
                None => Ok(()),
            }
        })
        .await;
        match setup {
            Ok(Err(error)) => self.status(&error, true),
            Err(error) => self.status(&error.to_string(), true),
            _ => {}
        }
        // A failed import is retried by every sync attempt, including Sync Now.
        if let Err(error) = self.ensure_migrated().await {
            self.status(&error, true);
        }
        self.sync.initialize().await;
    }
    fn migrated(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .data
            .data
            .migrated
    }
    /// Imports the browser build's dictionary and history once. Sync waits for
    /// this so remote data is never merged into a store missing local records.
    pub async fn ensure_migrated(&self) -> Result<(), String> {
        if self.migrated() {
            return Ok(());
        }
        let _migration = self.migration.lock().await;
        if self.migrated() {
            return Ok(());
        }
        native_app::spawn_blocking(|| {
            let (dictionary, history) =
                crate::native_migrate::read_legacy(&get().app.app_local_data_dir()?)?;
            let mut state = get().state.lock().unwrap_or_else(|e| e.into_inner());
            state.data.import(&dictionary, &history)
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|result| result)
        .map_err(|error| format!("Cannot import previous data: {error}"))?;
        crate::ui::refresh();
        Ok(())
    }
    pub fn schedule_sync(&self) {
        let generation = self.sync_generation.fetch_add(1, Ordering::AcqRel) + 1;
        native_app::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if get().sync_generation.load(Ordering::Acquire) == generation {
                get().sync.sync_now().await;
            }
        });
    }
    pub fn register_shortcut(&self, id: &str, combo: &str) -> Result<(), String> {
        let dict = id == "dict-hotkey-btn";
        let key = if dict { "dictHotkey" } else { "hotkey" };
        let old = self.snapshot().setting(key, "");
        let result = if dict {
            crate::register_dict_hotkey(self.app.clone(), combo.into())
        } else {
            crate::register_hotkey(self.app.clone(), combo.into())
        };
        if result.is_err() && !old.is_empty() {
            let _ = if dict {
                crate::register_dict_hotkey(self.app.clone(), old.clone())
            } else {
                crate::register_hotkey(self.app.clone(), old.clone())
            };
        }
        result?;
        if let Err(error) = self.set_setting(key, json!(combo)) {
            let _ = if dict {
                crate::register_dict_hotkey(self.app.clone(), old)
            } else {
                crate::register_hotkey(self.app.clone(), old)
            };
            return Err(error);
        }
        self.change(|s| s.recording_hotkey = None);
        Ok(())
    }
    pub fn action(&self, id: &str, value: String) {
        if self
            .capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
        {
            return;
        }
        if matches!(id, "sub-file-zone" | "sub-new-btn")
            && self.snapshot().busy.contains("sub-generate-btn")
        {
            return;
        }
        match id {
            "dict-add-btn" => {
                self.data_change(|d| {
                    d.dictionary.add(&value);
                });
            }
            "history-clear" => {
                self.data_change(|d| d.history.clear());
            }
            "dict-remove" => {
                self.data_change(|d| d.dictionary.remove(&value));
            }
            "history-remove" => {
                self.data_change(|d| d.history.remove(&value));
            }
            "history-copy" | "sub-summary-copy-btn" => {
                let text = if id == "sub-summary-copy-btn" {
                    self.snapshot().summary
                } else {
                    value
                };
                let _ = self.copy_text(&text);
            }
            "hotkey-btn" | "dict-hotkey-btn" => {
                self.change(|s| s.recording_hotkey = Some(id.into()))
            }
            "dict-hotkey-clear" => {
                if let Err(error) = self.register_shortcut("dict-hotkey-btn", "") {
                    self.status(&error, true);
                }
            }
            "theme-toggle" => {
                if let Err(error) = self.set_setting(
                    "theme",
                    json!(if self.snapshot().dark() {
                        "light"
                    } else {
                        "dark"
                    }),
                ) {
                    self.status(&error, true);
                }
            }
            "grammar-toggle" => {
                let enabled = !self.snapshot().checked("grammarCleanupEnabled");
                if let Err(error) = self.set_setting("grammarCleanupEnabled", json!(enabled)) {
                    self.status(&error, true);
                    return;
                }
                if !enabled {
                    native_app::spawn(async {
                        let _ = crate::unload_gec_model().await;
                        get().change(|s| {
                            s.ready.remove("gec-loaded");
                        });
                    });
                }
            }
            "mic-select"
            | "mode-select"
            | "live-lang-select"
            | "sub-lang-select"
            | "acceleration-select"
            | "azure-endpoint-input" => {
                let key = match id {
                    "mic-select" => "audioDeviceId",
                    "mode-select" => "transcriptionMode",
                    "live-lang-select" => "liveLanguage",
                    "sub-lang-select" => "subtitleLanguage",
                    "acceleration-select" => "whisperAcceleration",
                    _ => "azureMaiEndpoint",
                };
                let value = if id == "acceleration-select" {
                    "vulkan".into()
                } else if id == "mode-select"
                    && !matches!(value.as_str(), "groq" | "azure-mai" | "local")
                {
                    "groq".into()
                } else {
                    value
                };
                if let Err(e) = self.set_setting(key, json!(value)) {
                    self.status(&e, true);
                    return;
                }
                if id == "mode-select" {
                    self.unload_idle_whisper();
                }
            }
            "apikey-input" | "apikey-input:groq" | "apikey-input:azure-mai" => {
                let mode = id
                    .strip_prefix("apikey-input:")
                    .map(str::to_owned)
                    .unwrap_or_else(|| self.mode());
                if let Err(error) = self.save_provider_key(&mode, value) {
                    self.status(&error, true);
                }
            }
            "openrouter-key-input" => {
                let value = value.trim().to_owned();
                match crate::save_openrouter_api_key(value.clone()) {
                    Ok(()) => self.change(|state| {
                        state.labels.insert(id.into(), value);
                    }),
                    Err(error) => self.status(&error, true),
                }
            }
            "sub-file-zone" => crate::ui::pick_file(false),
            "sub-save-btn" => crate::ui::pick_file(true),
            "sub-new-btn" => self.change(|s| {
                s.file = None;
                s.entries.clear();
                s.summary.clear();
                s.progress.remove("sub");
            }),
            "whisper-pause-btn" => {
                crate::cancel_whisper_model_download();
            }
            "ob-open-groq" | "ob-open-azure" => {
                let url = if id == "ob-open-groq" {
                    "https://console.groq.com/keys"
                } else {
                    "https://ai.azure.com/"
                };
                if open::that_detached(url).is_err() {
                    self.status("Cannot open the browser", true);
                }
            }
            "sync-cancel-btn" => {
                self.sync.cancel_sign_in();
            }
            "sync-signout-btn" => {
                get().sync.sign_out();
            }
            "sync-reopen-btn" => {
                if let Err(e) = crate::google_auth::reopen_google_oauth() {
                    self.status(&e, true);
                }
            }
            "sync-signin-btn" => {
                native_app::spawn(async {
                    get().sync.sign_in().await;
                });
            }
            "sync-now-btn" => {
                native_app::spawn(async {
                    get().sync.sync_now().await;
                });
            }
            "whisper-download-btn"
            | "whisper-load-btn"
            | "gec-download-btn"
            | "sub-ffmpeg-download-btn"
            | "sub-vad-download-btn"
            | "sub-generate-btn"
            | "sub-summarize-btn" => {
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.busy.insert(id.into()) {
                        return;
                    }
                    if let Some(key) = download_key(id) {
                        state.labels.remove(&format!("{key}-status-error"));
                    }
                }
                crate::ui::refresh();
                let id = id.to_owned();
                native_app::spawn(async move {
                    let mut result = get().async_action(&id).await;
                    // Pausing keeps the partial file; it is not a failure.
                    let paused = result
                        .as_ref()
                        .is_err_and(|e| e == crate::download::DOWNLOAD_CANCELLED);
                    if paused {
                        result = Ok(());
                    }
                    get().change(|s| {
                        s.busy.remove(&id);
                        if let Some(key) = download_key(&id) {
                            s.progress.remove(key);
                            if let Err(error) = &result {
                                s.labels
                                    .insert(format!("{key}-status-error"), error.clone());
                            } else {
                                s.labels.remove(&format!("{key}-status-error"));
                            }
                        }
                    });
                    if id == "whisper-download-btn" {
                        get().update_partial_download();
                        if paused {
                            get().status("Download paused", false);
                        }
                    }
                    get().refresh_model_state();
                    get().unload_idle_whisper();
                    if let Err(e) = result {
                        get().status(&e, true);
                        if id.starts_with("sub-") {
                            get().change(|s| {
                                s.progress
                                    .insert("sub".into(), (1.0, format!("Error: {e}")));
                            });
                        }
                    }
                });
            }
            _ => {}
        }
    }
    async fn async_action(&self, id: &str) -> Result<(), String> {
        if self
            .capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown
        {
            return Ok(());
        }
        let s = self.snapshot();
        let app = self.app.clone();
        let ready = match id {
            "whisper-download-btn" => {
                crate::download_whisper_model(app).await?;
                "whisper"
            }
            "whisper-load-btn" => {
                let _operation = self.whisper_operation.lock().await;
                crate::load_whisper_model(app, Some("vulkan".into())).await?;
                "whisper-loaded"
            }
            "gec-download-btn" => {
                crate::download_gec_model(app.clone()).await?;
                "gec"
            }
            "sub-ffmpeg-download-btn" => {
                crate::download_ffmpeg(app).await?;
                "ffmpeg"
            }
            "sub-vad-download-btn" => {
                crate::download_vad_model(app).await?;
                "vad"
            }
            "sub-generate-btn" => {
                let file = s.file.clone().ok_or("Choose a file first")?;
                if !s.ready.contains("ffmpeg") || !s.ready.contains("vad") {
                    return Err("Download subtitle dependencies first".into());
                }
                let mode = s.mode();
                let engine = if mode == "local" || mode == "azure-mai" {
                    mode.as_str()
                } else {
                    "groq"
                };
                if engine == "local" {
                    let _operation = self.whisper_operation.lock().await;
                    crate::load_whisper_model(app.clone(), Some("vulkan".into())).await?;
                    self.refresh_model_state();
                }
                let key = s
                    .labels
                    .get(if engine == "azure-mai" {
                        "azure-key"
                    } else {
                        "groq-key"
                    })
                    .cloned();
                if engine != "local" && key.as_ref().is_none_or(|key| key.trim().is_empty()) {
                    return Err("Set an API key in Settings first".into());
                }
                let lang = s.setting("subtitleLanguage", "auto");
                self.change(|s| {
                    s.summary.clear();
                    s.progress.insert("sub".into(), (0.0, "Starting...".into()));
                });
                let entries = crate::generate_subtitles(
                    app,
                    file.clone(),
                    engine.into(),
                    key,
                    Some(s.setting("azureMaiEndpoint", "")),
                    s.data.data.dictionary.prompt(),
                    (lang != "auto").then_some(lang),
                )
                .await?;
                self.change(|s| {
                    if s.file.as_deref() != Some(file.as_str()) {
                        return;
                    }
                    if entries.is_empty() {
                        s.progress.insert(
                            "sub".into(),
                            (1.0, "No speech detected in the audio.".into()),
                        );
                    } else {
                        s.progress.remove("sub");
                    }
                    s.entries = entries;
                });
                return Ok(());
            }
            "sub-summarize-btn" => {
                if s.entries.is_empty() {
                    return Err("Generate subtitles first".into());
                }
                self.change(|s| {
                    s.progress
                        .insert("sub".into(), (0.3, "Summarizing subtitles...".into()));
                });
                let summary = crate::summarize_subtitles(s.entries).await?;
                self.change(|state| {
                    if state.file == s.file {
                        state.summary = summary;
                        state.progress.remove("sub");
                    }
                });
                return Ok(());
            }
            _ => return Ok(()),
        };
        self.change(|s| {
            s.ready.insert(ready.into());
            s.progress.remove(ready);
        });
        Ok(())
    }

    /// Records a paused model download so it can be offered as "Resume".
    fn update_partial_download(&self) {
        let bytes = crate::partial_whisper_model_bytes(self.app.clone());
        self.change(|s| {
            if bytes > 0 {
                s.labels.insert("whisper-partial".into(), bytes.to_string());
            } else {
                s.labels.remove("whisper-partial");
            }
        });
    }

    fn refresh_model_state(&self) {
        let whisper = crate::whisper_local::is_loaded();
        let gec = crate::gec::is_loaded();
        self.change(|s| {
            if whisper {
                s.ready.insert("whisper-loaded".into());
            } else {
                s.ready.remove("whisper-loaded");
            }
            if gec {
                s.ready.insert("gec-loaded".into());
            } else {
                s.ready.remove("gec-loaded");
            }
        });
    }

    fn unload_idle_whisper(&self) {
        if self.mode() == "local" || !crate::whisper_local::is_loaded() {
            return;
        }
        native_app::spawn(async {
            let core = get();
            let _operation = core.whisper_operation.lock().await;
            {
                let recording = core.capture.lock().unwrap_or_else(|e| e.into_inner());
                let state = core.snapshot();
                if recording.active
                    || recording.processing
                    || recording.shutdown
                    || state.mode() == "local"
                    || state.busy.contains("whisper-load-btn")
                    || state.busy.contains("sub-generate-btn")
                {
                    return;
                }
            }
            let _ = crate::unload_whisper_model().await;
            core.refresh_model_state();
        });
    }

    pub fn shutdown(&self) {
        let session = {
            let mut recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
            recording.shutdown = true;
            recording.generation = recording.generation.wrapping_add(1);
            recording.active = false;
            recording.processing = false;
            recording.session.take()
        };
        drop(session);
        self.sync.shutdown();
        self.sync_generation.fetch_add(1, Ordering::AcqRel);
        self.status_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn current_recording(&self, generation: u64) -> bool {
        let recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
        !recording.shutdown && recording.generation == generation
    }

    fn capture_failed(&self, generation: u64, error: String) {
        let session = {
            let mut recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
            if recording.shutdown || !recording.active || recording.generation != generation {
                return;
            }
            recording.active = false;
            let _ = crate::ui::overlay::hide();
            self.change(|s| {
                s.labels.insert("status-detail".into(), error);
            });
            self.status("Microphone failed", true);
            recording.session.take()
        };
        // This callback runs on the completed capture thread. Drop cancels safely;
        // joining that same thread with Capture::stop would deadlock.
        drop(session);
    }

    pub fn hotkey(&self, prefix: &str, down: bool) {
        if self.recording_hotkey_active() && (down || prefix == "dict-hotkey") {
            return;
        }
        if prefix == "dict-hotkey" {
            // Capture on press, as the browser build did; capture_selected_text
            // releases the hotkey's modifiers before sending Ctrl+C.
            if down {
                if self
                    .capture
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .shutdown
                {
                    return;
                }
                // Overlapping captures would race on the clipboard sentinel and
                // could restore the wrong clipboard contents.
                if self.capturing_selection.swap(true, Ordering::AcqRel) {
                    return;
                }
                native_app::spawn(async {
                    struct Release;
                    impl Drop for Release {
                        fn drop(&mut self) {
                            get().capturing_selection.store(false, Ordering::Release);
                        }
                    }
                    let _release = Release;
                    let selected =
                        native_app::spawn_blocking(|| crate::capture_selected_text(Some(80))).await;
                    if get()
                        .capture
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .shutdown
                    {
                        return;
                    }
                    match selected {
                        Ok(Ok(text)) => get().add_captured_term(&text),
                        _ => get().status("Capture failed", true),
                    }
                });
            }
            return;
        }
        if down {
            let mut recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
            if recording.shutdown || recording.active || recording.processing {
                return;
            }
            let config =
                RecordingConfig::from_state(&self.state.lock().unwrap_or_else(|e| e.into_inner()));
            if config.mode != "local" && config.api_key.trim().is_empty() {
                self.status("Set API key", true);
                return;
            }
            if config.mode == "azure-mai" && config.endpoint.trim().is_empty() {
                self.status("Set Azure endpoint", true);
                return;
            }
            recording.generation = recording.generation.wrapping_add(1);
            let generation = recording.generation;
            recording.active = true;
            self.status("Listening…", false);
            let _ = crate::ui::overlay::show_listening();
            if config.mode == "azure-mai" {
                let endpoint = config.endpoint.clone();
                native_app::spawn(async move {
                    let _ = crate::warm_azure_transcription(endpoint).await;
                });
            }
            // Load the model while the user speaks rather than at startup, so the
            // worker's memory is only committed once local dictation is used.
            if config.mode == "local" && !crate::whisper_local::is_loaded() {
                native_app::spawn(async {
                    let core = get();
                    let _operation = core.whisper_operation.lock().await;
                    if core.mode() == "local"
                        && crate::check_whisper_model(core.app.clone()).unwrap_or(false)
                        && crate::load_whisper_model(core.app.clone(), Some("vulkan".into()))
                            .await
                            .is_ok()
                    {
                        core.refresh_model_state();
                    }
                });
            }
            let capture =
                native_audio::Capture::start_with_error(config.device.clone(), move |error| {
                    get().capture_failed(generation, error)
                });
            recording.session = Some(Session { capture, config });
        } else {
            let session = {
                let mut recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
                if recording.shutdown || !recording.active {
                    return;
                }
                recording.active = false;
                let session = recording.session.take();
                recording.processing = session.is_some();
                (recording.generation, session)
            };
            let _ = crate::ui::overlay::hide();
            if let (generation, Some(session)) = session {
                self.status("Transcribing…", false);
                native_app::spawn(async move {
                    let audio = native_app::spawn_blocking(move || {
                        let samples = session.capture.stop()?;
                        if !native_audio::speech_present(&samples) {
                            return Ok(None);
                        }
                        Ok(Some((native_audio::wav(&samples)?, session.config)))
                    })
                    .await;
                    let result = match audio {
                        Ok(Ok(Some((bytes, config)))) => {
                            get().transcribe(bytes, config, generation).await
                        }
                        Ok(Ok(None)) => Ok(String::new()),
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(error.to_string()),
                    };
                    get().finish(generation, result).await;
                });
            } else {
                self.status("Ready", false);
            }
        }
    }

    fn add_captured_term(&self, text: &str) {
        let (term, short) = match captured_term(text) {
            Ok(term) => term,
            Err(error) => {
                self.status(error, true);
                return;
            }
        };
        let normalized = native_data::key(&term);
        let duplicate = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .data
            .data
            .dictionary
            .terms
            .iter()
            .any(|existing| native_data::key(&existing.value) == normalized);
        if duplicate {
            self.status(&format!("\"{short}\" already in dictionary"), false);
            let _ = crate::ui::overlay::show_confirmation("duplicate", short);
        } else if self.data_change(|d| {
            d.dictionary.add(&term);
        }) {
            self.status(&format!("Added \"{short}\""), false);
            let _ = crate::ui::overlay::show_confirmation("added", short);
        }
    }

    async fn transcribe(
        &self,
        bytes: Vec<u8>,
        config: RecordingConfig,
        generation: u64,
    ) -> Result<String, String> {
        if !self.current_recording(generation) {
            return Ok(String::new());
        }
        let audio = base64::engine::general_purpose::STANDARD.encode(bytes);
        match config.mode.as_str() {
            "local" => {
                let _operation = self.whisper_operation.lock().await;
                if !self.current_recording(generation) {
                    return Ok(String::new());
                }
                crate::load_whisper_model(self.app.clone(), Some("vulkan".into())).await?;
                self.refresh_model_state();
                crate::transcribe_audio_local(audio, config.prompt, config.language).await
            }
            "azure-mai" => {
                crate::transcribe_audio_azure(
                    audio,
                    config.api_key,
                    config.endpoint,
                    config.prompt,
                    config.language,
                )
                .await
            }
            _ => {
                crate::transcribe_audio(audio, config.api_key, config.prompt, config.language).await
            }
        }
    }

    async fn finish(&self, generation: u64, result: Result<String, String>) {
        if !self.current_recording(generation) {
            return;
        }
        let result = async {
            let mut text = result?.trim().to_owned();
            if text.is_empty() {
                return Ok(false);
            }
            if self.snapshot().checked("grammarCleanupEnabled")
                && crate::check_gec_model(self.app.clone()).unwrap_or(false)
            {
                self.status("Cleaning grammar…", false);
                if crate::load_gec_model(self.app.clone()).await.is_ok() {
                    self.refresh_model_state();
                    if let Ok(corrected) = crate::correct_grammar(text.clone()).await {
                        if !corrected.trim().is_empty() {
                            text = corrected.trim().into();
                        }
                    }
                }
            }
            if !self.current_recording(generation) {
                return Ok(false);
            }
            let text = normalize_output(&text);
            let paste = text.clone();
            self.status("Pasting…", false);
            native_app::spawn_blocking(move || {
                if get().current_recording(generation) {
                    crate::paste_text(paste, Some(80))
                } else {
                    Ok(())
                }
            })
            .await
            .map_err(|e| e.to_string())??;
            if !self.current_recording(generation) {
                return Ok(false);
            }
            if !self.data_change(|d| d.history.add(&text)) {
                return Err("Could not save transcription history".into());
            }
            Ok(true)
        }
        .await;
        {
            let mut recording = self.capture.lock().unwrap_or_else(|e| e.into_inner());
            if recording.shutdown || recording.generation != generation {
                return;
            }
            recording.processing = false;
            match result {
                Ok(true) => self.status("Done", false),
                Ok(false) => self.status("No speech", false),
                Err(error) => {
                    self.change(|s| {
                        s.labels.insert("status-detail".into(), error);
                    });
                    self.status("Transcribe failed", true);
                }
            }
        }
        self.unload_idle_whisper();
    }
}

/// Returns the dictionary term for a captured selection and its short label.
fn captured_term(text: &str) -> Result<(String, String), &'static str> {
    let term = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let length = term.chars().count();
    if length == 0 {
        return Err("No text selected");
    }
    // Dictionary terms are words or phrases; reject accidental large selections.
    if length > MAX_CAPTURED_TERM_CHARS {
        return Err("Selection too long");
    }
    let short = if length > 24 {
        format!("{}…", term.chars().take(24).collect::<String>())
    } else {
        term.clone()
    };
    Ok((term, short))
}

fn download_key(id: &str) -> Option<&'static str> {
    match id {
        "whisper-download-btn" | "whisper-load-btn" => Some("whisper"),
        "gec-download-btn" => Some("gec"),
        "sub-ffmpeg-download-btn" => Some("ffmpeg"),
        "sub-vad-download-btn" => Some("vad"),
        _ => None,
    }
}

fn mutate_data(
    store: &mut native_data::Store,
    mutation: impl FnOnce(&mut native_data::Data),
) -> Result<(), String> {
    let previous = store.data.clone();
    mutation(&mut store.data);
    if let Err(error) = store.save() {
        store.data = previous;
        return Err(error);
    }
    Ok(())
}

pub fn normalize_output(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    crate::platform::sanitize_text_for_input(&text.nfc().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_output_preserves_the_existing_unicode_and_newline_rules() {
        assert_eq!(
            normalize_output("Cafe\u{301}\u{a0}hello\u{200b}\nworld"),
            "Café hello\r\nworld"
        );
    }

    #[test]
    fn captured_selections_match_the_browser_dictionary_rules() {
        assert_eq!(
            captured_term("  Annotate\r\n  app\t").unwrap(),
            ("Annotate app".into(), "Annotate app".into())
        );
        let long = "é".repeat(30);
        let (term, short) = captured_term(&long).unwrap();
        assert_eq!(term, long);
        assert_eq!(short, format!("{}…", "é".repeat(24)));
        assert_eq!(captured_term(" \n "), Err("No text selected"));
        assert!(captured_term(&"a".repeat(100)).is_ok());
        assert_eq!(captured_term(&"a".repeat(101)), Err("Selection too long"));
    }

    #[test]
    fn failed_data_write_does_not_leave_an_unsaved_edit_in_memory() {
        let path = std::env::temp_dir().join(format!(
            "annotate-core-test-{}-{}",
            std::process::id(),
            native_data::now()
        ));
        let mut store = native_data::Store::open(&path).unwrap();
        std::fs::write(&path, "This file prevents creating a data directory").unwrap();
        assert!(mutate_data(&mut store, |data| {
            data.dictionary.add("Must not survive a failed save");
        })
        .is_err());
        assert!(store.data.dictionary.terms.is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
