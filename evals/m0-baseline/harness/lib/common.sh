# shellcheck shell=bash
# Shared defaults and helpers of the M0 baseline harness. Sourced by the scripts, never run.
#
# Every location derives from where the harness lives (<m0-baseline>/harness), so the tree
# can move as a whole; each one can be overridden from the environment:
#   M0_ROOT        <m0-baseline>             (default: the parent of this harness directory)
#   M0_TASKS_ROOT  <m0-baseline>/tasks       task directories (task.json, setup.sh, verify.sh)
#   M0_RUNS_ROOT   <m0-baseline>/runs        results: <UTC stamp>/<model>/<task>/{result.json,...}
#   M0_WORK_ROOT   <m0-baseline>/work        workspaces, one fresh copy per run
#   M0_CACHE_DIR   <m0-baseline>/cache       pinned downloads and the image build context
#   M0_MODELS_JSON <harness>/config/models.json

M0_HARNESS=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
M0_ROOT=${M0_ROOT:-$(dirname "$M0_HARNESS")}
M0_TASKS_ROOT=${M0_TASKS_ROOT:-$M0_ROOT/tasks}
M0_RUNS_ROOT=${M0_RUNS_ROOT:-$M0_ROOT/runs}
M0_WORK_ROOT=${M0_WORK_ROOT:-$M0_ROOT/work}
M0_CACHE_DIR=${M0_CACHE_DIR:-$M0_ROOT/cache}
M0_MODELS_JSON=${M0_MODELS_JSON:-$M0_HARNESS/config/models.json}
export M0_HARNESS M0_ROOT M0_TASKS_ROOT M0_RUNS_ROOT M0_WORK_ROOT M0_CACHE_DIR M0_MODELS_JSON

M0_OC_VERSION=2.2.1
M0_IMAGE_REPO=localhost/m0-opencode

# m0_image TARGET -> the image tag prepare-image.sh builds for that target
m0_image() {
    case $1 in
        opencode) echo "$M0_IMAGE_REPO:$M0_OC_VERSION" ;;
        *) echo "$M0_IMAGE_REPO:$M0_OC_VERSION-$1" ;;
    esac
}

# Log lines go to stderr and, when M0_LOG_FILE is set, are appended to that file too.
m0_log() {
    local line
    line="$(date '+%F %T') $*"
    printf '%s\n' "$line" >&2
    if [[ -n ${M0_LOG_FILE:-} ]]; then
        printf '%s\n' "$line" >> "$M0_LOG_FILE" 2> /dev/null || true
    fi
}

m0_utc() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Our own helper scripts, run in isolated mode.
m0_py() { python3 -I "$M0_HARNESS/scripts/$1" "${@:2}"; }

# Paths that end up in `-v SRC:DST[:ro]` must not contain the separators of that syntax.
m0_mountable() { [[ $1 == /* && $1 != *[:,]* ]]; }

# m0_port_open HOST PORT -> 0 when something accepts connections there
m0_port_open() { (exec 3<> "/dev/tcp/$1/$2") 2> /dev/null; }

# m0_wait_port HOST PORT SECONDS
m0_wait_port() {
    local i
    for ((i = 0; i < $3 * 10; i++)); do
        m0_port_open "$1" "$2" && return 0
        sleep 0.1
    done
    return 1
}

# Git environment for host-side workspace scripts (setup.sh, solve.sh): neither the owner's
# global/system configuration nor GIT_* overrides from the caller reach the workspace. A script
# that commits without its own -c user.name/-c user.email therefore fails instead of using
# the owner's identity.
m0_git_env() {
    printf '%s\0' -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE -u GIT_OBJECT_DIRECTORY \
        -u GIT_AUTHOR_NAME -u GIT_AUTHOR_EMAIL -u GIT_AUTHOR_DATE \
        -u GIT_COMMITTER_NAME -u GIT_COMMITTER_EMAIL -u GIT_COMMITTER_DATE \
        -u GIT_CONFIG_PARAMETERS -u GIT_CONFIG_COUNT -u EMAIL \
        GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
}
