//! Whisper Worker — separate process for local whisper.cpp inference.
//!
//! Reads JSON commands from stdin, writes JSON responses to stdout.
//! This runs in its own process so CUDA DLLs (~573 MB) are only loaded
//! here and fully freed when the process exits.
//!
//! Protocol:
//!   → {"cmd":"load","data_dir":"C:\\..."}
//!   ← {"ok":true}
//!
//!   → {"cmd":"transcribe","audio_b64":"...","prompt":"optional"}
//!   ← {"ok":true,"text":"transcribed text"}
//!
//!   → {"cmd":"transcribe_segments","audio_b64":"...","prompt":"optional"}
//!   ← {"ok":true,"segments":[{"start":0.0,"end":2.5,"text":"...","no_speech_prob":0.1,"avg_logprob":-0.5}]}
//!
//!   → {"cmd":"quit"}
//!   (process exits)

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

static CTX: Mutex<Option<WhisperContext>> = Mutex::new(None);

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, r#"{{"ok":false,"error":"Invalid JSON: {}"}}"#, e);
                let _ = out.flush();
                continue;
            }
        };

        let cmd = msg["cmd"].as_str().unwrap_or("");

        match cmd {
            "load" => {
                let data_dir = msg["data_dir"].as_str().unwrap_or("");
                let result = load_model(data_dir);
                match result {
                    Ok(()) => {
                        let _ = writeln!(out, r#"{{"ok":true}}"#);
                    }
                    Err(e) => {
                        let _ = writeln!(out, r#"{{"ok":false,"error":"{}"}}"#, e.replace('"', "'"));
                    }
                }
            }
            "transcribe" => {
                let audio_b64 = msg["audio_b64"].as_str().unwrap_or("");
                let prompt = msg["prompt"].as_str();
                let result = transcribe(audio_b64, prompt);
                match result {
                    Ok(text) => {
                        let resp = serde_json::json!({"ok": true, "text": text});
                        let _ = writeln!(out, "{}", resp);
                    }
                    Err(e) => {
                        let _ = writeln!(out, r#"{{"ok":false,"error":"{}"}}"#, e.replace('"', "'"));
                    }
                }
            }
            "transcribe_segments" => {
                let audio_b64 = msg["audio_b64"].as_str().unwrap_or("");
                let prompt = msg["prompt"].as_str();
                let language = msg["language"].as_str();
                let result = transcribe_segments(audio_b64, prompt, language);
                match result {
                    Ok(segments) => {
                        let resp = serde_json::json!({"ok": true, "segments": segments});
                        let _ = writeln!(out, "{}", resp);
                    }
                    Err(e) => {
                        let _ = writeln!(out, r#"{{"ok":false,"error":"{}"}}"#, e.replace('"', "'"));
                    }
                }
            }
            "quit" => {
                let _ = writeln!(out, r#"{{"ok":true}}"#);
                let _ = out.flush();
                std::process::exit(0);
            }
            _ => {
                let _ = writeln!(out, r#"{{"ok":false,"error":"Unknown command: {}"}}"#, cmd);
            }
        }

        let _ = out.flush();
    }
}

const MODEL_FILENAME: &str = "ggml-large-v3-turbo-q5_0.bin";

fn load_model(data_dir: &str) -> Result<(), String> {
    let mut guard = CTX.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
    if guard.is_some() {
        return Ok(());
    }

    let path = PathBuf::from(data_dir).join(MODEL_FILENAME);
    if !path.exists() {
        return Err(format!("Model not found: {}", path.display()));
    }

    eprintln!("[whisper-worker] Loading model from {:?}", path);
    let start = std::time::Instant::now();

    let ctx = WhisperContext::new_with_params(
        path.to_str().ok_or("Invalid path")?,
        WhisperContextParameters::default(),
    )
    .map_err(|e| format!("Failed to load: {e}"))?;

    eprintln!("[whisper-worker] Model loaded in {:.1}s", start.elapsed().as_secs_f64());

    *guard = Some(ctx);
    Ok(())
}

// ── Anti-hallucination thresholds (matching cloud model) ──
const NO_SPEECH_PROB_THRESHOLD: f32 = 0.6;
const AVG_LOGPROB_THRESHOLD: f32 = -1.0;

fn transcribe(audio_b64: &str, prompt: Option<&str>) -> Result<String, String> {
    use base64::Engine;

    // Decode base64 WAV
    let wav_bytes = base64::engine::general_purpose::STANDARD
        .decode(audio_b64)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;

    // Parse WAV to f32 PCM
    let pcm = wav_to_pcm_f32(&wav_bytes)?;

    // ── RMS energy gate ──
    // If the audio is near-silent, skip inference entirely.
    // This prevents the model from hallucinating text on background noise.
    let rms = if pcm.is_empty() {
        0.0
    } else {
        (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len() as f32).sqrt()
    };
    const SILENCE_RMS_THRESHOLD: f32 = 0.01; // ~-40 dBFS
    if rms < SILENCE_RMS_THRESHOLD {
        eprintln!(
            "[whisper-worker] Audio too quiet (RMS={:.5}), skipping inference",
            rms
        );
        return Ok(String::new());
    }

    // Run inference
    let guard = CTX.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
    let ctx = guard.as_ref().ok_or("Model not loaded")?;

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_no_timestamps(true);

    if let Some(p) = prompt {
        if !p.is_empty() {
            params.set_initial_prompt(p);
        }
    }

    state
        .full(params, &pcm)
        .map_err(|e| format!("Inference failed: {e}"))?;

    // Filter segments using anti-hallucination heuristics (same as cloud model)
    let n_segments = state.full_n_segments();
    let mut text = String::new();

    for i in 0..n_segments {
        let seg = match state.get_segment(i) {
            Some(s) => s,
            None => continue,
        };


        // 1. No-speech probability — directly from whisper.cpp
        let no_speech_prob = seg.no_speech_probability();

        // 2. Average log probability — computed from per-token plog
        let n_tokens = seg.n_tokens();
        let avg_logprob = if n_tokens > 0 {
            let sum: f32 = (0..n_tokens)
                .filter_map(|t| seg.get_token(t))
                .map(|tok| tok.token_data().plog)
                .sum();
            sum / n_tokens as f32
        } else {
            0.0
        };

        let seg_text = seg.to_str_lossy().unwrap_or_default().to_string();

        // ── Whisper filtering logic (transcribe.py lines 304-316) ──
        // Skip if no_speech_prob > threshold, UNLESS avg_logprob is high
        // enough (confident speech overrides the no-speech detector).
        let mut should_skip = no_speech_prob > NO_SPEECH_PROB_THRESHOLD;
        if avg_logprob > AVG_LOGPROB_THRESHOLD {
            should_skip = false;
        }

        if should_skip {
            eprintln!(
                "[whisper-worker] Dropping segment: {:?} \
                 (no_speech={:.3}, logprob={:.3})",
                seg_text, no_speech_prob, avg_logprob,
            );
            continue;
        }

        text.push_str(&seg_text);
    }

    // Final guard: lone punctuation on silence
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_punctuation()) {
        return Ok(String::new());
    }

    Ok(text)
}

/// Transcribe audio and return timestamped segments (for subtitle generation).
///
/// Key differences from `transcribe`:
/// - Timestamps are enabled (no_timestamps = false)
/// - condition_on_previous_text = true (better context within long segments)
/// - Returns structured segments with timing and quality metrics
/// - No RMS pre-filter (VAD already handles silence detection upstream)
fn transcribe_segments(audio_b64: &str, prompt: Option<&str>, language: Option<&str>) -> Result<serde_json::Value, String> {
    use base64::Engine;

    // Decode base64 WAV
    let wav_bytes = base64::engine::general_purpose::STANDARD
        .decode(audio_b64)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;

    // Parse WAV to f32 PCM
    let pcm = wav_to_pcm_f32(&wav_bytes)?;

    if pcm.is_empty() {
        return Ok(serde_json::json!([]));
    }

    // Run inference with timestamps
    let guard = CTX.lock().map_err(|e| format!("Lock poisoned: {}", e))?;
    let ctx = guard.as_ref().ok_or("Model not loaded")?;

    let mut state = ctx
        .create_state()
        .map_err(|e| format!("Failed to create state: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language); // None = auto-detect, Some("en") = English, etc.
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);

    // KEY DIFFERENCE: enable timestamps for subtitle generation
    params.set_no_timestamps(false);

    if let Some(p) = prompt {
        if !p.is_empty() {
            params.set_initial_prompt(p);
        }
    }

    state
        .full(params, &pcm)
        .map_err(|e| format!("Inference failed: {e}"))?;

    // Collect all segments with timestamps and quality metrics
    let n_segments = state.full_n_segments();
    let mut segments = Vec::new();

    for i in 0..n_segments {
        let seg = match state.get_segment(i) {
            Some(s) => s,
            None => continue,
        };

        let seg_text = seg.to_str_lossy().unwrap_or_default().to_string();
        let trimmed = seg_text.trim();

        // Skip empty segments
        if trimmed.is_empty() || trimmed.chars().all(|c| c.is_ascii_punctuation()) {
            continue;
        }

        let no_speech_prob = seg.no_speech_probability();

        let n_tokens = seg.n_tokens();
        let avg_logprob = if n_tokens > 0 {
            let sum: f32 = (0..n_tokens)
                .filter_map(|t| seg.get_token(t))
                .map(|tok| tok.token_data().plog)
                .sum();
            sum / n_tokens as f32
        } else {
            0.0
        };

        // Get timestamps (in centiseconds from whisper.cpp, convert to seconds)
        let start_ts = seg.start_timestamp();
        let end_ts = seg.end_timestamp();
        let start = start_ts as f64 / 100.0;
        let end = end_ts as f64 / 100.0;

        segments.push(serde_json::json!({
            "start": start,
            "end": end,
            "text": seg_text.trim(),
            "no_speech_prob": no_speech_prob,
            "avg_logprob": avg_logprob,
            "compression_ratio": 0.0,  // Not directly available from whisper.cpp
        }));
    }

    Ok(serde_json::Value::Array(segments))
}

fn wav_to_pcm_f32(raw: &[u8]) -> Result<Vec<f32>, String> {
    let cursor = std::io::Cursor::new(raw);
    let reader = hound::WavReader::new(cursor)
        .map_err(|e| format!("Failed to read WAV: {e}"))?;

    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
        hound::SampleFormat::Float => {
            reader
                .into_samples::<f32>()
                .filter_map(|s| s.ok())
                .collect()
        }
    };

    // Stereo → mono
    let mono = if spec.channels == 2 {
        samples
            .chunks(2)
            .map(|pair| {
                if pair.len() == 2 {
                    (pair[0] + pair[1]) / 2.0
                } else {
                    pair[0]
                }
            })
            .collect()
    } else {
        samples
    };

    // Resample to 16 kHz if needed
    let target_rate = 16000u32;
    if spec.sample_rate != target_rate {
        let ratio = spec.sample_rate as f64 / target_rate as f64;
        let new_len = (mono.len() as f64 / ratio) as usize;
        let mut out = Vec::with_capacity(new_len);
        for i in 0..new_len {
            let src_idx = i as f64 * ratio;
            let idx0 = src_idx as usize;
            let frac = (src_idx - idx0 as f64) as f32;
            let s0 = mono.get(idx0).copied().unwrap_or(0.0);
            let s1 = mono.get(idx0 + 1).copied().unwrap_or(s0);
            out.push(s0 + frac * (s1 - s0));
        }
        Ok(out)
    } else {
        Ok(mono)
    }
}
