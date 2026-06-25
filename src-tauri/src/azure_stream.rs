use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};

const VOICE_LIVE_API_VERSION: &str = "2026-04-10";
const VOICE_LIVE_SESSION_MODEL: &str = "gpt-4.1";
const VOICE_LIVE_TRANSCRIPTION_MODEL: &str = "mai-transcribe-1.5";

struct AzureStreamSession {
    sender: mpsc::UnboundedSender<Message>,
    result: Option<oneshot::Receiver<Result<String, String>>>,
}

static SESSION: LazyLock<Mutex<Option<AzureStreamSession>>> = LazyLock::new(|| Mutex::new(None));

pub async fn start(endpoint: &str, api_key: &str, language: Option<&str>) -> Result<(), String> {
    cancel().await;

    let url = voice_live_url(endpoint)?;
    let mut request = url
        .into_client_request()
        .map_err(|e| format!("Invalid Voice Live request: {e}"))?;
    request.headers_mut().insert(
        "api-key",
        HeaderValue::from_str(api_key).map_err(|e| format!("Invalid API key header: {e}"))?,
    );

    let connect_start = Instant::now();
    let (stream, response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("Voice Live connection failed: {e}"))?;
    log::info!(
        "[Timing][azure-mai-stream] websocket connect: {:.1}ms status={} model={} transcription_model={}",
        connect_start.elapsed().as_secs_f64() * 1000.0,
        response.status(),
        VOICE_LIVE_SESSION_MODEL,
        VOICE_LIVE_TRANSCRIPTION_MODEL
    );

    let (mut write, mut read) = stream.split();
    let mut transcription = json!({
        "model": VOICE_LIVE_TRANSCRIPTION_MODEL,
    });
    if let Some(language) = language.map(str::trim).filter(|value| !value.is_empty()) {
        transcription["language"] = Value::String(language.to_string());
    }

    let update = json!({
        "type": "session.update",
        "session": {
            "modalities": ["text"],
            "input_audio_format": "pcm16",
            "input_audio_sampling_rate": 16000,
            "input_audio_transcription": transcription,
            "turn_detection": Value::Null,
        }
    });
    write
        .send(Message::Text(update.to_string().into()))
        .await
        .map_err(|e| format!("Failed to configure Voice Live session: {e}"))?;

    let ready_start = Instant::now();
    let ready_result: Result<(), String> = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = read.next().await {
            let message = message.map_err(|e| format!("Voice Live setup read failed: {e}"))?;
            let Some(event) = message_json(&message)? else {
                continue;
            };

            match event_type(&event) {
                Some("session.updated") => return Ok(()),
                Some("error") => return Err(event_error(&event)),
                _ => {}
            }
        }

        Err("Voice Live closed before session setup completed".to_string())
    })
    .await
    .map_err(|_| "Voice Live session setup timed out".to_string())?;
    ready_result?;
    log::info!(
        "[Timing][azure-mai-stream] session ready: {:.1}ms language={}",
        ready_start.elapsed().as_secs_f64() * 1000.0,
        language.unwrap_or("auto")
    );

    let (sender, mut outbound) = mpsc::unbounded_channel::<Message>();
    let (result_sender, result_receiver) = oneshot::channel::<Result<String, String>>();

    tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            let is_close = matches!(message, Message::Close(_));
            if let Err(err) = write.send(message).await {
                log::warn!("[AzureStream] websocket write failed: {err}");
                break;
            }
            if is_close {
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut result_sender = Some(result_sender);
        while let Some(message) = read.next().await {
            let event = match message {
                Ok(message) => match message_json(&message) {
                    Ok(Some(event)) => event,
                    Ok(None) => continue,
                    Err(err) => {
                        send_result(&mut result_sender, Err(err));
                        return;
                    }
                },
                Err(err) => {
                    send_result(
                        &mut result_sender,
                        Err(format!("Voice Live websocket read failed: {err}")),
                    );
                    return;
                }
            };

            match event_type(&event) {
                Some("conversation.item.input_audio_transcription.delta") => {
                    if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                        log::debug!("[AzureStream] transcript delta: {delta:?}");
                    }
                }
                Some("conversation.item.input_audio_transcription.completed") => {
                    let transcript = event
                        .get("transcript")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    send_result(&mut result_sender, Ok(transcript));
                    return;
                }
                Some("conversation.item.input_audio_transcription.failed") | Some("error") => {
                    send_result(&mut result_sender, Err(event_error(&event)));
                    return;
                }
                _ => {}
            }
        }

        send_result(
            &mut result_sender,
            Err("Voice Live closed before returning a transcript".to_string()),
        );
    });

    *SESSION.lock().await = Some(AzureStreamSession {
        sender,
        result: Some(result_receiver),
    });
    Ok(())
}

