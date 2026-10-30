#!/usr/bin/env bash
# Usage: solve.sh <workspace>: the reference solution.
set -euo pipefail
WS="${1:-${WS:-}}"
printf 'bye\n' > "$WS/goodbye.txt"
