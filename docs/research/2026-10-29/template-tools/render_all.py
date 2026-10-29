"""Render every fixed scenario through every template variant with three engines:

  py-hf    : Python Jinja2 configured like transformers.apply_chat_template, messages as an
             HF user passes them (the golden-oracle view).
  py-lcpp  : same Jinja2 engine, but messages/kwargs pre-processed exactly like llama.cpp
             5e03a1ff4 does before rendering (isolates pure Jinja-engine differences).
  lcpp     : llama.cpp's own pipeline (common_chat_templates_apply) via _lcpp/lcpp_render.

usage: uv run --with jinja2 python -I render_all.py
Outputs: <model>/rendered/*.txt, <model>/rendered/_summary.json, <model>/diffs/render__*.diff
"""
import copy
import datetime
import difflib
import hashlib
import json
import os
import subprocess

import jinja2
from jinja2 import nodes
from jinja2.ext import Extension, loopcontrols
from jinja2.sandbox import ImmutableSandboxedEnvironment

ROOT = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates'
SCEN = os.path.join(ROOT, '_scenarios')
HARNESS = os.path.join(ROOT, '_lcpp', 'lcpp_render')
LCPP_TPL = '/home/kristian/Рабочий стол/llama.cpp/models/templates/'
DISK = '/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/'
TODAY = datetime.datetime.now(datetime.timezone.utc)


# ---------------------------------------------------------------- Jinja2 env like transformers
class AssistantTracker(Extension):
    tags = {'generation'}

    def parse(self, parser):
        lineno = next(parser.stream).lineno
        body = parser.parse_statements(['name:endgeneration'], drop_needle=True)
        return nodes.CallBlock(self.call_method('_render', []), [], [], body).set_lineno(lineno)

    def _render(self, caller):
        return caller()


def make_env():
    env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True, extensions=[AssistantTracker, loopcontrols])

    def tojson(x, ensure_ascii=False, indent=None, separators=None, sort_keys=False):
        return json.dumps(x, ensure_ascii=ensure_ascii, indent=indent, separators=separators, sort_keys=sort_keys)

    def raise_exception(message):
        raise jinja2.exceptions.TemplateError(message)

    env.filters['tojson'] = tojson
    env.globals['raise_exception'] = raise_exception
    env.globals['strftime_now'] = lambda fmt: TODAY.strftime(fmt)
    return env


ENV = make_env()

