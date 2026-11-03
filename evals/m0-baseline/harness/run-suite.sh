#!/usr/bin/env bash
# M0 baseline suite for one model: per task, build a fresh workspace with the task's setup.sh,
# run the agent in its container (run-task.sh) with a per-task timeout, run the task's
# verify.sh in a fresh --network none container (verify-task.sh), and write
#   runs/<UTC stamp>/<model>/<task-id>/{result.json,trajectory/,logs/}
# With --server it also starts and stops the pinned llama-server for the model.
#
#   run-suite.sh [options] MODEL [TASK_ID...]
#   run-suite.sh [options] --agent reference|none [TASK_ID...]
#     MODEL    gpt-oss-20b | gemma-4-26b-a4b | qwen3.8-27b (config/models.json; "llama/" prefix ok)
#     TASK_ID  default: every tasks/*/task.json with "role": "selected"
#
# Options:
#   --server          start the pinned llama-server for MODEL on 127.0.0.1:PORT (flags from the
#                     ognivo profile, scripts/llama_cmd.py) and stop it at the end; refuses when
#                     another process holds GPU memory (nvidia-smi --query-compute-apps) or the
#                     port is taken. Without --server a llama-server must already serve MODEL.
#   --port N          llama-server port (default 8090, config/models.json "port"; 8080 is the owner's own llama-server)
#   --stamp STAMP     write into runs/STAMP (default: a new UTC timestamp). An existing STAMP is
#                     resumed: tasks with a final result.json are skipped, the others re-run
#                     (the earlier attempt moves to <model>/_attempts/).
#   --timeout SEC     agent timeout per task (default: task.json agent_timeout_s, else the
#                     model's agent_timeout_s: 480 gpt-oss/gemma, 1500 qwen)
#   --verify-timeout SEC   (default 900)
#   --deadline WHEN   epoch seconds or HH:MM (local, the next one): start a task only when it
#                     can finish before WHEN; an agent still running at WHEN is stopped and its
#                     run marked interrupted (re-run on resume); the server stops at WHEN
#   --harness opencode|claude|ognivo   (default opencode; claude results go to <model>.claude-code/,
#                     ognivo results to <model>.ognivo/)
#   --agent container|reference|none   container (default) = the model. reference = the task's
#                     solve.sh on the host and none = no change, both followed by the real
#                     verify container: they check setup, verify and the image without a model
#                     (results under reference/ and unsolved/; reference must pass, none fail)
#   --tap | --no-tap  record raw LLM traffic to trajectory/llm.jsonl (default: --tap)
#   --keep-ws         keep finished workspaces (default: removed; unfinished runs keep theirs)
#   --verify-env K=V  extra environment for verify.sh, repeatable (e.g. FC_SKIP_FORMAT=1)
#   --tasks-root DIR  (default <m0-baseline>/tasks)
#   --dry-run         print the plan (runs, server and container commands) and exit
#
# Exit: 0 every planned task is final; 10 the deadline stopped the suite (tasks left);
# 11 every task ran but some results are not final (setup/infra/verify errors: re-run them
# with --stamp); 20 llama-server could not be started or does not match the profile; 21 GPU or port busy;
# 22 llama-server down and not restartable; 3 another suite is running; 2 usage;
# 130 interrupted (TERM/INT/HUP).
# Environment: M0_ROOT M0_RUNS_ROOT M0_WORK_ROOT M0_TASKS_ROOT (lib/common.sh), ENGINE IMAGE CPUS
#   MEMORY PIDS M0_NICE TAP_PORT (run-task.sh), LLAMA_SERVER_BIN OGNIVO_REPO M0_MODELS_JSON
#   (scripts/llama_cmd.py), NVIDIA_SMI=nvidia-smi, TEMPLATE_CHECK=strict|warn,
#   SERVER_LOAD_TIMEOUT=900, SETUP_TIMEOUT=900, MAX_SERVER_RESTARTS=2, GPU_RETRIES=4 checks
#   GPU_RETRY_SLEEP=5 s apart before refusing;
#   start-guard estimates M0_SETUP_EST=120 M0_VERIFY_EST (default 4 x verify_seconds + 30,
#   at least 60) M0_GUARD_OVERHEAD=60, M0_START_GUARD=0 disables the guard (tests only).
set -euo pipefail
{   # parsed whole before it runs: editing this file cannot derail a running instance
HERE=$(cd "$(dirname "$(realpath "$0")")" && pwd)
# shellcheck source=lib/common.sh
source "$HERE/lib/common.sh"

usage() {
    sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0" >&2
    exit 2
}
die() {
    m0_log "error: $1"
    exit "${2:-2}"
}

parse_deadline() {
    local now t
    now=$(date +%s)
    if [[ $1 =~ ^[0-9]+$ ]]; then
        echo "$1"
    elif [[ $1 =~ ^[0-2][0-9]:[0-5][0-9]$ ]]; then
        t=$(date -d "today $1" +%s)
        ((t > now)) || t=$(date -d "tomorrow $1" +%s)
        echo "$t"
    else
        return 1
    fi
}

SERVER=0 PORT= STAMP= TIMEOUT= VERIFY_TIMEOUT=900 DEADLINE=0 HARNESS=opencode AGENT_MODE=container
TAP=1 KEEP_WS=0 DRY_RUN=0
VERIFY_ENV_LIST=()
while (($#)); do
    case $1 in
        --server) SERVER=1 ;;
        --port) PORT=${2:?}; shift ;;
        --stamp) STAMP=${2:?}; shift ;;
        --timeout) TIMEOUT=${2:?}; shift ;;
        --verify-timeout) VERIFY_TIMEOUT=${2:?}; shift ;;
        --deadline) DEADLINE=$(parse_deadline "${2:?}") || die "bad --deadline $2"; shift ;;
        --harness) HARNESS=${2:?}; shift ;;
        --agent) AGENT_MODE=${2:?}; shift ;;
        --tap) TAP=1 ;;
        --no-tap) TAP=0 ;;
        --keep-ws) KEEP_WS=1 ;;
        --verify-env) VERIFY_ENV_LIST+=("${2:?}"); shift ;;
        --tasks-root) M0_TASKS_ROOT=$(realpath -e -- "${2:?}") || die "no tasks root $2"; shift ;;
        --dry-run) DRY_RUN=1 ;;
        -h | --help) usage ;;
        --) shift; break ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
    shift
