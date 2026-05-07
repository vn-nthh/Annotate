//! Subtitle generation pipeline.
//!
//! Pipeline: File → FFmpeg (audio extract) → VAD (speech detection) →
//!           Whisper (transcription with timestamps) → SRT formatting.
//!
//! Anti-hallucination strategy:
//!   1. VAD pre-filter: only speech regions are sent to Whisper
//!   2. Segment-level filtering: no_speech_prob, avg_logprob, compression_ratio
//!   3. Final guard: lone punctuation on silence
//!
//! SRT conventions:
//!   - Max 42 characters per line, max 2 lines per subtitle
//!   - 15 CPS reading speed target
//!   - Min 1.0s, max 7.0s display duration
//!   - Min 80ms gap between subtitles (~2 frames @ 24fps)

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::vad;

// ── Constants ──────────────────────────────────────────

/// FFmpeg binary filename
const FFMPEG_FILENAME: &str = "ffmpeg.exe";

/// FFmpeg download URL (gyan.dev essentials build — Windows x64, ~80 MB zip)
const FFMPEG_URL: &str = "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/ffmpeg/ffmpeg.exe";

// SRT formatting constants
const MAX_CHARS_PER_LINE: usize = 42;
const MAX_LINES_PER_SUB: usize = 2;
const MIN_DISPLAY_DURATION: f64 = 1.0;
const MAX_DISPLAY_DURATION: f64 = 7.0;
const MIN_GAP_SECONDS: f64 = 0.08; // ~2 frames at 24fps

// ── Types ──────────────────────────────────────────────

/// A single subtitle entry ready for SRT output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SrtEntry {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A timestamped segment from Whisper transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhisperSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub no_speech_prob: f64,
    #[serde(default)]
    pub avg_logprob: f64,
    #[serde(default)]
    pub compression_ratio: f64,
}

/// Progress update emitted during subtitle generation.
#[derive(Debug, Clone, Serialize)]
pub struct SubtitleProgress {
    pub stage: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
}

// ── FFmpeg Management ──────────────────────────────────

/// Return the path where ffmpeg should live (in app data dir).
fn ffmpeg_path(data_dir: &Path) -> PathBuf {
    data_dir.join(FFMPEG_FILENAME)
}

/// Check if ffmpeg is available.
pub fn is_ffmpeg_available(data_dir: &Path) -> bool {
    ffmpeg_path(data_dir).exists()
}

/// Download ffmpeg if not already present.
pub async fn ensure_ffmpeg(
    data_dir: &Path,
    progress_cb: impl Fn(u64, u64) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path = ffmpeg_path(data_dir);
    if path.exists() {
        log::info!("[Subtitle] FFmpeg already available at {:?}", path);
        return Ok(());
    }

    std::fs::create_dir_all(data_dir)?;
    log::info!("[Subtitle] Downloading FFmpeg to {:?}", path);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;

    let resp = client.get(FFMPEG_URL).send().await?;
    if !resp.status().is_success() {
        return Err(format!("FFmpeg download failed: HTTP {}", resp.status()).into());
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(&path)?;

    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    use std::io::Write;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        progress_cb(downloaded, total);
    }

    log::info!("[Subtitle] FFmpeg downloaded: {} bytes", downloaded);
    Ok(())
}

/// Extract audio from a video/audio file to 16kHz mono WAV using ffmpeg.
pub async fn extract_audio(input_path: &str, data_dir: &Path) -> Result<PathBuf, String> {
    let ffmpeg = ffmpeg_path(data_dir);
    if !ffmpeg.exists() {
        return Err("FFmpeg not available. Please download it first.".into());
    }

    // Output to a temp file in the data directory
    let output_path = data_dir.join("_subtitle_temp_audio.wav");

    // Remove old temp file if exists
    let _ = std::fs::remove_file(&output_path);

    log::info!(
        "[Subtitle] Extracting audio: {:?} -> {:?}",
        input_path,
        output_path
    );

    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.args([
        "-i",
        input_path,
        "-vn", // No video
        "-acodec",
        "pcm_s16le", // 16-bit PCM
        "-ar",
        "16000", // 16kHz
        "-ac",
        "1",  // Mono
        "-y", // Overwrite
        output_path.to_str().ok_or("Invalid temp path")?,
    ]);

    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("FFmpeg execution failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "FFmpeg failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(500).collect::<String>()
        ));
    }

    if !output_path.exists() {
        return Err("FFmpeg produced no output file".into());
    }

    log::info!(
        "[Subtitle] Audio extracted: {} bytes",
        output_path.metadata().map(|m| m.len()).unwrap_or(0)
    );

    Ok(output_path)
}

