#!/usr/bin/env bash
# The hidden check of one task, after the agent: TASK_DIR/verify.sh runs in a fresh container
# of the same image with --network none. Only now are the tasks mounted, read-only.
#
#   verify-task.sh TASK_DIR WORKSPACE RUN_DIR
#
# The tasks root (TASKS_ROOT, default the parent of TASK_DIR) is mounted read-only at its own
# absolute path, so verify.sh finds ../_common/<group>; the workspace is mounted at /work and
# passed as `verify.sh /work` (and WS=/work). verify.sh runs as root under nice, TMPDIR=/tmp
# inside the container (its writable layer, discarded with the container).
# Writes RUN_DIR/logs/verify.log and RUN_DIR/logs/verify_exit: 0 solved, 1 not solved, 2 the
# check could not run; 124/137/143 the time limit hit; 125-127 engine errors; 130 interrupted.
#
# Env: ENGINE=podman IMAGE=localhost/m0-opencode:2.2.1-full VERIFY_TIMEOUT=900
#      CPUS=8 MEMORY=16g PIDS=4096 M0_NICE=10 TASKS_ROOT=
#      VERIFY_ENV="K=V ..."  extra environment for verify.sh (e.g. FC_SKIP_FORMAT=1)
#      DRY_RUN=1             print the engine command and exit
set -euo pipefail
{   # parsed whole before it runs: editing this file cannot derail a running instance
usage() {
    sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0" >&2
    exit 2
}
[[ $# -eq 3 ]] || usage
TASK_DIR=$(realpath -e -- "$1" 2> /dev/null) || { echo "verify-task: no task dir $1" >&2; exit 2; }
if [[ ${DRY_RUN:-0} == 1 ]]; then   # a plan: the workspace need not exist yet
    WS=$(realpath -m -- "$2")
else
    WS=$(realpath -e -- "$2" 2> /dev/null) || { echo "verify-task: no workspace $2" >&2; exit 2; }
fi
RUN=$3
TASKS_ROOT=$(realpath -e -- "${TASKS_ROOT:-$(dirname "$TASK_DIR")}")
[[ -f $TASK_DIR/verify.sh ]] || { echo "verify-task: $TASK_DIR/verify.sh missing" >&2; exit 2; }
[[ $TASK_DIR == "$TASKS_ROOT"/* ]] || { echo "verify-task: $TASK_DIR is not under $TASKS_ROOT" >&2; exit 2; }
for p in "$WS" "$TASKS_ROOT"; do
    [[ $p != *[:,]* ]] || { echo "verify-task: path must not contain ':' or ',': $p" >&2; exit 2; }
done
ENGINE=${ENGINE:-podman}
IMAGE=${IMAGE:-localhost/m0-opencode:2.2.1-full}
VERIFY_TIMEOUT=${VERIFY_TIMEOUT:-900}
NAME="m0-verify-$(basename "$TASK_DIR")-$(date +%Y%m%d%H%M%S)-$$"
NAME=${NAME//[^a-zA-Z0-9_.-]/-}
venv=()
for kv in ${VERIFY_ENV:-}; do
    [[ $kv == [A-Za-z_]*=* ]] || { echo "verify-task: bad VERIFY_ENV entry $kv" >&2; exit 2; }
    venv+=(-e "$kv")
done
cmd=("$ENGINE" run --rm --name "$NAME" --pull never --network none
    --cpus "${CPUS:-8}" --memory "${MEMORY:-16g}" --memory-swap "${MEMORY:-16g}" --pids-limit "${PIDS:-4096}"
    --cap-drop all --security-opt no-new-privileges
    -v "$WS:/work" -v "$TASKS_ROOT:$TASKS_ROOT:ro"
    -e WS=/work -e TMPDIR=/tmp -e OGNIVO_VERIFY_TMPDIR=/tmp -e CARGO_NET_OFFLINE=true "${venv[@]}"
    --entrypoint nice "$IMAGE" -n "${M0_NICE:-10}" bash "$TASK_DIR/verify.sh" /work)
# shq ARG...: the arguments as one shell line, quoted only where needed (dry runs)
shq() {
    local a out=()
    for a in "$@"; do
        if [[ $a =~ ^[A-Za-z0-9_@%+=:,./-]+$ ]]; then out+=("$a"); else out+=("'${a//\'/\'\\\'\'}'"); fi
    done
    printf '%s' "${out[*]}"
}
if [[ ${DRY_RUN:-0} == 1 ]]; then
    shq timeout --preserve-status -k 30 "$VERIFY_TIMEOUT" "${cmd[@]}"
    echo
    exit 0
fi
LOGS=$RUN/logs
mkdir -p "$LOGS"
interrupted=0 cpid=
on_signal() {
    interrupted=1
    [[ -n $cpid ]] && kill -TERM "$cpid" 2> /dev/null
    ("$ENGINE" rm -f "$NAME" > /dev/null 2>&1 &)
}
trap on_signal TERM INT HUP
t0=$(date +%s.%N)
timeout --preserve-status -k 30 "$VERIFY_TIMEOUT" "${cmd[@]}" < /dev/null > "$LOGS/verify.log" 2>&1 &
cpid=$!
while :; do
    r=0
    wait "$cpid" || r=$?
    if ((r > 128)) && kill -0 "$cpid" 2> /dev/null; then continue; fi
    break
done
if ((r > 128)); then
    r2=0
    wait "$cpid" 2> /dev/null || r2=$?
    ((r2 != 127)) && r=$r2
fi
trap - TERM INT HUP
t1=$(date +%s.%N)
((interrupted)) && r=130
"$ENGINE" rm -f "$NAME" > /dev/null 2>&1 || true
echo "$r" > "$LOGS/verify_exit"
{
    echo "verify_exit=$r"
    echo "verify_wall_s=$(python3 -I -c "print(round($t1 - $t0, 3))")"
    echo "verify_container=$NAME"
} >> "$LOGS/run.env"
exit 0
exit
}
