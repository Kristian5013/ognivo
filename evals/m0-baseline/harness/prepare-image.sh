#!/usr/bin/env bash
# Build the M0 baseline image with rootless Podman from the sources in harness/ plus pinned
# inputs. Nothing is installed on the host and harness/ never receives a binary.
#
#   prepare-image.sh [--dry-run] [STEP] [TARGET]
#     STEP    fetch    download/verify OpenCode v2.2.1 into <m0-baseline>/cache/opencode/2.2.1/
#             context  fetch + assemble the build context in <m0-baseline>/cache/build-context/
#             build    context + podman build + smoke test (default)
#     TARGET  full     OpenCode + the task toolchains: the night baseline (default)
#             opencode OpenCode + git + ripgrep + python3 only (smoke tests)
#             claude   full + Claude Code 2.1.311 (optional second baseline)
#
# Pinned inputs:
#   OpenCode 2.2.1   https://github.com/anomalyco/opencode/releases/download/v2.2.1/opencode-linux-x64.tar.gz
#                    tarball sha256 3179551d..., binary sha256 32e95ec1... (checked here and in the image)
#   base image       docker.io/library/ubuntu:26.10 by digest (BASE)
#   Rust 1.99.0, torch 2.13.0 (CPU), the Ubuntu 26.10 archive: fetched by the build itself
#   cargo crates     a copy of the host's ~/.cargo/{registry/index,registry/cache,git/db} (~200 MB)
#   Claude Code      target claude only: the owner's installed binary
#                    ~/.local/share/claude/versions/2.1.311 (what ~/.local/bin/claude points to),
#                    checked against the sha256 of the official 2.1.311 manifest. It is copied into
#                    the local build context and the local image only; never downloaded or uploaded.
#
# Env: ENGINE=podman (docker: smoke tests only), BASE=docker.io/library/ubuntu@sha256:262df65c...,
#      OC_URL (another source of the same tarball, e.g. file:///path; the sha256 must match),
#      CARGO_SRC=~/.cargo, CC_BIN (another copy of the Claude Code 2.1.311 binary, same sha256),
#      TORCH_VERSION=2.13.0 (none = leave torch out), UA=research-bot/1.0
set -euo pipefail
HERE=$(cd "$(dirname "$(realpath "$0")")" && pwd)
# shellcheck source=lib/common.sh
source "$HERE/lib/common.sh"

DRY_RUN=0
if [[ ${1:-} == --dry-run ]]; then
    DRY_RUN=1
    shift
fi
STEP=build TARGET=full
for a in "$@"; do
    case $a in
        fetch | context | build) STEP=$a ;;
        full | opencode | claude) TARGET=$a ;;
        -h | --help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
        *) echo "prepare-image: unknown argument $a (see --help)" >&2; exit 2 ;;
    esac
done

