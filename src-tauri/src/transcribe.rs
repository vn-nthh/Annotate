use base64::Engine;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Instant;

/// Shared HTTP client — reuses connections across all transcription requests.
/// Creating a new `reqwest::Client` per call leaks socket handles and TLS state.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("Failed to build HTTP client")
});

// ── Whisper anti-hallucination thresholds ───────────────
// These mirror the defaults used in openai/whisper's decode logic.
const NO_SPEECH_PROB_THRESHOLD: f64 = 0.6;
const AVG_LOGPROB_THRESHOLD: f64 = -1.0;
const COMPRESSION_RATIO_THRESHOLD: f64 = 2.4;
const AZURE_SPEECH_API_VERSION: &str = "2025-10-15";
const AZURE_MAI_DEFAULT_MODEL: &str = "mai-transcribe-1.5";

#[derive(Deserialize, Debug)]
struct VerboseResponse {
    segments: Option<Vec<Segment>>,
}

#[derive(Deserialize, Debug)]
struct Segment {
    text: String,
    #[serde(default)]
    start: f64,
    #[serde(default)]
    end: f64,
    #[serde(default)]
    no_speech_prob: f64,
    #[serde(default)]
    avg_logprob: f64,
    #[serde(default)]
    compression_ratio: f64,
}

#[derive(Serialize, Debug)]
struct AzureTranscriptionDefinition {
    #[serde(skip_serializing_if = "Option::is_none")]
    locales: Option<Vec<String>>,
    #[serde(rename = "phraseList", skip_serializing_if = "Option::is_none")]
    phrase_list: Option<AzurePhraseList>,
    #[serde(rename = "enhancedMode")]
    enhanced_mode: AzureEnhancedMode,
}

#[derive(Serialize, Debug)]
struct AzurePhraseList {
    phrases: Vec<String>,
}

#[derive(Serialize, Debug)]
struct AzureEnhancedMode {
    enabled: bool,
    model: String,
}

#[derive(Deserialize, Debug)]
struct AzureTranscriptionResponse {
    #[serde(default, rename = "combinedPhrases")]
    combined_phrases: Vec<AzureCombinedPhrase>,
    #[serde(default)]
    phrases: Vec<AzurePhrase>,
}

#[derive(Deserialize, Debug)]
struct AzureCombinedPhrase {
    text: String,
}

#[derive(Deserialize, Debug)]
struct AzurePhrase {
    text: String,
    #[serde(default, rename = "offsetMilliseconds")]
    offset_milliseconds: f64,
    #[serde(default, rename = "durationMilliseconds")]
    duration_milliseconds: f64,
}

/// Transcribe audio using Groq's Whisper Large V3 Turbo API
/// with anti-hallucination filtering on the returned segments.
pub async fn transcribe_with_groq(
    audio_base64: &str,
    api_key: &str,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Decode base64 audio
    let audio_bytes = base64::engine::general_purpose::STANDARD.decode(audio_base64)?;

    // Build multipart form — request verbose_json to get per-segment metadata
    let audio_part = multipart::Part::bytes(audio_bytes)
        .file_name("audio.webm")
        .mime_str("audio/webm")?;

    let mut form = multipart::Form::new()
        .part("file", audio_part)
        .text("model", "whisper-large-v3-turbo")
        .text("response_format", "verbose_json");

    let language = language.map(str::trim).filter(|value| !value.is_empty());
    if let Some(language) = language {
        form = form.text("language", language.to_string());
    }
    log::info!(
        "[Groq] transcription language={}",
        language.unwrap_or("auto")
    );

    // Add dictionary terms as initial_prompt for improved accuracy
    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            form = form.text("prompt", prompt.to_string());
        }
    }

    // Send to Groq API
    let response = HTTP_CLIENT
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Groq API error {}: {}", status, body).into());
    }

    let result: VerboseResponse = response.json().await?;

    // Filter segments using official Whisper anti-hallucination logic
    // (see openai/whisper transcribe.py lines 304-316)
    let text = match result.segments {
        Some(segments) => {
            let filtered: Vec<&str> = segments
                .iter()
                .filter(|seg| {
                    // Skip if no_speech_prob > threshold, unless logprob is high enough
                    let mut should_skip = seg.no_speech_prob > NO_SPEECH_PROB_THRESHOLD;
                    if seg.avg_logprob > AVG_LOGPROB_THRESHOLD {
                        should_skip = false; // confident speech overrides
                    }
                    // Skip if too repetitive (hallucination loops)
                    if seg.compression_ratio > COMPRESSION_RATIO_THRESHOLD {
                        should_skip = true;
                    }

                    if should_skip {
                        log::debug!(
                            "Dropping hallucinated segment: {:?} \
                             (no_speech={:.3}, logprob={:.3}, compression={:.3})",
                            seg.text,
                            seg.no_speech_prob,
                            seg.avg_logprob,
                            seg.compression_ratio,
                        );
                        return false;
                    }
                    true
                })
                .map(|seg| seg.text.as_str())
                .collect();

            filtered.join("")
        }
        None => String::new(),
    };

    // Final guard: Whisper often hallucinates a lone "." on silence
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_punctuation()) {
        return Ok(String::new());
    }

    Ok(text)
}

