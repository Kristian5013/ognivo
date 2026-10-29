#!/usr/bin/env bash
# Run scripts/probe/server_caps.py for each (server build × model) pair, one server at a
# time on a private port. Needs the GPU to itself (M0, docs/architecture.md §13).
#
# Usage: scripts/probe/run-matrix.sh [OUT_DIR]
set -euo pipefail

OUT=${1:-bench/m0/probes}
PINNED=$HOME/.cache/llama-pinned/build-12cc59ba4914/bin/llama-server
OLD="/home/kristian/Рабочий стол/llama.cpp/build/bin/llama-server"      # 5e03a1ff4, no PR #31244
IK="/home/kristian/Рабочий стол/ik_llama.cpp/build/bin/llama-server"
M=/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e
declare -A MODEL=(
    [gpt-oss]="$M/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf"
    [gemma4]="$M/gemma-4-26B-A4B-it-UD-Q4_K_M.gguf"
    [qwen38]="$M/Qwen3.8-27B-Q4_0.gguf"
)
PORT=18090
RUNS=${RUNS:-}   # optional space-separated subset of run names
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$OUT"

run() {  # run NAME SERVER_BIN MODEL_KEY [extra server flags...]
    local name=$1 bin=$2 key=$3
    shift 3
    if [[ -n $RUNS && " $RUNS " != *" $name "* ]]; then return 0; fi
    PORT=$((PORT + 1))   # a fresh port per run: the previous socket may still be in TIME_WAIT
    echo "== $name"
    "$bin" -m "${MODEL[$key]}" --host 127.0.0.1 --port "$PORT" -ngl 99 -fa on -c 16384 -np 1 "$@" \
        > "$OUT/$name.server.log" 2>&1 &
    local pid=$! up=0
    for _ in $(seq 1 240); do
        if curl -sf "http://127.0.0.1:$PORT/health" > /dev/null; then up=1; break; fi
        kill -0 "$pid" 2> /dev/null || break
        sleep 1
    done
    if [[ $up == 1 ]]; then
        python3 -I "$HERE/server_caps.py" --url "http://127.0.0.1:$PORT" --out "$OUT/$name.json" > /dev/null \
            || echo "probe failed: $name" >&2
    else
        echo "server did not come up: $name (see $OUT/$name.server.log)" >&2
    fi
    kill "$pid" 2> /dev/null || true
    wait "$pid" 2> /dev/null || true
}

run pinned-gpt-oss "$PINNED" gpt-oss
run old-gpt-oss    "$OLD"    gpt-oss
run pinned-gemma4  "$PINNED" gemma4
run pinned-qwen38  "$PINNED" qwen38
run ik-gpt-oss     "$IK"     gpt-oss --jinja
run ik-gemma4      "$IK"     gemma4  --jinja
echo "reports: $OUT"
