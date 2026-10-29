"""List the free (undeclared) variables each template reads = its full kwarg surface."""
import json
import sys

import jinja2
from jinja2 import meta
from jinja2.ext import loopcontrols

ROOT = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
env = jinja2.Environment(extensions=[loopcontrols])
files = {
    'gpt-oss canonical': 'gpt-oss-20b/canonical.jinja',
    'gpt-oss gguf(unsloth)': 'gpt-oss-20b/gguf-embedded.jinja',
    'gemma canonical(07-15)': 'gemma-4-26B-A4B-it/canonical.jinja',
    'gemma gguf(unsloth)': 'gemma-4-26B-A4B-it/gguf-embedded.jinja',
    'qwen canonical': 'Qwen3.8-27B/canonical.jinja',
    'qwen gguf(unsloth)': 'Qwen3.8-27B/gguf-embedded.jinja',
}
out = {}
for k, f in files.items():
    src = open(ROOT + f, encoding='utf-8').read()
    ast = env.parse(src)
    out[k] = sorted(meta.find_undeclared_variables(ast))
    print(f'{k:24} {out[k]}')
json.dump(out, open(ROOT + '_api/kwargs_inventory.json', 'w'), indent=1)
