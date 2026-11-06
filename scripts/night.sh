#!/usr/bin/env bash
# The nightly T2-mini measurement (docs/architecture.md §11) in the GPU window, 03:00-06:00 by
# default. For each model of the plan: the pinned llama-server, started from its profile and
# checked against it the way the M0 harness does (evals/m0-baseline/harness/scripts/
# llama_cmd.py), T2-mini with paired seeds, the server stopped again. Then the morning report,
# with the change since the night before.
#
#   scripts/night.sh [options]
#
# It first builds the committed HEAD in a copy of its own, so the night measures what was
# committed, not the working tree; the copy also gives the scenarios, the profiles and
# llama_cmd.py. Then it waits for the window. From --lock-lead hours before the window until
# its end it holds a systemd-inhibit lock against sleep and the lid switch: the owner closes
# the lid around midnight, and logind suspends on AC too.
#
# Before each model the GPU must have NEED_MIB free (vLLM or ollama may hold it) and the port
# must be free; it waits up to 30 min for them. A model with less than 15 min of the window
# left waits for another night. T2 stops a minute before the window ends, and what it ran
# stays in the report.
#
# Results: ~/.local/share/ognivo/night/<date of the window>/ (OGNIVO_NIGHT_DIR), with night.log,
# night.json, report.md, and one T2 directory per model. A run with --now is named by its
# start, <date>-<HHMM>: T2 resumes what a directory holds, so a second run of the day must
# not land in the first one's.
#
# Options:
#   --window HH:MM-HH:MM  the GPU window (default 03:00-06:00)
#   --models "A B C"      profiles, run in this order (default: gpt-oss-20b gemma-4-26b-a4b
#                         qwen3.8-27b qwen3.6-35b-a3b); ID+native runs that profile on the Native path,
#                         on a server of its own, in a directory of that name, and
#                         ID+native+priors with gpt-oss's own tools there; +mcp-search
#                         and +mcp-declare give the mcp scenarios' sessions their
#                         server's tools that way instead of as a session decides
#   --seeds LIST          (default 1,2,3)
#   --only TEXT           only the scenarios whose id holds TEXT or whose category it is
#   --lock-lead HOURS     take the sleep lock this long before the window (default 8)
#   --max-wait SECONDS    exit (0) when the window opens later than that
#   --no-inhibit          hold no sleep lock
#   --now MINUTES         run now, in a window of that many minutes
#   --dry-run             print the plan and exit
#
# Env: OGNIVO_NIGHT_DIR, OGNIVO_NIGHT_CACHE (the copy and its build, default
#      ~/.cache/ognivo-night), NEED_MIB (default 20000), LLAMA_SERVER_BIN (for tests),
#      CARGO.
# Exit: 0 done or nothing to do; 3 another night is running; 2 usage; 1 error.
set -euo pipefail
{   # parsed whole before it runs: editing this file cannot derail a running night
REPO=$(cd "$(dirname "$(realpath "$0")")/.." && pwd)
OUT_ROOT=${OGNIVO_NIGHT_DIR:-$HOME/.local/share/ognivo/night}
CACHE=${OGNIVO_NIGHT_CACHE:-$HOME/.cache/ognivo-night}
NEED_MIB=${NEED_MIB:-20000}
CARGO=${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}
PORT=8090
MIN_LEFT=$((15 * 60))
WAIT_FOR_GPU=$((30 * 60))
SERVER_LOAD_TIMEOUT=600

WINDOW=03:00-06:00 MODELS="gpt-oss-20b gemma-4-26b-a4b qwen3.8-27b qwen3.6-35b-a3b" SEEDS=1,2,3
LOCK_LEAD_H=8 MAX_WAIT=-1 INHIBIT=1 NOW_MIN= DRY_RUN=0 ONLY=
usage() {
    sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0" >&2
    exit 2
}
while (($#)); do
    case $1 in
        --window) WINDOW=${2:?}; shift ;;
        --models) MODELS=${2:?}; shift ;;
        --seeds) SEEDS=${2:?}; shift ;;
        --only) ONLY=${2:?}; shift ;;
        --lock-lead) LOCK_LEAD_H=${2:?}; shift ;;
        --max-wait) MAX_WAIT=${2:?}; shift ;;
        --no-inhibit) INHIBIT=0 ;;
        --now) NOW_MIN=${2:?}; shift ;;
        --dry-run) DRY_RUN=1 ;;
        -h | --help) usage ;;
        *) echo "night.sh: unknown argument $1 (see --help)" >&2; exit 2 ;;
    esac
    shift
