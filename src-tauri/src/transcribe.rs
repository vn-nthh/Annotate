use base64::Engine;
use reqwest::multipart;
use serde::Deserialize;
use std::sync::LazyLock;

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

#[derive(Deserialize, Debug)]
struct VerboseResponse {
    segments: Option<Vec<Segment>>,
}

#[derive(Deserialize, Debug)]
struct Segment {
    text: String,
    #[serde(default)]
    no_speech_prob: f64,
    #[serde(default)]
    avg_logprob: f64,
    #[serde(default)]
    compression_ratio: f64,
}

/// Transcribe audio using Groq's Whisper Large V3 Turbo API
/// with anti-hallucination filtering on the returned segments.
pub async fn transcribe_with_groq(
    audio_base64: &str,
    api_key: &str,
    initial_prompt: Option<&str>,
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
