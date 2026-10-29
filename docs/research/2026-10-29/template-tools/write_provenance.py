"""Assemble machine-readable provenance + pins for each model into <model>/PROVENANCE.json
from the saved HF API responses, local hashes and the render summaries."""
import hashlib
import json
import os

ROOT = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
API = ROOT + '_api/'
LCPP = '/home/kristian/Рабочий стол/llama.cpp/models/templates/'
DISK = '/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/'


def sha256_file(p):
    return hashlib.sha256(open(p, 'rb').read()).hexdigest()


def tree_entry(tree_file, path):
    for f in json.load(open(API + tree_file)):
        if f['path'] == path:
            lc = f.get('lastCommit') or {}
            return {'blob_oid': f.get('oid'), 'size': f.get('size'), 'lfs_sha256': (f.get('lfs') or {}).get('oid'),
                    'last_commit': {'id': lc.get('id'), 'date': lc.get('date'), 'title': lc.get('title')}}
    return None


def commits(name):
    return {c['id']: (c['date'], c['title']) for c in json.load(open(API + f'commits_{name}.json'))}


def history(model, name, files):
    cm = commits(name)
    out = []
    for f in sorted(os.listdir(ROOT + model + '/upstream/history')):
        if not f.endswith('chat_template.jinja'):
            continue
        short = f.split('.')[0]
        full = next(k for k in cm if k.startswith(short))
        out.append({'commit': full, 'date': cm[full][0], 'title': cm[full][1],
                    'chat_template_sha256': sha256_file(ROOT + model + '/upstream/history/' + f), 'saved_as': 'upstream/history/' + f})
    return sorted(out, key=lambda x: x['date'])


def caps(model):
    return {r['variant']: {'detected_format': r.get('detected_format'), 'caps': r.get('caps')}
            for r in json.load(open(ROOT + model + '/rendered/_summary.json')) if 'caps' in r}


def meta(model):
    m = json.load(open(ROOT + model + '/gguf-meta.json'))
    return {k: m[k] for k in m if k.startswith('general.') or k in ('templates', 'special_ids', 'file')}


INV = json.load(open(API + 'kwargs_inventory.json'))
TODAY = '2026-10-29'

P = {}

# ------------------------------------------------------------------ gpt-oss-20b
P['gpt-oss-20b'] = {
    'checked_on': TODAY,
    'canonical': {
        'repo': 'openai/gpt-oss-20b', 'main_revision': '6cee5e81ee83917806bbde320786a8fb61efebee',
        'file': 'chat_template.jinja', **tree_entry('tree_openai_gpt-oss-20b.json', 'chat_template.jinja'),
        'sha256': sha256_file(ROOT + 'gpt-oss-20b/canonical.jinja'),
        'url': 'https://huggingface.co/openai/gpt-oss-20b/resolve/6cee5e81ee83917806bbde320786a8fb61efebee/chat_template.jinja',
        'tokenizer_config_json': {'sha256': sha256_file(ROOT + 'gpt-oss-20b/upstream/main@6cee5e81.tokenizer_config.json'),
                                  'has_chat_template': False},
        'note': 'identical blob in openai/gpt-oss-120b (dc7bb119..., 2025-08-08); chat_template.json removed in ef533c92 (#59)',
        'saved_as': 'canonical.jinja',
    },
    'upstream_history': history('gpt-oss-20b', 'openai_gpt-oss-20b', None),
    'gguf_embedded': {
        'gguf_path': DISK + 'models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf',
        'source_repo': 'unsloth/gpt-oss-20b-GGUF', 'downloaded_revision': 'd449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4',
        'file': tree_entry('tree_unsloth_gpt-oss-20b-GGUF.json', 'gpt-oss-20b-F16.gguf'),
        'identical_to_repo_main_on_check_date': True,
        'metadata': meta('gpt-oss-20b'), 'saved_as': 'gguf-embedded.jinja',
        'sha256': sha256_file(ROOT + 'gpt-oss-20b/gguf-embedded.jinja'),
        'author': 'Unsloth (header: "Chat template fixes by Unsloth")',
    },
    'other_copies': [
        {'path': DISK + 'models/gpt-oss-20b-bf16/chat_template.jinja', 'sha256': sha256_file(DISK + 'models/gpt-oss-20b-bf16/chat_template.jinja'),
         'equals': 'canonical', 'hf_download_metadata': 'openai/gpt-oss-20b@6cee5e81, blob dc7bb119'},
        {'path': LCPP + 'openai-gpt-oss-120b.jinja', 'sha256': sha256_file(LCPP + 'openai-gpt-oss-120b.jinja'),
         'equals': 'canonical', 'llama_cpp_checkout': '5e03a1ff4 (2026-10-16)'},
    ],
    'reference_renderer': {
        'name': 'openai/harmony (openai-harmony 0.0.8, git abd677f7ac962629c808197caa1feb9e3e95d2b0)',
        'golden_render_seq': 'rendered/seq.effort-high.harmony-reference-0.0.8.txt',
        'golden_render_seq_sha256': sha256_file(ROOT + 'gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.txt'),
        'vocab': 'o200k_base rebuilt from local tokenizer.json, sha256 446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d (matches tiktoken)',
    },
    'kwargs_surface': {'canonical': ['messages', 'tools', 'add_generation_prompt', 'reasoning_effort', 'model_identity', 'builtin_tools'],
                       'gguf_embedded_extra': ['developer_instructions'], 'globals': ['strftime_now', 'raise_exception']},
    'llama_cpp_5e03a1ff4': caps('gpt-oss-20b'),
    'pins': {
        'generic_chat_template_file': {'file': 'canonical.jinja', 'sha256': 'a4c9919cbbd4acdd51ccffe22da049264b1b73e59055fa58811a99efbd7c8146',
                                       'source': 'openai/gpt-oss-20b@6cee5e81 (file commit cbf31f62, 2025-08-08)'},
        'oracle': {'primary': 'openai-harmony 0.0.8 (abd677f7) + manual CoT-drop rule',
                   'secondary_jinja': 'canonical.jinja a4c9919c... (only for system/developer header, tool namespace of flat schemas, CoT-drop parity)'},
        'kwargs': {'reasoning_effort': 'low|medium|high (set explicitly; template default medium)', 'model_identity': 'leave undefined',
                   'builtin_tools': 'leave undefined', 'llama_server': '--reasoning-effort <level>; --reasoning-preserve has no effect (caps.preserve_reasoning=false)'},
    },
}