done
[[ $WINDOW =~ ^([0-2][0-9]:[0-5][0-9])-([0-2][0-9]:[0-5][0-9])$ ]] || { echo "night.sh: bad --window $WINDOW" >&2; exit 2; }
W_START=${BASH_REMATCH[1]} W_END=${BASH_REMATCH[2]}
[[ $SEEDS =~ ^[0-9]+(,[0-9]+)*$ ]] || { echo "night.sh: bad --seeds $SEEDS" >&2; exit 2; }
[[ $LOCK_LEAD_H =~ ^[0-9]+$ && $MAX_WAIT =~ ^(-1|[0-9]+)$ ]] || { echo "night.sh: bad --lock-lead or --max-wait" >&2; exit 2; }
[[ -z $NOW_MIN || $NOW_MIN =~ ^[0-9]+$ ]] || { echo "night.sh: bad --now" >&2; exit 2; }

# --- the window: the next one that has not ended ----------------------------------------------
NOW=$(date +%s)
if [[ -n $NOW_MIN ]]; then
    DATE=$(date -d "@$NOW" +%F-%H%M) START=$NOW END=$((NOW + NOW_MIN * 60))
else
    window_on() {   # window_on DAY -> START END
        local s e
        s=$(date -d "$1 $W_START" +%s)
        e=$(date -d "$1 $W_END" +%s)
        ((e > s)) || e=$(date -d "$1 $W_END next day" +%s)
        echo "$s $e"
    }
    DATE=$(date +%F)
    read -r START END < <(window_on "$DATE")
    if ((NOW >= END)); then
        DATE=$(date -d tomorrow +%F)
        read -r START END < <(window_on "$DATE")
    fi
fi
NIGHT=$OUT_ROOT/$DATE
SHA=$(git -C "$REPO" rev-parse HEAD)
# the window as it is tonight (--now gives one of its own)
SHOWN="$(date -d "@$START" '+%H:%M')-$(date -d "@$END" '+%H:%M')"

if ((DRY_RUN)); then
    echo "night $DATE: window $(date -d "@$START" '+%F %H:%M')-$(date -d "@$END" '+%H:%M')"
    echo "commit $SHA; models: $MODELS; seeds: $SEEDS${ONLY:+; only: $ONLY}"
    echo "results: $NIGHT"
    exit 0
fi

mkdir -p "$OUT_ROOT"
exec 9> "$OUT_ROOT/.lock"
flock -n 9 || { echo "night.sh: another night is running" >&2; exit 3; }
mkdir -p "$NIGHT"
LOG=$NIGHT/night.log
log() { printf '%s %s\n' "$(date '+%F %T')" "$*" | tee -a "$LOG" >&2; }
NOTES=()
note() { NOTES+=("$1"); log "note: $1"; }

