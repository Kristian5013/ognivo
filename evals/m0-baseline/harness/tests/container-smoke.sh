#!/usr/bin/env bash
# Smoke test of the real container path, for the owner before the first night: needs rootless
# Podman, pasta and the image (prepare-image.sh); no GPU, no model, no network.
#
#   tests/container-smoke.sh [--image IMAGE] [--harness opencode|ognivo] [--port N] [--reference] [--keep]
#
# 1 network   a container with the runs' network flags reaches the host's 127.0.0.1:PORT (a
#             mock llama-server) and nothing else: not host port 22, not another listening host
#             port (PORT+2, and the host's 8080), not 169.254.1.2, not 1.1.1.1, not
#             IPv6, no DNS
# 2 suite     run-suite.sh --server on the fixture tasks with ENGINE=podman and the real image;
#             the "llama-server" is tests/fake-llama-server.sh (scripts/mock_llama.py) and the
#             GPU check tests/fake-nvidia-smi.sh: st-hello and st-inline solved, st-unsolvable
#             failed, verify in its own --network none container. --harness ognivo runs ognivo
#             (image target ognivo) instead of OpenCode: `--approve auto` works only because
#             this is a real container
# 3 reference (--reference) run-suite.sh --agent reference and --agent none over the real
#             selected tasks: each setup.sh on the host, solve.sh (or nothing), then verify.sh in
#             the image. Every reference must pass and every base must fail; this checks the
#             image's toolchains offline. Cold Rust builds make it take 10-20 min.
# Everything goes to a new directory under ${TMPDIR:-/tmp} (removed unless --keep).
set -uo pipefail
T=$(cd "$(dirname "$(realpath "$0")")" && pwd)
H=$(dirname "$T")
# shellcheck source=../lib/common.sh
source "$H/lib/common.sh"
IMAGE= PORT=18090 REFERENCE=0 KEEP=0 HARNESS=opencode
while (($#)); do
    case $1 in
        --image) IMAGE=$2; shift ;;
        --harness) HARNESS=$2; shift ;;
        --port) PORT=$2; shift ;;
        --reference) REFERENCE=1 ;;
        --keep) KEEP=1 ;;
        -h | --help) sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
        *) echo "container-smoke: unknown argument $1" >&2; exit 2 ;;
    esac
    shift
done
case $HARNESS in
    opencode) IMAGE=${IMAGE:-$(m0_image full)} LABEL=gpt-oss-20b ;;
    ognivo) IMAGE=${IMAGE:-$(m0_image ognivo)} LABEL=gpt-oss-20b.ognivo ;;
    *) echo "container-smoke: --harness opencode|ognivo" >&2; exit 2 ;;
esac
FAILS=0
ok() { printf '  ok    %s\n' "$*"; }
bad() { FAILS=$((FAILS + 1)); printf '  FAIL  %s\n' "$*"; }
command -v podman > /dev/null || { echo "podman is not installed: sudo apt install podman passt" >&2; exit 2; }
podman image exists "$IMAGE" || { echo "no image $IMAGE: run $H/prepare-image.sh" >&2; exit 2; }
SB=$(mktemp -d "${TMPDIR:-/tmp}/m0-smoke.XXXXXX")
cleanup() {
    [[ -n ${MOCK:-} ]] && kill "$MOCK" 2> /dev/null
    [[ -n ${OTHER:-} ]] && kill "$OTHER" 2> /dev/null
    if ((KEEP)); then echo "kept: $SB"; else rm -rf "$SB" 2> /dev/null || podman unshare rm -rf "$SB"; fi
}
trap cleanup EXIT

echo "== 1 network isolation ($IMAGE)"
python3 -I "$H/scripts/mock_llama.py" --host 127.0.0.1 --port "$PORT" --log "$SB/probe-requests.jsonl" \
    > "$SB/mock.log" 2>&1 &
MOCK=$!
m0_wait_port 127.0.0.1 "$PORT" 10 || { echo "mock did not start" >&2; exit 1; }
# a second host listener the container must NOT reach (only -T PORT is forwarded)
python3 -I -c 'import socket,sys,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", int(sys.argv[1]))); s.listen(); time.sleep(600)' "$((PORT + 2))" &
OTHER=$!
m0_wait_port 127.0.0.1 "$((PORT + 2))" 10 || { echo "second listener did not start" >&2; exit 1; }
probe=$(podman run --rm --pull never --network "pasta:--splice-only,-T,$PORT" --cap-drop all \
    --security-opt no-new-privileges --entrypoint python3 "$IMAGE" -I -c '
import json, socket, sys, urllib.request
port = int(sys.argv[1])
def tcp(h, p):
    s = socket.socket(socket.AF_INET6 if ":" in h else socket.AF_INET)
    s.settimeout(3)
    try:
        s.connect((h, p))
        return True
    except OSError:
        return False
    finally:
        s.close()
r = {"llama_health": False, "host_ssh": tcp("127.0.0.1", 22), "host_other_port": tcp("127.0.0.1", port + 2),
     "host_8080": tcp("127.0.0.1", 8080), "gateway": tcp("169.254.1.2", port),
     "internet": tcp("1.1.1.1", 80), "ipv6": tcp("2606:4700:4700::1111", 80)}
try:
    r["llama_health"] = urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=5).status == 200
