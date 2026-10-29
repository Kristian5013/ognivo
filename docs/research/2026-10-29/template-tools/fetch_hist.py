"""Fetch chat templates at specific HF revisions; writes a file only on HTTP 200.
usage: fetch_hist.py REPO OUTDIR REV... (files: chat_template.jinja, chat_template.json, tokenizer_config.json)"""
import hashlib
import os
import sys
import urllib.error
import urllib.request

repo, outdir, revs = sys.argv[1], sys.argv[2], sys.argv[3:]
os.makedirs(outdir, exist_ok=True)
for rev in revs:
    for f in ('chat_template.jinja', 'chat_template.json', 'tokenizer_config.json'):
        url = f'https://huggingface.co/{repo}/resolve/{rev}/{f}'
        req = urllib.request.Request(url, headers={'User-Agent': 'research-bot/1.0'})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                data = r.read()
        except urllib.error.HTTPError as e:
            print(f'{e.code} {rev[:12]} {f}')
            continue
        out = os.path.join(outdir, f'{rev[:12]}.{f}')
        with open(out, 'wb') as fh:
            fh.write(data)
        print(f'200 {rev[:12]} {f} sha256={hashlib.sha256(data).hexdigest()[:16]} size={len(data)}')