# ------------------------------------------------------------------ Gemma 4 26B A4B
P['gemma-4-26B-A4B-it'] = {
    'checked_on': TODAY,
    'canonical': {
        'repo': 'google/gemma-4-26B-A4B-it', 'main_revision': '4d7ae4984b7db7de8f8457170b3f1a419ee76d52',
        'file': 'chat_template.jinja', **tree_entry('tree_google_gemma-4-26B-A4B-it.json', 'chat_template.jinja'),
        'sha256': sha256_file(ROOT + 'gemma-4-26B-A4B-it/canonical.jinja'),
        'url': 'https://huggingface.co/google/gemma-4-26B-A4B-it/resolve/4d7ae4984b7db7de8f8457170b3f1a419ee76d52/chat_template.jinja',
        'tokenizer_config_json': {'sha256': sha256_file(ROOT + 'gemma-4-26B-A4B-it/upstream/main@4d7ae498.tokenizer_config.json'),
                                  'has_chat_template': False, 'has_response_template': True,
                                  'last_commit': tree_entry('tree_google_gemma-4-26B-A4B-it.json', 'tokenizer_config.json')['last_commit']},
        'header': 'Published: 2026-07-09 (Google Gemma 4 Canonical Chat Template)',
        'saved_as': 'canonical.jinja',
    },
    'upstream_history': history('gemma-4-26B-A4B-it', 'google_gemma-4-26B-A4B-it', None),
    'gguf_embedded': {
        'gguf_path': DISK + 'gemma-4-26B-A4B-it-UD-Q4_K_M.gguf',
        'source_repo': 'unsloth/gemma-4-26B-A4B-it-GGUF', 'downloaded_revision': 'c099eb48e663fd284577b04978a94ffccb261841',
        'file': tree_entry('tree_unsloth_gemma-4-26B-A4B-it-GGUF.json', 'gemma-4-26B-A4B-it-UD-Q4_K_M.gguf'),
        'identical_to_repo_main_on_check_date': True,
        'metadata': meta('gemma-4-26B-A4B-it'), 'saved_as': 'gguf-embedded.jinja',
        'sha256': sha256_file(ROOT + 'gemma-4-26B-A4B-it/gguf-embedded.jinja'),
        'author': 'Unsloth: July canonical minus header comment + string-arguments fallback',
    },
    'other_copies': [
        {'path': DISK + 'models/gemma4-26b-nvfp4/chat_template.jinja', 'sha256': sha256_file(DISK + 'models/gemma4-26b-nvfp4/chat_template.jinja'),
         'equals': 'upstream 4c55b528 (2026-04-28, #36)', 'hf_download_metadata': 'nvidia/Gemma-4-26B-A4B-NVFP4@a19cfe00, blob f62ca843 (still main on 2026-10-29)'},
        {'path': LCPP + 'google-gemma-4-31B-it.jinja', 'sha256': sha256_file(LCPP + 'google-gemma-4-31B-it.jinja'),
         'equals': 'upstream 1db3cff1 (2026-04-10, #21) of the 26B-A4B repo', 'llama_cpp_checkout': '5e03a1ff4'},
        {'path': LCPP + 'google-gemma-4-31B-it-interleaved.jinja', 'sha256': sha256_file(LCPP + 'google-gemma-4-31B-it-interleaved.jinja'),
         'equals': 'llama.cpp-local variant of the 2026-04-02 release (test fixture only)'},
    ],
    'kwargs_surface': {'canonical': ['messages', 'tools', 'add_generation_prompt', 'bos_token', 'enable_thinking', 'preserve_thinking'],
                       'globals': ['raise_exception']},
    'llama_cpp_5e03a1ff4': caps('gemma-4-26B-A4B-it'),
    'pins': {
        'generic_chat_template_file': {'file': 'canonical.jinja', 'sha256': 'ae53464bf3be25802b3a5b37def7fd89667067d7577049b3b2d74c4d8de4c6d4',
                                       'source': 'google/gemma-4-26B-A4B-it@4d7ae498 (file commit 35b4173c, 2026-07-15, #47)'},
        'oracle': {'primary': 'canonical.jinja ae53464b... under Jinja2 (transformers env)',
                   'parse_rules': 'tokenizer_config.json response_template @4d7ae498 sha256 9f4fec4b1dc6ecddf8f4a92e9caea5971c0e67d81309f3f9066a2bee8c362633'},
        'kwargs': {'enable_thinking': 'true (explicit; template default false)',
                   'preserve_thinking': 'false = Google-documented behaviour (template default); llama-server default flips it to true -> pass --no-reasoning-preserve unless deliberately testing preserve',
                   'bos_token': '<bos> (llama.cpp strips it and adds the BOS token)'},
    },
}

