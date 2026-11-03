#!/usr/bin/env bash
# Run ONE M0 baseline task: the agent (OpenCode v2.2.1 headless, Claude Code --bare or ognivo) in a
# fresh rootless Podman container that sees only the workspace and the host's llama-server.
#
#   run-task.sh TASK_ID MODEL WORKSPACE PROMPT_FILE RUN_DIR
#     MODEL      llama/gpt-oss-20b | llama/gemma-4-26b-a4b | llama/qwen3.8-27b
#     WORKSPACE  a fresh copy built by the task's setup.sh: the container's only mount (/work, rw)
#     RUN_DIR    new directory; gets trajectory/ (prompt, events.jsonl, session*.json or
#                stream.jsonl, diff.patch, meta.json, llm.jsonl) and logs/ (run.env, container
#                and agent stderr, opencode-data/, verify.log)
#
# Env knobs (defaults):
#   HARNESS=opencode        or claude (Claude Code --bare; image target claude), or ognivo
#                           (`ognivo run --approve auto`; image target ognivo)
#   IMAGE=localhost/m0-opencode:2.2.1-full   (HARNESS=claude: ...-claude, HARNESS=ognivo: ...-ognivo)
#   M0_OG_APPROVE=auto      HARNESS=ognivo only: deny makes ognivo refuse what would ask
#   M0_OG_TIME_LIMIT        HARNESS=ognivo only: ognivo's own time limit, s (0: none; default:
#                           the agent timeout minus 10, see container/m0-entry-ognivo.sh)
#   ENGINE=podman           docker works for smoke tests only (no --timeout, no pasta; README)
#   LLAMA_PORT=8090         the host llama-server, bound to 127.0.0.1 (8080 is the owner's own llama-server)
#   AGENT_TIMEOUT=1200      seconds for the agent inside the container
#   CPUS=8 MEMORY=16g PIDS=4096   M0_NICE=10 (niceness inside the container)
#   NETWORK=pasta:--splice-only,-T,<port>   the container has only lo; 127.0.0.1:<port> is the host's
#   CONTAINER_LLAMA_URL=http://127.0.0.1:<port>/v1
#   TAP=0                   1 = put scripts/llm_tap.py (127.0.0.1:$TAP_PORT, default 8091) between
#                           the container and llama-server: raw requests, SSE and llama timings
#                           -> trajectory/llm.jsonl
#   DISK_BUDGET_GB=40 DISK_FLOOR_GB=30   the container is stopped when this run has used 40 GB
#                           of the workspace's (or Podman storage's) filesystem, or when less than
#                           30 GB is left there while free space is falling (>= 1 GB since the
#                           start); run.env gets disk_limit=budget|floor. 0 = stop at once (tests)
#   VERIFY_DIR=             task dir: afterwards run verify-task.sh (fresh --network none container)
#   DRY_RUN=0               1 = print the engine command and exit
#
# Exit code: the container's: 0 ok, 1 agent error event, 124 agent timeout, 3 llama-server not
# reachable, 2 no /work; 125-127 engine errors; 137/143 the host-side kill fired (the agent
# hung past AGENT_TIMEOUT+420 s, or the disk guard stopped it: see disk_limit in run.env);
# 130 interrupted (TERM/INT/HUP: the container is stopped).
set -euo pipefail
{   # parsed whole before it runs: editing this file cannot derail a running instance
HERE=$(cd "$(dirname "$(realpath "$0")")" && pwd)
usage() {
    sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0" >&2
    exit 2
}
[[ $# -eq 5 ]] || usage
TASK_ID=$1 MODEL=$2 RUN=$5
if [[ ${DRY_RUN:-0} == 1 ]]; then   # a plan: the paths need not exist yet
    WS=$(realpath -m -- "$3") PROMPT=$(realpath -m -- "$4")
else
    WS=$(realpath -e -- "$3" 2> /dev/null) || { echo "run-task: no workspace $3" >&2; exit 2; }
    PROMPT=$(realpath -e -- "$4" 2> /dev/null) || { echo "run-task: no prompt file $4" >&2; exit 2; }
fi
[[ $WS != *[:,]* ]] || { echo "run-task: the workspace path must not contain ':' or ','" >&2; exit 2; }
[[ $MODEL == */* ]] || MODEL=llama/$MODEL
HARNESS=${HARNESS:-opencode}
case $HARNESS in
    opencode) IMAGE=${IMAGE:-localhost/m0-opencode:2.2.1-full} ;;
    claude) IMAGE=${IMAGE:-localhost/m0-opencode:2.2.1-claude} ;;
    ognivo) IMAGE=${IMAGE:-localhost/m0-opencode:2.2.1-ognivo} ;;
    *) echo "run-task: HARNESS must be opencode, claude or ognivo" >&2; exit 2 ;;
esac
ENGINE=${ENGINE:-podman}
LLAMA_PORT=${LLAMA_PORT:-8090}
AGENT_TIMEOUT=${AGENT_TIMEOUT:-1200}
DISK_BUDGET_GB=${DISK_BUDGET_GB:-40} DISK_FLOOR_GB=${DISK_FLOOR_GB:-30}
[[ $DISK_BUDGET_GB =~ ^[0-9]+$ && $DISK_FLOOR_GB =~ ^[0-9]+$ ]] || { echo "run-task: DISK_BUDGET_GB/DISK_FLOOR_GB must be integers" >&2; exit 2; }
TAP=${TAP:-0} TAP_PORT=${TAP_PORT:-8091}
PORT_IN=$LLAMA_PORT
[[ $TAP == 1 ]] && PORT_IN=$TAP_PORT
NETWORK=${NETWORK:-pasta:--splice-only,-T,$PORT_IN}
CONTAINER_LLAMA_URL=${CONTAINER_LLAMA_URL:-http://127.0.0.1:$PORT_IN/v1}

alias=${MODEL##*/}
case $alias in gemma-4-26b-a4b) ctx=65536 ;; *) ctx=98304 ;; esac   # = limit.context in opencode.json
NAME="m0-$HARNESS-$TASK_ID-$alias-$(date +%Y%m%d%H%M%S)-$$"
NAME=${NAME//[^a-zA-Z0-9_.-]/-}

entry=() tail=()
if [[ $HARNESS == claude ]]; then
    entry=(--entrypoint /usr/bin/tini -e M0_CC_MODEL="$alias" -e M0_CTX="$ctx")
    tail=(-- /usr/local/bin/m0-entry-claude)
elif [[ $HARNESS == ognivo ]]; then
    # the profile sets context, output budget and sampling; the alias is the profile id
    entry=(--entrypoint /usr/bin/tini -e M0_OG_PROFILE="$alias" -e M0_OG_APPROVE="${M0_OG_APPROVE:-auto}")
    [[ -n ${M0_OG_TIME_LIMIT:-} ]] && entry+=(-e M0_OG_TIME_LIMIT="$M0_OG_TIME_LIMIT")
    tail=(-- /usr/local/bin/m0-entry-ognivo)
fi
limits=(--cpus "${CPUS:-8}" --memory "${MEMORY:-16g}" --memory-swap "${MEMORY:-16g}" --pids-limit "${PIDS:-4096}")
extra=()
if [[ $ENGINE != docker && $ENGINE != */docker ]]; then
    # conmon kills the container if the entrypoint itself hangs (export, git) past the agent budget
    extra+=(--timeout $((AGENT_TIMEOUT + 300)))
fi
cmd=("$ENGINE" run --name "$NAME" -i --pull never --network "$NETWORK"
    "${limits[@]}" "${extra[@]}" --cap-drop all --security-opt no-new-privileges
    -v "$WS:/work"
    -e M0_TASK_ID="$TASK_ID" -e M0_MODEL="$MODEL" -e M0_AGENT_TIMEOUT="$AGENT_TIMEOUT"
    -e M0_NICE="${M0_NICE:-10}" -e LLAMA_BASE_URL="$CONTAINER_LLAMA_URL" "${entry[@]}"
    "$IMAGE" "${tail[@]}")
# shq ARG...: the arguments as one shell line, quoted only where needed (dry runs)
shq() {
    local a out=()
    for a in "$@"; do
        if [[ $a =~ ^[A-Za-z0-9_@%+=:,./-]+$ ]]; then out+=("$a"); else out+=("'${a//\'/\'\\\'\'}'"); fi
    done
    printf '%s' "${out[*]}"
}
if [[ ${DRY_RUN:-0} == 1 ]]; then
    printf '%s < %s\n' "$(shq timeout --preserve-status -k 30 $((AGENT_TIMEOUT + 420)) "${cmd[@]}")" "$(shq "$PROMPT")"
    exit 0
fi

# RUN_DIR may exist already with logs/ from the suite (setup.log), but never from another run.
if [[ -e $RUN/trajectory || -e $RUN/logs/run.env ]]; then
    echo "run-task: $RUN already holds a run" >&2
    exit 2
fi
TRAJ=$RUN/trajectory LOGS=$RUN/logs
mkdir -p "$TRAJ" "$LOGS"
printf '%s\n' "task=$TASK_ID" "harness=$HARNESS" "model=$MODEL" "image=$IMAGE" "network=$NETWORK" \
    "container=$NAME" "agent_timeout=$AGENT_TIMEOUT" "llama_url=$CONTAINER_LLAMA_URL" "tap=$TAP" \
    "started=$(date -u +%FT%TZ)" > "$LOGS/run.env"
if ! image_id=$("$ENGINE" image inspect --format '{{.Id}}' "$IMAGE" 2>> "$LOGS/container.stderr"); then
    echo "run-task: image $IMAGE not found (build it with prepare-image.sh)" >&2
    echo "container_exit=125" >> "$LOGS/run.env"
    exit 125
fi
echo "image_id=$image_id" >> "$LOGS/run.env"

# The agent runs in the background so that TERM/INT/HUP (the suite's deadline watchdog, ^C) are
# handled at once: the container is stopped, what it wrote so far is copied out, and the run is
# marked interrupted.
interrupted=0 cpid= tap_pid=
on_signal() {
    interrupted=1
    [[ -n $cpid ]] && kill -TERM "$cpid" 2> /dev/null
    ("$ENGINE" stop -t 10 "$NAME" > /dev/null 2>&1 &)
}
trap on_signal TERM INT HUP
stop_tap() {
    if [[ -n $tap_pid ]]; then
        kill "$tap_pid" 2> /dev/null || true
        wait "$tap_pid" 2> /dev/null || true
        tap_pid=
    fi
}
# The tap records into logs/ and its file is moved to trajectory/llm.jsonl only after /out is
# copied: a /out/llm.jsonl the agent wrote must not replace the host's record.
TAP_LOG=$LOGS/.llm.jsonl.tap
if [[ $TAP == 1 ]]; then
    # --pdeathsig: the tap dies with this script even on SIGKILL, so no orphan keeps the port.
    # Ready = the tap's own line after bind, not "the port answers": an orphan on the port
    # would answer too and record this run into another run's file.
    pdeath=()
    command -v setpriv > /dev/null && pdeath=(setpriv --pdeathsig TERM)
    "${pdeath[@]}" python3 -I "$HERE/scripts/llm_tap.py" --listen "127.0.0.1:$TAP_PORT" \
        --upstream "http://127.0.0.1:$LLAMA_PORT" --log "$TAP_LOG" 2> "$LOGS/tap.stderr" &
    tap_pid=$!
    up=0
    for _ in $(seq 50); do
        kill -0 "$tap_pid" 2> /dev/null || break
        if grep -q '^llm_tap ' "$LOGS/tap.stderr" 2> /dev/null; then up=1; break; fi
        sleep 0.1
    done
    if ((!up)); then
        stop_tap
        echo "llm_tap did not start on 127.0.0.1:$TAP_PORT (see logs/tap.stderr)" > "$LOGS/infra_error"
        echo "container_exit=3" >> "$LOGS/run.env"
        echo "run-task: llm_tap did not start on 127.0.0.1:$TAP_PORT" >&2
        exit 3
    fi
fi
if ((interrupted)); then
    stop_tap
    echo "interrupted=1" >> "$LOGS/run.env"
    exit 130
fi
# Disk guard: the workspace bind mount and the container's writable layer have no quota. Stop the
# container when this run has used DISK_BUDGET_GB there (the agent's doing: final, agent_error)
# or less than DISK_FLOOR_GB is left while free space keeps falling, i.e. at least 1 GB since
# the start (the host's doing: infra_error, re-run). A small filesystem alone (the tests' tmpfs)
# never trips the floor.
avail_kb() { df -Pk -- "$1" 2> /dev/null | awk 'NR == 2 { print $4 }'; }
disk_watch() {   # disk_watch PID: runs while PID (the container's timeout wrapper) lives
    local pid=$1 paths=("$WS") base=() store p a i used
    store=$("$ENGINE" info --format '{{.Store.GraphRoot}}' 2> /dev/null) || store=
    if [[ -n $store && -d $store && $(stat -c %d -- "$store") != "$(stat -c %d -- "$WS")" ]]; then
        paths+=("$store")
    fi
    for p in "${paths[@]}"; do base+=("$(avail_kb "$p")"); done
    while kill -0 "$pid" 2> /dev/null; do
        sleep 5
        for i in "${!paths[@]}"; do
            a=$(avail_kb "${paths[i]}")
            [[ $a =~ ^[0-9]+$ && ${base[i]} =~ ^[0-9]+$ ]] || continue
            used=$(((base[i] - a) / 1048576))
            if ((used >= DISK_BUDGET_GB)); then
                echo "disk_limit=budget: the run used $used GB on ${paths[i]}" >> "$LOGS/run.env"
            elif ((a / 1048576 < DISK_FLOOR_GB && used >= 1)); then
                echo "disk_limit=floor: $((a / 1048576)) GB left on ${paths[i]}" >> "$LOGS/run.env"
                echo "disk nearly full: $((a / 1048576)) GB left on ${paths[i]}" > "$LOGS/infra_error"
            else
                continue
            fi
            "$ENGINE" stop -t 5 "$NAME" > /dev/null 2>&1
            return 0
        done
    done
}
timeout --preserve-status -k 30 $((AGENT_TIMEOUT + 420)) "${cmd[@]}" \
    < "$PROMPT" > "$LOGS/container.stdout" 2> "$LOGS/container.stderr" &
cpid=$!
disk_watch "$cpid" &
dwatch_pid=$!
rc=0
while :; do
    r=0
    wait "$cpid" || r=$?
    if ((r > 128)) && kill -0 "$cpid" 2> /dev/null; then continue; fi   # a trapped signal
    break
done
if ((r > 128)); then   # the status of a child that exited while wait was interrupted
    r2=0
    wait "$cpid" 2> /dev/null || r2=$?
    ((r2 != 127)) && r=$r2
fi
rc=$r
trap - TERM INT HUP
kill "$dwatch_pid" 2> /dev/null || true
wait "$dwatch_pid" 2> /dev/null || true
if [[ -n $tap_pid ]] && ! kill -0 "$tap_pid" 2> /dev/null; then
    # the agent's requests failed at a dead proxy: not the model's failure
    echo "llm_tap died during the run (see logs/tap.stderr)" > "$LOGS/infra_error"
fi
stop_tap
echo "container_exit=$rc" >> "$LOGS/run.env"
"$ENGINE" cp "$NAME:/out/." "$TRAJ/" 2>> "$LOGS/container.stderr" || echo "run-task: no /out copied from $NAME" >&2
# The agent is root in the container and can write /out too: a symlink there (events.jsonl ->
# ~/.ssh/...) would make host-side readers such as summarize_run.py read host files, and a FIFO
# would block them. Keep only regular files and directories.
find "$TRAJ" ! -type f ! -type d -printf 'run-task: dropped non-regular /out entry %P\n' -delete \
    >> "$LOGS/container.stderr" 2>&1 || true
# Names the host writes and the scorer reads (from logs/ only) must not come from the agent
# either: a /out/verify_exit holding 0 would otherwise read as solved.
rm -rf "$TRAJ/llm.jsonl" "$TRAJ/run.env" "$TRAJ/suite.env" "$TRAJ/verify_exit" "$TRAJ/verify.log" \
    "$TRAJ/result.json"
if [[ -f $TAP_LOG ]]; then mv -f "$TAP_LOG" "$TRAJ/llm.jsonl"; fi
# diff.patch includes the build trees of workspaces without a .gitignore: diff.src.patch is the
# source change set the failure catalogue reads
if [[ -f $TRAJ/diff.patch ]]; then
    python3 -I "$HERE/scripts/filter_patch.py" "$TRAJ/diff.patch" "$TRAJ/diff.src.patch" \
        2>> "$LOGS/container.stderr" || true
fi
# An OOM kill anywhere in the container's cgroup (opencode, rustc, ...) sets OOMKilled; 137 alone
# would read as a timeout.
oom=$("$ENGINE" inspect --format '{{.State.OOMKilled}}' "$NAME" 2> /dev/null) || oom=unknown
echo "oom_killed=$oom" >> "$LOGS/run.env"
"$ENGINE" rm -f "$NAME" > /dev/null 2>&1 || true
for f in opencode.stderr export.stderr claude.stderr opencode-data ognivo.stderr ognivo-data; do
    if [[ -e $TRAJ/$f ]]; then mv "$TRAJ/$f" "$LOGS/"; fi
done
# The entrypoint writes /out/infra_error only on its exit-3 path (llama-server unreachable,
# before the agent starts); any other one came from the agent and would buy it a re-run.
if [[ -f $TRAJ/infra_error && $rc == 3 ]]; then
    mv -f "$TRAJ/infra_error" "$LOGS/infra_error"
else
    rm -rf "$TRAJ/infra_error"
fi
echo "finished=$(date -u +%FT%TZ)" >> "$LOGS/run.env"
if ((interrupted)); then
    echo "interrupted=1" >> "$LOGS/run.env"
    echo "$TASK_ID $MODEL interrupted -> $RUN" >&2
    exit 130
fi

# Hidden check: the agent's container is gone; the tasks are mounted only now, read-only.
if [[ -n ${VERIFY_DIR:-} ]]; then
    case $rc in
        2 | 3 | 125 | 126 | 127) echo "run-task: agent did not run (exit $rc), verify skipped" >&2 ;;
        *) ENGINE=$ENGINE IMAGE=$IMAGE "$HERE/verify-task.sh" "$VERIFY_DIR" "$WS" "$RUN" || true ;;
    esac
fi
echo "$TASK_ID $MODEL exit=$rc verify=$(cat "$LOGS/verify_exit" 2> /dev/null || echo skipped) -> $RUN" >&2
exit "$rc"
exit
}