# ---------------------------------------------------------------- models / variants / kwargs
MODELS = {
    'gpt-oss-20b': {
        'special': {'bos_token': '<|startoftext|>', 'eos_token': '<|return|>', 'pad_token': '<|endoftext|>'},
        'lcpp_tokens': {'bos_token': '<|startoftext|>', 'eos_token': '<|return|>'},
        'variants': {
            'canonical': 'gpt-oss-20b/canonical.jinja',
            'gguf': 'gpt-oss-20b/gguf-embedded.jinja',
        },
        'kwsets': {
            # hf: kwargs an HF user passes; lcpp: request fields for llama-server
            'effort-high': {'hf': {'reasoning_effort': 'high'},
                            'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True, 'reasoning_effort': 'high'}}},
            'defaults': {'hf': {},
                         'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True}}},
        },
    },
    'gemma-4-26B-A4B-it': {
        'special': {'bos_token': '<bos>', 'eos_token': '<eos>', 'pad_token': '<pad>', 'unk_token': '<unk>'},
        'lcpp_tokens': {'bos_token': '<bos>', 'eos_token': '<turn|>'},
        'variants': {
            'canonical': 'gemma-4-26B-A4B-it/canonical.jinja',
            'gguf': 'gemma-4-26B-A4B-it/gguf-embedded.jinja',
            'disk-nvfp4-2026-04-28': DISK + 'gemma4-26b-nvfp4/chat_template.jinja',
            'llamacpp-bundled-2026-04-10': LCPP_TPL + 'google-gemma-4-31B-it.jinja',
        },
        'kwsets': {
            'think-preserve': {'hf': {'enable_thinking': True, 'preserve_thinking': True},
                               'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True}}},
            'think-nopreserve': {'hf': {'enable_thinking': True, 'preserve_thinking': False},
                                 'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': False}}},
            'nothink': {'hf': {'enable_thinking': False},
                        'lcpp': {'enable_thinking': False, 'chat_template_kwargs': {'preserve_reasoning': True}}},
            'hf-defaults': {'hf': {},
                            'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True}}},
        },
    },
    'Qwen3.8-27B': {
        'special': {'eos_token': '<|im_end|>', 'pad_token': '<|endoftext|>'},
        'lcpp_tokens': {'bos_token': '<|endoftext|>', 'eos_token': '<|im_end|>'},
        'variants': {
            'canonical': 'Qwen3.8-27B/canonical.jinja',
            'gguf': 'Qwen3.8-27B/gguf-embedded.jinja',
            'upstream-initial-72a217af': 'Qwen3.8-27B/upstream/history/72a217afab80.chat_template.jinja',
        },
        'kwsets': {
            'think-medium-preserve': {'hf': {'enable_thinking': True, 'preserve_thinking': True, 'reasoning_effort': 'medium'},
                                      'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True, 'reasoning_effort': 'medium'}}},
            'think-medium-nopreserve': {'hf': {'enable_thinking': True, 'preserve_thinking': False, 'reasoning_effort': 'medium'},
                                        'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': False, 'reasoning_effort': 'medium'}}},
            'think-high': {'hf': {'enable_thinking': True, 'reasoning_effort': 'high'},
                           'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True, 'reasoning_effort': 'high'}}},
            'nothink': {'hf': {'enable_thinking': False},
                        'lcpp': {'enable_thinking': False, 'chat_template_kwargs': {'preserve_reasoning': True}}},
            'hf-defaults': {'hf': {},
                            'lcpp': {'enable_thinking': True, 'chat_template_kwargs': {'preserve_reasoning': True}}},
        },
    },
}

# scenario -> kwsets to run (others are skipped to keep the matrix readable)
MAIN_KW = {'gpt-oss-20b': ['effort-high', 'defaults'],
           'gemma-4-26B-A4B-it': ['think-preserve', 'think-nopreserve', 'nothink', 'hf-defaults'],
           'Qwen3.8-27B': ['think-medium-preserve', 'think-medium-nopreserve', 'think-high', 'nothink', 'hf-defaults']}
LCPP_SKIP_SCENARIOS = {'multimodal'}  # needs mtmd media loading; template placeholders are py-only


def load_variant(path):
    p = path if path.startswith('/') else os.path.join(ROOT, path)
    with open(p, encoding='utf-8') as fh:
        return fh.read()


# ---------------------------------------------------------------- message adapters
def adapt_hf(model, msgs):
    """What an HF transformers user would pass natively for this template family."""
    out = copy.deepcopy(msgs)
    if model == 'gpt-oss-20b':
        for m in out:
            if 'reasoning_content' in m:
                m['thinking'] = m.pop('reasoning_content')
    return out


def lcpp_caps(model, src):
    """Caps as reported by the harness for this template (filled lazily)."""
    return CAPS_CACHE[(model, src)]


def adapt_lcpp(model, msgs, caps):
    """Mirror llama.cpp 5e03a1ff4: common_chat_msgs_parse_oaicompat -> to_json_oaicompat ->
    workarounds (developer->system, non-null content, object args) -> gpt-oss handler."""
    out = []
    for m in msgs:
        j = {'role': m['role']}
        c = m.get('content')
        if isinstance(c, str) and c != '':
            j['content'] = c
        elif isinstance(c, list) and c:
            texts = [p.get('text', '') for p in c if p.get('type') == 'text']
            j['content'] = '\n'.join(texts) if not caps.get('supports_typed_content') or caps.get('supports_string_content') else [{'type': 'text', 'text': t} for t in texts]
        else:
            j['content'] = ''
        if m.get('reasoning_content'):
            j['reasoning_content'] = m['reasoning_content']
        if m.get('name'):
            j['name'] = m['name']
        if m.get('tool_call_id'):
            j['tool_call_id'] = m['tool_call_id']
        if m.get('tool_calls'):
            tcs = []
            for tc in m['tool_calls']:
                args = tc['function']['arguments']
                args = args if isinstance(args, str) else json.dumps(args, ensure_ascii=False, separators=(',', ':'))
                t = {'type': 'function', 'function': {'name': tc['function']['name'], 'arguments': args}}
                if tc.get('id'):
                    t['id'] = tc['id']
                tcs.append(t)
            j['tool_calls'] = tcs
        out.append(j)
    if model != 'gpt-oss-20b':
        for j in out:
            if j['role'] == 'developer':
                j['role'] = 'system'
    if caps.get('supports_object_arguments'):
        for j in out:
            for tc in j.get('tool_calls', []):
                if isinstance(tc['function']['arguments'], str):
                    tc['function']['arguments'] = json.loads(tc['function']['arguments'])
    if model == 'gpt-oss-20b':
        for j in out:
            if isinstance(j.get('reasoning_content'), str):
                j['thinking'] = j['reasoning_content']
                if j.get('tool_calls'):
                    j.pop('content', None)
    return out


def lcpp_patch_source(src):
    """llama.cpp rewrites the gpt-oss content check to `if false` at load time (chat.cpp:762)."""
    needle = ('{%- if "<|channel|>analysis<|message|>" in message.content or '
              '"<|channel|>final<|message|>" in message.content %}')
    if '<|channel|>' in src and 'in message.content or' in src:
        src = src.replace(needle, '{%- if false %}')
    return src


def lcpp_kwargs(req):
    kw = {'enable_thinking': req.get('enable_thinking', True)}
    ctk = dict(req.get('chat_template_kwargs', {}))
    if isinstance(ctk.get('preserve_reasoning'), bool):
        on = ctk['preserve_reasoning']
        kw.update({'preserve_thinking': on, 'clear_thinking': not on, 'truncate_history_thinking': not on, 'drop_thinking': not on})
    if isinstance(ctk.get('reasoning_effort'), str) and ctk['reasoning_effort']:
        kw.update({'reasoning_effort': ctk['reasoning_effort'], 'reasoning_strength': ctk['reasoning_effort']})
    kw['datetime'] = TODAY.strftime('%b %d %Y')
    kw['date_string'] = TODAY.strftime('%d %b %Y')
    kw.update(ctk)
    return kw


def render_py(src, messages, tools, add_gen, kwargs):
    tmpl = ENV.from_string(src)
    ctx = dict(kwargs)
    ctx['messages'] = messages
    if tools:
        ctx['tools'] = tools
    ctx['add_generation_prompt'] = add_gen
    return tmpl.render(**ctx)


def run_lcpp(tpl_path, request, tmpdir, tag):
    req_path = os.path.join(tmpdir, tag + '.request.json')
    with open(req_path, 'w', encoding='utf-8') as fh:
        json.dump(request, fh, ensure_ascii=False)
    env = dict(os.environ, CUDA_VISIBLE_DEVICES='', TZ='UTC')
    p = subprocess.run([HARNESS, tpl_path, req_path], capture_output=True, env=env, timeout=120)
    meta_txt = p.stderr.decode('utf-8', 'replace').strip()
    # the harness prints exactly one JSON document on stderr (llama.cpp may log before it)
    start = meta_txt.find('{')
    try:
        meta = json.loads(meta_txt[start:]) if start >= 0 else {'error': meta_txt}
    except json.JSONDecodeError:
        meta = {'error': meta_txt[-2000:]}
    return p.returncode, p.stdout.decode('utf-8', 'replace'), meta


CAPS_CACHE = {}


def sha(s):
    return hashlib.sha256(s.encode('utf-8')).hexdigest()[:12]


def main():
    scenarios = {}
    for fn in sorted(os.listdir(SCEN)):
        if fn.endswith('.json'):
            with open(os.path.join(SCEN, fn), encoding='utf-8') as fh:
                scenarios[fn[:-5]] = json.load(fh)
    for model, cfg in MODELS.items():
        rdir = os.path.join(ROOT, model, 'rendered')
        tmpdir = os.path.join(ROOT, model, 'rendered', '_requests')
        os.makedirs(tmpdir, exist_ok=True)
        summary = []
        for vname, vpath in cfg['variants'].items():
            src = load_variant(vpath)
            tpl_abs = vpath if vpath.startswith('/') else os.path.join(ROOT, vpath)
            # caps from llama.cpp (one cheap call)
            rc, _, meta = run_lcpp(tpl_abs, {'messages': [{'role': 'user', 'content': 'x'}], **cfg['lcpp_tokens']}, tmpdir, f'caps.{vname}')
            caps = meta.get('caps', {})
            CAPS_CACHE[(model, vname)] = caps
            summary.append({'variant': vname, 'caps': caps, 'detected_format': meta.get('format'), 'caps_error': meta.get('error')})
            for sname, sc in scenarios.items():
                for kwname in MAIN_KW[model]:
                    kw = cfg['kwsets'][kwname]
                    base = f'{sname}.{kwname}.{vname}'
                    row = {'scenario': sname, 'kwset': kwname, 'variant': vname}
                    # py-hf
                    try:
                        out = render_py(src, adapt_hf(model, sc['messages']), sc.get('tools'), True, {**cfg['special'], **kw['hf']})
                        name = base + '.py-hf.txt'
                    except Exception as e:  # noqa: BLE001
                        out = f'{type(e).__name__}: {e}'
                        name = base + '.py-hf.ERROR.txt'
                    with open(os.path.join(rdir, name), 'w', encoding='utf-8') as fh:
                        fh.write(out)
                    row['py-hf'] = sha(out) if 'ERROR' not in name else 'ERROR: ' + out[:160]
                    if sname in LCPP_SKIP_SCENARIOS:
                        summary.append(row)
                        continue
                    req = {'messages': sc['messages'], **cfg['lcpp_tokens'], 'add_generation_prompt': True, **copy.deepcopy(kw['lcpp'])}
                    if sc.get('tools'):
                        req['tools'] = sc['tools']
                    # py-lcpp (llama.cpp-equivalent preprocessing, Jinja2 engine)
                    try:
                        msgs = adapt_lcpp(model, sc['messages'], caps)
                        kwargs = {**cfg['lcpp_tokens'], **lcpp_kwargs(req)}
                        out2 = render_py(lcpp_patch_source(src), msgs, sc.get('tools'), True, kwargs)
                        name2 = base + '.py-lcpp.txt'
                    except Exception as e:  # noqa: BLE001
                        out2 = f'{type(e).__name__}: {e}'
                        name2 = base + '.py-lcpp.ERROR.txt'
                    with open(os.path.join(rdir, name2), 'w', encoding='utf-8') as fh:
                        fh.write(out2)
                    row['py-lcpp'] = sha(out2) if 'ERROR' not in name2 else 'ERROR: ' + out2[:160]
                    # real llama.cpp pipeline
                    rc, out3, meta = run_lcpp(tpl_abs, req, tmpdir, base)
                    if rc == 0:
                        name3 = base + '.lcpp.txt'
                        row['lcpp'] = sha(out3)
                        row['lcpp_generation_prompt'] = meta.get('generation_prompt')
                    else:
                        out3 = json.dumps(meta, ensure_ascii=False)
                        name3 = base + '.lcpp.ERROR.txt'
                        row['lcpp'] = 'ERROR: ' + out3[:200]
                    with open(os.path.join(rdir, name3), 'w', encoding='utf-8') as fh:
                        fh.write(out3)
                    row['engine_equal(py-lcpp==lcpp)'] = row.get('py-lcpp') == row.get('lcpp')
                    row['oracle_equal(py-hf==lcpp)'] = row.get('py-hf') == row.get('lcpp')
                    if rc == 0 and 'ERROR' not in name2 and out2 != out3:
                        d = difflib.unified_diff(out2.splitlines(True), out3.splitlines(True), 'py-lcpp (Jinja2)', 'lcpp (llama.cpp jinja)', n=1)
                        with open(os.path.join(ROOT, model, 'diffs', f'render__{base}__py-lcpp_vs_lcpp.diff'), 'w', encoding='utf-8') as fh:
                            fh.writelines(d)
                    summary.append(row)
        with open(os.path.join(rdir, '_summary.json'), 'w', encoding='utf-8') as fh:
            json.dump(summary, fh, ensure_ascii=False, indent=1)
        print('==', model)
        for r in summary:
            if 'caps' in r:
                print('  caps', r['variant'], r.get('detected_format'), json.dumps(r['caps'], sort_keys=True), r.get('caps_error') or '')
        for r in summary:
            if 'scenario' in r:
                flag = ''
                if 'lcpp' in r:
                    flag = 'ENG=' + ('ok' if r['engine_equal(py-lcpp==lcpp)'] else 'DIFF') + ' ORA=' + ('ok' if r['oracle_equal(py-hf==lcpp)'] else 'diff')
                print(f"  {r['scenario']:17} {r['kwset']:24} {r['variant']:28} hf={str(r.get('py-hf'))[:40]:40} pl={str(r.get('py-lcpp', '-'))[:30]:30} lc={str(r.get('lcpp', '-'))[:30]:30} {flag}")


if __name__ == '__main__':
    main()
