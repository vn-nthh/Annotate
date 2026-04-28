import json, numpy as np, onnxruntime as ort
from transformers import AutoTokenizer

sess = ort.InferenceSession('scripts/gector-onnx/model.onnx')
tok = AutoTokenizer.from_pretrained('scripts/gector-onnx')
labels = json.load(open('scripts/gector-onnx/labels.json'))

text = 'I goes to the store yesterday'
enc = tok(text, return_tensors='np', padding=False, truncation=True, max_length=128)
ids = enc['input_ids'].astype(np.int64)
mask = enc['attention_mask'].astype(np.int64)

out = sess.run(None, {'input_ids': ids, 'attention_mask': mask})
logits = out[0]
print(f'Output shape: {logits.shape}')
print(f'Labels count: {len(labels)}')
preds = np.argmax(logits[0], axis=-1)
tokens = tok.convert_ids_to_tokens(ids[0])
for t, p in zip(tokens, preds):
    tag = labels[p] if p < len(labels) else '???'
    if tag != '$KEEP':
        print(f'  {t:15s} -> {tag}')
    else:
        print(f'  {t:15s}    (keep)')
