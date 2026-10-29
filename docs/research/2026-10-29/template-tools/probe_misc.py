"""Small probes: Qwen role/system/user-query raises; Gemma Apr-10 reserved property names; gpt-oss items:{}."""
import datetime
import json

import jinja2
from jinja2.ext import loopcontrols
from jinja2.sandbox import ImmutableSandboxedEnvironment

R = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
L = '/home/kristian/Рабочий стол/llama.cpp/models/templates/'
env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True, extensions=[loopcontrols])
env.filters['tojson'] = lambda x, **k: json.dumps(x, ensure_ascii=False)
def rx(m):
    raise jinja2.exceptions.TemplateError(m)
env.globals['raise_exception'] = rx
env.globals['strftime_now'] = lambda f: datetime.datetime(2026, 10, 29).strftime(f)


def run(path, **ctx):
    try:
        return env.from_string(open(path, encoding='utf-8').read()).render(**ctx)
    except Exception as e:  # noqa: BLE001
        return f'RAISE {type(e).__name__}: {e}'


q = R + 'Qwen3.8-27B/canonical.jinja'
qg = R + 'Qwen3.8-27B/gguf-embedded.jinja'
for name, msgs in {
    'developer role': [{'role': 'developer', 'content': 'D'}, {'role': 'user', 'content': 'U'}],
    'two system msgs': [{'role': 'system', 'content': 'S1'}, {'role': 'system', 'content': 'S2'}, {'role': 'user', 'content': 'U'}],
    'mid-conversation system': [{'role': 'user', 'content': 'U'}, {'role': 'assistant', 'content': 'A'}, {'role': 'system', 'content': 'S'}, {'role': 'user', 'content': 'U2'}],
    'no user query': [{'role': 'system', 'content': 'S'}, {'role': 'assistant', 'content': 'A'}],
}.items():
    for lab, p in (('canonical', q), ('unsloth', qg)):
        out = run(p, messages=msgs, add_generation_prompt=True, enable_thinking=True, reasoning_effort='medium')
        print(f'qwen {lab:9} {name:24} ->', out[:110].replace('\n', '\\n'))

tool = [{'type': 'function', 'function': {'name': 'create_task', 'description': 'Create', 'parameters': {
    'type': 'object', 'properties': {'description': {'type': 'string', 'description': 'Task text'}, 'type': {'type': 'string', 'enum': ['bug', 'feat']}, 'title': {'type': 'string'}},
    'required': ['title', 'description']}}}]
for lab, p in (('apr-10 (llama.cpp bundled)', L + 'google-gemma-4-31B-it.jinja'), ('apr-28 (nvfp4 disk)', R + 'gemma-4-26B-A4B-it/upstream/history/4c55b528bdc4.chat_template.jinja'), ('jul-15 canonical', R + 'gemma-4-26B-A4B-it/canonical.jinja')):
    out = run(p, messages=[{'role': 'user', 'content': 'x'}], tools=tool, add_generation_prompt=True, bos_token='<bos>')
    i = out.find('properties:{')
    print(f'gemma {lab:28}', out[i:i + 170])
