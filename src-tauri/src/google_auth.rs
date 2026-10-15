//! Google OAuth for an installed application using a loopback redirect.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CREDENTIAL_SERVICE: &str = "Annotate";
const REFRESH_TOKEN_USER: &str = "GoogleDriveRefreshToken";
const OAUTH_TIMEOUT: Duration = Duration::from_secs(300);

static OAUTH_ACTIVE: AtomicBool = AtomicBool::new(false);
static OAUTH_CANCELLED: AtomicBool = AtomicBool::new(false);
static OAUTH_URL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static OAUTH_CANCEL: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub(crate) fn reopen_google_oauth() -> Result<(), String> {
    let url = OAUTH_URL
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or("No sign-in is pending")?;
    open::that(url).map_err(|e| e.to_string())
}

struct OAuthActiveGuard;

impl OAuthActiveGuard {
    fn acquire() -> Result<Self, String> {
        OAUTH_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| "Google sign-in is already in progress".to_string())?;
        OAUTH_CANCELLED.store(false, Ordering::SeqCst);
        Ok(Self)
    }
}

impl Drop for OAuthActiveGuard {
    fn drop(&mut self) {
        *OAUTH_URL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        OAUTH_CANCELLED.store(false, Ordering::SeqCst);
        OAUTH_ACTIVE.store(false, Ordering::SeqCst);
    }
}

pub struct OAuthTokenResult {
    pub(crate) access_token: String,
    pub(crate) expires_in: u64,
    pub(crate) refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: Option<String>,
    expires_in: Option<u64>,
    refresh_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

struct AuthorizationCode {
    code: String,
    redirect_uri: String,
}

pub async fn google_oauth(client_id: String, scopes: String) -> Result<OAuthTokenResult, String> {
    validate_client_id(&client_id)?;
    let _guard = OAuthActiveGuard::acquire()?;

    let state = random_base64url(32);
    let verifier = random_base64url(32);
    let challenge = pkce_challenge(&verifier);
    let code = crate::app::spawn_blocking({
        let client_id = client_id.clone();
        let scopes = scopes.clone();
        let state = state.clone();
        move || capture_authorization_code(&client_id, &scopes, &state, &challenge)
    })
    .await
    .map_err(|error| format!("OAuth task failed: {error}"))??;

    let cancelled = OAUTH_CANCEL.notified();
    tokio::pin!(cancelled);
    cancelled.as_mut().enable();
    if OAUTH_CANCELLED.load(Ordering::SeqCst) {
        return Err("oauth_cancelled".into());
    }
    let token = tokio::select! {
        _ = &mut cancelled => return Err("oauth_cancelled".into()),
        token = exchange_authorization_code(&client_id, &code, &verifier) => token?,
    };
    // Native sync persists this only after checking its current session. Saving
    // here would allow sign-out during the exchange to restore the credential.
    token_result(token)
}

pub async fn google_refresh_access_token(client_id: String) -> Result<OAuthTokenResult, String> {
    validate_client_id(&client_id)?;
    let refresh_token = load_refresh_token()?.ok_or("No Google refresh token is stored")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    let body = urlencoded_form(&[
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ]);
    let response = client
        .post(TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|error| format!("Google token refresh failed: {error}"))?;
    token_result(parse_token_response(response).await?)
}

pub fn has_google_refresh_token() -> Result<bool, String> {
    has_refresh_token()
}

pub fn migrate_google_refresh_token(token: String) -> Result<(), String> {
    if token.trim().is_empty() || has_refresh_token()? {
        return Ok(());
    }
    save_refresh_token(token.trim())
}

pub fn clear_google_refresh_token() -> Result<(), String> {
    let entry = refresh_token_entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("Failed to remove Google credential: {error}")),
    }
}

pub(crate) fn store_google_refresh_token(token: &str) -> Result<(), String> {
    if token.trim().is_empty() {
        return Err("Google refresh token is empty".into());
    }
    save_refresh_token(token)
}

pub fn cancel_google_oauth() {
    if OAUTH_ACTIVE.load(Ordering::SeqCst) {
        OAUTH_CANCELLED.store(true, Ordering::SeqCst);
        OAUTH_CANCEL.notify_waiters();
    }
}

fn capture_authorization_code(
    client_id: &str,
    scopes: &str,
    expected_state: &str,
    challenge: &str,
) -> Result<AuthorizationCode, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("OAuth loopback bind failed: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("OAuth listener setup failed: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let auth_url = authorization_url(client_id, scopes, &redirect_uri, expected_state, challenge);
    *OAUTH_URL.lock().unwrap_or_else(|e| e.into_inner()) = Some(auth_url.clone());

    open::that(&auth_url).map_err(|error| format!("Failed to open browser: {error}"))?;
    let deadline = Instant::now() + OAUTH_TIMEOUT;

    while Instant::now() < deadline {
        if OAUTH_CANCELLED.load(Ordering::SeqCst) {
            return Err("oauth_cancelled".into());
        }

        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(error) => return Err(format!("OAuth loopback accept failed: {error}")),
        };

        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buffer = [0_u8; 16_384];
        let read = match stream.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => {
                send_browser_response(&mut stream, false);
                continue;
            }
        };
        let request = String::from_utf8_lossy(&buffer[..read]);
        match authorization_callback(&request, expected_state) {
            Ok(Some(code)) => {
                send_browser_response(&mut stream, true);
                return Ok(AuthorizationCode { code, redirect_uri });
            }
            Ok(None) => send_browser_response(&mut stream, false),
            Err(error) => {
                send_browser_response(&mut stream, false);
                return Err(error);
            }
        }
    }

    Err("Google sign-in timed out".into())
}

