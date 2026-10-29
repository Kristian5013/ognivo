"""Compact view of <model>/rendered/_summary.json. usage: summarize.py MODEL [filter]"""
import json
import sys

model = sys.argv[1]
flt = sys.argv[2] if len(sys.argv) > 2 else ''
rows = json.load(open(f'/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/{model}/rendered/_summary.json'))
for r in rows:
    if 'caps' in r:
        c = r['caps']
        on = sorted(k.replace('supports_', '') for k, v in c.items() if v)
        print(f"caps {r['variant']:28} fmt={r.get('detected_format')} on={on} {r.get('caps_error') or ''}")
for r in rows:
    if 'scenario' not in r or flt not in f"{r['scenario']}.{r['kwset']}.{r['variant']}":
        continue
    def s(k):
        v = str(r.get(k, '-'))
        return v if not v.startswith('ERROR') else 'ERR'
    flag = ''
    if 'lcpp' in r:
        flag = ('ENG=ok ' if r['engine_equal(py-lcpp==lcpp)'] else 'ENG=DIFF ') + ('ORA=ok' if r['oracle_equal(py-hf==lcpp)'] else 'ORA=diff')
    print(f"{r['scenario']:16} {r['kwset']:24} {r['variant']:28} hf={s('py-hf'):12} pl={s('py-lcpp'):12} lc={s('lcpp'):12} {flag}")