# --- what stays behind ---------------------------------------------------------------------
SERVER_PID= LOCK_PID= T2_PID=
server_stop() {
    [[ -n $SERVER_PID ]] || return 0
    kill -TERM "$SERVER_PID" 2> /dev/null || true
    local i
    for ((i = 0; i < 60; i++)); do kill -0 "$SERVER_PID" 2> /dev/null || break; sleep 1; done
    kill -KILL "$SERVER_PID" 2> /dev/null || true
    wait "$SERVER_PID" 2> /dev/null || true
    SERVER_PID=
}
lock_release() {
    [[ -n $LOCK_PID ]] || return 0
    pkill -P "$LOCK_PID" 2> /dev/null || true
    kill "$LOCK_PID" 2> /dev/null || true
    wait "$LOCK_PID" 2> /dev/null || true
    LOCK_PID=
}
write_night() {
    python3 -I - "$NIGHT/night.json" "$DATE" "$SHA" "$SHOWN" "$SEEDS" "$MODELS" "${NOTES[@]}" << 'PY'
import json, sys
path, date, sha, window, seeds, models, *notes = sys.argv[1:]
json.dump({"date": date, "commit": sha, "window": window, "seeds": seeds,
           "models": models.split(), "notes": notes}, open(path, "w"), indent=1, ensure_ascii=False)
PY
}
finish() {
    local rc=$?
    [[ -n $T2_PID ]] && kill -TERM "$T2_PID" 2> /dev/null || true
    server_stop
    lock_release
    write_night || true
    exit "$rc"
}
trap finish EXIT
trap 'log "stopped by a signal"; exit 1' TERM INT

lock_hold_until() {   # lock_hold_until EPOCH
    lock_release
    ((INHIBIT)) || return 0
    local secs what
    secs=$(($1 - $(date +%s)))
    ((secs > 0)) || return 0
    if ! command -v systemd-inhibit > /dev/null; then
        log "note: no systemd-inhibit: keep the machine from sleeping yourself"
        return 0
    fi
    for what in sleep:idle:handle-lid-switch sleep:idle; do
        systemd-inhibit --what="$what" --who=ognivo --why=probe --mode=block true 2> /dev/null || continue
        # the lock dies with this script (pdeathsig) and times out by itself; 9>&-: the night
        # lock must not live on in it
        setpriv --pdeathsig TERM -- systemd-inhibit --what="$what" --who=ognivo \
            --why="ognivo night ($SHOWN)" --mode=block sleep "$secs" < /dev/null > /dev/null 2>&1 9>&- &
        LOCK_PID=$!
        log "holding a systemd-inhibit lock ($what) until $(date -d "@$1" '+%F %H:%M')"
        [[ $what == sleep:idle ]] && log "note: no lid-switch lock here: closing the lid will still suspend the machine"
        return 0
    done
    log "note: no systemd-inhibit lock allowed here: keep the machine from sleeping yourself"
}

# --- the build: the committed HEAD, in a copy of its own ------------------------------------
log "night $DATE: window $(date -d "@$START" '+%F %H:%M')-$(date -d "@$END" '+%H:%M'), commit $SHA, models: $MODELS, seeds: $SEEDS"
[[ -n $(git -C "$REPO" status --porcelain --untracked-files=no) ]] \
    && log "note: the working tree has uncommitted changes; the night measures $SHA without them"
# one copy per commit, never changed: another night or a test may be running from it
SRC=$CACHE/src-${SHA:0:12}
if [[ ! -e $SRC/.complete ]]; then
    rm -rf "$SRC"
    mkdir -p "$SRC"
    git -C "$REPO" archive "$SHA" | tar -x -C "$SRC"
    touch "$SRC/.complete"
fi
find "$CACHE" -maxdepth 1 -name 'src-*' -mtime +7 -exec rm -rf {} + 2> /dev/null || true
export CARGO_TARGET_DIR=$CACHE/target
log "building $SHA in $SRC"
if ! (cd "$SRC" && "$CARGO" build --release --locked --offline -q -p ognivo-evals --bin t2 --bin night-report) >> "$LOG" 2>&1; then
    note "the build of $SHA failed (see night.log)"
    exit 1
