#!/usr/bin/env bash
# Usage: verify.sh <workspace>. The mock agent never writes goodbye.txt: this task fails.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$HERE/../_common/st-lib.sh" ] || { echo "verify: no ../_common (mount the tasks root)"; exit 2; }
# shellcheck source=../_common/st-lib.sh
. "$HERE/../_common/st-lib.sh"
WS="${1:-${WS:-$PWD}}"
[ -d "$WS" ] || { echo "verify: no workspace $WS"; exit 2; }
st_expect_file "$WS/goodbye.txt" "bye"
