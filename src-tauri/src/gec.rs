//! GECToR (Grammatical Error Correction: Tag, Not Rewrite) inference module.
//!
//! Uses ONNX Runtime to run a BERT-base encoder with GECToR tag heads.
//! The model predicts edit tags (KEEP, DELETE, REPLACE, APPEND, etc.)
//! which are applied iteratively to correct grammar in transcribed text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use ort::session::Session;
use tokenizers::Tokenizer;

// ── Constants ──────────────────────────────────────────

/// Cloudflare R2 public URL for the pre-exported ONNX model bundle
const MODEL_URL: &str = "https://pub-e97b79d01db7403587a869136310a65d.r2.dev/gector/gector-bert-base-onnx.zip";
const MODEL_DIR_NAME: &str = "gector";
const MODEL_FILENAME: &str = "model.onnx";
const TOKENIZER_FILENAME: &str = "tokenizer.json";
const LABELS_FILENAME: &str = "labels.json";
const VERB_VOCAB_FILENAME: &str = "verb-form-vocab.txt";

/// Max iterations for iterative correction
const MAX_ITERATIONS: usize = 3;
/// Max sequence length for BERT
const MAX_SEQ_LEN: usize = 128;

// ── Global State ───────────────────────────────────────

struct GecModel {
    session: Session,
    tokenizer: Tokenizer,
    id2label: Vec<String>,
    verb_vocab: HashMap<String, String>,
}

static GEC_MODEL: LazyLock<Mutex<Option<GecModel>>> = LazyLock::new(|| Mutex::new(None));

// ── Path Helpers ───────────────────────────────────────

pub fn model_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(MODEL_DIR_NAME)
}

/// Check whether all required GEC model files exist on disk.
pub fn is_model_downloaded(data_dir: &Path) -> bool {
    let dir = model_dir(data_dir);
    dir.join(MODEL_FILENAME).exists()
        && dir.join(TOKENIZER_FILENAME).exists()
        && dir.join(LABELS_FILENAME).exists()
        && dir.join(VERB_VOCAB_FILENAME).exists()
}

// ── Download ───────────────────────────────────────────

/// Download and extract the GEC model bundle.
pub async fn download_model(
    data_dir: &Path,
    progress_cb: impl Fn(u64, u64) + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dest_dir = model_dir(data_dir);
    std::fs::create_dir_all(&dest_dir)?;

    log::info!("[GEC] Downloading model to {:?}", dest_dir);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;

    let resp = client.get(MODEL_URL).send().await?;
    if !resp.status().is_success() {
        return Err(format!("Download failed: HTTP {}", resp.status()).into());
    }

    let total = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut zip_bytes: Vec<u8> = Vec::with_capacity(total as usize);

    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        zip_bytes.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        progress_cb(downloaded, total);
    }

    // Extract zip
    log::info!("[GEC] Extracting {} bytes ...", zip_bytes.len());
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        // Skip directories and hidden files
        if name.ends_with('/') || name.starts_with("__MACOSX") || name.starts_with('.') {
            continue;
        }

        // Extract just the filename (strip any directory prefix in the zip)
        let filename = Path::new(&name)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or(name.clone());

        let out_path = dest_dir.join(&filename);
        let mut out_file = std::fs::File::create(&out_path)?;
        std::io::copy(&mut file, &mut out_file)?;
        log::info!("[GEC] Extracted: {}", filename);
    }

    log::info!("[GEC] Download and extraction complete");
    Ok(())
}

// ── Load ───────────────────────────────────────────────

/// Check if the model is currently loaded in memory.
pub fn is_loaded() -> bool {
    GEC_MODEL.lock().ok().map_or(false, |g| g.is_some())
}

