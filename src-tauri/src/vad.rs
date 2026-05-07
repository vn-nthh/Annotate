//! Silero VAD (Voice Activity Detection) module.
//!
//! Uses the Silero VAD v6 ONNX model to detect speech segments in audio.
//! Returns timestamped speech regions that can be fed to Whisper for
//! transcription, preventing hallucination on silent/non-speech audio.
//!
//! Model: ~2 MB ONNX, runs on CPU via ONNX Runtime (shared with GEC).

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use ort::session::Session;

// ── Constants ──────────────────────────────────────────

/// Cloudflare R2 public URL for the Silero VAD v5 ONNX model
const VAD_MODEL_URL: &str =
    "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/silero_vad_v6.onnx";
const VAD_DIR_NAME: &str = "vad";
const VAD_MODEL_FILENAME: &str = "silero_vad_v6.onnx";

/// Silero VAD v6 parameters for 16kHz audio
const SAMPLE_RATE: i64 = 16000;
const WINDOW_SIZE: usize = 512; // 32ms at 16kHz
const CONTEXT_SIZE: usize = 64; // v6 context buffer (64 samples at 16kHz)
const STATE_DIM: usize = 128; // LSTM hidden size

/// Default VAD thresholds
const SPEECH_THRESHOLD: f32 = 0.5;
const MIN_SPEECH_DURATION_MS: f32 = 250.0;
const MIN_SILENCE_DURATION_MS: f32 = 300.0;
/// Padding added before and after each detected speech segment
const SPEECH_PAD_MS: f32 = 400.0;

// ── Types ──────────────────────────────────────────────

/// A detected speech segment with start and end times in seconds.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpeechSegment {
    pub start: f64,
    pub end: f64,
}

// ── Global State ───────────────────────────────────────

static VAD_SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));

// ── Path Helpers ───────────────────────────────────────

pub fn model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(VAD_DIR_NAME)
}

pub fn model_path(data_dir: &Path) -> PathBuf {
    model_dir(data_dir).join(VAD_MODEL_FILENAME)
}

pub fn is_model_downloaded(data_dir: &Path) -> bool {
    let p = model_path(data_dir);
    p.exists() && p.metadata().map(|m| m.len() > 100_000).unwrap_or(false)
}

// ── Download ───────────────────────────────────────────

pub async fn download_model(
    data_dir: &Path,
    progress_cb: impl Fn(u64, u64) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = model_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(VAD_MODEL_FILENAME);

    log::info!("[VAD] Downloading Silero VAD model to {:?}", dest);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let resp = client.get(VAD_MODEL_URL).send().await?;
    if !resp.status().is_success() {
        return Err(format!("VAD download failed: HTTP {}", resp.status()).into());
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(&dest)?;

    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        progress_cb(downloaded, total);
    }

    log::info!("[VAD] Download complete: {} bytes", downloaded);
    Ok(())
}

// ── Load / Unload ──────────────────────────────────────

pub fn ensure_loaded(data_dir: &Path) -> Result<(), String> {
    let mut guard = VAD_SESSION
        .lock()
        .map_err(|e| format!("VAD lock poisoned: {}", e))?;
    if guard.is_some() {
        return Ok(());
    }

    let path = model_path(data_dir);
    if !path.exists() {
        return Err("VAD model not downloaded yet".into());
    }

    log::info!("[VAD] Loading Silero VAD model from {:?}", path);
    let start = std::time::Instant::now();

    let session = Session::builder()
        .map_err(|e| format!("Failed to create VAD session builder: {}", e))?
        .with_intra_threads(2)
        .map_err(|e| format!("Failed to set VAD threads: {}", e))?
        .commit_from_file(&path)
        .map_err(|e| format!("Failed to load VAD model: {}", e))?;

    log::info!(
        "[VAD] Model loaded in {:.1}s",
        start.elapsed().as_secs_f32()
    );

    *guard = Some(session);
    Ok(())
}

pub fn is_loaded() -> bool {
    VAD_SESSION.lock().ok().map_or(false, |g| g.is_some())
}

pub fn unload() {
    if let Ok(mut guard) = VAD_SESSION.lock() {
        if guard.is_some() {
            *guard = None;
            log::info!("[VAD] Model unloaded");
        }
    }
}

// ── Inference ──────────────────────────────────────────

