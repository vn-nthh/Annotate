//! Google OAuth loopback redirect flow.
//!
//! Opens the system browser for Google consent, captures the auth code
//! via a local TCP listener on a random port.
//!
//! Supports cancellation: `cancel_google_oauth` connects to the waiting port
//! to unblock `accept()`. A 5-minute timeout does the same automatically.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;

/// The port currently listening for an OAuth redirect (None if not active).
static PENDING_PORT: Mutex<Option<u16>> = Mutex::new(None);

/// Result of the OAuth flow — auth code + redirect URI for the frontend to exchange.
#[derive(serde::Serialize)]
pub struct OAuthCodeResult {
    code: String,
    redirect_uri: String,
}

/// Starts a local TCP listener, opens the system browser to Google OAuth,
/// waits for the redirect, and returns the auth code.
///
/// Can be cancelled at any time by calling `cancel_google_oauth()`.
/// Times out automatically after 5 minutes.
#[tauri::command]
pub async fn google_oauth(client_id: String, scopes: String) -> Result<OAuthCodeResult, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<OAuthCodeResult, String> {
        // 1. Bind to a random port on loopback
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("TCP bind failed: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        // Register the active port so cancel / timeout can reach us
        *PENDING_PORT.lock().unwrap() = Some(port);
        log::info!("[OAuth] Listening on {redirect_uri}");

        // 2. Build the Google OAuth URL
        let auth_url = format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
            urlenc(&client_id),
            urlenc(&redirect_uri),
            urlenc(&scopes),
        );

        // 3. Open system browser
        open::that(&auth_url).map_err(|e| format!("Failed to open browser: {e}"))?;
        log::info!("[OAuth] Browser opened, waiting for redirect...");

        // 4. Timeout thread — self-connects after 5 minutes to unblock accept()
        {
            let port = port;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(300));
                let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
            });
        }

        // 5. Accept the redirect connection (blocks until browser redirects, cancelled, or timeout)
        let (mut stream, _) = listener.accept().map_err(|e| format!("Accept failed: {e}"))?;

        // Clear the pending port — we're no longer listening
        *PENDING_PORT.lock().unwrap() = None;

        let mut buf = vec![0u8; 8192];
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("Read failed: {e}"))?;
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        // 6. Extract the authorization code from GET /?code=...
        //    If there's no code, this was a cancel/timeout self-connect.
        let code = extract_query_param(&request, "code").ok_or_else(|| {
            let error = extract_query_param(&request, "error").unwrap_or_default();
            if error.is_empty() {
                "oauth_cancelled".to_string()
            } else {
                format!("OAuth failed: {error}")
            }
        })?;

        log::info!("[OAuth] Got auth code");

        // 7. Send a "close this tab" page back to the browser
        let html = r#"<!DOCTYPE html>
<html><head><title>Annotate</title>
<style>body{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#faf9f6;color:#1a1a1a}
.card{text-align:center;padding:2rem}.ok{font-size:2rem;margin-bottom:1rem}</style></head>
<body><div class="card"><div class="ok">Done</div><p>You can close this tab and return to Annotate.</p></div></body></html>"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        drop(stream);
        drop(listener);

        Ok(OAuthCodeResult {
            code,
            redirect_uri,
        })
    })
    .await
    .map_err(|e| format!("Task join error: {e}"))?
}

/// Cancel a pending OAuth flow by self-connecting to the listener.
/// Safe to call even if no flow is active.
#[tauri::command]
pub fn cancel_google_oauth() {
    if let Some(port) = *PENDING_PORT.lock().unwrap() {
        log::info!("[OAuth] Cancelling pending auth on port {port}");
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
    }
}

/// Minimal URL encoding for query parameters.
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

/// Extract a query parameter value from a raw HTTP request line.
fn extract_query_param(request: &str, param: &str) -> Option<String> {
    let first_line = request.lines().next()?;
    let path = first_line.split_whitespace().nth(1)?;
    let query = path.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(key), Some(val)) = (kv.next(), kv.next()) {
            if key == param {
                return Some(urldec(val));
            }
        }
    }
    None
}

/// Minimal URL decoding.
fn urldec(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