/// Load the ONNX model, tokenizer, and tag vocabulary into memory.
pub fn ensure_loaded(data_dir: &Path) -> Result<(), String> {
    let mut guard = GEC_MODEL.lock().map_err(|e| format!("GEC lock poisoned: {}", e))?;
    if guard.is_some() {
        return Ok(());
    }

    let dir = model_dir(data_dir);

    // Load ONNX session
    let model_path = dir.join(MODEL_FILENAME);
    if !model_path.exists() {
        return Err("GEC model not downloaded yet".into());
    }

    log::info!("[GEC] Loading ONNX model from {:?} ...", model_path);
    let start = std::time::Instant::now();

    let session = Session::builder()
        .map_err(|e| format!("Failed to create ONNX session builder: {}", e))?
        .with_intra_threads(4)
        .map_err(|e| format!("Failed to set threads: {}", e))?
        .commit_from_file(&model_path)
        .map_err(|e| format!("Failed to load ONNX model: {}", e))?;

    // Load tokenizer
    let tokenizer_path = dir.join(TOKENIZER_FILENAME);
    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| format!("Failed to load tokenizer: {}", e))?;

    // Load labels (id -> tag string)
    let labels_path = dir.join(LABELS_FILENAME);
    let labels_json = std::fs::read_to_string(&labels_path)
        .map_err(|e| format!("Failed to read labels: {}", e))?;
    let id2label: Vec<String> = serde_json::from_str(&labels_json)
        .map_err(|e| format!("Failed to parse labels: {}", e))?;

    // Load verb vocabulary
    let verb_path = dir.join(VERB_VOCAB_FILENAME);
    let verb_vocab = load_verb_vocab(&verb_path)?;

    log::info!(
        "[GEC] Model loaded in {:.1}s ({} labels, {} verb forms)",
        start.elapsed().as_secs_f32(),
        id2label.len(),
        verb_vocab.len()
    );

    *guard = Some(GecModel {
        session,
        tokenizer,
        id2label,
        verb_vocab,
    });

    Ok(())
}

/// Unload the GEC model from memory, freeing ~150 MB.
pub fn unload() {
    if let Ok(mut guard) = GEC_MODEL.lock() {
        if guard.is_some() {
            *guard = None;
            log::info!("[GEC] Model unloaded");
        }
    }
}

/// Parse verb-form-vocab.txt: each line is "verb\told_form\tnew_form\tconjugated"
/// We store as "verb_oldform_newform" → "conjugated"
fn load_verb_vocab(path: &Path) -> Result<HashMap<String, String>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read verb vocab: {}", e))?;

    let mut map = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        // Format: "word_conjugated:FROM_TO"
        if let Some((word_part, tag_part)) = line.split_once(':') {
            if let Some((base_word, conjugated)) = word_part.split_once('_') {
                let key = format!("{}_{}", base_word.to_lowercase(), tag_part);
                map.insert(key, conjugated.to_string());
            }
        }
    }
    Ok(map)
}

// ── Inference ──────────────────────────────────────────

/// Correct grammar in the given text using GECToR.
/// Applies iterative correction (up to MAX_ITERATIONS passes).
pub fn correct_text(text: &str) -> Result<String, String> {
    let mut guard = GEC_MODEL
        .lock()
        .map_err(|e| format!("GEC model lock poisoned: {}", e))?;

    let model = guard
        .as_mut()
        .ok_or("GEC model not loaded")?;

    let mut current = text.to_string();

    for iteration in 0..MAX_ITERATIONS {
        let corrected = run_single_pass(model, &current)?;

        if corrected == current {
            log::info!("[GEC] Converged after {} iteration(s)", iteration + 1);
            break;
        }
        current = corrected;
    }

    Ok(current)
}

