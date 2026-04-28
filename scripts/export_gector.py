"""
One-time script to export gotutiyan/gector-bert-base-cased-5k to ONNX INT8.

Usage:
  pip install torch "transformers>=4.49,<5" onnx onnxruntime safetensors huggingface_hub
  pip install git+https://github.com/gotutiyan/gector.git
  python scripts/export_gector.py

Output: scripts/gector-onnx/ directory containing:
  - model.onnx (INT8 quantized)
  - tokenizer.json
  - labels.json (ordered array of tag labels)
  - verb-form-vocab.txt (downloaded from grammarly/gector)
"""

import json
import urllib.request
import torch
from pathlib import Path
from huggingface_hub import hf_hub_download
from safetensors.torch import load_file
from transformers import AutoTokenizer

OUT_DIR = Path(__file__).parent / "gector-onnx"
OUT_DIR.mkdir(exist_ok=True)

MODEL_ID = "gotutiyan/gector-bert-base-cased-5k"

# ── Step 1: Load model ──────────────────────────────────
print("[1/5] Loading GECToR model...")

# Download config as raw JSON (avoids AutoConfig needing model_type)
config_path = hf_hub_download(MODEL_ID, "config.json")
with open(config_path, "r") as f:
    config_dict = json.load(f)

print(f"   Config: {config_dict.get('num_labels', '?')} labels, encoder: {config_dict.get('model_id', '?')}")

# Load via gector library
from gector.configuration import GECToRConfig
from gector.modeling import GECToR

# Build a proper GECToRConfig from the raw dict
# Pass id2label and label2id so PretrainedConfig sets num_labels correctly
# GECToRConfig.__init__ takes specific named params + **kwargs passed to PretrainedConfig
gec_config = GECToRConfig(
    model_id=config_dict.get("model_id", "bert-base-cased"),
    p_dropout=config_dict.get("p_dropout", 0),
    label_pad_token=config_dict.get("label_pad_token", "<PAD>"),
    label_oov_token=config_dict.get("label_oov_token", "<OOV>"),
    keep_label=config_dict.get("keep_label", "$KEEP"),
    correct_label=config_dict.get("correct_label", "$CORRECT"),
    incorrect_label=config_dict.get("incorrect_label", "$INCORRECT"),
    label_smoothing=config_dict.get("label_smoothing", 0.0),
    has_add_pooling_layer=config_dict.get("has_add_pooling_layer", True),
    initializer_range=config_dict.get("initializer_range", 0.02),
    # These go to PretrainedConfig via **kwargs:
    id2label=config_dict.get("id2label", {}),
    label2id=config_dict.get("label2id", {}),
)

print(f"   num_labels from config: {gec_config.num_labels}")

# Verify against actual weights to ensure exact match
# label_proj_layer has shape [num_labels - 1, hidden] (minus PAD)
print("   Checking weight dimensions...")
weights_path = hf_hub_download(MODEL_ID, "model.safetensors")
state_dict = load_file(weights_path)
if "label_proj_layer.weight" in state_dict:
    proj_size = state_dict["label_proj_layer.weight"].shape[0]
    expected_num_labels = proj_size + 1  # +1 for PAD
    if gec_config.num_labels != expected_num_labels:
        print(f"   Adjusting: config has {gec_config.num_labels}, weights need {expected_num_labels}")
        gec_config.num_labels = expected_num_labels
    print(f"   Final num_labels: {gec_config.num_labels} (proj layer: {proj_size})")

n_labels = gec_config.num_labels

print("   Constructing model...")
model = GECToR(gec_config)

print("   Loading safetensors weights...")
load_result = model.load_state_dict(state_dict, strict=False)
if load_result.missing_keys:
    # Missing keys for BERT sub-model are expected since GECToR
    # constructor already loaded bert-base-cased inside __init__
    print(f"   ({len(load_result.missing_keys)} BERT keys already loaded by constructor)")

model.eval()
model.cpu()

tokenizer = AutoTokenizer.from_pretrained(MODEL_ID)
print("   Model and tokenizer loaded")

# ── Step 2: Export to ONNX ───────────────────────────────
print("[2/5] Exporting to ONNX...")

dummy_text = "This is a test sentence."
inputs = tokenizer(dummy_text, return_tensors="pt", padding="max_length", max_length=128, truncation=True)

onnx_fp32_path = OUT_DIR / "model_fp32.onnx"

# Wrapper that returns only the logits tensor (torch.onnx.export needs plain tensors)
class GECToRLogitsWrapper(torch.nn.Module):
    def __init__(self, gector_model):
        super().__init__()
        self.gector = gector_model
    
    def forward(self, input_ids, attention_mask):
        output = self.gector(input_ids, attention_mask)
        return output.logits_labels

wrapper = GECToRLogitsWrapper(model)
wrapper.eval()

with torch.no_grad():
    torch.onnx.export(
        wrapper,
        (inputs["input_ids"].cpu(), inputs["attention_mask"].cpu()),
        str(onnx_fp32_path),
        input_names=["input_ids", "attention_mask"],
        output_names=["logits"],
        dynamic_axes={
            "input_ids": {0: "batch", 1: "seq_len"},
            "attention_mask": {0: "batch", 1: "seq_len"},
            "logits": {0: "batch", 1: "seq_len"},
        },
        opset_version=14,
        do_constant_folding=True,
    )
print(f"   FP32 ONNX: {onnx_fp32_path.stat().st_size / 1024 / 1024:.1f} MB")

# ── Step 3: INT8 Quantize ────────────────────────────────
print("[3/5] Quantizing to INT8...")
from onnxruntime.quantization import quantize_dynamic, QuantType

onnx_int8_path = OUT_DIR / "model.onnx"
quantize_dynamic(
    str(onnx_fp32_path),
    str(onnx_int8_path),
    weight_type=QuantType.QInt8,
)
print(f"   INT8 ONNX: {onnx_int8_path.stat().st_size / 1024 / 1024:.1f} MB")

onnx_fp32_path.unlink()

# ── Step 4: Save tokenizer + labels ──────────────────────
print("[4/5] Saving tokenizer and labels...")
tokenizer.save_pretrained(str(OUT_DIR))

id2label = config_dict.get("id2label", {})
labels = ["$KEEP"] * n_labels
for idx_str, label in id2label.items():
    idx = int(idx_str)
    if idx < n_labels:
        labels[idx] = label
with open(OUT_DIR / "labels.json", "w") as f:
    json.dump(labels, f)
print(f"   labels.json: {n_labels} labels")

# ── Step 5: Download verb-form-vocab.txt ─────────────────
print("[5/5] Downloading verb-form-vocab.txt...")
verb_url = "https://raw.githubusercontent.com/grammarly/gector/master/data/verb-form-vocab.txt"
verb_path = OUT_DIR / "verb-form-vocab.txt"
urllib.request.urlretrieve(verb_url, str(verb_path))
print(f"   verb-form-vocab.txt: {verb_path.stat().st_size / 1024:.1f} KB")

print(f"\nDone! All files exported to: {OUT_DIR}")
print("  Files:")
for f in sorted(OUT_DIR.iterdir()):
    size = f.stat().st_size
    if size > 1024 * 1024:
        print(f"    {f.name:30s} {size / 1024 / 1024:.1f} MB")
    else:
        print(f"    {f.name:30s} {size / 1024:.1f} KB")

print("\nNext steps:")
print("  1. Zip the gector-onnx/ directory into gector-bert-base-onnx.zip")
print("  2. Upload as a GitHub release asset")
print("  3. Update MODEL_URL in src-tauri/src/gec.rs")
