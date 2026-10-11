//! Native Google Drive sync. Credentials stay in the existing Windows keyring.
use crate::{
    native_app,
    native_core::get,
    native_data::{Dictionary, History, Store},
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const DRIVE: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
const SCOPES: &str = "https://www.googleapis.com/auth/drive.appdata https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/userinfo.email";
const CANCELLED: &str = "oauth_cancelled";
const MAX_AVATAR_BYTES: usize = 512 * 1024;

fn client_id() -> Result<String, String> {
    serde_json::from_str::<Value>(include_str!(concat!(env!("OUT_DIR"), "/oauth-config.json")))
        .ok()
        .and_then(|v| v["client_id"].as_str().filter(|v| !v.is_empty()).map(str::to_owned))
        .ok_or("Google Sign-In is not configured. Place oauth.config.json in the src/ folder. See oauth.config.json.example.".into())
}

#[derive(Default)]
struct Session {
    generation: u64,
    token: Option<(String, Instant)>,
    signing_in: Option<u64>,
}
impl Session {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.token = None;
        self.signing_in = None;
    }
    fn check(&self, generation: u64) -> Result<(), String> {
        if self.generation == generation {
            Ok(())
        } else {
            Err(CANCELLED.into())
        }
    }
    fn accepts_avatar(&self, generation: u64, url: &str, current_url: Option<&str>) -> bool {
        self.check(generation).is_ok() && self.token.is_some() && current_url == Some(url)
    }
    fn accept_token(
        &mut self,
        generation: u64,
        token: crate::google_auth::OAuthTokenResult,
    ) -> Result<String, String> {
        self.check(generation)?;
        let access = token.access_token.clone();
        self.token = Some((
            token.access_token,
            Instant::now() + Duration::from_secs(token.expires_in.saturating_sub(60)),
        ));
        Ok(access)
    }
}