/// Transcribe audio using Groq's Whisper API and return timestamped segments.
/// Used by the subtitle pipeline — requests `timestamp_granularities[]=segment`.
pub async fn transcribe_segments_with_groq(
    audio_base64: &str,
    api_key: &str,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<Vec<crate::subtitle::WhisperSegment>, Box<dyn std::error::Error + Send + Sync>> {
    let audio_bytes = base64::engine::general_purpose::STANDARD.decode(audio_base64)?;

    let audio_part = multipart::Part::bytes(audio_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")?;

    let mut form = multipart::Form::new()
        .part("file", audio_part)
        .text("model", "whisper-large-v3-turbo")
        .text("response_format", "verbose_json")
        .text("timestamp_granularities[]", "segment");

    if let Some(lang) = language {
        if !lang.is_empty() {
            form = form.text("language", lang.to_string());
        }
    }

    if let Some(prompt) = initial_prompt {
        if !prompt.is_empty() {
            form = form.text("prompt", prompt.to_string());
        }
    }

    let response = HTTP_CLIENT
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Groq API error {}: {}", status, body).into());
    }

    let result: VerboseResponse = response.json().await?;

    let segments = match result.segments {
        Some(segs) => segs
            .into_iter()
            .map(|seg| crate::subtitle::WhisperSegment {
                start: seg.start,
                end: seg.end,
                text: seg.text,
                no_speech_prob: seg.no_speech_prob,
                avg_logprob: seg.avg_logprob,
                compression_ratio: seg.compression_ratio,
            })
            .collect(),
        None => Vec::new(),
    };

    Ok(segments)
}

/// Transcribe 16 kHz WAV audio using Microsoft MAI-Transcribe via Azure Speech in Foundry Tools.
pub async fn transcribe_with_azure_mai(
    audio_base64: &str,
    api_key: &str,
    endpoint: &str,
    model: Option<&str>,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let total_start = Instant::now();

    let decode_start = Instant::now();
    let audio_bytes = base64::engine::general_purpose::STANDARD.decode(audio_base64)?;
    let audio_len = audio_bytes.len();
    log::info!(
        "[Timing][azure-mai] rust base64 decode: {:.1}ms bytes={}",
        decode_start.elapsed().as_secs_f64() * 1000.0,
        audio_len
    );

    let form_start = Instant::now();
    let form = build_azure_mai_form(audio_bytes, model, initial_prompt, language)?;
    let url = azure_transcribe_url(endpoint)?;
    log::info!(
        "[Timing][azure-mai] rust form build: {:.1}ms url={}",
        form_start.elapsed().as_secs_f64() * 1000.0,
        url
    );

    let send_start = Instant::now();
    let response = HTTP_CLIENT
        .post(url)
        .header("Ocp-Apim-Subscription-Key", api_key)
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    log::info!(
        "[Timing][azure-mai] rust HTTP send+response: {:.1}ms status={}",
        send_start.elapsed().as_secs_f64() * 1000.0,
        status
    );
    log_azure_response_transport("azure-mai", &response);

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Azure MAI transcription error {}: {}", status, body).into());
    }

    let parse_start = Instant::now();
    let result: AzureTranscriptionResponse = response.json().await?;
    log::info!(
        "[Timing][azure-mai] rust JSON parse: {:.1}ms combined={} phrases={}",
        parse_start.elapsed().as_secs_f64() * 1000.0,
        result.combined_phrases.len(),
        result.phrases.len()
    );

    let text_start = Instant::now();
    let text = azure_response_text(&result);
    log::info!(
        "[Timing][azure-mai] rust response text: {:.1}ms chars={}",
        text_start.elapsed().as_secs_f64() * 1000.0,
        text.chars().count()
    );

    let trimmed = text.trim();

    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_punctuation()) {
        log::info!(
            "[Timing][azure-mai] rust total: {:.1}ms empty=true",
            total_start.elapsed().as_secs_f64() * 1000.0
        );
        return Ok(String::new());
    }

    log::info!(
        "[Timing][azure-mai] rust total: {:.1}ms empty=false",
        total_start.elapsed().as_secs_f64() * 1000.0
    );

    Ok(text)
}

