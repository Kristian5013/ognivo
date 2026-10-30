# shellcheck shell=bash
# Shared helpers of the self-test fixture tasks: sourced by setup.sh/verify.sh/solve.sh through
# ../_common, like the real tasks' _common/<group> helpers (so verify needs the tasks root mount).

# st_setup WS: a one-commit git workspace with an anonymous identity (no global git config).
st_setup() {
    local ws=$1
    [ -n "$ws" ] || { echo "usage: setup.sh <new dir>" >&2; exit 2; }
    if [ -e "$ws" ] && [ -n "$(ls -A "$ws" 2> /dev/null)" ]; then
        echo "setup: $ws exists and is not empty" >&2
        exit 2
    fi
    mkdir -p "$ws"
    printf '# fixture workspace\n\nA tiny repository for the harness self-test.\n' > "$ws/README.md"
    git -C "$ws" init -q -b main
    git -C "$ws" add -A
    git -C "$ws" -c user.name=st-fixture -c user.email=fixture@localhost \
        -c commit.gpgsign=false commit -q -m "fixture base"
    echo "workspace ready: $ws"
}

# st_expect_file FILE CONTENT: exit 0 when FILE holds exactly CONTENT (trailing newline ignored).
st_expect_file() {
    if [ "$(cat "$1" 2> /dev/null)" = "$2" ]; then
        echo "RESULT: solved"
        exit 0
    fi
    echo "RESULT: not solved ($1 does not hold '$2')"
    exit 1
}
