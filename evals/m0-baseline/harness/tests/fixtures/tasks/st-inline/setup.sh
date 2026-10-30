#!/usr/bin/env bash
# Usage: setup.sh <new dir>   or   WS=<new dir> setup.sh
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../_common/st-lib.sh
. "$HERE/../_common/st-lib.sh"
st_setup "${1:-${WS:-}}"