// ── WAV Reading ────────────────────────────────────────

/// Read a 16kHz mono WAV file into f32 PCM samples.
pub fn read_wav_to_pcm(path: &Path) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("Failed to open WAV: {}", e))?;

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
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|s| s.ok())
            .collect(),
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

    Ok(mono)
}

/// Extract a slice of PCM audio between start_sec and end_sec.
pub fn extract_pcm_segment(pcm: &[f32], start_sec: f64, end_sec: f64) -> Vec<f32> {
    let sr = 16000.0;
    let start_idx = (start_sec * sr) as usize;
    let end_idx = ((end_sec * sr) as usize).min(pcm.len());

    if start_idx >= pcm.len() || start_idx >= end_idx {
        return Vec::new();
    }

    pcm[start_idx..end_idx].to_vec()
}

/// Encode f32 PCM samples as a 16kHz mono WAV and return as bytes.
pub fn pcm_to_wav_bytes(pcm: &[f32]) -> Result<Vec<u8>, String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|e| format!("WAV writer create failed: {}", e))?;

        for &sample in pcm {
            let clamped = sample.max(-1.0).min(1.0);
            let int_sample = (clamped * 32767.0) as i16;
            writer
                .write_sample(int_sample)
                .map_err(|e| format!("WAV write failed: {}", e))?;
        }

        writer
            .finalize()
            .map_err(|e| format!("WAV finalize failed: {}", e))?;
    }

    Ok(cursor.into_inner())
}

// ── Whisper Segment Filtering ──────────────────────────

/// Anti-hallucination thresholds (matching openai/whisper transcribe.py)
const NO_SPEECH_PROB_THRESHOLD: f64 = 0.6;
const AVG_LOGPROB_THRESHOLD: f64 = -1.0;
const COMPRESSION_RATIO_THRESHOLD: f64 = 2.4;

/// Filter whisper segments using anti-hallucination heuristics.
pub fn filter_segments(segments: Vec<WhisperSegment>) -> Vec<WhisperSegment> {
    segments
        .into_iter()
        .filter(|seg| {
            let text = seg.text.trim();

            // Skip empty or punctuation-only
            if text.is_empty() || text.chars().all(|c| c.is_ascii_punctuation()) {
                return false;
            }

            // Whisper anti-hallucination logic
            let mut should_skip = seg.no_speech_prob > NO_SPEECH_PROB_THRESHOLD;
            if seg.avg_logprob > AVG_LOGPROB_THRESHOLD {
                should_skip = false; // Confident speech overrides
            }
            if seg.compression_ratio > COMPRESSION_RATIO_THRESHOLD {
                should_skip = true; // Repetitive hallucination loops
            }

            if should_skip {
                log::debug!(
                    "[Subtitle] Dropping hallucinated segment: {:?} \
                     (no_speech={:.3}, logprob={:.3}, compression={:.3})",
                    text,
                    seg.no_speech_prob,
                    seg.avg_logprob,
                    seg.compression_ratio,
                );
            }

            !should_skip
        })
        .collect()
}

/// Offset all segment timestamps by a base offset (VAD segment start time).
pub fn offset_segments(segments: Vec<WhisperSegment>, offset_sec: f64) -> Vec<WhisperSegment> {
    segments
        .into_iter()
        .map(|mut seg| {
            seg.start += offset_sec;
            seg.end += offset_sec;
            seg
        })
        .collect()
}

// ── SRT Formatting ─────────────────────────────────────