fi
# the night's own binaries: the shared target directory gets the next build
mkdir -p "$NIGHT/bin"
cp "$CARGO_TARGET_DIR/release/t2" "$CARGO_TARGET_DIR/release/night-report" "$NIGHT/bin/"
T2=$NIGHT/bin/t2
REPORT=$NIGHT/bin/night-report
export OGNIVO_REPO=$SRC M0_MODELS_JSON=$SRC/evals/m0-baseline/harness/config/models.json
llama_cmd() { python3 -I "$SRC/evals/m0-baseline/harness/scripts/llama_cmd.py" "$@"; }
for m in $MODELS; do
    llama_cmd argv "${m%%+*}" --port "$PORT" > /dev/null 2>> "$LOG" || note "$m: no server command line (llama_cmd.py argv ${m%%+*}: model file, template or profile missing)"
done

# --- the window ----------------------------------------------------------------------------
# by the wall clock, a minute at a time: sleep(1) does not count the time the machine is
# suspended, so one long sleep would end late after a suspend
sleep_until() {
    local left
    while left=$(($1 - $(date +%s))) && ((left > 0)); do
        sleep $((left < 60 ? left : 60))
    done
}
NOW=$(date +%s)
if ((START > NOW)); then
    if ((MAX_WAIT >= 0 && START - NOW > MAX_WAIT)); then
        log "the window opens in $(((START - NOW) / 60)) min, later than --max-wait: nothing to do"
        exit 0
    fi
    LEAD=$((START - LOCK_LEAD_H * 3600))
    if ((LEAD > NOW)); then
        log "waiting until $(date -d "@$LEAD" '+%F %H:%M') to take the sleep lock"
        sleep_until "$LEAD"
    fi
    lock_hold_until "$END"
    NOW=$(date +%s)
    if ((START > NOW)); then
        log "waiting for the window ($(date -d "@$START" '+%F %H:%M'))"
        sleep_until "$START"
    fi
else
    lock_hold_until "$END"
fi

gpu_free_mib() {
    nvidia-smi --query-gpu=memory.total,memory.used --format=csv,noheader,nounits 2> /dev/null \
        | awk -F', *' 'NR == 1 { print $1 - $2 }'
}
port_open() { (exec 3<> "/dev/tcp/127.0.0.1/$PORT") 2> /dev/null; }
healthy() { curl -fsS -m 5 -o /dev/null "http://127.0.0.1:$PORT/health" 2> /dev/null; }

# Waits for the GPU memory and the port: 0 when both are free.
wait_free() {
    local until=$(($(date +%s) + WAIT_FOR_GPU)) free
    while :; do
        free=$(gpu_free_mib)
        if [[ $free =~ ^[0-9]+$ ]] && ((free >= NEED_MIB)) && ! port_open; then
            return 0
        fi
        (($(date +%s) + 60 < until && $(date +%s) + 60 < END - MIN_LEFT)) || {
            log "GPU free ${free:-?} MiB (needs $NEED_MIB), port $PORT $(port_open && echo busy || echo free)"
            nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader >> "$LOG" 2>&1 || true
            return 1
        }
        sleep 60
    done
}