/// Detect speech segments in 16kHz mono PCM audio.
///
/// Returns a list of speech segments with start/end times in seconds.
/// Each segment has `SPEECH_PAD_MS` padding on both sides.
pub fn detect_speech(pcm: &[f32]) -> Result<Vec<SpeechSegment>, String> {
    let mut guard = VAD_SESSION
        .lock()
        .map_err(|e| format!("VAD lock poisoned: {}", e))?;
    let session = guard.as_mut().ok_or("VAD model not loaded")?;

    let audio_len = pcm.len();
    if audio_len == 0 {
        return Ok(Vec::new());
    }

    let samples_per_ms = SAMPLE_RATE as f32 / 1000.0;

    // Initialize LSTM state (zeros) and context buffer
    let mut state = ndarray::Array3::<f32>::zeros((2, 1, STATE_DIM));
    let sr_array = ndarray::Array1::from_vec(vec![SAMPLE_RATE]);
    let mut context = vec![0.0f32; CONTEXT_SIZE];

    // Process audio in WINDOW_SIZE chunks, collecting per-window probabilities
    let mut speech_probs: Vec<f32> = Vec::new();
    let num_windows = (audio_len + WINDOW_SIZE - 1) / WINDOW_SIZE;

    for w in 0..num_windows {
        let start_idx = w * WINDOW_SIZE;
        let end_idx = (start_idx + WINDOW_SIZE).min(audio_len);

        // Build input chunk (pad with zeros if needed)
        let mut chunk = vec![0.0f32; WINDOW_SIZE];
        chunk[..end_idx - start_idx].copy_from_slice(&pcm[start_idx..end_idx]);

        // Prepend context to input (v6 requirement)
        let mut input_with_context = Vec::with_capacity(CONTEXT_SIZE + WINDOW_SIZE);
        input_with_context.extend_from_slice(&context);
        input_with_context.extend_from_slice(&chunk);

        let input =
            ndarray::Array2::from_shape_vec((1, CONTEXT_SIZE + WINDOW_SIZE), input_with_context)
                .map_err(|e| format!("VAD input array error: {}", e))?;

        let inputs = ort::inputs![
            "input" => ort::value::Tensor::from_array(input)
                .map_err(|e| format!("VAD input tensor error: {}", e))?,
            "state" => ort::value::Tensor::from_array(state.clone())
                .map_err(|e| format!("VAD state tensor error: {}", e))?,
            "sr" => ort::value::Tensor::from_array(sr_array.clone())
                .map_err(|e| format!("VAD sr tensor error: {}", e))?,
        ];

        let outputs = session
            .run(inputs)
            .map_err(|e| format!("VAD inference failed: {}", e))?;

        // Extract speech probability
        let prob_result = outputs["output"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("VAD output extract failed: {}", e))?;
        let prob = prob_result.1[0];
        speech_probs.push(prob);

        // Update LSTM state
        let new_state = outputs["stateN"]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("VAD state extract failed: {}", e))?;
        // Copy into our owned array
        let new_state_data = new_state.1;
        for (i, val) in new_state_data.iter().enumerate() {
            let d0 = i / (1 * STATE_DIM);
            let rest = i % (1 * STATE_DIM);
            let d1 = rest / STATE_DIM;
            let d2 = rest % STATE_DIM;
            if d0 < 2 && d1 < 1 && d2 < STATE_DIM {
                state[[d0, d1, d2]] = *val;
            }
        }

        // Update context with last CONTEXT_SIZE samples from this chunk
        if chunk.len() >= CONTEXT_SIZE {
            context.copy_from_slice(&chunk[chunk.len() - CONTEXT_SIZE..]);
        }
    }

    // Convert per-window probabilities to speech segments
    let segments = probs_to_segments(
        &speech_probs,
        audio_len,
        WINDOW_SIZE,
        samples_per_ms,
        SPEECH_THRESHOLD,
        MIN_SPEECH_DURATION_MS,
        MIN_SILENCE_DURATION_MS,
        SPEECH_PAD_MS,
    );

    log::info!(
        "[VAD] Detected {} speech segments in {:.1}s of audio",
        segments.len(),
        audio_len as f64 / SAMPLE_RATE as f64
    );

    Ok(segments)
}

/// Convert per-window speech probabilities into merged speech segments.
fn probs_to_segments(
    probs: &[f32],
    total_samples: usize,
    window_size: usize,
    samples_per_ms: f32,
    threshold: f32,
    min_speech_ms: f32,
    min_silence_ms: f32,
    pad_ms: f32,
) -> Vec<SpeechSegment> {
    let total_duration_s = total_samples as f64 / SAMPLE_RATE as f64;

    // Find raw speech regions (start_sample, end_sample)
    let mut raw_segments: Vec<(usize, usize)> = Vec::new();
    let mut in_speech = false;
    let mut speech_start: usize = 0;
    let mut silence_samples: usize = 0;
    let min_silence_samples = (min_silence_ms * samples_per_ms) as usize;

    for (i, &prob) in probs.iter().enumerate() {
        let sample_pos = i * window_size;

        if prob >= threshold {
            if !in_speech {
                speech_start = sample_pos;
                in_speech = true;
            }
            silence_samples = 0;
        } else if in_speech {
            silence_samples += window_size;
            if silence_samples >= min_silence_samples {
                // End of speech segment
                let speech_end = sample_pos - silence_samples + window_size;
                raw_segments.push((speech_start, speech_end));
                in_speech = false;
                silence_samples = 0;
            }
        }
    }

    // Close any open segment
    if in_speech {
        raw_segments.push((speech_start, total_samples));
    }

    // Filter by minimum speech duration
    let min_speech_samples = (min_speech_ms * samples_per_ms) as usize;
    let raw_segments: Vec<(usize, usize)> = raw_segments
        .into_iter()
        .filter(|(s, e)| e - s >= min_speech_samples)
        .collect();

    // Apply padding and convert to seconds
    let pad_samples = (pad_ms * samples_per_ms) as usize;
    let mut segments: Vec<SpeechSegment> = Vec::new();

    for (start, end) in raw_segments {
        let padded_start = start.saturating_sub(pad_samples);
        let padded_end = (end + pad_samples).min(total_samples);

        let start_s = padded_start as f64 / SAMPLE_RATE as f64;
        let end_s = padded_end as f64 / SAMPLE_RATE as f64;

        // Merge with previous segment if they overlap
        if let Some(last) = segments.last_mut() {
            if start_s <= last.end {
                last.end = end_s.min(total_duration_s);
                continue;
            }
        }

        segments.push(SpeechSegment {
            start: start_s,
            end: end_s.min(total_duration_s),
        });
    }

    segments
}