/// Convert whisper segments into SRT entries with proper formatting.
pub fn segments_to_srt(segments: &[WhisperSegment]) -> Vec<SrtEntry> {
    let mut entries: Vec<SrtEntry> = Vec::new();

    for seg in segments {
        let text = seg.text.trim().to_string();
        if text.is_empty() {
            continue;
        }

        let start = seg.start;
        let mut end = seg.end;

        // Enforce minimum display duration
        let duration = end - start;
        if duration < MIN_DISPLAY_DURATION {
            end = start + MIN_DISPLAY_DURATION;
        }

        // Check if we need to split (either too long in duration or too much text)
        let max_chars = MAX_CHARS_PER_LINE * MAX_LINES_PER_SUB; // 84
        if duration > MAX_DISPLAY_DURATION || text.chars().count() > max_chars {
            let sub_entries = split_long_segment(start, end, &text);
            for sub in sub_entries {
                let adjusted = enforce_gap(sub, entries.last());
                entries.push(adjusted);
            }
            continue;
        }

        // Line-wrap the text
        let wrapped = wrap_subtitle_text(&text);

        let entry = SrtEntry {
            index: 0, // Will be numbered later
            start,
            end,
            text: wrapped,
        };

        let adjusted = enforce_gap(entry, entries.last());
        entries.push(adjusted);
    }

    // Anti-flicker pass: close tiny gaps (<200ms) between consecutive entries
    // by snapping both timestamps to the midpoint
    const FLICKER_THRESHOLD: f64 = 0.200;
    for i in 1..entries.len() {
        let gap = entries[i].start - entries[i - 1].end;
        if gap > 0.0 && gap < FLICKER_THRESHOLD {
            let mid = entries[i - 1].end + gap / 2.0;
            entries[i - 1].end = mid;
            entries[i].start = mid;
        }
    }

    // Number the entries
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.index = i + 1;
    }

    entries
}

/// Split a segment that exceeds MAX_DISPLAY_DURATION or max text length into sub-segments.
fn split_long_segment(start: f64, end: f64, text: &str) -> Vec<SrtEntry> {
    let total_duration = end - start;
    let words: Vec<&str> = text.split_whitespace().collect();

    if words.is_empty() {
        return Vec::new();
    }

    // Calculate how many sub-segments we need based on duration
    let num_by_duration = if total_duration > MAX_DISPLAY_DURATION {
        (total_duration / MAX_DISPLAY_DURATION).ceil() as usize
    } else {
        1
    };

    // Calculate how many sub-segments we need based on text length
    let max_chars = MAX_CHARS_PER_LINE * MAX_LINES_PER_SUB; // 84
    let text_chars = text.chars().count();
    let num_by_text = if text_chars > max_chars {
        (text_chars + max_chars - 1) / max_chars
    } else {
        1
    };

    // Use whichever requires more splits
    let num_subs = num_by_duration.max(num_by_text).max(1);
    let words_per_sub = (words.len() + num_subs - 1) / num_subs;

    let mut entries = Vec::new();

    for (i, chunk) in words.chunks(words_per_sub).enumerate() {
        let chunk_text = chunk.join(" ");
        let chunk_start = start + (i as f64 * total_duration / num_subs as f64);
        let chunk_end = start + ((i + 1) as f64 * total_duration / num_subs as f64);

        entries.push(SrtEntry {
            index: 0,
            start: chunk_start,
            end: chunk_end.min(end),
            text: wrap_subtitle_text(&chunk_text),
        });
    }

    entries
}

/// Enforce minimum gap between consecutive subtitles.
fn enforce_gap(mut entry: SrtEntry, prev: Option<&SrtEntry>) -> SrtEntry {
    if let Some(prev) = prev {
        let gap = entry.start - prev.end;
        if gap < MIN_GAP_SECONDS && gap >= 0.0 {
            entry.start = prev.end + MIN_GAP_SECONDS;
            // Don't let start exceed end
            if entry.start >= entry.end {
                entry.end = entry.start + MIN_DISPLAY_DURATION;
            }
        }
    }
    entry
}

