use std::sync::LazyLock;
use std::time::Duration;

use serde::Deserialize;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_MODEL: &str = "openai/gpt-5.4-mini";
const OPENROUTER_TITLE: &str = "Annotate";

const SUMMARY_SYSTEM_PROMPT: &str = "You summarize subtitle transcripts for a desktop transcription app. Be concise, accurate, and faithful to the transcript. Ignore timestamps, filler words, and repeated phrases. Preserve names, numbers, decisions, action items, and key topics. Return valid JSON with a single `summary` string. Use the same language as the transcript when possible.";

const SUMMARY_MAX_TOKENS: u32 = 512;

static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("Failed to build OpenRouter HTTP client")
});

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

pub fn entries_to_transcript(entries: &[crate::subtitle::SrtEntry]) -> String {
    entries
        .iter()
        .map(|entry| normalize_text(&entry.text))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub async fn summarize_entries(
    api_key: &str,
    entries: &[crate::subtitle::SrtEntry],
) -> Result<String, String> {
    let transcript = entries_to_transcript(entries);
    summarize_text(api_key, &transcript).await
}

pub async fn summarize_text(
    api_key: &str,
    transcript: &str,
) -> Result<String, String> {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return Err("Subtitle transcript is empty".to_string());
    }

    let user_prompt = format!(
        "Summarize the transcript below. Keep the response concise and useful to a person reviewing the audio or video file.\n\nTranscript:\n{}",
        transcript,
    );

    let request_body = serde_json::json!({
        "model": OPENROUTER_MODEL,
        "messages": [
            {
                "role": "system",
                "content": SUMMARY_SYSTEM_PROMPT,
            },
            {
                "role": "user",
                "content": user_prompt,
            },
        ],
        "max_tokens": SUMMARY_MAX_TOKENS,
        "response_format": { "type": "json_object" },
    });

    let response = HTTP_CLIENT
        .post(OPENROUTER_URL)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {}", api_key.trim()))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("X-OpenRouter-Title", OPENROUTER_TITLE)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("OpenRouter request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("OpenRouter summary error {}: {}", status, body));
    }

    let completion: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse OpenRouter response: {}", e))?;
    let content = completion
        .choices
        .first()
        .and_then(|choice| choice.message.content.as_deref())
        .ok_or_else(|| "OpenRouter summary response was empty".to_string())?;

    extract_summary_text(content)
}

fn extract_summary_text(content: &str) -> Result<String, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("OpenRouter summary response was empty".to_string());
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(summary) = value.get("summary").and_then(|value| value.as_str()) {
            let summary = summary.trim();
            if !summary.is_empty() {
                return Ok(summary.to_string());
            }
        }
    }

    Ok(trimmed.to_string())
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_entries_into_plain_transcript_lines() {
        let entries = vec![
            crate::subtitle::SrtEntry {
                index: 1,
                start: 0.0,
                end: 1.0,
                text: " Hello\nworld ".to_string(),
            },
            crate::subtitle::SrtEntry {
                index: 2,
                start: 1.0,
                end: 2.0,
                text: "  second\tline  ".to_string(),
            },
        ];

        assert_eq!(entries_to_transcript(&entries), "Hello world\nsecond line");
    }

    #[test]
    fn extracts_summary_from_json_payload() {
        let summary = extract_summary_text(r#"{"summary":"Hello\nworld"}"#).unwrap();
        assert_eq!(summary, "Hello\nworld");
    }
}
