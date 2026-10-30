#!/usr/bin/env bash
# Emulates what rootless Podman does for `--network pasta:--splice-only,-T,PORT` without Podman:
# a user namespace (rootlesskit, net=host so the parent netns is the real host one), a fresh
# network namespace, and pasta started with Podman's default options plus ours.
# Usage: test-pasta-splice.sh [PASTA_BIN] PORT   (default /usr/bin/pasta; a server, e.g.
#   scripts/mock_llama.py, must listen on 127.0.0.1:PORT on the host). Needs rootlesskit, nsenter, ip.
# Expected: OK for 127.0.0.1:PORT; FAIL for port 22, 169.254.1.2, 1.1.1.1, IPv6 and DNS.
set -uo pipefail
if [[ $# -eq 1 ]]; then set -- /usr/bin/pasta "$1"; fi
[[ $# -eq 2 ]] || { sed -n 2,9p "$0" >&2; exit 2; }
PASTA=$1 PORT=$2
exec rootlesskit --net=host --state-dir="$(mktemp -d "${TMPDIR:-/tmp}/rootlesskit.XXXXXX")" -- bash -c '
PASTA=$1 PORT=$2
unshare --net --fork sleep 30 &
holder=$!
sleep 0.5
nspid=$(pgrep -P "$holder" sleep || echo "$holder")
NS=/proc/$nspid/ns/net
# Podman 5.x defaults (podman-run(1)): --config-net --dns-forward 169.254.1.1 -t none -u none -U none
# --no-map-gw --map-guest-addr 169.254.1.2 --quiet --netns <path>; -T none is dropped when -T is given.
"$PASTA" --config-net --dns-forward 169.254.1.1 -t none -u none -U none --no-map-gw --quiet \
    --map-guest-addr 169.254.1.2 --netns "$NS" --netns-only --splice-only -T "$PORT"
echo "pasta rc=$?"
sleep 0.5
inns() { nsenter --net="$NS" "$@"; }
echo "--- interfaces in the container netns:"; inns ip -brief addr
echo "--- routes:"; inns ip route; inns ip -6 route
probe() { inns python3 -I -c "
import socket,sys
h,p=sys.argv[1],int(sys.argv[2])
s=socket.socket(socket.AF_INET6 if \":\" in h else socket.AF_INET); s.settimeout(3)
try:
    s.connect((h,p)); s.sendall(b\"GET /health HTTP/1.0\r\n\r\n\"); print(\"OK  \", h, p, s.recv(200).split(b\"\r\n\")[0].decode())
except Exception as e:
    print(\"FAIL\", h, p, type(e).__name__, e)
" "$1" "$2"; }
probe 127.0.0.1 "$PORT"          # expected OK: spliced to the host loopback port
probe 127.0.0.1 22               # expected FAIL: only PORT is forwarded
probe 169.254.1.2 "$PORT"        # expected FAIL: no tap, so no host.containers.internal mapping
probe 1.1.1.1 80                 # expected FAIL: no route to the outside
probe 2606:4700:4700::1111 80    # expected FAIL
inns python3 -I -c "
import socket
try: print(\"DNS \", socket.getaddrinfo(\"github.com\", 443)[0][4])
except Exception as e: print(\"FAIL DNS\", type(e).__name__, e)"
kill $holder 2>/dev/null
' bash "$PASTA" "$PORT"