async fn exchange_authorization_code(
    client_id: &str,
    code: &AuthorizationCode,
    verifier: &str,
) -> Result<GoogleTokenResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    let body = urlencoded_form(&[
        ("code", code.code.as_str()),
        ("client_id", client_id),
        ("redirect_uri", code.redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier),
    ]);
    let response = client
        .post(TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|error| format!("Google token exchange failed: {error}"))?;
    parse_token_response(response).await
}

fn urlencoded_form(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

async fn parse_token_response(response: reqwest::Response) -> Result<GoogleTokenResponse, String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Failed to read Google token response: {error}"))?;
    let token: GoogleTokenResponse = serde_json::from_str(&body)
        .map_err(|error| format!("Invalid Google token response: {error}"))?;
    if !status.is_success() {
        return Err(token
            .error_description
            .or(token.error)
            .unwrap_or_else(|| format!("Google token request failed: HTTP {status}")));
    }
    Ok(token)
}

fn token_result(token: GoogleTokenResponse) -> Result<OAuthTokenResult, String> {
    Ok(OAuthTokenResult {
        access_token: token
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or("Google token response did not include an access token")?,
        expires_in: token.expires_in.unwrap_or(3600),
        refresh_token: token.refresh_token,
    })
}

fn authorization_url(
    client_id: &str,
    scopes: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}&code_challenge={}&code_challenge_method=S256",
        urlenc(client_id),
        urlenc(redirect_uri),
        urlenc(scopes),
        urlenc(state),
        urlenc(challenge),
    )
}

fn validate_client_id(client_id: &str) -> Result<(), String> {
    if client_id.trim().is_empty() {
        Err("Google OAuth client ID is not configured".into())
    } else {
        Ok(())
    }
}

fn random_base64url(byte_count: usize) -> String {
    let mut bytes = vec![0_u8; byte_count];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn refresh_token_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(CREDENTIAL_SERVICE, REFRESH_TOKEN_USER)
        .map_err(|error| format!("Google credential entry failed: {error}"))
}

fn save_refresh_token(token: &str) -> Result<(), String> {
    refresh_token_entry()?
        .set_password(token)
        .map_err(|error| format!("Failed to store Google credential: {error}"))
}

fn load_refresh_token() -> Result<Option<String>, String> {
    match refresh_token_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Failed to load Google credential: {error}")),
    }
}

fn has_refresh_token() -> Result<bool, String> {
    Ok(load_refresh_token()?.is_some())
}

fn send_browser_response(stream: &mut std::net::TcpStream, success: bool) {
    let (status, message) = if success {
        (
            "200 OK",
            "Sign-in complete. You can close this tab and return to Annotate.",
        )
    } else {
        (
            "400 Bad Request",
            "This sign-in response was not accepted. Return to Annotate and try again.",
        )
    };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Annotate</title></head><body><main><p>{message}</p></main></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\nConnection: close\r\n\r\n{html}",
        html.len(),
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn urlenc(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn extract_query_param(request: &str, param: &str) -> Option<String> {
    let first_line = request.lines().next()?;
    let path = first_line.split_whitespace().nth(1)?;
    let query = path.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(key, value)| (key == param).then(|| value.into_owned()))
}

fn authorization_callback(request: &str, expected_state: &str) -> Result<Option<String>, String> {
    let mut first_line = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if first_line.next() != Some("GET")
        || !first_line.next().is_some_and(|path| path.starts_with("/?"))
    {
        return Ok(None);
    }
    // Even an OAuth error must belong to this flow. Unrelated requests such as a
    // favicon or a forged error cannot cancel a legitimate pending sign-in.
    if extract_query_param(request, "state").as_deref() != Some(expected_state) {
        return Ok(None);
    }
    if let Some(error) = extract_query_param(request, "error") {
        return Err(format!("Google OAuth failed: {error}"));
    }
    Ok(extract_query_param(request, "code").filter(|code| !code.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_rfc_7636_s256_challenge() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorization_url_contains_state_and_pkce() {
        let url = authorization_url(
            "client",
            "scope",
            "http://127.0.0.1:1234",
            "state",
            "challenge",
        );
        assert!(url.contains("state=state"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(!url.contains("client_secret"));
    }

    #[test]
    fn extracts_and_decodes_query_parameters() {
        let request = "GET /?code=hello%20world&state=a%2Bb HTTP/1.1\r\nHost: 127.0.0.1\r\n";
        assert_eq!(
            extract_query_param(request, "code").as_deref(),
            Some("hello world")
        );
        assert_eq!(
            extract_query_param(request, "state").as_deref(),
            Some("a+b")
        );
    }

    #[test]
    fn oauth_errors_require_the_current_state_and_unicode_queries_do_not_panic() {
        assert_eq!(
            authorization_callback("GET /?error=access_denied&state=wrong HTTP/1.1", "current")
                .unwrap(),
            None
        );
        assert_eq!(
            authorization_callback("GET /?code=valid&state=current HTTP/1.1", "current").unwrap(),
            Some("valid".into())
        );
        assert!(authorization_callback(
            "GET /?error=access_denied&state=current HTTP/1.1",
            "current"
        )
        .is_err());
        assert_eq!(
            authorization_callback("POST /?code=valid&state=current HTTP/1.1", "current").unwrap(),
            None
        );
        assert_eq!(
            extract_query_param("GET /?code=%é&state=current HTTP/1.1", "code").as_deref(),
            Some("%é")
        );
    }
}