/// Wrap subtitle text to MAX_CHARS_PER_LINE, max MAX_LINES_PER_SUB lines.
/// Long text is split upstream in split_long_segment, so this only handles
/// fitting within 2 lines.
fn wrap_subtitle_text(text: &str) -> String {
    let text = text.trim();
    let char_count = text.chars().count();

    // If fits in one line, return as-is
    if char_count <= MAX_CHARS_PER_LINE {
        return text.to_string();
    }

    // Split into two lines at the best break point (byte-safe)
    let midpoint_chars = char_count / 2;
    let break_byte_pos = find_best_break(text, midpoint_chars);
    let line1 = text[..break_byte_pos].trim_end();
    let line2 = text[break_byte_pos..].trim_start();
    format!("{}\n{}", line1, line2)
}

/// Find the best line-break position near `target_chars` (in character count).
/// Returns a **byte position** that is safe to slice at.
/// Prefers breaking after punctuation or before conjunctions/prepositions.
fn find_best_break(text: &str, target_chars: usize) -> usize {
    // Convert target_chars to an approximate byte position
    let target_byte = text
        .char_indices()
        .nth(target_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());

    // Find all whitespace positions (these are always safe to split at)
    let ws_positions: Vec<usize> = text
        .match_indices(char::is_whitespace)
        .map(|(i, _)| i)
        .collect();

    if ws_positions.is_empty() {
        // No whitespace — find the nearest char boundary to target
        return text
            .char_indices()
            .nth(target_chars)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
    }

    let mut best_pos = ws_positions[0];
    let mut best_score = i32::MAX;

    // Conjunctions and prepositions that prefer a break BEFORE them
    let break_before = [
        "and", "but", "or", "so", "because", "in", "on", "at", "for", "with", "to", "that",
        "which", "who", "when", "where", "if", "of", "as", "by",
    ];

    for pos in &ws_positions {
        let pos = *pos;
        if pos == 0 || pos >= text.len() - 1 {
            continue;
        }

        let distance = (pos as i32 - target_byte as i32).abs();
        let mut score = distance;

        // Bonus: break after punctuation (check the char just before this whitespace)
        if let Some(ch) = text[..pos].chars().last() {
            if matches!(
                ch,
                ',' | '.' | ';' | ':' | '!' | '?' | '、' | '。' | '！' | '？'
            ) {
                score -= 10;
            }
        }

        // Bonus: break before conjunctions/prepositions
        let word_after = text[pos..]
            .trim_start()
            .split_whitespace()
            .next()
            .unwrap_or("");
        if break_before
            .iter()
            .any(|w| word_after.eq_ignore_ascii_case(w))
        {
            score -= 8;
        }

        if score < best_score {
            best_score = score;
            best_pos = pos;
        }
    }

    best_pos
}

/// Format SRT entries as a complete .srt file string.
pub fn format_srt(entries: &[SrtEntry]) -> String {
    let mut output = String::new();

    for entry in entries {
        output.push_str(&entry.index.to_string());
        output.push('\n');
        output.push_str(&format_timestamp(entry.start));
        output.push_str(" --> ");
        output.push_str(&format_timestamp(entry.end));
        output.push('\n');
        output.push_str(&entry.text);
        output.push_str("\n\n");
    }

    output
}

/// Format seconds as SRT timestamp: HH:MM:SS,mmm
fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let s = total_secs % 60;
    let total_mins = total_secs / 60;
    let m = total_mins % 60;
    let h = total_mins / 60;

    format!("{:02}:{:02}:{:02},{:03}", h, m, s, ms)
}

// ── Full Pipeline ──────────────────────────────────────