#[derive(Default)]
pub struct Sync {
    session: Mutex<Session>,
    refresh: tokio::sync::Mutex<()>,
    operation: tokio::sync::Mutex<()>,
    invalidated: tokio::sync::Notify,
    heartbeat_started: AtomicBool,
    status_generation: AtomicU64,
}
impl Sync {
    fn session(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn migrate_saved_credentials(&self) -> Result<(), String> {
        let session = self.session();
        let store = get().app.store("settings.json")?;
        // Sign-out may be clicked before the background bootstrap reaches this
        // migration. Never restore its legacy credential after that invalidation.
        if session.generation == 0 {
            if let Some(token) = store
                .get("gd_refresh_token")
                .and_then(|value| value.as_str().map(str::to_owned))
            {
                crate::google_auth::migrate_google_refresh_token(token)?;
            }
        }
        store.update_and_save(|values| {
            for key in ["gd_refresh_token", "gd_access_token", "gd_token_expiry"] {
                values.remove(key);
            }
        })
    }

    pub async fn initialize(&self) {
        // Keep the daily heartbeat available when signing in after startup.
        if !self.heartbeat_started.swap(true, Ordering::AcqRel) {
            native_app::spawn(async {
                loop {
                    tokio::time::sleep(Duration::from_secs(86400)).await;
                    get().sync.sync_now().await;
                }
            });
        }
        let generation = self.session().generation;
        match crate::google_auth::has_google_refresh_token() {
            Ok(false) => {
                let session = self.session();
                if session.check(generation).is_ok() && session.signing_in.is_none() {
                    get().change(|s| {
                        s.user = None;
                        s.avatar = None;
                        s.avatar_url = None;
                    });
                }
                return;
            }
            Err(error) => {
                get().status(&error, true);
                return;
            }
            Ok(true) => {}
        }
        if let Err(error) = self.valid_token(generation).await {
            let session = self.session();
            if session.check(generation).is_err() {
                return;
            }
            get().change(|s| {
                s.avatar = None;
                s.avatar_url = None;
            });
            if error != CANCELLED {
                get().status(&error, true);
            }
            return;
        }
        self.fetch_avatar(generation);
        self.sync_now().await;
    }

    fn fetch_avatar(&self, generation: u64) {
        if std::env::var_os("ANNOTATE_UI_TEST").is_some() {
            return;
        }
        let url = {
            let session = self.session();
            if session.check(generation).is_err() || session.token.is_none() {
                return;
            }
            let mut state = get().state.lock().unwrap_or_else(|e| e.into_inner());
            let url = picture_url(state.user.as_ref());
            state.avatar = None;
            state.avatar_url = url.clone();
            url
        };
        crate::native_ui::refresh();
        let Some(url) = url else { return };
        // Avatar downloads run outside refresh/Drive locks and never delay sync.
        native_app::spawn(async move {
            let sync = &get().sync;
            let cancelled = sync.invalidated.notified();
            tokio::pin!(cancelled);
            cancelled.as_mut().enable();
            if sync.session().check(generation).is_err() {
                return;
            }
            let result = tokio::select! {
                _ = &mut cancelled => return,
                result = download_avatar(&url) => result,
            };
            let Ok(bytes) = result else {
                log::debug!("Profile avatar download failed; sync remains available");
                return;
            };
            let session = sync.session();
            let mut state = get().state.lock().unwrap_or_else(|e| e.into_inner());
            if session.accepts_avatar(generation, &url, state.avatar_url.as_deref())
                && picture_url(state.user.as_ref()).as_deref() == Some(url.as_str())
            {
                state.avatar = Some(Arc::new(bytes));
                drop(state);
                crate::native_ui::refresh();
            }
        });
    }

    async fn valid_token(&self, generation: u64) -> Result<String, String> {
        // The session lock never spans an await: sign-out can invalidate even a
        // slow refresh immediately, without a late response restoring its token.
        let _refresh = self.refresh.lock().await;
        {
            let session = self.session();
            session.check(generation)?;
            if let Some((token, expiry)) = &session.token {
                if *expiry > Instant::now() {
                    return Ok(token.clone());
                }
            }
        }
        let token = crate::google_auth::google_refresh_access_token(client_id()?).await?;
        let mut session = self.session();
        session.check(generation)?;
        if let Some(refresh_token) = token.refresh_token.as_deref() {
            crate::google_auth::store_google_refresh_token(refresh_token)?;
        }
        session.accept_token(generation, token)
    }

    pub async fn sign_in(&self) {
        let generation = {
            let mut session = self.session();
            if session.signing_in.is_some() || get().snapshot().user.is_some() {
                return;
            }
            session.invalidate();
            let generation = session.generation;
            session.signing_in = Some(generation);
            get().change(|s| s.auth_waiting = true);
            generation
        };
        let result = async {
            let token = crate::google_auth::google_oauth(client_id()?, SCOPES.into()).await?;
            let user = reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|e| e.to_string())?
                .get("https://www.googleapis.com/oauth2/v2/userinfo")
                .bearer_auth(&token.access_token)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .json::<Value>()
                .await
                .map_err(|e| e.to_string())?;
            // Serialize credential/profile persistence with sign-out.
            let mut session = self.session();
            session.check(generation)?;
            let refresh_token = token
                .refresh_token
                .as_deref()
                .ok_or("Google did not return a refresh token; revoke access and sign in again")?;
            crate::google_auth::store_google_refresh_token(refresh_token)?;
            let store = get()
                .app
                .store("settings.json")
                .map_err(|e| e.to_string())?;
            if let Err(error) = store.set_and_save("gd_user_info", user.clone()) {
                let _ = crate::google_auth::clear_google_refresh_token();
                return Err(error);
            }
            session.accept_token(generation, token)?;
            get().change(|s| s.user = Some(user));
            Ok::<(), String>(())
        }
        .await;
        {
            let mut session = self.session();
            if session.signing_in == Some(generation) {
                session.signing_in = None;
                get().change(|s| s.auth_waiting = false);
            }
            if session.check(generation).is_err() {
                return;
            }
        }
        match result {
            Ok(()) => {
                self.fetch_avatar(generation);
                self.sync_now().await;
            }
            Err(error) if error == CANCELLED => {}
            Err(error) => get().status(&error, true),
        }
    }

    pub fn cancel_sign_in(&self) {
        let mut session = self.session();
        if session.signing_in.is_none() {
            return;
        }
        session.invalidate();
        self.invalidated.notify_waiters();
        crate::google_auth::cancel_google_oauth();
        get().change(|s| s.auth_waiting = false);
    }

    pub fn shutdown(&self) {
        self.session().invalidate();
        self.invalidated.notify_waiters();
        crate::google_auth::cancel_google_oauth();
    }

    pub fn sign_out(&self) {
        let error = {
            let mut session = self.session();
            session.invalidate();
            self.invalidated.notify_waiters();
            self.status_generation.fetch_add(1, Ordering::AcqRel);
            crate::google_auth::cancel_google_oauth();
            let credential = crate::google_auth::clear_google_refresh_token();
            let profile = get().app.store("settings.json").and_then(|store| {
                store.update_and_save(|values| {
                    for key in [
                        "gd_user_info",
                        "gd_refresh_token",
                        "gd_access_token",
                        "gd_token_expiry",
                    ] {
                        values.remove(key);
                    }
                })
            });
            get().change(|s| {
                s.user = None;
                s.avatar = None;
                s.avatar_url = None;
                s.auth_waiting = false;
                s.busy.remove("sync-now-btn");
                s.labels.remove("sync-now-text");
            });
            credential.and(profile).err()
        };
        if let Some(error) = error {
            get().status(&error, true);
        }
    }

