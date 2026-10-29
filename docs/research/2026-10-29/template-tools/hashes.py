"""Print sha256, git-blob sha1 and size for files; for tokenizer_config.json also inspect chat_template keys."""
import hashlib
import json
import sys


def blob_sha1(b):
    return hashlib.sha1(b'blob %d\0' % len(b) + b).hexdigest()


for p in sys.argv[1:]:
    b = open(p, 'rb').read()
    print(f'{p}\n  size={len(b)} sha256={hashlib.sha256(b).hexdigest()} gitblob={blob_sha1(b)}')
    if p.endswith('tokenizer_config.json'):
        d = json.loads(b)
        ct = d.get('chat_template')
        print('  keys:', sorted(d.keys()))
        if isinstance(ct, str):
            print('  chat_template(str) len', len(ct), 'sha256', hashlib.sha256(ct.encode()).hexdigest())
        elif isinstance(ct, list):
            for e in ct:
                print('  chat_template[name=%s] len %d sha256 %s' % (e.get('name'), len(e.get('template', '')), hashlib.sha256(e.get('template', '').encode()).hexdigest()))
        else:
            print('  chat_template:', type(ct))
        for k in ('bos_token', 'eos_token', 'pad_token', 'unk_token', 'add_bos_token', 'add_eos_token', 'model_max_length', 'tokenizer_class', 'response_template', 'extra_special_tokens'):
            if k in d:
                v = d[k]
                s = json.dumps(v, ensure_ascii=False)
                print(f'  {k} = {s[:400]}')