except Exception:
    pass
try:
    socket.getaddrinfo("github.com", 443)
    r["dns"] = True
except OSError:
    r["dns"] = False
print(json.dumps(r))' "$PORT" 2>&1)
echo "  probe: $probe"
python3 -I -c 'import json,sys
r = json.loads(sys.argv[1].splitlines()[-1])
sys.exit(0 if r["llama_health"] and not any(v for k, v in r.items() if k != "llama_health") else 1)' "$probe" \
    && ok "only 127.0.0.1:$PORT (the host llama-server) is reachable" || bad "network isolation (see the probe line)"
kill "$MOCK" "$OTHER" 2> /dev/null
wait "$MOCK" "$OTHER" 2> /dev/null
MOCK= OTHER=

echo "== 2 suite on the fixture tasks with Podman"
mkdir -p "$SB/models"
python3 -I - "$M0_MODELS_JSON" "$SB/models" "$SB/models.json" << 'PY'
import json, os, sys
src, mdir, dst = sys.argv[1:]
c = json.load(open(src))
for m in c["models"].values():
    p = os.path.join(mdir, os.path.basename(m["gguf"]))
    open(p, "w").close()
    m["gguf"] = p
json.dump(c, open(dst, "w"), indent=1, ensure_ascii=False)
PY
export M0_RUNS_ROOT=$SB/runs M0_WORK_ROOT=$SB/work
M0_MODELS_JSON=$SB/models.json LLAMA_SERVER_BIN=$T/fake-llama-server.sh NVIDIA_SMI=$T/fake-nvidia-smi.sh \
    ENGINE=podman IMAGE=$IMAGE TAP_PORT=$((PORT + 1)) \
    "$H/run-suite.sh" --server --port "$PORT" --stamp smoke --tasks-root "$T/fixtures/tasks" --timeout 180 \
    --harness "$HARNESS" gpt-oss-20b > "$SB/suite.log" 2>&1
rc=$?
st() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["status"])' "$M0_RUNS_ROOT/smoke/$LABEL/$1/result.json" 2> /dev/null; }
((rc == 0)) && ok "run-suite.sh exit 0" || bad "run-suite.sh exit $rc (log: $SB/suite.log)"
[[ $(st st-hello) == solved && $(st st-inline) == solved && $(st st-unsolvable) == failed ]] \
    && ok "fixtures: solved, solved, failed" || bad "fixtures: $(st st-hello) $(st st-inline) $(st st-unsolvable)"
grep -q '^network=pasta:--splice-only' "$M0_RUNS_ROOT/smoke/$LABEL/st-hello/logs/run.env" 2> /dev/null \
    && ok "agent container used pasta --splice-only" || bad "agent network (run.env)"
if [[ $HARNESS == ognivo ]]; then
    python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1])); og = r["summary"]["ognivo"]
assert og["exit"] == 0 and og["stats"]["refused"] == 0 and r["summary"]["tools"] == {"write": 1, "shell": 1}, og
assert json.load(open(sys.argv[2]))["approve"] == "auto"' \
        "$M0_RUNS_ROOT/smoke/$LABEL/st-hello/result.json" "$M0_RUNS_ROOT/smoke/$LABEL/st-hello/trajectory/meta.json" 2> /dev/null \
        && ok "ognivo: --approve auto inside the container, write and shell both ran" || bad "ognivo summary (result.json)"
else
    [[ -s $M0_RUNS_ROOT/smoke/$LABEL/st-hello/trajectory/session.json ]] && ok "session exported" || bad "no session.json"
fi
[[ -z $(podman ps -a --filter name=m0- --format '{{.Names}}') ]] && ok "no m0-* container left" \
    || bad "containers left: $(podman ps -a --filter name=m0- --format '{{.Names}}' | tr '\n' ' ')"

if ((REFERENCE)); then
    echo "== 3 reference / unsolved runs of the real tasks in the image (slow)"
    ENGINE=podman IMAGE=$IMAGE "$H/run-suite.sh" --agent reference --stamp ref > "$SB/ref.log" 2>&1
    ENGINE=podman IMAGE=$IMAGE "$H/run-suite.sh" --agent none --stamp ref > "$SB/none.log" 2>&1
    python3 -I "$H/scripts/summarize_run.py" --table "$M0_RUNS_ROOT/ref"
    python3 -I - "$M0_RUNS_ROOT/ref" << 'PY' && ok "every reference passes, every base fails" || bad "reference/none table above"
import glob, json, os, sys
d = sys.argv[1]
ref = {os.path.basename(os.path.dirname(p)): json.load(open(p))["status"] for p in glob.glob(f"{d}/reference/*/result.json")}
non = {os.path.basename(os.path.dirname(p)): json.load(open(p))["status"] for p in glob.glob(f"{d}/unsolved/*/result.json")}
bad = [t for t, s in ref.items() if s != "solved"] + [t for t, s in non.items() if s != "failed"]
print("  problems:", bad) if bad else None
sys.exit(1 if bad or not ref else 0)
PY
fi
echo "== $FAILS failure(s)"
exit $((FAILS > 0))