    pub async fn sync_now(&self) {
        let generation = self.session().generation;
        // Queue an edit whose debounce expires during an upload; try_lock would
        // silently discard that update.
        let _operation = self.operation.lock().await;
        let cancelled = self.invalidated.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        {
            let session = self.session();
            if session.check(generation).is_err() {
                return;
            }
            let state = get().snapshot();
            if state.user.is_none() || !state.data.data.migrated {
                return;
            }
            get().change(|s| {
                s.busy.insert("sync-now-btn".into());
                s.labels.insert("sync-now-text".into(), "Sync Now".into());
            });
        }
        let result = tokio::select! {
            _ = &mut cancelled => Err(CANCELLED.to_string()),
            result = self.sync_round(generation) => result,
        };
        let status_generation = self.status_generation.fetch_add(1, Ordering::AcqRel) + 1;
        {
            let session = self.session();
            if session.check(generation).is_err() {
                return;
            }
            get().change(|s| {
                s.busy.remove("sync-now-btn");
                s.labels.insert(
                    "sync-now-text".into(),
                    if result.is_ok() { "Synced" } else { "Retry" }.into(),
                );
            });
        }
        let delay = if result.is_ok() { 15 } else { 3 };
        if let Err(error) = result {
            get().status(&error, true);
        }
        native_app::spawn(async move {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            let sync = &get().sync;
            let session = sync.session();
            if session.check(generation).is_ok()
                && sync.status_generation.load(Ordering::Acquire) == status_generation
            {
                get().change(|s| {
                    if !s.busy.contains("sync-now-btn") {
                        s.labels.insert("sync-now-text".into(), "Sync Now".into());
                    }
                });
            }
        });
    }