# ------------------------------------------------------------------ Qwen3.8-27B
P['Qwen3.8-27B'] = {
    'checked_on': TODAY,
    'canonical': {
        'repo': 'Qwen/Qwen3.8-27B', 'main_revision': '1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0',
        'file': 'chat_template.jinja', **tree_entry('tree_Qwen_Qwen3.8-27B.json', 'chat_template.jinja'),
        'sha256': sha256_file(ROOT + 'Qwen3.8-27B/canonical.jinja'),
        'url': 'https://huggingface.co/Qwen/Qwen3.8-27B/resolve/1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0/chat_template.jinja',
        'tokenizer_config_json': {'sha256': sha256_file(ROOT + 'Qwen3.8-27B/upstream/main@1d4bf0f2.tokenizer_config.json'),
                                  'has_chat_template': True, 'chat_template_sha256': 'c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041',
                                  'identical_to_chat_template_jinja': True,
                                  'last_commit': tree_entry('tree_Qwen_Qwen3.8-27B.json', 'tokenizer_config.json')['last_commit']},
        'saved_as': 'canonical.jinja',
    },
    'upstream_history': history('Qwen3.8-27B', 'Qwen_Qwen3.8-27B', None),
    'gguf_embedded': {
        'gguf_path': DISK + 'Qwen3.8-27B-Q4_0.gguf',
        'source_repo': 'unsloth/Qwen3.8-27B-GGUF', 'downloaded_revision': '4ca720788d1e01f1bff70c033e0d0028fd02e502',
        'file': tree_entry('tree_unsloth_Qwen3.8-27B-GGUF.json', 'Qwen3.8-27B-Q4_0.gguf'),
        'identical_to_repo_main_on_check_date': True,
        'metadata': meta('Qwen3.8-27B'), 'saved_as': 'gguf-embedded.jinja',
        'sha256': sha256_file(ROOT + 'Qwen3.8-27B/gguf-embedded.jinja'),
        'author': 'Unsloth ("Unsloth fixes - developer role, merged system messages, tool calling")',
    },
    'other_copies': [
        {'path': LCPP + 'Qwen3.5-4B.jinja', 'sha256': sha256_file(LCPP + 'Qwen3.5-4B.jinja'),
         'equals': 'none - Qwen3.5 ancestor (no reasoning_effort/preserve_thinking; non-string args via |string)'},
    ],
    'kwargs_surface': {'canonical': ['messages', 'tools', 'add_generation_prompt', 'enable_thinking', 'preserve_thinking', 'reasoning_effort', 'add_vision_id'],
                       'globals': ['raise_exception']},
    'llama_cpp_5e03a1ff4': caps('Qwen3.8-27B'),
    'pins': {
        'generic_chat_template_file': {'file': 'canonical.jinja', 'sha256': 'c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041',
                                       'source': 'Qwen/Qwen3.8-27B@1d4bf0f2 (file commit 412f8b6b, 2026-08-13)'},
        'oracle': {'primary': 'canonical.jinja c3cf9e34... under Jinja2 (transformers env)'},
        'kwargs': {'enable_thinking': 'true (explicit)', 'preserve_thinking': 'true (explicit; template and model-card default)',
                   'reasoning_effort': 'one of xhigh|medium|low, explicit (undefined => xhigh line injected into system; "high" raises)',
                   'add_vision_id': 'leave undefined unless multi-image numbering is wanted',
                   'llama_server': '--reasoning-effort xhigh|medium|low (never high); keep default --reasoning-preserve'},
    },
}

for model, data in P.items():
    data['kwargs_inventory_raw'] = {k: v for k, v in INV.items() if k.split()[0] in model.lower() or (model.startswith('gemma') and k.startswith('gemma'))}
    with open(ROOT + model + '/PROVENANCE.json', 'w', encoding='utf-8') as fh:
        json.dump(data, fh, ensure_ascii=False, indent=1)
    print(model, 'ok', len(json.dumps(data)))