pub async fn send_audio(audio_base64: &str) -> Result<(), String> {
    let sender = SESSION
        .lock()
        .await
        .as_ref()
        .map(|session| session.sender.clone())
        .ok_or("Voice Live session is not active")?;
    let event = json!({
        "type": "input_audio_buffer.append",
        "audio": audio_base64,
    });
    sender
        .send(Message::Text(event.to_string().into()))
        .map_err(|_| "Voice Live writer stopped".to_string())
}

pub async fn finish() -> Result<String, String> {
    let mut session = SESSION
        .lock()
        .await
        .take()
        .ok_or("Voice Live session is not active")?;
    let result = session
        .result
        .take()
        .ok_or("Voice Live result receiver is missing")?;

    session
        .sender
        .send(Message::Text(
            json!({ "type": "input_audio_buffer.commit" })
                .to_string()
                .into(),
        ))
        .map_err(|_| "Voice Live writer stopped before commit".to_string())?;

    let finish_start = Instant::now();
    let outcome = match tokio::time::timeout(Duration::from_secs(15), result).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Voice Live result worker stopped".to_string()),
        Err(_) => Err("Voice Live transcription timed out".to_string()),
    };
    log::info!(
        "[Timing][azure-mai-stream] commit-to-transcript: {:.1}ms status={}",
        finish_start.elapsed().as_secs_f64() * 1000.0,
        if outcome.is_ok() { "ok" } else { "err" }
    );

    let _ = session.sender.send(Message::Close(None));
    outcome
}

pub async fn cancel() {
    if let Some(session) = SESSION.lock().await.take() {
        let _ = session.sender.send(Message::Text(
            json!({ "type": "input_audio_buffer.clear" })
                .to_string()
                .into(),
        ));
        let _ = session.sender.send(Message::Close(None));
    }
}

fn voice_live_url(endpoint: &str) -> Result<String, String> {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Azure Foundry endpoint is required".to_string());
    }

    let mut url = reqwest::Url::parse(trimmed)
        .map_err(|error| format!("Invalid Azure Foundry endpoint: {error}"))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("Azure Foundry endpoint must use HTTPS".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Azure Foundry endpoint must not include credentials".to_string());
    }

    url.set_scheme("wss")
        .map_err(|_| "Failed to create secure Azure WebSocket URL".to_string())?;
    url.set_path("/voice-live/realtime");
    url.set_query(None);
    url.set_fragment(None);
    url.query_pairs_mut()
        .append_pair("api-version", VOICE_LIVE_API_VERSION)
        .append_pair("model", VOICE_LIVE_SESSION_MODEL);
    Ok(url.into())
}

fn message_json(message: &Message) -> Result<Option<Value>, String> {
    match message {
        Message::Text(text) => serde_json::from_str(text.as_str())
            .map(Some)
            .map_err(|e| format!("Invalid Voice Live event JSON: {e}")),
        Message::Close(frame) => Err(format!("Voice Live closed the connection: {frame:?}")),
        _ => Ok(None),
    }
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

fn event_error(event: &Value) -> String {
    event
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| event.get("message").and_then(Value::as_str))
        .unwrap_or("Voice Live returned an unknown error")
        .to_string()
}

fn send_result(
    sender: &mut Option<oneshot::Sender<Result<String, String>>>,
    result: Result<String, String>,
) {
    if let Some(sender) = sender.take() {
        let _ = sender.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_project_endpoint_to_voice_live_websocket() {
        let url =
            voice_live_url("https://contoso.services.ai.azure.com/api/projects/speech-project")
                .unwrap();

        assert_eq!(
            url,
            "wss://contoso.services.ai.azure.com/voice-live/realtime?api-version=2026-04-10&model=gpt-4.1"
        );
    }

    #[test]
    fn rejects_insecure_voice_live_endpoint() {
        let error = voice_live_url("http://contoso.services.ai.azure.com/api/projects/test")
            .unwrap_err();
        assert!(error.contains("HTTPS"));
    }
}