/// Run a single correction pass: tokenize → infer → apply tags.
fn run_single_pass(model: &mut GecModel, text: &str) -> Result<String, String> {
    // Tokenize
    let encoding = model
        .tokenizer
        .encode(text, true)
        .map_err(|e| format!("Tokenization failed: {}", e))?;

    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
    let tokens: Vec<&str> = encoding.get_tokens().iter().map(|s| s.as_str()).collect();
    let offsets: Vec<(usize, usize)> = encoding.get_offsets().to_vec();
    let seq_len = input_ids.len().min(MAX_SEQ_LEN);

    // Truncate to MAX_SEQ_LEN
    let input_ids = &input_ids[..seq_len];
    let attention_mask = &attention_mask[..seq_len];

    // Create input tensors using (shape, data) tuple format
    let ids_tensor = ort::value::Tensor::from_array(
        ndarray::Array2::from_shape_vec((1, seq_len), input_ids.to_vec())
            .map_err(|e| format!("Failed to create input tensor: {}", e))?
    )
    .map_err(|e| format!("Failed to create ids tensor: {}", e))?;

    let mask_tensor = ort::value::Tensor::from_array(
        ndarray::Array2::from_shape_vec((1, seq_len), attention_mask.to_vec())
            .map_err(|e| format!("Failed to create mask tensor: {}", e))?
    )
    .map_err(|e| format!("Failed to create mask tensor: {}", e))?;

    // Run ONNX inference — inputs! returns a Vec, not a Result
    let inputs = ort::inputs![
        "input_ids" => ids_tensor,
        "attention_mask" => mask_tensor,
    ];

    let outputs = model
        .session
        .run(inputs)
        .map_err(|e| format!("ONNX inference failed: {}", e))?;

    // Extract logits: shape [1, seq_len, num_output_classes]
    let logits_result = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("Failed to extract logits: {}", e))?;

    let _logits_shape = logits_result.0;
    let logits_data = logits_result.1;

    // Derive num_output_classes from the flat data length
    // logits_data.len() = 1 * seq_len * num_output_classes
    let num_output_classes = logits_data.len() / seq_len;

    log::info!(
        "[GEC] Logits: {} elements, seq_len={}, output_classes={}, id2label={}",
        logits_data.len(), seq_len, num_output_classes, model.id2label.len()
    );

    // Get predicted tag for each token (argmax over last dimension)
    let mut predicted_tags: Vec<&str> = Vec::with_capacity(seq_len);
    for i in 0..seq_len {
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        let row_offset = i * num_output_classes;
        for j in 0..num_output_classes {
            let val = logits_data[row_offset + j];
            if val > best_val {
                best_val = val;
                best_idx = j;
            }
        }
        // Map to label (safe bounds check)
        let tag = if best_idx < model.id2label.len() {
            &model.id2label[best_idx]
        } else {
            "$KEEP"
        };
        predicted_tags.push(tag);
    }

    // Apply tags to reconstruct corrected text
    apply_tags(text, &tokens, &offsets, &predicted_tags, &model.verb_vocab)
}