done
[[ $STAMP =~ ^[A-Za-z0-9._-]*$ ]] || die "bad --stamp $STAMP"
[[ $TIMEOUT =~ ^[0-9]*$ && $VERIFY_TIMEOUT =~ ^[0-9]+$ ]] || die "timeouts are whole seconds"
case $HARNESS in opencode | claude | ognivo) ;; *) die "--harness opencode|claude|ognivo" ;; esac
case $AGENT_MODE in container | reference | none) ;; *) die "--agent container|reference|none" ;; esac
for kv in "${VERIFY_ENV_LIST[@]}"; do [[ $kv =~ ^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*$ ]] || die "bad --verify-env $kv"; done

if [[ $AGENT_MODE == container ]]; then
    (($#)) || usage
    MODEL=${1#llama/}
    shift
    info=$(m0_py llama_cmd.py info "$MODEL") || die "unknown model $MODEL"
    eval "$info"
    MODEL_LABEL=$MODEL
    [[ $HARNESS == claude ]] && MODEL_LABEL=$MODEL.claude-code
    [[ $HARNESS == ognivo ]] && MODEL_LABEL=$MODEL.ognivo
    PORT=${PORT:-$M0_LLAMA_PORT}
else
    MODEL=$AGENT_MODE M0_MODEL_TIMEOUT=0 M0_LLAMA_SERVER=
    MODEL_LABEL=reference
    [[ $AGENT_MODE == none ]] && MODEL_LABEL=unsolved
    PORT=${PORT:-8090}
    if ((SERVER)); then
        m0_log "note: --server is ignored with --agent $AGENT_MODE (no model runs)"
        SERVER=0
    fi
fi
[[ $PORT =~ ^[0-9]+$ ]] || die "bad --port $PORT"
TAP_PORT=${TAP_PORT:-$((PORT + 1))}
export TAP_PORT
ENGINE=${ENGINE:-podman}
if [[ -z ${IMAGE:-} ]]; then
    IMAGE=$(m0_image full)
    [[ $HARNESS == claude ]] && IMAGE=$(m0_image claude)
    [[ $HARNESS == ognivo ]] && IMAGE=$(m0_image ognivo)
fi
export ENGINE IMAGE
NVIDIA_SMI=${NVIDIA_SMI:-nvidia-smi}
SERVER_LOAD_TIMEOUT=${SERVER_LOAD_TIMEOUT:-900}
SETUP_TIMEOUT=${SETUP_TIMEOUT:-900}
MAX_SERVER_RESTARTS=${MAX_SERVER_RESTARTS:-2}
GPU_RETRIES=${GPU_RETRIES:-4} GPU_RETRY_SLEEP=${GPU_RETRY_SLEEP:-5}
GUARD_OVERHEAD=${M0_GUARD_OVERHEAD:-60}

taskf=$(mktemp)
trap 'rm -f "$taskf"' EXIT
if ! m0_py plan.py tasks "$M0_TASKS_ROOT" "$@" > "$taskf"; then
    die "invalid task list (see above)"
fi
mapfile -t TASKS < "$taskf"
((${#TASKS[@]})) || die "no tasks to run in $M0_TASKS_ROOT"

STAMP=${STAMP:-$(date -u +%Y%m%dT%H%M%SZ)}
STAMP_DIR=$M0_RUNS_ROOT/$STAMP
MODEL_DIR=$STAMP_DIR/$MODEL_LABEL
WORK_DIR=$M0_WORK_ROOT/$STAMP/$MODEL_LABEL
m0_py plan.py pending "$MODEL_DIR" "${TASKS[@]}" > "$taskf"
mapfile -t PENDING < "$taskf"
for p in "$M0_WORK_ROOT" "$M0_TASKS_ROOT"; do
    m0_mountable "$p" || die "$p cannot be bind-mounted (needs an absolute path without ':' or ',')"
done

server_url() { echo "http://127.0.0.1:$PORT"; }

# --- dry run -------------------------------------------------------------------------------
if ((DRY_RUN)); then
    echo "stamp      $STAMP ($STAMP_DIR)"
    echo "model      $MODEL_LABEL   harness=$HARNESS agent=$AGENT_MODE"
    echo "engine     $ENGINE   image=$IMAGE"
    echo "tasks      ${#TASKS[@]} (${#PENDING[@]} to run, $((${#TASKS[@]} - ${#PENDING[@]})) already final)"
    if ((DEADLINE)); then echo "deadline   $(date -d "@$DEADLINE" '+%F %T %Z')"; fi
    if [[ $AGENT_MODE == container ]]; then
        if ((SERVER)); then
            echo "server     managed: started here, stopped at the end or at the deadline"
            if ! m0_py llama_cmd.py show "$MODEL" --port "$PORT" 2> /dev/null | sed 's/^/           /'; then
                m0_py llama_cmd.py show "$MODEL" --port "$PORT" --no-file-check | sed 's/^/           /'
                m0_py llama_cmd.py show "$MODEL" --port "$PORT" 2>&1 > /dev/null | sed 's/^/           WARNING: /' || true
            fi
            echo "           GPU guard: $NVIDIA_SMI --query-compute-apps=pid,process_name,used_memory --format=csv,noheader must be empty"
        else
            echo "server     expected at $(server_url) serving alias $MODEL"
        fi
        echo "tap        $TAP (127.0.0.1:$TAP_PORT -> $(server_url))"
    fi
    printf '%-34s %-9s %8s %8s  %s\n' task role timeout verify-est state
    first=
    for tid in "${TASKS[@]}"; do
        envs=$(m0_py plan.py task-env "$M0_TASKS_ROOT/$tid" /dev/null) || die "task $tid is not valid"
        eval "$envs"
        t=${TIMEOUT:-${TASK_AGENT_TIMEOUT:-$M0_MODEL_TIMEOUT}}
        v=${TASK_VERIFY_SECONDS:-15}
        vest=${M0_VERIFY_EST:-$((4 * v + 30 > 60 ? 4 * v + 30 : 60))}
        state=run
        if ! grep -qxF -- "$tid" <<< "$(printf '%s\n' "${PENDING[@]}")"; then state="final (skip)"; elif [[ -z $first ]]; then first=$tid; fi
        [[ $AGENT_MODE == container ]] && t="${t}s" || t=-
        printf '%-34s %-9s %8s %7ss  %s\n' "$tid" "${TASK_ROLE:--}" "$t" "$vest" "$state"
    done
    if [[ -n $first ]]; then
        ws=$WORK_DIR/$first
        echo "setup      env GIT_CONFIG_GLOBAL=/dev/null ... nice -n 10 timeout $SETUP_TIMEOUT bash $M0_TASKS_ROOT/$first/setup.sh $ws"
        if [[ $AGENT_MODE == container ]]; then
            echo "agent      (first task)"
            mkdir -p "$(dirname "$taskf")"
            t=${TIMEOUT:-$M0_MODEL_TIMEOUT}
            DRY_RUN=1 HARNESS=$HARNESS AGENT_TIMEOUT=$t TAP=$TAP LLAMA_PORT=$PORT \
                "$HERE/run-task.sh" "$first" "llama/$MODEL" "$ws" "$WORK_DIR/$first.prompt.txt" "$MODEL_DIR/$first" \
                | sed 's/^/           /'
        else
            echo "agent      $AGENT_MODE: $([[ $AGENT_MODE == reference ]] && echo "bash $M0_TASKS_ROOT/$first/solve.sh $ws" || echo "no change")"
        fi
        echo "verify     (first task)"
        DRY_RUN=1 VERIFY_TIMEOUT=$VERIFY_TIMEOUT VERIFY_ENV="${VERIFY_ENV_LIST[*]}" TASKS_ROOT=$M0_TASKS_ROOT \
            "$HERE/verify-task.sh" "$M0_TASKS_ROOT/$first" "$ws" "$MODEL_DIR/$first" | sed 's/^/           /'
    fi
    exit 0
fi

# --- real run ------------------------------------------------------------------------------
mkdir -p "$M0_RUNS_ROOT" "$MODEL_DIR" "$WORK_DIR"
exec 9> "$M0_RUNS_ROOT/.suite.lock"
flock -n 9 || die "another run-suite.sh holds $M0_RUNS_ROOT/.suite.lock" 3
export M0_LOG_FILE=$MODEL_DIR/suite.log
mapfile -d '' GITENV < <(m0_git_env)

SERVER_PID= SERVER_STARTS=0 SERVER_RESTARTS=0 SUITE_INT=0 WATCH_PID= RC=0 WATCH_HIT=0 PLANNED_STOP=0
PIDFILE=$M0_RUNS_ROOT/.llama-server.pid

server_healthy() { curl -fsS -m 5 -o /dev/null "$(server_url)/health" 2> /dev/null; }

gpu_busy() {   # prints what holds GPU memory; status 0 = busy or unknown, 1 = free
    local out
    # timeout: after a driver wedge (Xid) nvidia-smi can block forever; a timeout counts as busy
    if ! out=$(timeout -k 5 30 "$NVIDIA_SMI" --query-compute-apps=pid,process_name,used_memory --format=csv,noheader 2>&1); then
        printf 'nvidia-smi failed: %s\n' "$out"
        return 0
    fi
    out=$(sed '/^[[:space:]]*$/d' <<< "$out")
    [[ -n $out ]] && { printf '%s\n' "$out"; return 0; }
    return 1
}

gpu_has_pid() {
    local out
    out=$(timeout -k 5 30 "$NVIDIA_SMI" --query-compute-apps=pid --format=csv,noheader 2> /dev/null) || return 1
    grep -qE "^[[:space:]]*$1[[:space:]]*$" <<< "$out"
}

server_check() {
    local url
    url=$(server_url)
    if ! curl -fsS -m 15 "$url/props" -o "$MODEL_DIR/server-props.json" 2> /dev/null; then
        m0_log "GET $url/props failed"
        return 1
    fi
    curl -fsS -m 15 "$url/v1/models" -o "$MODEL_DIR/server-models.json" 2> /dev/null || : > "$MODEL_DIR/server-models.json"
    local ok=0
    m0_py llama_cmd.py check "$MODEL" "$MODEL_DIR/server-props.json" "$MODEL_DIR/server-models.json" \
        > "$MODEL_DIR/server.json" || ok=$?
    python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1]))
for w in r.get("warnings", []): print("warning: " + w)
for p in r.get("problems", []): print("problem: " + p)
print("server: build %s, n_ctx %s, slots %s, template %s" % (r.get("build_info"), r.get("n_ctx"), r.get("total_slots"), r.get("template_verdict")))' \
        "$MODEL_DIR/server.json" | while IFS= read -r l; do m0_log "$l"; done
    return "$ok"
}

stale_server_cleanup() {
    [[ -f $PIDFILE ]] || return 0
    local pid exe want
    pid=$(cat "$PIDFILE" 2> /dev/null || true)
    if [[ $pid =~ ^[0-9]+$ ]] && kill -0 "$pid" 2> /dev/null; then
        exe=$(readlink -f "/proc/$pid/exe" 2> /dev/null || true)
        want=$(readlink -f "$M0_LLAMA_SERVER" 2> /dev/null || true)
        if [[ -n $exe && $exe == "$want" ]]; then
            m0_log "stopping a stale llama-server left by an earlier suite (pid $pid)"
            kill -TERM "$pid" 2> /dev/null || true
            for _ in $(seq 60); do kill -0 "$pid" 2> /dev/null || break; sleep 1; done
            kill -KILL "$pid" 2> /dev/null || true
        fi
    fi
    rm -f "$PIDFILE"
}

server_stop() {
    [[ -n $SERVER_PID ]] || return 0
    local pid=$SERVER_PID i
    m0_log "stopping llama-server (pid $pid)"
    kill -TERM "$pid" 2> /dev/null || true
    for ((i = 0; i < 60; i++)); do kill -0 "$pid" 2> /dev/null || break; sleep 1; done
    if kill -0 "$pid" 2> /dev/null; then
        m0_log "llama-server ignored TERM for 60 s, killing it"
        kill -KILL "$pid" 2> /dev/null || true
    fi
    wait "$pid" 2> /dev/null || true
    rm -f "$PIDFILE"
    for ((i = 0; i < 30; i++)); do gpu_has_pid "$pid" || break; sleep 1; done
    SERVER_PID=
}

server_start() {
    local busy i argvf t0 unset=() v
    # A server we just stopped may still be listed, and run-night.sh asks for a longer wait
    # (GPU_RETRIES x GPU_RETRY_SLEEP) so that a GPU or port still taken at the window's start
    # does not end the night at once. Never waits past the deadline.
    for ((i = 1; i <= GPU_RETRIES; i++)); do
        if busy=$(gpu_busy); then
            busy="another process holds GPU memory:"$'\n'"$busy"
        elif m0_port_open 127.0.0.1 "$PORT"; then
            busy="127.0.0.1:$PORT is already in use"
        else
            busy=
            break
        fi
        ((i < GPU_RETRIES)) || break
        ((DEADLINE > 0 && $(date +%s) + GPU_RETRY_SLEEP >= DEADLINE)) && break
        ((i == 1 && GPU_RETRIES > 4)) && m0_log "GPU or port busy: waiting up to $(((GPU_RETRIES - 1) * GPU_RETRY_SLEEP)) s"
        sleep "$GPU_RETRY_SLEEP"
        ((SUITE_INT)) && return 130
    done
    if [[ -n $busy ]]; then
        m0_log "refusing to start llama-server: $(head -1 <<< "$busy")"
        tail -n +2 <<< "$busy" | while IFS= read -r v; do m0_log "  $v"; done
        return 21
    fi
    argvf=$MODEL_DIR/llama-server.argv
    m0_py llama_cmd.py argv "$MODEL" --port "$PORT" > "$argvf" || return 20
    local -a sargv
    mapfile -d '' sargv < "$argvf"
    m0_py llama_cmd.py show "$MODEL" --port "$PORT" > "$MODEL_DIR/llama-server.cmd" 2> /dev/null \
        || printf '%q ' "${sargv[@]}" > "$MODEL_DIR/llama-server.cmd"
    for v in $(compgen -e); do
        if [[ $v == LLAMA_ARG_* ]]; then unset+=(-u "$v"); fi   # llama-server reads LLAMA_ARG_* as flags
    done
    m0_log "starting llama-server for $MODEL on 127.0.0.1:$PORT (log: $MODEL_DIR/llama-server.log)"
    echo "=== $(m0_utc) start: $(cat "$MODEL_DIR/llama-server.cmd")" >> "$MODEL_DIR/llama-server.log"
    # pdeathsig: the server dies with this script even after a SIGKILL; setsid: ^C does not hit it.
    # 8>&- 9>&-: the night/suite locks must not be inherited by long-lived children.
    env "${unset[@]}" setpriv --pdeathsig TERM -- setsid "${sargv[@]}" \
        >> "$MODEL_DIR/llama-server.log" 2>&1 < /dev/null 8>&- 9>&- &
    SERVER_PID=$!
    echo "$SERVER_PID" > "$PIDFILE"
    t0=$(date +%s)
    while ! server_healthy; do
        if ! kill -0 "$SERVER_PID" 2> /dev/null; then
            m0_log "llama-server exited while loading; last lines of its log:"
            tail -n 15 "$MODEL_DIR/llama-server.log" | while IFS= read -r v; do m0_log "  $v"; done
            wait "$SERVER_PID" 2> /dev/null || true
            SERVER_PID=
            rm -f "$PIDFILE"
            return 20
        fi
        if (($(date +%s) - t0 > SERVER_LOAD_TIMEOUT)); then
            m0_log "llama-server not healthy after ${SERVER_LOAD_TIMEOUT}s"
            server_stop
            return 20
        fi
        if ((SUITE_INT)); then
            server_stop
            return 130
        fi
        sleep 2
    done
    m0_log "llama-server healthy after $(($(date +%s) - t0)) s"
    if ! server_check; then
        m0_log "llama-server does not match the profile (see $MODEL_DIR/server.json)"
        server_stop
        return 20
    fi
    SERVER_STARTS=$((SERVER_STARTS + 1))
    return 0
}

# Make sure a healthy server is there before an agent starts: 0 ok, else an exit code.
server_ensure() {
    server_healthy && return 0
    if ((!SERVER)); then
        m0_log "llama-server at $(server_url) is not healthy"
        return 22
    fi
    if ((PLANNED_STOP)); then   # we stopped it ourselves before a verify: not a crash
        PLANNED_STOP=0
        m0_log "starting llama-server again after the planned stop"
    elif ((SERVER_STARTS > 0)); then
        if ((SERVER_RESTARTS >= MAX_SERVER_RESTARTS)); then
            m0_log "llama-server died again; $MAX_SERVER_RESTARTS restarts used up"
            return 22
        fi
        SERVER_RESTARTS=$((SERVER_RESTARTS + 1))
        m0_log "llama-server is down: restart $SERVER_RESTARTS of $MAX_SERVER_RESTARTS"
        server_stop
    fi
    server_start
}

on_suite_signal() {
    SUITE_INT=1
    [[ -n $WATCH_PID ]] && kill -TERM "$WATCH_PID" 2> /dev/null
    return 0
}
trap on_suite_signal TERM INT HUP
trap 'server_stop; rm -f "$taskf"' EXIT

# run_watched LABEL DEADLINE LOGFILE CMD...: CMD in the background (output appended to LOGFILE);
# TERM at DEADLINE (0 = none) or when the suite is interrupted, KILL 90 s later.
# Sets RC, and WATCH_HIT=1 when the deadline stopped it.
run_watched() {
    local label=$1 deadline=$2 logf=$3 sent=0 t_sent=0 now
    shift 3
    WATCH_HIT=0
    "$@" >> "$logf" 2>&1 8>&- 9>&- &
    WATCH_PID=$!
    while kill -0 "$WATCH_PID" 2> /dev/null; do
        now=$(date +%s)
        if ((sent == 0)); then
            if ((deadline > 0 && now >= deadline)); then
                m0_log "deadline reached: stopping $label"
                WATCH_HIT=1 sent=1 t_sent=$now
                kill -TERM "$WATCH_PID" 2> /dev/null || true
            elif ((SUITE_INT)); then
                sent=1 t_sent=$now
                kill -TERM "$WATCH_PID" 2> /dev/null || true
            fi
        elif ((now - t_sent > 90)); then
            kill -KILL "$WATCH_PID" 2> /dev/null || true
        fi
        sleep 1
    done
    RC=0
    wait "$WATCH_PID" || RC=$?
    WATCH_PID=
}

rm_tree() {
    [[ -e $1 ]] || return 0
    rm -rf -- "$1" 2> /dev/null && return 0
    if [[ $ENGINE == podman ]] && podman unshare rm -rf -- "$1" 2> /dev/null; then return 0; fi
    m0_log "warning: could not remove $1"
}

# The no-model agents: the task's solve.sh (reference) or nothing (none), on the host.
agent_local() {
    local tid=$1 tdir=$2 ws=$3 prompt=$4 run=$5 traj base rc=0 idx t0 t1
    traj=$run/trajectory
    mkdir -p "$traj" "$run/logs"
    cp "$prompt" "$traj/prompt.txt"
    base=$(git -C "$ws" rev-parse --verify -q HEAD 2> /dev/null || true)
    [[ -n $base ]] && echo "$base" > "$traj/base_commit"
    printf '%s\n' "task=$tid" "harness=$AGENT_MODE" "model=$AGENT_MODE" "image=$IMAGE" "network=none" \
        "started=$(date -u +%FT%TZ)" > "$run/logs/run.env"
    t0=$(date +%s.%N)
    if [[ $AGENT_MODE == reference ]]; then
        if [[ -f $tdir/solve.sh ]]; then
            run_watched "solve.sh $tid" 0 "$run/logs/agent.log" \
                env "${GITENV[@]}" WS="$ws" nice -n 10 timeout -k 10 600 bash "$tdir/solve.sh" "$ws"
            rc=$RC
        else
            echo "no solve.sh in $tdir" >> "$run/logs/agent.log"
            rc=127
        fi
    fi
    t1=$(date +%s.%N)
    if [[ -n $base ]]; then
        idx=$(mktemp)
        (cd "$ws" && GIT_INDEX_FILE=$idx git read-tree "$base" && GIT_INDEX_FILE=$idx git add -A \
            && GIT_INDEX_FILE=$idx git diff --cached --binary "$base") > "$traj/diff.patch" 2> /dev/null || true
        rm -f "$idx"
    fi
    echo "$rc" > "$traj/exit_code"
    printf '{"task_id": "%s", "model": "%s", "harness": "%s", "exit_code": %d, "wall_s": %s}\n' \
        "$tid" "$AGENT_MODE" "$AGENT_MODE" "$rc" "$(python3 -I -c "print(round($t1 - $t0, 3))")" > "$traj/meta.json"
    { echo "container_exit=$rc"; echo "finished=$(date -u +%FT%TZ)"; } >> "$run/logs/run.env"
    RC=$rc
}

# run_one TASK_ID -> sets LAST_STATUS (and LAST_FINAL=1/0)
run_one() {
    local tid=$1 attempt=1 tdir run ws prompt n started t0 t1 setup_rc=0 setup_wall=0 agent_rc=0 hit=0
    local interrupted=0 sok= kept=1 timeout_s skip_verify=0 status final
    tdir=$M0_TASKS_ROOT/$tid run=$MODEL_DIR/$tid ws=$WORK_DIR/$tid prompt=$WORK_DIR/$tid.prompt.txt
    started=$(m0_utc)
    n=0
    [[ -d $MODEL_DIR/_attempts ]] && n=$(find "$MODEL_DIR/_attempts" -maxdepth 1 -name "$tid.*" | wc -l)
    if [[ -e $run ]]; then   # an earlier, unfinished attempt
        mkdir -p "$MODEL_DIR/_attempts"
        n=$((n + 1))
        mv "$run" "$MODEL_DIR/_attempts/$tid.$n"
    fi
    attempt=$((n + 1))   # counts every attempt of this task in this stamp
    rm_tree "$ws"
    mkdir -p "$run/logs" "$WORK_DIR"
    local envs
    if ! envs=$(m0_py plan.py task-env "$tdir" "$prompt" 2>> "$run/logs/setup.log"); then
        envs="TASK_PROMPT_SHA256= TASK_PROMPT_SHA256_EXPECTED= TASK_AGENT_TIMEOUT="
        setup_rc=2
    fi
    eval "$envs"
    timeout_s=${TIMEOUT:-${TASK_AGENT_TIMEOUT:-$M0_MODEL_TIMEOUT}}
    # The task's content hash and the host state at the start: the agent budget is wall-clock,
    # so AC power, the platform profile and the GPU's limits belong to the result.
    local task_sha host_ac host_profile host_gpu host_load cap_n=1 p
    task_sha=$(m0_py manifest.py task "$tdir" 2> /dev/null) || task_sha=
    host_ac=$(cat /sys/class/power_supply/ADP*/online 2> /dev/null) || host_ac=
    host_ac=${host_ac%%$'\n'*}
    host_profile=$(cat /sys/firmware/acpi/platform_profile 2> /dev/null) || host_profile=
    host_gpu=$(timeout 5 "$NVIDIA_SMI" --query-gpu=temperature.gpu,power.limit,clocks.max.sm,pstate \
        --format=csv,noheader 2> /dev/null) || host_gpu=
    host_gpu=${host_gpu%%$'\n'*}   # the first GPU
    host_load=$(cut -d' ' -f1-3 /proc/loadavg 2> /dev/null) || host_load=
    # Attempts that count toward M0_MAX_ATTEMPTS: this one and every earlier one that was not
    # cut off by a deadline or a signal (summarize_run.py turns a non-final run into "abandoned").
    for p in "$MODEL_DIR"/_attempts/"$tid".*/result.json; do
        [[ -f $p ]] || continue
        grep -q '"status": "interrupted"' "$p" || cap_n=$((cap_n + 1))
    done
    m0_log "[$tid] attempt $attempt: setup"

    t0=$(date +%s.%N)
    if ((setup_rc == 0)); then
        run_watched "setup.sh $tid" 0 "$run/logs/setup.log" \
            env "${GITENV[@]}" WS="$ws" nice -n 10 timeout -k 10 "$SETUP_TIMEOUT" bash "$tdir/setup.sh" "$ws"
        setup_rc=$RC
    fi
    t1=$(date +%s.%N)
    setup_wall=$(python3 -I -c "print(round($t1 - $t0, 3))")
    ((SUITE_INT)) && interrupted=1
    if ((setup_rc == 0 && !interrupted)) && ! git -C "$ws" rev-parse --verify -q HEAD > /dev/null 2>&1; then
        echo "setup.sh left no git repository with a commit in $ws" >> "$run/logs/setup.log"
        setup_rc=1
    fi
    # The workspace must not show where the task's diff went (mtimes, inode order, unreachable
    # objects): scripts/normalize_ws.py, before any agent sees it.
    if ((setup_rc == 0 && !interrupted)) \
        && ! env "${GITENV[@]}" python3 -I "$HERE/scripts/normalize_ws.py" "$ws" >> "$run/logs/setup.log" 2>&1; then
        echo "normalize_ws.py failed on $ws" >> "$run/logs/setup.log"
        setup_rc=1
    fi
    if ((setup_rc != 0 && !interrupted)); then
        m0_log "[$tid] setup failed (exit $setup_rc, see $run/logs/setup.log)"
    elif ((!interrupted)); then
        if [[ $AGENT_MODE == container ]]; then
            local srv=0
            server_ensure || srv=$?
            if ((srv != 0)); then   # no agent ran: leave nothing behind, the task stays pending
                LAST_STATUS=server_down LAST_FINAL=0 SERVER_FAIL=$srv
                rm -rf -- "$run"
                rm_tree "$ws"
                return 0
            fi
            m0_log "[$tid] agent: $MODEL_LABEL, timeout ${timeout_s}s"
            run_watched "the agent on $tid" "$DEADLINE" "$run/logs/run-task.log" \
                env HARNESS="$HARNESS" AGENT_TIMEOUT="$timeout_s" TAP="$TAP" TAP_PORT="$TAP_PORT" \
                LLAMA_PORT="$PORT" "$HERE/run-task.sh" "$tid" "llama/$MODEL" "$ws" "$prompt" "$run"
            agent_rc=$RC hit=$WATCH_HIT
            if ((agent_rc == 137)) && [[ -f $run/logs/run.env ]]; then   # run-task.sh itself was killed
                local cname
                cname=$(sed -n 's/^container=//p' "$run/logs/run.env" | tail -1)
                [[ -n $cname ]] && "$ENGINE" rm -f "$cname" > /dev/null 2>&1 || true
            fi
            if server_healthy; then sok=1; else sok=0; fi
            grep -q '^interrupted=1' "$run/logs/run.env" 2> /dev/null && interrupted=1
            ((hit || SUITE_INT)) && interrupted=1
        else
            agent_local "$tid" "$tdir" "$ws" "$prompt" "$run"
            agent_rc=$RC
            sok=
            ((SUITE_INT)) && interrupted=1
        fi
        case $agent_rc in 2 | 3 | 125 | 126 | 127) skip_verify=1 ;; esac
        [[ $sok == 0 ]] && skip_verify=1
        # When no further agent can start before the deadline (this verify plus a next task of
        # the same size), give the GPU back now, before the CPU-only verify, not after it. A
        # wrong estimate only costs a server restart: server_ensure starts it again.
        if ((SERVER && DEADLINE > 0)) && [[ -n $SERVER_PID ]] \
            && ! fits "${CUR_T:-0}" "$((2 * ${CUR_VEST:-60}))" "${M0_SETUP_EST:-120}"; then
            server_stop
            PLANNED_STOP=1
        fi
        if ((!interrupted && !skip_verify)); then
            m0_log "[$tid] verify (agent exit $agent_rc)"
            run_watched "verify $tid" 0 "$run/logs/verify-task.log" \
                env VERIFY_TIMEOUT="$VERIFY_TIMEOUT" VERIFY_ENV="${VERIFY_ENV_LIST[*]}" TASKS_ROOT="$M0_TASKS_ROOT" \
                "$HERE/verify-task.sh" "$tdir" "$ws" "$run"
            ((SUITE_INT)) && interrupted=1
        fi
    fi

    write_env() {
        printf '%s\n' "suite_sha256=$SUITE_SHA" "task_sha256=$task_sha" "host_ac_online=$host_ac" \
            "host_platform_profile=$host_profile" "host_gpu=$host_gpu" "host_loadavg=$host_load" \
            "attempts_toward_cap=$cap_n" > "$run/logs/suite.env"
        printf '%s\n' "task_id=$tid" "model=$MODEL_LABEL" "harness=$HARNESS" "agent_mode=$AGENT_MODE" \
            "attempt=$attempt" "stamp=$STAMP" "task_dir=$tdir" "workspace=$ws" \
            "setup_exit=$setup_rc" "setup_wall_s=$setup_wall" "agent_timeout=$timeout_s" \
            "verify_timeout=$VERIFY_TIMEOUT" "interrupted=$interrupted" "deadline_hit=$hit" \
            "server_ok_after=$sok" "server_managed=$SERVER" "server_url=$(server_url)" \
            "started_utc=$started" "finished_utc=$(m0_utc)" "prompt_sha256=${TASK_PROMPT_SHA256:-}" \
            "prompt_sha256_expected=${TASK_PROMPT_SHA256_EXPECTED:-}" "workspace_kept=$kept" >> "$run/logs/suite.env"
        if m0_py summarize_run.py --result "$run" > "$run/result.json.tmp"; then
            mv "$run/result.json.tmp" "$run/result.json"
        else
            m0_log "[$tid] warning: could not write result.json (summarize_run.py failed)"
        fi
    }
    write_env
    status=unknown final=0
    read -r status final < <(python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1])); print(r["status"], int(bool(r["final"])))' "$run/result.json" 2> /dev/null) || true
    if ((final && !KEEP_WS)); then
        rm_tree "$ws"
        rm -f "$prompt"
        kept=0
        write_env
    fi
    LAST_STATUS=$status LAST_FINAL=$final
    m0_log "[$tid] $status$([[ $final == 1 ]] || echo " (not final: re-run on resume)") -> $run/result.json"
}

