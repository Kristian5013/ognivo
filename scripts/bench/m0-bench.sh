#!/usr/bin/env bash
# M0 measurements (docs/architecture.md §13): prefill/decode speed of the three
# reference models at several context depths, and aggregate decode throughput with
# 1/2/4 parallel sequences (subagents). Needs the GPU to itself.
#
# Usage: scripts/bench/m0-bench.sh [OUT_DIR]
# Env:   LLAMA_BIN  dir with llama-bench and llama-batched-bench (default: pinned build)
#        MODELS_ROOT (default: the owner's data NVMe)
#        ONLY       space-separated subset of model keys (gpt-oss gemma4 qwen38)
set -euo pipefail

LLAMA_BIN=${LLAMA_BIN:-$HOME/.cache/llama-pinned/build-12cc59ba4914/bin}
MODELS_ROOT=${MODELS_ROOT:-/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e}
OUT=${1:-bench/m0/$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$OUT"

declare -A MODEL=(
    [gpt-oss]="$MODELS_ROOT/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf"
    [gemma4]="$MODELS_ROOT/gemma-4-26B-A4B-it-UD-Q4_K_M.gguf"
    [qwen38]="$MODELS_ROOT/Qwen3.8-27B-Q4_0.gguf"
)
KEYS=${ONLY:-gpt-oss gemma4 qwen38}
DEPTHS=0,16384,32768,65536,98304

# Refuse to run next to another GPU tenant: numbers would be meaningless.
busy=$(nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader)
if [[ -n $busy ]]; then
    echo "GPU is in use:" >&2
    echo "$busy" >&2
    exit 1
fi

# Capture whole outputs first: `cmd | head` would SIGPIPE the producer under pipefail.
version=$("$LLAMA_BIN/llama-bench" --version 2>&1 || true)
profile=$(asusctl profile get 2>/dev/null || true)
{
    echo "date_utc=$(date -u +%FT%TZ)"
    echo "llama_bench=$LLAMA_BIN/llama-bench"
    grep -E '^(version|built with)' <<< "$version" | sed 's/^/llama: /'
    nvidia-smi --query-gpu=name,driver_version,memory.total,temperature.gpu,power.limit,clocks.max.sm --format=csv,noheader | sed 's/^/gpu: /'
    nvcc --version | tail -1 | sed 's/^/cuda: /'
    sed -n '1s/^/power_profile: /p' <<< "$profile"
    uname -r | sed 's/^/kernel: /'
} > "$OUT/conditions.txt"

for key in $KEYS; do
    m=${MODEL[$key]}
    echo "== $key: depth sweep"
    "$LLAMA_BIN/llama-bench" -m "$m" -ngl 99 -fa on -b 2048 -ub 2048 \
        -p 2048 -n 128 -d "$DEPTHS" -r 3 -o jsonl > "$OUT/$key-depth.jsonl"

    echo "== $key: parallel sequences"
    "$LLAMA_BIN/llama-batched-bench" -m "$m" -ngl 99 -fa on -c 49152 -b 2048 -ub 512 \
        -npp 8192 -ntg 256 -npl 1,2,4 --output-format jsonl > "$OUT/$key-parallel.jsonl" 2> "$OUT/$key-parallel.log"
done

nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader | sed 's/^/gpu_temp_after: /' >> "$OUT/conditions.txt"
echo "results: $OUT"
