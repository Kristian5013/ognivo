#!/usr/bin/env bash
# Host-side smoke test of `opencode run` against mock_llama.py (no container, no GPU): the pinned
# OpenCode from <m0-baseline>/cache with an isolated HOME/XDG tree (OpenCode writes logs even for
# --version), the harness config as its only config, and strace recording every connect().
# Usage: oc-host-test.sh TESTDIR PORT SCENARIO|- [opencode run args...]   (prompt on stdin)
#   e.g. echo "Create hello.txt" | tests/oc-host-test.sh /tmp/oc1 18080 write --format json --title t1 --model llama/gpt-oss-20b
# SCENARIO "-" means: do not start a mock (use an already running one or none at all).
# TESTDIR is deleted and re-created. Env: OC_TIMEOUT=120, MOCK_FAIL=ctx|500|slow, EXTRA_ENV.
set -uo pipefail
B=$(cd "$(dirname "$(realpath "$0")")/.." && pwd)
OC=${OC_BIN:-$B/../cache/opencode/2.2.1/bin/opencode}
[[ -x $OC ]] || { echo "no pinned OpenCode at $OC (run prepare-image.sh fetch)" >&2; exit 2; }
command -v strace > /dev/null || { echo "strace is needed" >&2; exit 2; }
T=$1 PORT=$2 SCEN=$3; shift 3
rm -rf "$T"; mkdir -p "$T/home/.config/opencode" "$T/ws"
cp "$B/config/opencode.json" "$T/home/.config/opencode/opencode.json"
git -C "$T/ws" init -q
printf 'hello world\n' > "$T/ws/README.md"
git -C "$T/ws" add -A && git -C "$T/ws" -c user.name=t -c user.email=t@example.invalid commit -qm init
MOCK=
if [[ $SCEN != - ]]; then
  python3 -I "$B/scripts/mock_llama.py" --port "$PORT" --log "$T/requests.jsonl" --scenario "$SCEN" ${MOCK_FAIL:+--fail $MOCK_FAIL} > "$T/mock.log" 2>&1 &
  MOCK=$!
  for _ in $(seq 50); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null && break; sleep 0.1; done
fi
H=$T/home
cd "$T/ws"
start=$(date +%s.%N)
env -i PATH=/usr/local/bin:/usr/bin:/bin HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share" \
  XDG_CACHE_HOME="$H/.cache" XDG_STATE_HOME="$H/.local/state" TERM=dumb \
  LLAMA_BASE_URL="http://127.0.0.1:$PORT/v1" OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_MODELS_FETCH=1 \
  OPENCODE_DISABLE_FREE_MODELS=1 ${EXTRA_ENV:-} \
  strace -f -qq -e trace=connect -o "$T/connect.trace" \
  timeout -k 5 "${OC_TIMEOUT:-120}" "$OC" run --standalone "$@" > "$T/events.jsonl" 2> "$T/stderr.log"
rc=$?
end=$(date +%s.%N)
echo "rc=$rc elapsed=$(python3 -I -c "print(round($end - $start, 2))")s events=$(wc -l < "$T/events.jsonl") requests=$( [[ -f $T/requests.jsonl ]] && grep -c '"POST"' "$T/requests.jsonl" || echo 0)"
echo "connects:"; grep -oE 'sin6?_port=htons\([0-9]+\), (sin_addr=inet_addr\("[^"]+"\)|inet_pton\(AF_INET6, "[^"]+")' "$T/connect.trace" | sort | uniq -c
[[ -n $MOCK ]] && kill "$MOCK" 2>/dev/null
exit 0