/// Generate subtitles from an audio/video file.
///
/// This is the main entry point called by the Tauri command.
/// The progress callback receives (stage, current_chunk, total_chunks, message).
pub async fn generate_subtitles(
    file_path: &str,
    engine: &str,
    api_key: Option<&str>,
    prompt: Option<&str>,
    language: Option<&str>,
    data_dir: &Path,
    progress_cb: impl Fn(SubtitleProgress) + Send + Clone + 'static,
) -> Result<Vec<SrtEntry>, String> {
    // Stage 1: Extract audio with FFmpeg
    progress_cb(SubtitleProgress {
        stage: "extract".into(),
        current: 0,
        total: 1,
        message: "Extracting audio...".into(),
    });

    let wav_path = extract_audio(file_path, data_dir).await?;

    // Stage 2: Read WAV
    progress_cb(SubtitleProgress {
        stage: "read".into(),
        current: 0,
        total: 1,
        message: "Reading audio...".into(),
    });

    let pcm = read_wav_to_pcm(&wav_path)?;

    // Cleanup temp WAV
    let _ = std::fs::remove_file(&wav_path);

    let audio_duration = pcm.len() as f64 / 16000.0;
    log::info!("[Subtitle] Audio duration: {:.1}s", audio_duration);

    // Stage 3: VAD — detect speech segments
    progress_cb(SubtitleProgress {
        stage: "vad".into(),
        current: 0,
        total: 1,
        message: "Detecting speech regions...".into(),
    });

    // Ensure VAD model is loaded
    vad::ensure_loaded(data_dir)?;

    let speech_segments = vad::detect_speech(&pcm)?;

    if speech_segments.is_empty() {
        log::info!("[Subtitle] No speech detected in audio");
        return Ok(Vec::new());
    }

    log::info!(
        "[Subtitle] VAD found {} speech segments",
        speech_segments.len()
    );

    // Stage 4: Transcribe each VAD segment with Whisper
    let total_segments = speech_segments.len();
    let mut all_whisper_segments: Vec<WhisperSegment> = Vec::new();

    for (i, vad_seg) in speech_segments.iter().enumerate() {
        progress_cb(SubtitleProgress {
            stage: "transcribe".into(),
            current: i + 1,
            total: total_segments,
            message: format!(
                "Transcribing segment {}/{} ({:.1}s - {:.1}s)...",
                i + 1,
                total_segments,
                vad_seg.start,
                vad_seg.end,
            ),
        });

        // Extract the audio for this VAD segment
        let segment_pcm = extract_pcm_segment(&pcm, vad_seg.start, vad_seg.end);
        if segment_pcm.is_empty() {
            continue;
        }

        // Encode as WAV bytes → base64
        let wav_bytes = pcm_to_wav_bytes(&segment_pcm)?;
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav_bytes);

        // Transcribe with timestamps
        let raw_segments = match engine {
            "local" => transcribe_segments_local(&audio_b64, prompt, language).await?,
            "groq" => {
                let key = api_key.ok_or("API key required for Groq engine")?;
                transcribe_segments_groq(&audio_b64, key, prompt, language).await?
            }
            _ => return Err(format!("Unknown engine: {}", engine)),
        };

        // Filter hallucinated segments
        let filtered = filter_segments(raw_segments);

        // Offset timestamps by VAD segment start time
        let offset = offset_segments(filtered, vad_seg.start);

        all_whisper_segments.extend(offset);
    }

    // Stage 5: Format as SRT
    progress_cb(SubtitleProgress {
        stage: "format".into(),
        current: 0,
        total: 1,
        message: "Formatting subtitles...".into(),
    });

    let srt_entries = segments_to_srt(&all_whisper_segments);

    log::info!(
        "[Subtitle] Generated {} SRT entries from {} whisper segments",
        srt_entries.len(),
        all_whisper_segments.len()
    );

    Ok(srt_entries)
}

// ── Whisper Transcription (with timestamps) ────────────

/// Transcribe audio via the local whisper worker, returning timestamped segments.
async fn transcribe_segments_local(
    audio_b64: &str,
    prompt: Option<&str>,
    language: Option<&str>,
) -> Result<Vec<WhisperSegment>, String> {
    crate::whisper_local::transcribe_segments_b64(audio_b64, prompt, language).await
}

/// Transcribe audio via Groq API, returning timestamped segments.
async fn transcribe_segments_groq(
    audio_b64: &str,
    api_key: &str,
    prompt: Option<&str>,
    language: Option<&str>,
) -> Result<Vec<WhisperSegment>, String> {
    crate::transcribe::transcribe_segments_with_groq(audio_b64, api_key, prompt, language)
        .await
        .map_err(|e| format!("Groq transcription failed: {}", e))
}

// ── Cleanup ────────────────────────────────────────────

/// Clean up any temporary files created during subtitle generation.
pub fn cleanup_temp_files(data_dir: &Path) {
    let temp = data_dir.join("_subtitle_temp_audio.wav");
    let _ = std::fs::remove_file(temp);
}
