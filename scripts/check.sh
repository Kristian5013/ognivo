#!/usr/bin/env bash
# Local CI and T0 (docs/architecture.md §11): formatting, lints and tests for the whole
# workspace against Cargo.lock as it is, the VS Code extension's types and tests where
# its dependencies are installed (npm ci), then the recorded chat completions read again
# against their snapshot, where the recordings are (they stay on the machine that made
# them).
# OGNIVO_RUNS points elsewhere than the M0 harness's runs directory.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

if [[ -d node_modules ]]; then
    npm run -s -w apps/vscode check
    npm run -s -w apps/vscode test
else
    echo "extension: no node_modules (npm ci), skipped"
fi

runs="${OGNIVO_RUNS:-$HOME/.local/share/ognivo/m0-baseline/runs}"
if [[ -d "$runs" ]]; then
    cargo run --release --locked -q -p ognivo-evals --bin replay -- "$runs" --check evals/t0/replay.json
else
    echo "replay: no recordings at $runs, skipped"
fi