# --- pre-flight: never start the GPU server for runs that cannot work ----------------------
preflight() {
    command -v "$ENGINE" > /dev/null || die "container engine $ENGINE not found (sudo apt install podman passt)" 2
    "$ENGINE" image inspect --format '{{.Id}}' "$IMAGE" > /dev/null 2>&1 \
        || die "image $IMAGE not found: build it with $HERE/prepare-image.sh $(case $HARNESS in claude | ognivo) echo "$HARNESS" ;; *) echo full ;; esac)" 2
    if [[ $AGENT_MODE == container && ${ENGINE##*/} == podman && -z ${NETWORK:-} ]]; then
        command -v pasta > /dev/null || die "pasta not found (sudo apt install passt)" 2
        local help   # capture first: `pasta --help | grep -q` can SIGPIPE pasta under pipefail
        help=$(pasta --help 2>&1 || true)
        grep -q -- '--splice-only' <<< "$help" \
            || die "this pasta has no --splice-only (passt >= 0.0~git20260120 needed): containers would get full network" 2
    fi
}
((${#PENDING[@]})) && preflight

# What judges these runs (harness, ognivo profiles and templates, the llama-server build, the
# image): the hash goes into every result.json, the file list into <model>/manifests/<hash>.json.
# Two files there = the suite changed between nights of one stamp.
SUITE_SHA=
if ((${#PENDING[@]})); then
    image_id=$("$ENGINE" image inspect --format '{{.Id}}' "$IMAGE" 2> /dev/null) || image_id=unknown
    SUITE_SHA=$(m0_py manifest.py suite "$image_id" "$MODEL_DIR/manifests" "$MODEL") \
        || { m0_log "warning: manifest.py failed: results carry no suite hash"; SUITE_SHA=; }
    [[ -n $SUITE_SHA ]] && m0_log "suite manifest $SUITE_SHA ($MODEL_DIR/manifests)"
fi

# --- main loop -----------------------------------------------------------------------------
m0_log "suite $STAMP: $MODEL_LABEL, ${#PENDING[@]} of ${#TASKS[@]} tasks to run, engine $ENGINE, image $IMAGE"
((DEADLINE)) && m0_log "deadline $(date -d "@$DEADLINE" '+%F %T %Z')"
if [[ $AGENT_MODE == container ]] && ((TAP)) && m0_port_open 127.0.0.1 "$TAP_PORT"; then
    die "the tap port 127.0.0.1:$TAP_PORT is in use (set TAP_PORT or use --no-tap)" 21
fi
EXIT=0 SERVER_FAIL=0
fits() {   # fits AGENT_TIMEOUT VERIFY_EST SETUP_EST
    ((DEADLINE == 0)) && return 0
    [[ ${M0_START_GUARD:-1} == 0 ]] && return 0
    (($(date +%s) + $3 + $1 + GUARD_OVERHEAD + $2 <= DEADLINE))
}
if [[ $AGENT_MODE == container ]] && ((${#PENDING[@]})); then
    if ! fits "${TIMEOUT:-$M0_MODEL_TIMEOUT}" "${M0_VERIFY_EST:-60}" "${M0_SETUP_EST:-120}"; then
        m0_log "not even one task fits before the deadline: nothing started"
        exit 10
    fi
    if ((SERVER)); then
        stale_server_cleanup
        srv=0
        server_start || srv=$?
        if ((srv != 0)); then
            m0_log "suite aborted: llama-server not started (exit $srv)"
            exit "$srv"
        fi
    elif ! server_healthy || ! server_check; then
        m0_log "suite aborted: no healthy llama-server serving $MODEL at $(server_url) (use --server to start one)"
        exit 22
    fi
fi

for tid in "${PENDING[@]}"; do
    ((SUITE_INT)) && break
    envs=$(m0_py plan.py task-env "$M0_TASKS_ROOT/$tid" /dev/null) || envs="TASK_AGENT_TIMEOUT= TASK_VERIFY_SECONDS= TASK_SETUP_SECONDS="
    eval "$envs"
    t=${TIMEOUT:-${TASK_AGENT_TIMEOUT:-$M0_MODEL_TIMEOUT}}
    v=${TASK_VERIFY_SECONDS:-15}
    vest=${M0_VERIFY_EST:-$((4 * v + 30 > 60 ? 4 * v + 30 : 60))}
    if ! fits "$t" "$vest" "${M0_SETUP_EST:-${TASK_SETUP_SECONDS:-120}}"; then
        m0_log "deadline: $tid (needs about $((t + vest + GUARD_OVERHEAD)) s) and the rest wait for the next night"
        EXIT=10
        break
    fi
    LAST_STATUS= LAST_FINAL=0 CUR_T=$t CUR_VEST=$vest
    run_one "$tid"
    if [[ $LAST_STATUS == server_down ]]; then
        EXIT=$SERVER_FAIL
        break
    fi
    if [[ $LAST_STATUS == infra_error && $AGENT_MODE == container ]] && ((!SUITE_INT)); then
        if fits "$t" "$vest" "${M0_SETUP_EST:-120}"; then
            m0_log "[$tid] infrastructure error: one more attempt"
            run_one "$tid"
            if [[ $LAST_STATUS == server_down ]]; then
                EXIT=$SERVER_FAIL
                break
            fi
        fi
    fi
    if [[ $LAST_STATUS == interrupted ]] && ((!SUITE_INT)); then
        EXIT=10   # stopped by the deadline
        break
    fi
done
server_stop

for d in "$MODEL_DIR"/*/; do
    if [[ -f $d/result.json ]]; then
        python3 -I -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1])), ensure_ascii=False))' "$d/result.json" || true
    fi
done > "$MODEL_DIR/results.jsonl"
{ m0_py summarize_run.py --table "$STAMP_DIR" || true; } | while IFS= read -r l; do m0_log "$l"; done
if ((SUITE_INT)); then
    m0_log "suite interrupted"
    exit 130
fi
if ((EXIT == 0)); then
    m0_py plan.py pending "$MODEL_DIR" "${TASKS[@]}" > "$taskf"
    if [[ -s $taskf ]]; then
        m0_log "not final yet (re-run them with --stamp $STAMP): $(tr '\n' ' ' < "$taskf")"
        EXIT=11
    fi
fi
m0_log "suite $STAMP $MODEL_LABEL done (exit $EXIT): $STAMP_DIR"
exit "$EXIT"
exit
}
