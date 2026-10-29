#!/usr/bin/env bash
# Build the pinned llama.cpp used for the owner's own llama-server and for evals.
#
# Pin: the head of upstream PR #31244 ("server: preserve sampled token IDs across
# native SSE buffering"), i.e. master b11923 (de5122d87, 2026-10-27) plus one commit.
# Without it, streamed /completion responses drop the IDs of tokens that end in an
# incomplete UTF-8 sequence, which breaks verbatim token replay (docs/architecture.md §5.4).
# Switch back to plain upstream once the PR is merged.
#
# Usage: scripts/llama/build-pinned.sh            (idempotent; prints the server path)
# Env:   LLAMA_PINNED_ROOT  (default ~/.cache/llama-pinned)
#        LLAMA_REF_REPO     local llama.cpp clone to borrow git objects from (optional)
#        CUDA_ARCH          (default 120 = RTX 50xx / Blackwell)
set -euo pipefail

PIN_SHA=12cc59ba491447e9c7e58360e857bff11801f3b5
PIN_REF=pull/31244/head
UPSTREAM=https://github.com/ggml-org/llama.cpp

ROOT=${LLAMA_PINNED_ROOT:-$HOME/.cache/llama-pinned}
SRC=$ROOT/src
BUILD=$ROOT/build-${PIN_SHA:0:12}
CUDA_ARCH=${CUDA_ARCH:-120}

mkdir -p "$ROOT"
if [[ ! -d $SRC/.git ]]; then
    if [[ -n ${LLAMA_REF_REPO:-} ]]; then
        git clone --reference "$LLAMA_REF_REPO" --dissociate "$UPSTREAM" "$SRC"
    else
        git clone "$UPSTREAM" "$SRC"
    fi
fi

if ! git -C "$SRC" cat-file -e "$PIN_SHA^{commit}" 2>/dev/null; then
    git -C "$SRC" fetch origin "$PIN_REF"
fi
git -C "$SRC" checkout --quiet --detach "$PIN_SHA"
[[ $(git -C "$SRC" rev-parse HEAD) == "$PIN_SHA" ]] || { echo "pin mismatch" >&2; exit 1; }

# Same feature set as the owner's working build; tests and examples off for speed.
cmake -S "$SRC" -B "$BUILD" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=ON \
    -DGGML_NATIVE=ON \
    -DGGML_CUDA=ON \
    -DCMAKE_CUDA_ARCHITECTURES="$CUDA_ARCH" \
    -DGGML_CUDA_FA=ON \
    -DGGML_CUDA_GRAPHS=ON \
    -DLLAMA_BUILD_SERVER=ON \
    -DLLAMA_BUILD_TOOLS=ON \
    -DLLAMA_BUILD_TESTS=OFF \
    -DLLAMA_BUILD_EXAMPLES=OFF

cmake --build "$BUILD" --config Release -j "$(nproc)" --target llama-server llama-bench llama-batched-bench

"$BUILD/bin/llama-server" --version
echo "llama-server: $BUILD/bin/llama-server"
