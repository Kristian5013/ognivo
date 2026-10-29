"""Jinja2 side of the float/unicode argument probe (compare with lcpp_render output)."""
import datetime
import json
import sys

import jinja2
from jinja2.ext import loopcontrols
from jinja2.sandbox import ImmutableSandboxedEnvironment

R = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
env = ImmutableSandboxedEnvironment(trim_blocks=True, lstrip_blocks=True, extensions=[loopcontrols])
env.filters['tojson'] = lambda x, ensure_ascii=False, indent=None, separators=None, sort_keys=False: json.dumps(x, ensure_ascii=ensure_ascii, indent=indent, separators=separators, sort_keys=sort_keys)
def rx(m):
    raise jinja2.exceptions.TemplateError(m)
env.globals['raise_exception'] = rx
env.globals['strftime_now'] = lambda f: datetime.datetime(2026, 10, 29).strftime(f)
req = json.load(open(R + '_lcpp/req_floats.json'))
out = env.from_string(open(R + sys.argv[1], encoding='utf-8').read()).render(messages=req['messages'], tools=req['tools'], add_generation_prompt=True, bos_token='<bos>', enable_thinking=True, reasoning_effort='medium', preserve_thinking=True)
print(out)