/// Transcribe 16 kHz WAV audio using Microsoft MAI-Transcribe and return timed phrases.
pub async fn transcribe_segments_with_azure_mai(
    audio_base64: &str,
    api_key: &str,
    endpoint: &str,
    model: Option<&str>,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<Vec<crate::subtitle::WhisperSegment>, Box<dyn std::error::Error + Send + Sync>> {
    let total_start = Instant::now();

    let decode_start = Instant::now();
    let audio_bytes = base64::engine::general_purpose::STANDARD.decode(audio_base64)?;
    let audio_len = audio_bytes.len();
    log::info!(
        "[Timing][azure-mai-segments] rust base64 decode: {:.1}ms bytes={}",
        decode_start.elapsed().as_secs_f64() * 1000.0,
        audio_len
    );

    let form_start = Instant::now();
    let form = build_azure_mai_form(audio_bytes, model, initial_prompt, language)?;
    let url = azure_transcribe_url(endpoint)?;
    log::info!(
        "[Timing][azure-mai-segments] rust form build: {:.1}ms url={}",
        form_start.elapsed().as_secs_f64() * 1000.0,
        url
    );

    let send_start = Instant::now();
    let response = HTTP_CLIENT
        .post(url)
        .header("Ocp-Apim-Subscription-Key", api_key)
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    log::info!(
        "[Timing][azure-mai-segments] rust HTTP send+response: {:.1}ms status={}",
        send_start.elapsed().as_secs_f64() * 1000.0,
        status
    );
    log_azure_response_transport("azure-mai-segments", &response);

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Azure MAI transcription error {}: {}", status, body).into());
    }

    let parse_start = Instant::now();
    let result: AzureTranscriptionResponse = response.json().await?;
    log::info!(
        "[Timing][azure-mai-segments] rust JSON parse: {:.1}ms combined={} phrases={}",
        parse_start.elapsed().as_secs_f64() * 1000.0,
        result.combined_phrases.len(),
        result.phrases.len()
    );

    let segments_start = Instant::now();
    let mut segments: Vec<crate::subtitle::WhisperSegment> = result
        .phrases
        .iter()
        .filter_map(|phrase| {
            let text = phrase.text.trim().to_string();
            if text.is_empty() {
                return None;
            }

            let start = phrase.offset_milliseconds / 1000.0;
            let duration = if phrase.duration_milliseconds > 0.0 {
                phrase.duration_milliseconds / 1000.0
            } else {
                1.0
            };

            Some(crate::subtitle::WhisperSegment {
                start,
                end: start + duration,
                text,
                no_speech_prob: 0.0,
                avg_logprob: 0.0,
                compression_ratio: 0.0,
            })
        })
        .collect();

    if segments.is_empty() {
        let text = azure_response_text(&result).trim().to_string();
        if !text.is_empty() {
            segments.push(crate::subtitle::WhisperSegment {
                start: 0.0,
                end: 1.0,
                text,
                no_speech_prob: 0.0,
                avg_logprob: 0.0,
                compression_ratio: 0.0,
            });
        }
    }

    log::info!(
        "[Timing][azure-mai-segments] rust segment mapping: {:.1}ms segments={}",
        segments_start.elapsed().as_secs_f64() * 1000.0,
        segments.len()
    );
    log::info!(
        "[Timing][azure-mai-segments] rust total: {:.1}ms",
        total_start.elapsed().as_secs_f64() * 1000.0
    );

    Ok(segments)
}