# Starts the model's server and checks it against its profile: 0 when it serves.
server_start() {   # server_start MODEL DIR
    local m=$1 dir=$2 t0 v
    local -a argv=() unset=()
    llama_cmd argv "$m" --port "$PORT" > "$dir/llama-server.argv" 2>> "$LOG" || return 1
    mapfile -d '' argv < "$dir/llama-server.argv"
    for v in $(compgen -e); do
        [[ $v == LLAMA_ARG_* ]] && unset+=(-u "$v")   # llama-server reads LLAMA_ARG_* as flags
    done
    log "$m: starting llama-server (log: $dir/llama-server.log)"
    env "${unset[@]}" setpriv --pdeathsig TERM -- setsid "${argv[@]}" \
        >> "$dir/llama-server.log" 2>&1 < /dev/null 9>&- &
    SERVER_PID=$!
    t0=$(date +%s)
    until healthy; do
        if ! kill -0 "$SERVER_PID" 2> /dev/null; then
            wait "$SERVER_PID" 2> /dev/null || true
            SERVER_PID=
            log "$m: llama-server exited while loading:"
            tail -n 10 "$dir/llama-server.log" >> "$LOG"
            return 1
        fi
        (($(date +%s) - t0 < SERVER_LOAD_TIMEOUT)) || { log "$m: not healthy after $SERVER_LOAD_TIMEOUT s"; server_stop; return 1; }
        sleep 2
    done
    log "$m: healthy after $(($(date +%s) - t0)) s"
    curl -fsS -m 15 "http://127.0.0.1:$PORT/props" -o "$dir/server-props.json" 2>> "$LOG" || { server_stop; return 1; }
    curl -fsS -m 15 "http://127.0.0.1:$PORT/v1/models" -o "$dir/server-models.json" 2> /dev/null || : > "$dir/server-models.json"
    if ! llama_cmd check "$m" "$dir/server-props.json" "$dir/server-models.json" > "$dir/server.json" 2>> "$LOG"; then
        log "$m: the server does not match the profile (see $dir/server.json)"
        server_stop
        return 1
    fi
}

# --- the models ----------------------------------------------------------------------------
grep -qx 1 /sys/class/power_supply/ADP*/online 2> /dev/null || log "warning: not on AC power"
for m in $MODELS; do
    dir=$NIGHT/$m
    mkdir -p "$dir"
    left=$((END - $(date +%s)))
    if ((left < MIN_LEFT)); then
        note "$m: not started, $((left / 60)) min of the window left"
        continue
    fi
    if ! wait_free; then
        note "$m: not started, the GPU or port $PORT stayed busy"
        continue
    fi
    path=()
    [[ $m == *+native* ]] && path+=(--native)
    [[ $m == *+priors* ]] && path+=(--priors)
    [[ $m == *+mcp-search* ]] && path+=(--mcp-tools search)
    [[ $m == *+mcp-declare* ]] && path+=(--mcp-tools declare)
    [[ -n $ONLY ]] && path+=(--only "$ONLY")
    if ! server_start "${m%%+*}" "$dir"; then
        note "$m: llama-server did not start or does not match the profile"
        continue
    fi
    budget=$((END - $(date +%s) - 60))
    log "$m: T2-mini, seeds $SEEDS, at most $((budget / 60)) min"
    rc=0
    timeout --signal=TERM --kill-after=30 "$budget" \
        "$T2" --url "http://127.0.0.1:$PORT" --profile "${m%%+*}" "${path[@]}" --out "$dir" --seeds "$SEEDS" \
        >> "$dir/t2.log" 2>&1 < /dev/null 9>&- &
    T2_PID=$!
    wait "$T2_PID" || rc=$?
    T2_PID=
    case $rc in
        0 | 1) log "$m: T2-mini done: $(grep -E '^[0-9]+ runs, ' "$dir/t2.log" | tail -1)" ;;
        124 | 137) note "$m: T2-mini stopped at the end of the window" ;;
        *) note "$m: T2-mini failed (exit $rc, see $dir/t2.log)" ;;
    esac
    server_stop
    nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader > "$dir/gpu-after.txt" 2>&1 || true
done
lock_release

# --- the report ----------------------------------------------------------------------------
write_night
prev=
for d in "$OUT_ROOT"/20??-??-??*; do
    [[ -d $d && $(basename "$d") < $DATE ]] && compgen -G "$d/*/summary.json" > /dev/null && prev=$d
done
if "$REPORT" "$NIGHT" ${prev:+--previous "$prev"} > "$NIGHT/report.md" 2>> "$LOG"; then
    log "report: $NIGHT/report.md"
    cat "$NIGHT/report.md" >> "$LOG"
    if command -v notify-send > /dev/null; then
        timeout 5 notify-send "ognivo night $DATE" "The report is in $NIGHT/report.md" 2> /dev/null || true
    fi
else
    note "no report (see night.log)"
fi
exit 0
}