    async fn sync_round(&self, generation: u64) -> Result<(), String> {
        let token = self.valid_token(generation).await?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        for (name, dictionary) in [
            ("annotate_dictionary.json", true),
            ("annotate_history.json", false),
        ] {
            let files = client
                .get(DRIVE)
                .bearer_auth(&token)
                .query(&[
                    ("spaces", "appDataFolder"),
                    ("q", &format!("name = '{name}' and trashed = false")),
                    ("fields", "files(id,name)"),
                    ("pageSize", "1"),
                ])
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .json::<Value>()
                .await
                .map_err(|e| e.to_string())?;
            let id = files["files"]
                .as_array()
                .and_then(|files| files.first())
                .and_then(|file| file["id"].as_str());
            let remote = if let Some(id) = id {
                client
                    .get(format!("{DRIVE}/{id}"))
                    .bearer_auth(&token)
                    .query(&[("alt", "media")])
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?
                    .json::<Value>()
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                Value::Null
            };
            let data = {
                let session = self.session();
                session.check(generation)?;
                // Read the current store after the network await, retaining edits
                // made during the fetch and rolling back any failed disk save.
                let mut state = get().state.lock().unwrap_or_else(|e| e.into_inner());
                merge_remote(&mut state.data, &remote, dictionary)?
            };
            crate::native_ui::refresh();
            self.session().check(generation)?;
            let nonempty = data
                .get(if dictionary { "terms" } else { "entries" })
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
                || data["deleted"].as_array().is_some_and(|v| !v.is_empty());
            if let Some(id) = id {
                client
                    .patch(format!("{UPLOAD}/{id}"))
                    .query(&[("uploadType", "media")])
                    .bearer_auth(&token)
                    .json(&data)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
            } else if nonempty {
                let boundary = format!("annotate_boundary_{}", crate::native_data::now());
                let metadata = json!({"name": name, "parents": ["appDataFolder"]});
                let body = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/json\r\n\r\n{data}\r\n--{boundary}--");
                client
                    .post(UPLOAD)
                    .query(&[("uploadType", "multipart")])
                    .bearer_auth(&token)
                    .header(
                        "Content-Type",
                        format!("multipart/related; boundary={boundary}"),
                    )
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
}

fn picture_url(user: Option<&Value>) -> Option<String> {
    let url = reqwest::Url::parse(user?["picture"].as_str()?).ok()?;
    (url.scheme() == "https" && url.username().is_empty() && url.password().is_none())
        .then(|| url.to_string())
}

fn append_avatar_chunk(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<(), String> {
    if chunk.len() > MAX_AVATAR_BYTES.saturating_sub(bytes.len()) {
        return Err("Profile avatar exceeds 512 KiB".into());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

async fn download_avatar(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_AVATAR_BYTES as u64)
    {
        return Err("Profile avatar exceeds 512 KiB".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        append_avatar_chunk(&mut bytes, &chunk)?;
    }
    if bytes.is_empty() {
        return Err("Profile avatar response was empty".into());
    }
    Ok(bytes)
}

fn merge_remote(store: &mut Store, remote: &Value, dictionary: bool) -> Result<Value, String> {
    let previous = store.data.clone();
    let value = if dictionary {
        store.data.dictionary = store.data.dictionary.merge(&Dictionary::parse(remote));
        serde_json::to_value(&store.data.dictionary)
    } else {
        store.data.history = store.data.history.merge(&History::parse(remote));
        serde_json::to_value(&store.data.history)
    }
    .map_err(|e| e.to_string())?;
    if let Err(error) = store.save() {
        store.data = previous;
        return Err(error);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_rejects_insecure_urls_and_oversized_chunked_bodies() {
        let secure = "https://lh3.googleusercontent.com/example=s64-c";
        assert_eq!(
            picture_url(Some(&json!({"picture": secure}))).as_deref(),
            Some(secure)
        );
        for invalid in [
            "http://example.com/avatar.png",
            "file:///C:/avatar.png",
            "https://user:password@example.com/avatar.png",
            "",
        ] {
            assert!(picture_url(Some(&json!({"picture": invalid}))).is_none());
        }
        let mut bytes = vec![0; MAX_AVATAR_BYTES - 1];
        append_avatar_chunk(&mut bytes, &[1]).unwrap();
        assert!(append_avatar_chunk(&mut bytes, &[2]).is_err());
        assert_eq!(bytes.len(), MAX_AVATAR_BYTES);
        assert_eq!(bytes.last(), Some(&1));
    }

    #[test]
    fn old_avatar_is_rejected_after_sign_out_and_another_sign_in() {
        let url = "https://example.com/avatar.png";
        let mut session = Session {
            token: Some((
                "fake token".into(),
                Instant::now() + Duration::from_secs(60),
            )),
            ..Session::default()
        };
        let generation = session.generation;
        assert!(session.accepts_avatar(generation, url, Some(url)));
        assert!(!session.accepts_avatar(generation, url, Some("https://example.com/new.png")));
        session.invalidate();
        assert!(!session.accepts_avatar(generation, url, Some(url)));
        session.token = Some((
            "new fake token".into(),
            Instant::now() + Duration::from_secs(60),
        ));
        assert!(!session.accepts_avatar(generation, url, Some(url)));
        assert!(session.accepts_avatar(session.generation, url, Some(url)));
    }

    #[test]
    fn sign_out_rejects_a_late_token_response() {
        let mut session = Session::default();
        let generation = session.generation;
        session.invalidate();
        let token = crate::google_auth::OAuthTokenResult {
            access_token: "late access token".into(),
            expires_in: 3600,
            refresh_token: None,
        };
        assert_eq!(
            session.accept_token(generation, token).unwrap_err(),
            CANCELLED
        );
        assert!(session.token.is_none());
        let token = crate::google_auth::OAuthTokenResult {
            access_token: "current access token".into(),
            expires_in: 3600,
            refresh_token: None,
        };
        assert_eq!(
            session.accept_token(session.generation, token).unwrap(),
            "current access token"
        );
    }

    #[test]
    fn remote_merge_keeps_edits_made_during_fetch_and_rolls_back_failed_save() {
        let directory = std::env::temp_dir().join(format!(
            "annotate-sync-test-{}-{}",
            std::process::id(),
            crate::native_data::now()
        ));
        let mut store = Store::open(&directory).unwrap();
        let remote = json!({"terms":[{"value":"Remote term","updatedAt":1}]});
        store.data.dictionary.add("Added while fetching");
        let upload = merge_remote(&mut store, &remote, true).unwrap();
        assert_eq!(upload["terms"].as_array().unwrap().len(), 2);
        assert_eq!(
            Store::open(&directory).unwrap().data.dictionary,
            store.data.dictionary
        );

        let previous = store.data.dictionary.clone();
        std::fs::remove_file(directory.join("native-data.json")).unwrap();
        std::fs::remove_dir(&directory).unwrap();
        std::fs::write(&directory, "This file prevents directory creation").unwrap();
        assert!(merge_remote(&mut store, &json!(["A failed remote addition"]), true).is_err());
        assert_eq!(store.data.dictionary, previous);
        std::fs::remove_file(directory).unwrap();
    }
}