fn build_azure_mai_form(
    audio_bytes: Vec<u8>,
    model: Option<&str>,
    initial_prompt: Option<&str>,
    language: Option<&str>,
) -> Result<multipart::Form, Box<dyn std::error::Error + Send + Sync>> {
    let audio_part = multipart::Part::bytes(audio_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")?;

    let definition = AzureTranscriptionDefinition {
        locales: azure_locales(language),
        phrase_list: azure_phrase_list(initial_prompt),
        enhanced_mode: AzureEnhancedMode {
            enabled: true,
            model: azure_model_name(model),
        },
    };

    log::info!(
        "[Timing][azure-mai] request options locales={:?} phrase_count={} model={}",
        definition.locales,
        definition
            .phrase_list
            .as_ref()
            .map_or(0, |list| list.phrases.len()),
        definition.enhanced_mode.model
    );

    Ok(multipart::Form::new()
        .part("audio", audio_part)
        .text("definition", serde_json::to_string(&definition)?))
}

fn log_azure_response_transport(scope: &str, response: &reqwest::Response) {
    let headers = response.headers();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("-")
    };

    log::info!(
        "[Timing][{}] transport http={:?} remote={:?} connection={} server_timing={} apim_request_id={} x_ms_request_id={}",
        scope,
        response.version(),
        response.remote_addr(),
        header("connection"),
        header("server-timing"),
        header("apim-request-id"),
        header("x-ms-request-id")
    );
}

fn azure_transcribe_url(
    endpoint: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Azure Foundry endpoint is required".into());
    }

    let base = match trimmed.split_once("/api/projects/") {
        Some((resource_endpoint, _)) => resource_endpoint,
        None => trimmed,
    };

    if !base.starts_with("https://") && !base.starts_with("http://") {
        return Err("Azure Foundry endpoint must start with https://".into());
    }

    Ok(format!(
        "{}/speechtotext/transcriptions:transcribe?api-version={}",
        base, AZURE_SPEECH_API_VERSION
    ))
}

fn azure_model_name(model: Option<&str>) -> String {
    let candidate = model.map(str::trim).filter(|value| !value.is_empty());
    match candidate {
        Some(value) if value.eq_ignore_ascii_case(AZURE_MAI_DEFAULT_MODEL) => {
            AZURE_MAI_DEFAULT_MODEL.to_string()
        }
        Some(value) => value.to_string(),
        None => AZURE_MAI_DEFAULT_MODEL.to_string(),
    }
}

fn azure_locales(language: Option<&str>) -> Option<Vec<String>> {
    let language = language?.trim();
    if language.is_empty() || language.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(vec![language.to_string()])
    }
}

fn azure_phrase_list(initial_prompt: Option<&str>) -> Option<AzurePhraseList> {
    let phrases: Vec<String> = initial_prompt?
        .split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .take(200)
        .map(ToOwned::to_owned)
        .collect();

    if phrases.is_empty() {
        None
    } else {
        Some(AzurePhraseList { phrases })
    }
}

fn azure_response_text(result: &AzureTranscriptionResponse) -> String {
    let combined = result
        .combined_phrases
        .iter()
        .map(|phrase| phrase.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();

    if !combined.is_empty() {
        return combined.join(" ");
    }

    result
        .phrases
        .iter()
        .map(|phrase| phrase.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_foundry_project_endpoint_to_speech_transcribe_url() {
        let url =
            azure_transcribe_url("https://catt-asr.services.ai.azure.com/api/projects/catt-asr")
                .unwrap();

        assert_eq!(
            url,
            "https://catt-asr.services.ai.azure.com/speechtotext/transcriptions:transcribe?api-version=2025-10-15"
        );
    }

    #[test]
    fn maps_dictionary_prompt_to_azure_phrase_list() {
        let phrase_list = azure_phrase_list(Some("Contoso, Jessie, , Rehaan")).unwrap();

        assert_eq!(
            phrase_list.phrases,
            vec![
                "Contoso".to_string(),
                "Jessie".to_string(),
                "Rehaan".to_string()
            ]
        );
    }

    #[test]
    fn normalizes_mai_display_name_to_api_model_name() {
        assert_eq!(
            azure_model_name(Some("MAI-Transcribe-1.5")),
            "mai-transcribe-1.5"
        );
    }
}
