//! First-run setup. The record lives in settings.json under `onboarding` so an
//! exited setup can be resumed at the same step after a restart.
use serde_json::{json, Value};

pub const KEY: &str = "onboarding";
pub const STEPS: [(&str, &str); 6] = [
    ("model", "Engine"),
    ("install", "Get model"),
    ("microphone", "Microphone"),
    ("hotkey", "Hotkey"),
    ("try", "Try it"),
    ("features", "Features"),
];
pub const ENGINES: [&str; 3] = ["groq", "azure-mai", "local"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    /// Shown whenever the window opens, until the user exits or finishes.
    Active,
    /// Exited by the user; resumable from Settings and the sidebar.
    Paused,
    Completed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Onboarding {
    pub status: Status,
    pub step: usize,
    /// Chosen in step 1; becomes the transcription engine once it can work.
    pub engine: String,
}

/// What the install step needs before the chosen engine can transcribe.
pub struct Readiness<'a> {
    pub groq_key: &'a str,
    pub azure_key: &'a str,
    pub endpoint: &'a str,
    pub model_downloaded: bool,
}

impl Onboarding {
    /// Existing users (with a saved hotkey) are not interrupted by setup.
    pub fn initial(existing_setup: bool, mode: &str) -> Self {
        Self {
            status: if existing_setup {
                Status::Completed
            } else {
                Status::Active
            },
            step: 0,
            engine: engine(mode),
        }
    }
    pub fn from_value(value: Option<&Value>, mode: &str) -> Self {
        let value = value.unwrap_or(&Value::Null);
        Self {
            status: match value["status"].as_str() {
                Some("active") => Status::Active,
                Some("paused") => Status::Paused,
                _ => Status::Completed,
            },
            step: value["step"]
                .as_str()
                .and_then(|id| STEPS.iter().position(|(step, _)| *step == id))
                .unwrap_or(0),
            engine: engine(value["engine"].as_str().unwrap_or(mode)),
        }
    }
    pub fn to_value(&self) -> Value {
        json!({
            "version": 1,
            "status": match self.status {
                Status::Active => "active",
                Status::Paused => "paused",
                Status::Completed => "completed",
            },
            "step": self.step_id(),
            "engine": self.engine,
        })
    }
    pub fn is_open(&self) -> bool {
        self.status == Status::Active
    }
    pub fn step_id(&self) -> &'static str {
        STEPS[self.step.min(STEPS.len() - 1)].0
    }
    pub fn step_label(&self) -> &'static str {
        STEPS[self.step.min(STEPS.len() - 1)].1
    }
    pub fn is_last(&self) -> bool {
        self.step + 1 >= STEPS.len()
    }
    pub fn next(&mut self) {
        if self.is_last() {
            self.complete();
        } else {
            self.step += 1;
        }
    }
    pub fn complete(&mut self) {
        self.status = Status::Completed;
        self.step = 0;
    }
    pub fn back(&mut self) {
        self.step = self.step.saturating_sub(1);
    }
    pub fn choose(&mut self, engine: &str) {
        self.engine = self::engine(engine);
    }
    pub fn pause(&mut self) {
        self.status = Status::Paused;
    }
    /// Resumes a paused setup where it stopped, or restarts a finished one.
    pub fn resume(&mut self, mode: &str) {
        if self.status == Status::Completed {
            self.step = 0;
            self.engine = engine(mode);
        }
        self.status = Status::Active;
    }
}

fn engine(mode: &str) -> String {
    if ENGINES.contains(&mode) {
        mode
    } else {
        "groq"
    }
    .into()
}

/// Why the install step cannot be left yet, if anything.
pub fn install_blocker(engine: &str, ready: &Readiness) -> Option<&'static str> {
    match engine {
        "local" if !ready.model_downloaded => Some("Download the model to continue"),
        "azure-mai" if ready.endpoint.trim().is_empty() => Some("Add the project endpoint"),
        "azure-mai" if !ready.endpoint.trim().starts_with("https://") => {
            Some("The endpoint starts with https://")
        }
        "azure-mai" if ready.azure_key.trim().is_empty() => Some("Add the API key"),
        "groq" if ready.groq_key.trim().is_empty() => Some("Paste your API key to continue"),
        _ => None,
    }
}

/// "230 MB" style sizes for download progress.
pub fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1024.0 / 1024.0;
    if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{mb:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_installs_start_setup_and_existing_users_skip_it() {
        let fresh = Onboarding::initial(false, "groq");
        assert!(fresh.is_open());
        assert_eq!(fresh.step_id(), "model");
        let existing = Onboarding::initial(true, "local");
        assert_eq!(existing.status, Status::Completed);
        assert_eq!(existing.engine, "local");
    }

    #[test]
    fn exiting_keeps_the_step_and_resuming_returns_to_it() {
        let mut setup = Onboarding::initial(false, "groq");
        setup.next();
        setup.next();
        setup.pause();
        let saved = Onboarding::from_value(Some(&setup.to_value()), "groq");
        assert_eq!(saved.status, Status::Paused);
        assert!(!saved.is_open());
        assert_eq!(saved.step_id(), "microphone");
        let mut resumed = saved.clone();
        resumed.resume("groq");
        assert!(resumed.is_open());
        assert_eq!(resumed.step_id(), "microphone");
    }

    #[test]
    fn finishing_closes_setup_and_running_it_again_starts_over() {
        let mut setup = Onboarding::initial(false, "groq");
        while !setup.is_last() {
            setup.next();
        }
        setup.next();
        assert_eq!(setup.status, Status::Completed);
        setup.resume("azure-mai");
        assert!(setup.is_open());
        assert_eq!((setup.step, setup.engine.as_str()), (0, "azure-mai"));
    }

    #[test]
    fn removed_or_unknown_engines_fall_back_to_groq() {
        let saved = Onboarding::from_value(
            Some(&json!({"status": "active", "step": "nope", "engine": "webspeech"})),
            "groq",
        );
        assert_eq!((saved.step, saved.engine.as_str()), (0, "groq"));
        assert_eq!(
            Onboarding::from_value(None, "local").status,
            Status::Completed
        );
        let mut setup = saved;
        setup.choose("cuda");
        assert_eq!(setup.engine, "groq");
        setup.back();
        assert_eq!(setup.step, 0);
    }

    #[test]
    fn install_step_waits_for_what_each_engine_needs() {
        let mut ready = Readiness {
            groq_key: " ",
            azure_key: "",
            endpoint: "http://example.com",
            model_downloaded: false,
        };
        assert!(install_blocker("groq", &ready).is_some());
        assert!(install_blocker("local", &ready).is_some());
        assert_eq!(
            install_blocker("azure-mai", &ready),
            Some("The endpoint starts with https://")
        );
        ready.groq_key = "gsk_test";
        ready.endpoint = "https://example.services.ai.azure.com/api/projects/p";
        ready.model_downloaded = true;
        assert_eq!(install_blocker("groq", &ready), None);
        assert_eq!(install_blocker("local", &ready), None);
        assert_eq!(
            install_blocker("azure-mai", &ready),
            Some("Add the API key")
        );
        ready.azure_key = "key";
        assert_eq!(install_blocker("azure-mai", &ready), None);
    }

    #[test]
    fn formats_download_sizes() {
        assert_eq!(megabytes(574_041_195), "547 MB");
        assert_eq!(megabytes(5 * 1024 * 1024 + 100_000), "5.1 MB");
    }
}
