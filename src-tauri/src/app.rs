//! Process lifecycle, settings, and the async runtime shared by every platform.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SETTINGS: OnceLock<Arc<Settings>> = OnceLock::new();

pub fn initialize_runtime() -> Result<(), String> {
    RUNTIME
        .set(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|_| "Runtime already initialized".into())
}
pub fn spawn<F: std::future::Future + Send + 'static>(
    future: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F::Output: Send + 'static,
{
    RUNTIME.get().unwrap().spawn(future)
}
pub fn spawn_blocking<F: FnOnce() -> R + Send + 'static, R: Send + 'static>(
    f: F,
) -> tokio::task::JoinHandle<R> {
    RUNTIME.get().unwrap().spawn_blocking(f)
}

#[derive(Clone)]
pub struct AppHandle;
impl AppHandle {
    pub fn path(&self) -> &Self {
        self
    }
    pub fn app_local_data_dir(&self) -> Result<PathBuf, String> {
        if let Some(directory) = std::env::var_os("ANNOTATE_DATA_DIR") {
            return Ok(directory.into());
        }
        crate::platform::local_data_dir().map(|p| p.join("com.annotate.app"))
    }
    pub fn store(&self, _name: &str) -> Result<Arc<Settings>, String> {
        if let Some(settings) = SETTINGS.get() {
            return Ok(settings.clone());
        }
        let directory = match std::env::var_os("ANNOTATE_DATA_DIR") {
            Some(directory) => PathBuf::from(directory),
            None => crate::platform::roaming_data_dir()?.join("com.annotate.app"),
        };
        let path = directory.join("settings.json");
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Cannot read settings; original file preserved: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        let settings = Arc::new(Settings {
            path,
            data: Mutex::new(data),
        });
        let _ = SETTINGS.set(settings);
        Ok(SETTINGS.get().unwrap().clone())
    }
    pub fn emit<T: serde::Serialize>(&self, event: &str, payload: T) -> Result<(), String> {
        let value = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        if event == "subtitle-progress" {
            let total = value["total"].as_f64().unwrap_or(1.0).max(1.0);
            crate::native_core::get().change(|s| {
                s.progress.insert(
                    "sub".into(),
                    (
                        (value["current"].as_f64().unwrap_or(0.0) / total) as f32,
                        value["message"].as_str().unwrap_or("Working...").into(),
                    ),
                );
            });
        } else if let Some(key) = event.strip_suffix("-download-progress") {
            let current = value[0].as_f64().unwrap_or(0.0);
            let total = value[1].as_f64().unwrap_or(1.0).max(1.0);
            crate::native_core::get().change(|s| {
                s.progress.insert(
                    key.into(),
                    (
                        (current / total) as f32,
                        format!("{:.0}%", current / total * 100.0),
                    ),
                );
            });
        }
        Ok(())
    }
    pub fn exit(&self, _code: i32) {
        crate::ui::quit();
    }
}
pub struct Settings {
    path: PathBuf,
    data: Mutex<BTreeMap<String, Value>>,
}
impl Settings {
    pub fn get(&self, key: &str) -> Option<Value> {
        self.data
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }
    fn save_locked(&self, data: &BTreeMap<String, Value>) -> Result<(), String> {
        crate::native_data::atomic_write(
            &self.path,
            &serde_json::to_vec(data).map_err(|e| e.to_string())?,
        )
    }
    pub fn update_and_save(
        &self,
        update: impl FnOnce(&mut BTreeMap<String, Value>),
    ) -> Result<(), String> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let previous = data.clone();
        update(&mut data);
        if let Err(error) = self.save_locked(&data) {
            *data = previous;
            return Err(error);
        }
        Ok(())
    }
    pub fn set_and_save(&self, key: &str, value: Value) -> Result<(), String> {
        self.update_and_save(|data| {
            data.insert(key.into(), value);
        })
    }
}

pub fn register(slot: usize, combo: &str) -> Result<(), String> {
    crate::platform::register_shortcut(slot, combo)
}

/// Runs the app until the user quits. The UI owns the main thread; shortcuts,
/// the tray, and single-instance activation run on the platform's own thread.
pub fn run() -> Result<(), String> {
    let directory = AppHandle.app_local_data_dir()?;
    let Some(instance) = crate::platform::acquire_instance(&directory)? else {
        return Ok(());
    };
    initialize_runtime()?;
    let instance = std::rc::Rc::new(instance);
    let services = instance.clone();
    let startup = crate::ui::run(move || {
        crate::native_core::initialize(AppHandle)?;
        crate::platform::start_services(&services)?;
        Ok(())
    });
    crate::platform::stop_services();
    if let Some(core) = crate::native_core::try_get() {
        core.shutdown();
    }
    crate::google_auth::cancel_google_oauth();
    RUNTIME.get().unwrap().block_on(async {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::whisper_local::unload(),
        )
        .await;
    });
    drop(instance);
    startup
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_updates_are_serialized_and_rollback_when_save_fails() {
        let directory = std::env::temp_dir().join(format!(
            "annotate-settings-test-{}-{}",
            std::process::id(),
            crate::native_data::now()
        ));
        let settings = Arc::new(Settings {
            path: directory.join("settings.json"),
            data: Mutex::new(BTreeMap::new()),
        });
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let settings = settings.clone();
                std::thread::spawn(move || {
                    settings
                        .set_and_save(&format!("setting-{index}"), serde_json::json!(index))
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let data: Value = serde_json::from_slice(&std::fs::read(&settings.path).unwrap()).unwrap();
        assert_eq!(data.as_object().unwrap().len(), 4);
        std::fs::remove_file(&settings.path).unwrap();
        std::fs::create_dir(&settings.path).unwrap();
        assert!(settings
            .set_and_save("must-not-survive", Value::Bool(true))
            .is_err());
        assert!(settings.get("must-not-survive").is_none());
        std::fs::remove_dir(&settings.path).unwrap();
        std::fs::remove_file(settings.path.with_extension("tmp")).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
