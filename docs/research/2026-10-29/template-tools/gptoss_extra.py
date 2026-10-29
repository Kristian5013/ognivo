"""Extra gpt-oss probes: builtin_tools=[] and developer_instructions on canonical vs Unsloth."""
import datetime
import json

import jinja2
from jinja2.sandbox import ImmutableSandboxedEnvironment
from jinja2.ext import loopcontrols

R = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/'
env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True, extensions=[loopcontrols])
env.filters['tojson'] = lambda x, **k: json.dumps(x, ensure_ascii=False)
def rx(m):
    raise jinja2.exceptions.TemplateError(m)
env.globals['raise_exception'] = rx
env.globals['strftime_now'] = lambda f: datetime.datetime(2026, 10, 29).strftime(f)
msgs = [{'role': 'system', 'content': 'SYSTEM-FROM-MESSAGES'}, {'role': 'user', 'content': 'Hi'}]
out = {}
for v in ('canonical', 'gguf'):
    t = env.from_string(open(R + v + '.jinja' if v == 'canonical' else R + 'gguf-embedded.jinja', encoding='utf-8').read())
    a = t.render(messages=msgs, add_generation_prompt=True, builtin_tools=[])
    b = t.render(messages=msgs, add_generation_prompt=True, developer_instructions='FROM-KWARG')
    print(f'--- {v} builtin_tools=[] :', repr(a[a.find('Reasoning'):a.find('<|end|>')]))
    print(f'--- {v} developer_instructions + system msg :', repr(b[b.find('<|start|>developer'):]))