ENGINE=${ENGINE:-podman}
BASE=${BASE:-docker.io/library/ubuntu@sha256:262df65c0ec6bf8d9dcc87454f4330a05fae4b97825f45854af3b136666584c2}   # ubuntu:26.10, 2026-10-30
UA=${UA:-research-bot/1.0}
OC_VER=$M0_OC_VERSION
OC_URL=${OC_URL:-https://github.com/anomalyco/opencode/releases/download/v$OC_VER/opencode-linux-x64.tar.gz}
OC_TGZ_SHA=3179551ddf69877ca24a960b0175e94405cf3f52147f1eec705094374bef49f2   # opencode-linux-x64.tar.gz
OC_BIN_SHA=32e95ec126d416ab833094447cf590290e17dd028a68b122b94d3539795c0dae   # the binary inside it
CC_VER=2.1.311
CC_BIN_SHA=86dc1fcc69498159eaf15f446613e847010f9036bfbbdffd4c49350be8489ada   # manifest.json, linux-x64
CC_BIN=${CC_BIN:-$HOME/.local/share/claude/versions/$CC_VER}
CARGO_SRC=${CARGO_SRC:-$HOME/.cargo}
TORCH_VERSION=${TORCH_VERSION:-2.13.0}
OC_DIR=$M0_CACHE_DIR/opencode/$OC_VER
CTX=$M0_CACHE_DIR/build-context
TAG=$(m0_image "$TARGET")

sha_ok() { [[ -f $1 ]] && echo "$2  $1" | sha256sum -c --status - 2> /dev/null; }
say() { echo "prepare-image: $*" >&2; }

# --- 1. OpenCode ---------------------------------------------------------------------------
fetch() {
    local tgz=$OC_DIR/opencode-linux-x64.tar.gz tmp
    mkdir -p "$OC_DIR"
    if sha_ok "$tgz" "$OC_TGZ_SHA"; then
        say "cached: $tgz (sha256 ok)"
    else
        say "downloading $OC_URL"
        rm -f "$tgz.part"
        curl -fsSL --proto '=https,file' --proto-redir '=https' -A "$UA" -o "$tgz.part" "$OC_URL"
        sha_ok "$tgz.part" "$OC_TGZ_SHA" || { rm -f "$tgz.part"; say "sha256 mismatch for $OC_URL"; exit 1; }
        mv "$tgz.part" "$tgz"
    fi
    if sha_ok "$OC_DIR/bin/opencode" "$OC_BIN_SHA"; then
        say "cached: $OC_DIR/bin/opencode (sha256 ok)"
        return 0
    fi
    # untrusted archive: extracted into its own empty directory, only the checked binary is kept
    tmp=$(mktemp -d "$OC_DIR/extract.XXXXXX")
    tar -xzf "$tgz" -C "$tmp" --no-same-owner --no-same-permissions opencode
    sha_ok "$tmp/opencode" "$OC_BIN_SHA" || { rm -rf "$tmp"; say "the opencode binary in $tgz has the wrong sha256"; exit 1; }
    mkdir -p "$OC_DIR/bin"
    install -m 0755 "$tmp/opencode" "$OC_DIR/bin/opencode"
    rm -rf "$tmp"
    say "OpenCode $OC_VER: $OC_DIR/bin/opencode"
}

# --- 2. build context ----------------------------------------------------------------------
context() {
    rm -rf "$CTX"
    mkdir -p "$CTX/cargo-home"
    install -m 0644 "$HERE/container/Containerfile" "$CTX/Containerfile"
    install -m 0755 "$HERE/container/m0-entry.sh" "$HERE/container/m0-entry-claude.sh" "$CTX/"
    install -m 0644 "$HERE/config/opencode.json" "$CTX/opencode.json"
    ln "$OC_DIR/bin/opencode" "$CTX/opencode" 2> /dev/null || cp "$OC_DIR/bin/opencode" "$CTX/opencode"
    if [[ $TARGET != opencode ]]; then
        local d
        for d in registry/index registry/cache git/db; do
            if [[ -d $CARGO_SRC/$d ]]; then
                mkdir -p "$CTX/cargo-home/$(dirname "$d")"
                cp -a "$CARGO_SRC/$d" "$CTX/cargo-home/$d"
            else
                say "warning: $CARGO_SRC/$d missing: offline Rust builds may fail"
            fi
        done
        say "cargo crates: $(du -sh "$CTX/cargo-home" | cut -f1) copied from $CARGO_SRC"
    fi
    : > "$CTX/claude"   # COPY needs the file; the claude target gets the real one
    if [[ $TARGET == claude ]]; then
        sha_ok "$CC_BIN" "$CC_BIN_SHA" || {
            say "Claude Code $CC_VER not found at $CC_BIN with sha256 $CC_BIN_SHA."
            say "Install that version locally or set CC_BIN to a copy of it; nothing is downloaded."
            exit 1
        }
        install -m 0755 "$CC_BIN" "$CTX/claude"
        say "Claude Code $CC_VER copied from $CC_BIN (local build context only)"
    fi
}

if ((DRY_RUN)); then
    echo "target   $TARGET -> $TAG (engine $ENGINE)"
    echo "base     $BASE"
    if sha_ok "$OC_DIR/bin/opencode" "$OC_BIN_SHA"; then
        echo "opencode cached and verified: $OC_DIR/bin/opencode"
    else
        echo "opencode would be fetched: $OC_URL -> $OC_DIR (sha256 $OC_TGZ_SHA)"
    fi
    if [[ $TARGET != opencode ]]; then
        for d in registry/index registry/cache git/db; do
            if [[ -d $CARGO_SRC/$d ]]; then echo "crates   $CARGO_SRC/$d ($(du -sh "$CARGO_SRC/$d" | cut -f1))"; else echo "crates   MISSING $CARGO_SRC/$d"; fi
        done
        echo "torch    ${TORCH_VERSION} (CPU, pip at build time)"
    fi
    if [[ $TARGET == claude ]]; then
        if sha_ok "$CC_BIN" "$CC_BIN_SHA"; then echo "claude   $CC_BIN (sha256 ok)"; else echo "claude   MISSING or wrong sha256: $CC_BIN"; fi
    fi
    echo "context  $CTX"
    echo "build    nice -n 10 $ENGINE build --target $TARGET --build-arg BASE=$BASE" \
        "--build-arg TORCH_VERSION=$TORCH_VERSION -t $TAG -f $CTX/Containerfile $CTX"
    command -v "$ENGINE" > /dev/null || echo "WARNING  $ENGINE is not installed (sudo apt install podman passt)"
    exit 0
fi

fetch
[[ $STEP == fetch ]] && exit 0
context
[[ $STEP == context ]] && { say "build context ready: $CTX"; exit 0; }

# --- 3. build + smoke test -----------------------------------------------------------------
command -v "$ENGINE" > /dev/null || { say "$ENGINE is not installed: sudo apt install podman passt"; exit 1; }
nice -n 10 "$ENGINE" build --target "$TARGET" --build-arg "BASE=$BASE" --build-arg "TORCH_VERSION=$TORCH_VERSION" \
    -t "$TAG" -f "$CTX/Containerfile" "$CTX"
"$ENGINE" run --rm --network none --entrypoint opencode "$TAG" --version
if [[ $TARGET != opencode ]]; then
    "$ENGINE" run --rm --network none --entrypoint bash "$TAG" -c \
        'set -e; cargo --version; rustc --version; cmake --version | head -1; cc --version | head -1
         clang-format --version; valgrind --version; python3 --version
         python3 -c "import pytest; print(\"pytest\", pytest.__version__)"
         python3 -c "import torch; print(\"torch\", torch.__version__)" || echo "torch: not installed"'
fi
if [[ $TARGET == claude ]]; then
    "$ENGINE" run --rm --network none --entrypoint claude "$TAG" --version
fi
: > "$CTX/claude"   # do not keep a second copy of the owner's binary around
say "image: $TAG"
