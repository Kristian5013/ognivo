"""Rebuild o200k_base.tiktoken (ranks 0..199997) from the gpt-oss tokenizer.json (byte-level BPE)."""
import base64
import hashlib
import json
import sys


def bytes_to_unicode():
    bs = list(range(ord('!'), ord('~') + 1)) + list(range(ord('¡'), ord('¬') + 1)) + list(range(ord('®'), ord('ÿ') + 1))
    cs = bs[:]
    n = 0
    for b in range(256):
        if b not in bs:
            bs.append(b)
            cs.append(256 + n)
            n += 1
    return {chr(c): b for b, c in zip(bs, cs)}


dec = bytes_to_unicode()
tj = json.load(open(sys.argv[1], encoding='utf-8'))
vocab = tj['model']['vocab']
by_id = {i: t for t, i in vocab.items()}
lines = []
for r in range(199998):
    t = by_id[r]
    lines.append(f'{base64.b64encode(bytes(dec[ch] for ch in t)).decode()} {r}\n')
text = ''.join(lines).encode()
open(sys.argv[2], 'wb').write(text)
print('sha256', hashlib.sha256(text).hexdigest(), 'expected 446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d')