/// Apply predicted GECToR tags to produce corrected text.
///
/// We work on the original text using character offsets from the tokenizer,
/// skipping special tokens ([CLS], [SEP], [PAD]).
fn apply_tags(
    original: &str,
    tokens: &[&str],
    offsets: &[(usize, usize)],
    tags: &[&str],
    verb_vocab: &HashMap<String, String>,
) -> Result<String, String> {
    let mut result_parts: Vec<String> = Vec::new();
    let mut last_end: usize = 0;
    // Track whether the leading subword of the current word was replaced/deleted.
    // If so, continuation subwords (##xxx) should be suppressed to avoid
    // Frankenwords like "Howeverro" from REPLACE("b")+"##ro".
    let mut suppress_continuations = false;

    for i in 0..tokens.len() {
        let tag = tags[i];
        let (start, end) = offsets[i];

        // Skip special tokens (offset 0,0 for [CLS], [SEP])
        if start == 0 && end == 0 && i > 0 {
            continue;
        }
        // Also skip the first [CLS] token
        if i == 0 && (tokens[i] == "[CLS]" || tokens[i] == "<s>") {
            continue;
        }

        let is_continuation = tokens[i].starts_with("##");

        // If the leading subword was replaced/deleted, skip continuation pieces
        if is_continuation && suppress_continuations {
            last_end = end;
            continue;
        }

        // New word (not a continuation) — reset suppression
        if !is_continuation {
            suppress_continuations = false;
        }

        // Get the original text span for this token
        let token_text = if start < original.len() && end <= original.len() {
            &original[start..end]
        } else {
            tokens[i]
        };

        // Add any whitespace/text between previous token and this one
        if start > last_end && last_end < original.len() {
            result_parts.push(original[last_end..start].to_string());
        }

        if tag == "$KEEP" || tag == "KEEP" {
            result_parts.push(token_text.to_string());
        } else if tag == "$DELETE" || tag == "DELETE" {
            // Skip this token (don't add to result)
            // Note: do NOT suppress continuations — model may want to delete
            // just a prefix (e.g. "un" from "unhappy" → "happy")
        } else if let Some(replacement) = tag.strip_prefix("$REPLACE_").or_else(|| tag.strip_prefix("REPLACE_")) {
            result_parts.push(replacement.to_string());
            // Suppress continuations — the replacement is a complete word,
            // keeping leftover subword pieces would create Frankenwords
            // (e.g. REPLACE("b")+"##ro" → "Howeverro")
            if !is_continuation {
                suppress_continuations = true;
            }
        } else if let Some(append_word) = tag.strip_prefix("$APPEND_").or_else(|| tag.strip_prefix("APPEND_")) {
            result_parts.push(token_text.to_string());
            result_parts.push(format!(" {}", append_word));
        } else if tag.starts_with("$TRANSFORM_VERB_") || tag.starts_with("TRANSFORM_VERB_") {
            let transformed = apply_verb_transform(token_text, tag, verb_vocab);
            result_parts.push(transformed);
        } else if tag.starts_with("$TRANSFORM_CASE_") || tag.starts_with("TRANSFORM_CASE_") {
            let transformed = apply_case_transform(token_text, tag);
            result_parts.push(transformed);
        } else if tag.starts_with("$MERGE_") || tag.starts_with("MERGE_") {
            // Merge with previous: remove trailing space from last part
            if let Some(last) = result_parts.last_mut() {
                *last = last.trim_end().to_string();
            }
            if tag.contains("HYPHEN") {
                result_parts.push(format!("-{}", token_text));
            } else {
                result_parts.push(token_text.to_string());
            }
        } else {
            // Unknown tag — keep token as-is
            result_parts.push(token_text.to_string());
        }

        last_end = end;
    }

    // Add any trailing text after the last token
    if last_end < original.len() {
        result_parts.push(original[last_end..].to_string());
    }

    let mut result = result_parts.join("");

    // Normalize whitespace introduced by tag operations:
    // 1. Collapse multiple spaces into one
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }
    // 2. Remove space before punctuation (e.g. "word ." → "word.")
    for p in &[".", ",", "!", "?", ":", ";", "'", "\""] {
        result = result.replace(&format!(" {}", p), p);
    }

    Ok(result)
}

/// Apply a verb transformation tag using the verb vocabulary.
/// Tag format: $TRANSFORM_VERB_{FROM}_{TO}  e.g. $TRANSFORM_VERB_VB_VBD
fn apply_verb_transform(
    token: &str,
    tag: &str,
    verb_vocab: &HashMap<String, String>,
) -> String {
    // Extract FROM and TO forms from tag
    let parts: Vec<&str> = tag.split('_').collect();
    if parts.len() >= 4 {
        // Tag might be $TRANSFORM_VERB_VB_VBD or TRANSFORM_VERB_VB_VBD
        let from = parts[parts.len() - 2];
        let to = parts[parts.len() - 1];
        let key = format!("{}_{}_{}",
            token.to_lowercase(),
            from,
            to
        );
        if let Some(conjugated) = verb_vocab.get(&key) {
            return conjugated.clone();
        }
    }
    // Fallback: return original token
    token.to_string()
}

/// Apply a case transformation tag.
/// Tag format: $TRANSFORM_CASE_{TYPE}  e.g. $TRANSFORM_CASE_CAPITAL
fn apply_case_transform(token: &str, tag: &str) -> String {
    if tag.ends_with("UPPER") {
        token.to_uppercase()
    } else if tag.ends_with("LOWER") {
        token.to_lowercase()
    } else if tag.ends_with("CAPITAL") {
        let mut chars = token.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
            None => String::new(),
        }
    } else {
        token.to_string()
    }
}
