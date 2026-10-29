"""Read-only extraction of chat templates + provenance metadata from the owner's GGUFs.
Writes <model>/gguf-embedded.jinja (+ any named variants) and <model>/gguf-meta.json."""
import hashlib
import json
import os
import sys

SCRATCH = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad'
OUT = os.path.join(SCRATCH, 'templates')
sys.argv = sys.argv[:1]  # gguf_meta runs its CLI loop over sys.argv[1:] at import time
sys.path.insert(0, SCRATCH)
from gguf_meta import meta  # noqa: E402

DISK = '/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e'
GGUFS = {
    'gpt-oss-20b': DISK + '/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf',
    'gemma-4-26B-A4B-it': DISK + '/gemma-4-26B-A4B-it-UD-Q4_K_M.gguf',
    'Qwen3.8-27B': DISK + '/Qwen3.8-27B-Q4_0.gguf',
}
for model, path in GGUFS.items():
    version, n_tensors, kv = meta(path)
    toks = kv.get('tokenizer.ggml.tokens') or []
    types = kv.get('tokenizer.ggml.token_type') or []
    info = {'file': os.path.basename(path), 'gguf_version': version, 'n_tensors': n_tensors, 'templates': {}}
    for k, v in kv.items():
        if k.startswith('general.') or (k.startswith('tokenizer.') and not isinstance(v, list) and 'chat_template' not in k):
            info[k] = v
    for k, v in kv.items():
        if 'chat_template' in k:
            name = 'gguf-embedded.jinja' if k == 'tokenizer.chat_template' else 'gguf-embedded.' + k.split('.')[-1] + '.jinja'
            with open(os.path.join(OUT, model, name), 'w', encoding='utf-8', newline='') as fh:
                fh.write(v)
            info['templates'][k] = {'saved_as': name, 'len': len(v), 'sha256': hashlib.sha256(v.encode()).hexdigest()}
    special = {}
    for key in ('bos_token_id', 'eos_token_id', 'eot_token_id', 'eom_token_id', 'padding_token_id', 'unknown_token_id', 'sep_token_id'):
        tid = kv.get('tokenizer.ggml.' + key)
        if isinstance(tid, int) and tid < len(toks):
            special[key] = [tid, toks[tid]]
    info['special_ids'] = special
    # control / user-defined tokens (types 3 = CONTROL, 4 = USER_DEFINED) - names only, truncated
    ctrl = [(i, t, types[i]) for i, t in enumerate(toks) if i < len(types) and types[i] in (3, 4)]
    info['n_control_or_user_defined'] = len(ctrl)
    info['control_or_user_defined_sample'] = [[i, t, ty] for i, t, ty in ctrl if not t.startswith(('<unused', '<|reserved', '[multimodal'))][:120]
    with open(os.path.join(OUT, model, 'gguf-meta.json'), 'w', encoding='utf-8') as fh:
        json.dump(info, fh, ensure_ascii=False, indent=1)
    print(model, json.dumps(info['templates']), {k: v for k, v in info.items() if k in ('general.name', 'general.repo_url', 'general.quantized_by', 'general.source.url', 'general.source.huggingface.repository', 'general.base_model.0.repo_url')})
