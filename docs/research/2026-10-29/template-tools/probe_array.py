"""Jinja2 behaviour of the gpt-oss template for an array param without items (llama.cpp #30781)."""
import datetime
import json

import jinja2
from jinja2.ext import loopcontrols
from jinja2.sandbox import ImmutableSandboxedEnvironment

R = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True, extensions=[loopcontrols])
env.filters['tojson'] = lambda x, **k: json.dumps(x, ensure_ascii=False)
env.globals['strftime_now'] = lambda f: datetime.datetime(2026, 10, 29).strftime(f)
req = json.load(open(R + '_lcpp/req_array_noitems.json'))
for f in ('gpt-oss-20b/canonical.jinja', 'gemma-4-26B-A4B-it/canonical.jinja'):
    out = env.from_string(open(R + f, encoding='utf-8').read()).render(messages=req['messages'], tools=req['tools'], add_generation_prompt=True, bos_token='<bos>')
    i = out.find('patterns')
    print(f, repr(out[i - 20:i + 160]))
