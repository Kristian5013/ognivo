"""Convert the length-prefixed o200k_base vocab shipped with VS Code Copilot into the standard
tiktoken text format (base64 + rank) and check it against tiktoken's published sha256."""
import base64
import hashlib
import sys

src, dst = sys.argv[1], sys.argv[2]
b = open(src, 'rb').read()
i, toks = 0, []
while i < len(b):
    n = b[i]
    toks.append(b[i + 1:i + 1 + n])
    i += 1 + n
text = ''.join(f'{base64.b64encode(t).decode()} {r}\n' for r, t in enumerate(toks)).encode()
open(dst, 'wb').write(text)
print('tokens', len(toks), 'sha256', hashlib.sha256(text).hexdigest())
print('expected 446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d (tiktoken o200k_base)')
