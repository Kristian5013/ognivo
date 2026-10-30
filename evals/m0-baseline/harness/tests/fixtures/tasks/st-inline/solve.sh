#!/usr/bin/env bash
# Usage: solve.sh <workspace>: the reference solution.
set -euo pipefail
WS="${1:-${WS:-}}"
printf 'hello from mock\n' > "$WS/hello.txt"
