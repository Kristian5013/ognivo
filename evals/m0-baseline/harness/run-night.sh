#!/usr/bin/env bash
# The M0 baseline night plan inside the GPU window (default 03:00-06:00 local time):
#   night 1: gpt-oss-20b, then gemma-4-26b-a4b, over the selected tasks
#   night 2: qwen3.8-27b
# A campaign is one runs/<UTC stamp>/ that every night of the plan writes into. Each night
# first finishes what earlier nights left, then its own phase (night k may run phases 1..k).
# It stops cleanly at the end of the window: no agent starts that could not finish before it,
# an agent still running then is stopped (its run is re-done next night) and the llama-server
# is shut down. The next night resumes the remaining runs.
#
#   run-night.sh --new [options]      start a campaign (frozen task list), then run tonight
#   run-night.sh --new --create-only  only create the campaign (e.g. for a timer), run nothing
#   run-night.sh [options]            continue the newest unfinished campaign
#   run-night.sh --campaign STAMP     continue that campaign
#   run-night.sh --status [--campaign STAMP]   show the progress and exit
#
# A campaign runs from a frozen copy: --new copies the harness, the campaign's tasks with
# tasks/_common and the effective config/models.json into runs/<stamp>/frozen/ and tags the image
# as localhost/m0-opencode:campaign-<stamp>; every later night re-runs this script from that copy,
# so edits to the live tree cannot change a running campaign. --refreeze (with --campaign or the
# newest campaign) copies the live tree again: a deliberate change, which result.json's
# versions.suite_sha256/task_sha256 then show.
#
# Options:
#   --until-done           keep going night after night until the plan is complete
#   --window HH:MM-HH:MM   the GPU window (default 03:00-06:00)
#   --plan SPEC            phases separated by ';', models by ',' (default
#                          "gpt-oss-20b,gemma-4-26b-a4b;qwen3.8-27b"; only with --new)
#   --tasks "ID ..."       (only with --new; default: every task.json with "role": "selected")
#   --greedy               a night may also start later phases when it has time left
#   --no-wait              outside the window: exit instead of sleeping until it opens
#   --max-wait SECONDS     exit (0) when the window opens later than that, e.g. a timer that fired
#                          on resume at 08:00 must not wait 19 h (README's timer uses 900)
#   --lock-lead HOURS      take the sleep lock this long before the window (default 8: from 19:00
#                          for 03:00); before that, closing the lid suspends the laptop as usual
#   --no-inhibit           do not hold a systemd-inhibit sleep lock
#   --dry-run              print what tonight would run (window, phases, suites) and exit
#   -- ARGS                passed to every run-suite.sh call (e.g. --no-tap --keep-ws --timeout 600)
# Test knobs: --night-date YYYY-MM-DD (the date that identifies the night),
#             --deadline-in SECONDS (the window is open now and ends that many seconds from now)
#
# Exit: 0 the night (or the campaign) is done, or there is nothing to do; 3 another night is
# running; 2 usage; 1 error.
# The laptop must stay awake: from --lock-lead hours before the window until it ends, a
# background systemd-inhibit holds "sleep:idle:handle-lid-switch" when logind allows it (a plain
# "sleep" lock does not stop the lid switch: logind's LidSwitchIgnoreInhibited=yes). The lock is
# never held across days. A pre-flight (image, pasta, model files, ports, GPU, AC, lid, load,
# disk) is logged at launch and again when the window opens; a GPU or port still busy then is
# waited for up to 30 min (GPU_RETRIES x GPU_RETRY_SLEEP) before the night gives up.
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
    exit "${2:-1}"
}

NEW=0 CAMPAIGN= UNTIL_DONE=0 WINDOW=03:00-06:00 PLAN="gpt-oss-20b,gemma-4-26b-a4b;qwen3.8-27b"
TASKS_ARG= GREEDY=0 WAIT=1 INHIBIT=1 DRY_RUN=0 STATUS=0 NIGHT_DATE= DEADLINE_IN= CREATE_ONLY=0
MAX_WAIT=-1 LOCK_LEAD_H=8 REFREEZE=0
SUITE_ARGS=() PASS=()   # PASS: the options a re-exec from the campaign's frozen copy gets again
while (($#)); do
    case $1 in
        --new) NEW=1 ;;
        --create-only) CREATE_ONLY=1 ;;
        --campaign) CAMPAIGN=${2:?}; shift ;;
        --status) STATUS=1 ;;
        --refreeze) REFREEZE=1 ;;
        --until-done) UNTIL_DONE=1; PASS+=("$1") ;;
        --window) WINDOW=${2:?}; PASS+=("$1" "$2"); shift ;;
        --plan) PLAN=${2:?}; shift ;;
        --tasks) TASKS_ARG=${2:?}; shift ;;
        --greedy) GREEDY=1; PASS+=("$1") ;;
        --no-wait) WAIT=0; PASS+=("$1") ;;
        --max-wait) MAX_WAIT=${2:?}; PASS+=("$1" "$2"); shift ;;
        --lock-lead) LOCK_LEAD_H=${2:?}; PASS+=("$1" "$2"); shift ;;
        --no-inhibit) INHIBIT=0; PASS+=("$1") ;;
        --dry-run) DRY_RUN=1 ;;
        --night-date) NIGHT_DATE=${2:?}; PASS+=("$1" "$2"); shift ;;
        --deadline-in) DEADLINE_IN=${2:?}; PASS+=("$1" "$2"); shift ;;
        --)
            shift
            SUITE_ARGS=("$@")
            break
            ;;
        -h | --help) usage ;;
        *) die "unknown argument $1 (see --help)" 2 ;;
    esac
    shift
done
[[ $WINDOW =~ ^([0-2][0-9]:[0-5][0-9])-([0-2][0-9]:[0-5][0-9])$ ]] || die "bad --window $WINDOW" 2
W_START=${BASH_REMATCH[1]} W_END=${BASH_REMATCH[2]}
[[ -z $DEADLINE_IN || $DEADLINE_IN =~ ^[0-9]+$ ]] || die "bad --deadline-in" 2
[[ $MAX_WAIT =~ ^(-1|[0-9]+)$ ]] || die "bad --max-wait" 2
[[ $LOCK_LEAD_H =~ ^[0-9]+$ ]] || die "bad --lock-lead" 2
[[ -z $NIGHT_DATE || $NIGHT_DATE =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die "bad --night-date" 2
((NEW && ${#CAMPAIGN})) && die "--new and --campaign exclude each other" 2
((CREATE_ONLY && !NEW)) && die "--create-only needs --new" 2
((CREATE_ONLY)) && INHIBIT=0

if ((STATUS)); then
    if [[ -z $CAMPAIGN ]]; then
        CAMPAIGN=$(m0_py plan.py campaign-latest "$M0_RUNS_ROOT") || {
            ls -1d "$M0_RUNS_ROOT"/*/campaign.json 2> /dev/null | sed 's|/campaign.json$||; s|.*/|finished campaign: |'
            echo "no unfinished campaign under $M0_RUNS_ROOT"
            exit 0
        }
    fi
    m0_py plan.py campaign-status "$M0_RUNS_ROOT/$CAMPAIGN" || true
    m0_py summarize_run.py --table "$M0_RUNS_ROOT/$CAMPAIGN"
    exit 0
fi

# --- sleep lock: from LOCK_LEAD_H before the window until it ends, never across days ---------
# A background systemd-inhibit holds the lock (its child sleep times it out even if this script
# is killed; --pdeathsig drops it at once). handle-lid-switch is needed as well: with logind's
# default LidSwitchIgnoreInhibited=yes a "sleep" lock does not stop HandleLidSwitch=suspend.
LOCK_PID= LOCK_TRIED=0
lock_release() {
    if [[ -n $LOCK_PID ]]; then
        pkill -P "$LOCK_PID" 2> /dev/null || true
        kill "$LOCK_PID" 2> /dev/null || true
        wait "$LOCK_PID" 2> /dev/null || true
        LOCK_PID=
    fi
    LOCK_TRIED=0
}
lock_hold_until() {   # lock_hold_until EPOCH
    lock_release
    LOCK_TRIED=1
    ((INHIBIT && !DRY_RUN)) || return 0
    local secs what pdeath=()
    secs=$(($1 - $(date +%s)))
    ((secs > 0)) || return 0
    if ! command -v systemd-inhibit > /dev/null; then
        m0_log "note: no systemd-inhibit: keep the machine from sleeping yourself"
        return 0
    fi
    command -v setpriv > /dev/null && pdeath=(setpriv --pdeathsig TERM)
    for what in sleep:idle:handle-lid-switch sleep:idle; do
        systemd-inhibit --what="$what" --who=m0-baseline --why=probe --mode=block true 2> /dev/null || continue
        # 8>&- 9>&-: the night/suite locks must not live on in this long-lived child
        "${pdeath[@]}" systemd-inhibit --what="$what" --who=m0-baseline --why="M0 baseline night ($WINDOW)" \
            --mode=block sleep "$secs" < /dev/null > /dev/null 2>&1 8>&- 9>&- &
        LOCK_PID=$!
        m0_log "holding a systemd-inhibit lock ($what) until $(date -d "@$1" '+%F %H:%M')"
        [[ $what == sleep:idle ]] \
            && m0_log "note: no lid-switch lock here: closing the lid will still suspend the machine"
        return 0
    done
    m0_log "note: no systemd-inhibit lock (not allowed here); keep the machine from sleeping yourself"
}

# --- pre-flight: the reasons a night would fail at 03:00, checked while someone can still act --
preflight() {   # preflight WHEN MODEL... -> logs one PRE-FLIGHT line (and a desktop notice on failure)
    local when=$1 probs=() warns=() m port help out free
    shift
    local engine=${ENGINE:-podman} image=${IMAGE:-$(m0_image full)}
    "$engine" image inspect "$image" > /dev/null 2>&1 || probs+=("no image $image")
    help=$(pasta --help 2>&1 || true)
    grep -q -- '--splice-only' <<< "$help" || probs+=("pasta without --splice-only")
    for m in "$@"; do
        m0_py llama_cmd.py argv "$m" > /dev/null 2>&1 || probs+=("$m: model file, template or profile missing (llama_cmd.py argv $m)")
    done
    port=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("port", 8090))' "$M0_MODELS_JSON" 2> /dev/null) || port=8090
    m0_port_open 127.0.0.1 "$port" && probs+=("127.0.0.1:$port is in use")
    m0_port_open 127.0.0.1 "$((port + 1))" && probs+=("127.0.0.1:$((port + 1)) (the tap) is in use")
    if ! out=$(timeout -k 5 30 "${NVIDIA_SMI:-nvidia-smi}" --query-compute-apps=pid,process_name,used_memory --format=csv,noheader 2>&1); then
        probs+=("nvidia-smi failed: $(head -1 <<< "$out")")
    elif [[ -n $(sed '/^[[:space:]]*$/d' <<< "$out") ]]; then
        probs+=("GPU in use: $(tr '\n' ';' <<< "$out")")
    fi
    grep -qx 1 /sys/class/power_supply/ADP*/online 2> /dev/null || warns+=("not on AC power (the GPU power limit and so every timeout differ)")
    grep -qs closed /proc/acpi/button/lid/*/state && warns+=("the lid is closed (higher temperatures, GPU throttling)")
    awk '{ exit !($1 > 4) }' /proc/loadavg 2> /dev/null && warns+=("host load $(cut -d' ' -f1 /proc/loadavg): the agent's builds compete for CPU")
    free=$(df -Pk -- "$M0_ROOT" 2> /dev/null | awk 'NR == 2 { print int($4 / 1048576) }')
    [[ $free =~ ^[0-9]+$ ]] && ((free < 100)) && probs+=("only $free GB free under $M0_ROOT")
    local w
    for w in "${warns[@]}"; do m0_log "pre-flight ($when) warning: $w"; done
    if ((${#probs[@]})); then
        m0_log "PRE-FLIGHT ($when) FAILED: $(printf '%s; ' "${probs[@]}")"
        # a desktop notice for the owner; never from the test knobs (--deadline-in) or M0_NOTIFY=0
        if [[ -z $DEADLINE_IN && ${M0_NOTIFY:-1} == 1 ]] && command -v notify-send > /dev/null; then
            timeout 5 notify-send -u critical "M0 baseline night" \
                "Pre-flight ($when) failed: $(printf '%s; ' "${probs[@]}")" 2> /dev/null || true
        fi
        return 1
    fi
    m0_log "PRE-FLIGHT ($when) OK"
}

if ((!DRY_RUN)); then
    mkdir -p "$M0_RUNS_ROOT"
    exec 8> "$M0_RUNS_ROOT/.night.lock"
    flock -n 8 || die "another run-night.sh holds $M0_RUNS_ROOT/.night.lock" 3
fi

tmpf=$(mktemp)
trap 'rm -f "$tmpf"; lock_release' EXIT
NIGHT_INT=0 SUITE_PID=
on_night_signal() {   # forward to the running suite: it stops its agent and the llama-server
    NIGHT_INT=1
    [[ -n $SUITE_PID ]] && kill -TERM "$SUITE_PID" 2> /dev/null
    return 0
}
trap on_night_signal TERM INT HUP
if ((NEW)); then
    if [[ -n $TASKS_ARG ]]; then
        # shellcheck disable=SC2086
        m0_py plan.py tasks "$M0_TASKS_ROOT" $TASKS_ARG > "$tmpf" || die "invalid --tasks"
    else
        m0_py plan.py tasks "$M0_TASKS_ROOT" > "$tmpf" || die "cannot list the selected tasks"
    fi
    mapfile -t TASKS < "$tmpf"
    ((${#TASKS[@]})) || die "no tasks for the campaign"
    CAMPAIGN=$(date -u +%Y%m%dT%H%M%SZ)
    if ((DRY_RUN)); then
        echo "new campaign $CAMPAIGN (not created: --dry-run)"
        echo "plan    $PLAN"
        echo "tasks   ${#TASKS[@]}: ${TASKS[*]}"
    else
        m0_py plan.py campaign-new "$M0_RUNS_ROOT/$CAMPAIGN" --plan "$PLAN" --window "$WINDOW" \
            --tasks "${TASKS[@]}" || die "cannot create the campaign"
        m0_log "campaign $CAMPAIGN created: plan \"$PLAN\", ${#TASKS[@]} tasks"
    fi
elif [[ -z $CAMPAIGN ]]; then
    # exit 0: a timer that fires after the campaign is complete has nothing to do, not failed
    CAMPAIGN=$(m0_py plan.py campaign-latest "$M0_RUNS_ROOT") || {
        m0_log "no unfinished campaign under $M0_RUNS_ROOT: nothing to do (start one with --new)"
        exit 0
    }
fi
CDIR=$M0_RUNS_ROOT/$CAMPAIGN
if [[ ! -f $CDIR/campaign.json ]] && ! ((NEW && DRY_RUN)); then die "no campaign $CDIR"; fi

# freeze_campaign: the harness, the campaign's tasks and the effective models.json into
# $CDIR/frozen; the image is pinned by a campaign tag (not for --harness claude runs).
freeze_campaign() {
    local fz=$CDIR/frozen tmp t ids=() img tag
    tmp=$(mktemp -d "$CDIR/.frozen.XXXXXX")
    cp -a "$M0_HARNESS" "$tmp/harness"
    cp "$M0_MODELS_JSON" "$tmp/harness/config/models.json"
    mkdir -p "$tmp/tasks"
    [[ -d $M0_TASKS_ROOT/_common ]] && cp -a "$M0_TASKS_ROOT/_common" "$tmp/tasks/"
    mapfile -t ids < <(python3 -I -c 'import json,sys; print("\n".join(json.load(open(sys.argv[1]))["tasks"]))' "$CDIR/campaign.json")
    for t in "${ids[@]}"; do cp -a "$M0_TASKS_ROOT/$t" "$tmp/tasks/"; done
    find "$tmp" -name __pycache__ -prune -exec rm -rf {} + 2> /dev/null || true
    img=${IMAGE:-$(m0_image full)} tag=
    if [[ " ${SUITE_ARGS[*]} " != *" --harness "* ]] \
        && "${ENGINE:-podman}" tag "$img" "$M0_IMAGE_REPO:campaign-$CAMPAIGN" > /dev/null 2>&1; then
        tag=$M0_IMAGE_REPO:campaign-$CAMPAIGN
        echo "$tag" > "$tmp/image"
    fi
    rm -rf "$fz"
    mv "$tmp" "$fz"
    m0_log "campaign $CAMPAIGN frozen: $fz (${#ids[@]} tasks${tag:+, image $tag})"
}
if ((!DRY_RUN)) && { ((NEW)) || ((REFREEZE)); }; then
    freeze_campaign
    ((CREATE_ONLY)) && { echo "$CAMPAIGN"; exit 0; }
fi
FROZEN=$CDIR/frozen/harness
if ((!DRY_RUN)) && [[ -x $FROZEN/run-night.sh && $M0_HARNESS != "$FROZEN" && -z ${M0_FROZEN_EXEC:-} ]]; then
    # every night of a campaign runs the campaign's own copy; the data roots stay where they are
    export M0_FROZEN_EXEC=1 M0_TASKS_ROOT=$CDIR/frozen/tasks M0_MODELS_JSON=$FROZEN/config/models.json
    [[ -f $CDIR/frozen/image ]] && export IMAGE=$(< "$CDIR/frozen/image")
    rm -f "$tmpf"
    m0_log "running from the campaign's frozen copy $FROZEN${IMAGE:+ (image $IMAGE)}"
    exec 8>&-   # the night lock: the frozen copy takes it again
    exec "$FROZEN/run-night.sh" --campaign "$CAMPAIGN" "${PASS[@]}" -- "${SUITE_ARGS[@]}"
fi
export M0_LOG_FILE=$CDIR/night.log
((DRY_RUN)) && unset M0_LOG_FILE

# window_next -> W_S W_E W_DATE: the next window that has not ended yet (it may be open now).
window_next() {
    local now off day s e dur
    now=$(date +%s)
    for off in -1 0 1 2; do
        day=$(date -d "today $off day" +%F)
        s=$(date -d "$day $W_START" +%s)
        e=$(date -d "$day $W_END" +%s)
        ((e > s)) || e=$(date -d "$day $W_END 1 day" +%s)   # a window across midnight
        dur=$((e - s))
        ((dur > 0)) || continue
        if ((e > now)); then
            W_S=$s W_E=$e W_DATE=$day
            return 0
        fi
    done
    return 1
}

tonight() {   # tonight DATE -> lines "MODEL ID..." into $tmpf
    local g=()
    ((GREEDY)) && g=(--greedy)
    if [[ -f $CDIR/campaign.json ]]; then
        m0_py plan.py campaign-tonight "$CDIR" --date "$1" "${g[@]}" > "$tmpf"
    else   # --new --dry-run: the first night of a campaign that does not exist yet
        local phase1=${PLAN%%;*} m
        ((GREEDY)) && phase1=${PLAN//;/,}
        : > "$tmpf"
        IFS=, read -r -a ms <<< "$phase1"
        for m in "${ms[@]}"; do echo "$m ${TASKS[*]}" >> "$tmpf"; done
    fi
}

run_night() {
    local now s e date started line model ids rc result="complete" suites=()
    if [[ -n $DEADLINE_IN ]]; then
        s=$(date +%s)
        e=$((s + DEADLINE_IN))
        date=${NIGHT_DATE:-$(date +%F)}
    else
        window_next || die "cannot compute the window"
        s=$W_S e=$W_E date=${NIGHT_DATE:-$W_DATE}
    fi
    now=$(date +%s)
    if ((DRY_RUN)); then
        echo "campaign $CAMPAIGN"
        echo "window   $(date -d "@$s" '+%F %H:%M') - $(date -d "@$e" '+%F %H:%M %Z') (night of $date)"
        tonight "$date"
        if [[ ! -s $tmpf ]]; then
            echo "tonight  nothing left for this night's phases"
            [[ -f $CDIR/campaign.json ]] && m0_py plan.py campaign-status "$CDIR" | sed 's/^/         /'
            return 0
        fi
        local total=0 t n
        while read -r model ids; do
            n=$(wc -w <<< "$ids")
            t=$(m0_py llama_cmd.py info "$model" | sed -n "s/^M0_MODEL_TIMEOUT=//p")
            total=$((total + n * (t + 120)))
            echo "suite    $model: $n tasks, agent timeout ${t}s: $ids"
            echo "         run-suite.sh --stamp $CAMPAIGN --server --deadline $e ${SUITE_ARGS[*]} $model $ids"
        done < "$tmpf"
        echo "budget   worst case about $((total / 60)) min of the $(((e - s) / 60)) min window" \
            "(each run: agent timeout + ~2 min setup/verify); what does not fit waits for the next night"
        return 0
    fi
    tonight "$date"
    local lines=() models=()
    mapfile -t lines < "$tmpf"
    for line in "${lines[@]}"; do models+=("${line%% *}"); done
    if ((now < s)); then
        if ((!WAIT)); then
            m0_log "outside the window (it opens $(date -d "@$s" '+%F %H:%M')): --no-wait, exiting"
            exit 0
        fi
        if ((MAX_WAIT >= 0 && s - now > MAX_WAIT)); then
            m0_log "the window opens $(date -d "@$s" '+%F %H:%M'), more than --max-wait ${MAX_WAIT}s away (a late timer activation?): exiting"
            exit 0
        fi
        preflight launch "${models[@]}" || true
        m0_log "waiting for the window: $(date -d "@$s" '+%F %H:%M') - $(date -d "@$e" '+%H:%M')"
        while (($(date +%s) < s)); do   # wall clock: robust against suspend
            ((NIGHT_INT)) && return 1
            if ((!LOCK_TRIED && $(date +%s) >= s - LOCK_LEAD_H * 3600)); then lock_hold_until $((e + 300)); fi
            sleep 10
        done
    fi
    ((LOCK_TRIED)) || lock_hold_until $((e + 300))
    started=$(m0_utc)
    if (($(date +%s) >= e)); then   # the machine slept through the window (suspend, a closed lid)
        m0_log "night of $date missed: the window ended before it could start (was the machine asleep?)"
        m0_py plan.py campaign-record "$CDIR" --date "$date" --result missed --started "$started"
        lock_release
        return 0
    fi
    m0_log "night of $date: window until $(date -d "@$e" '+%F %H:%M:%S'), campaign $CAMPAIGN"
    if ((!${#lines[@]})); then
        m0_log "nothing left for tonight's phases"
        m0_py plan.py campaign-record "$CDIR" --date "$date" --result idle --started "$started"
        lock_release
        return 0
    fi
    preflight "window start" "${models[@]}" || true
    for line in "${lines[@]}"; do
        read -r model ids <<< "$line"
        if (($(date +%s) >= e)); then
            result=deadline
            break
        fi
        m0_log "suite: $model ($(wc -w <<< "$ids") tasks)"
        ((NIGHT_INT)) && { result=interrupted; break; }
        # GPU_RETRIES x GPU_RETRY_SLEEP: a GPU or port still busy is waited for (30 min, never
        # past the deadline) instead of ending the night at once
        # shellcheck disable=SC2086
        GPU_RETRIES=${GPU_RETRIES:-31} GPU_RETRY_SLEEP=${GPU_RETRY_SLEEP:-60} \
            "$HERE/run-suite.sh" --stamp "$CAMPAIGN" --server --deadline "$e" "${SUITE_ARGS[@]}" "$model" $ids &
        SUITE_PID=$!
        while :; do
            rc=0
            wait "$SUITE_PID" || rc=$?
            if ((rc > 128)) && kill -0 "$SUITE_PID" 2> /dev/null; then continue; fi
            break
        done
        if ((rc > 128)); then
            r2=0
            wait "$SUITE_PID" 2> /dev/null || r2=$?
            ((r2 != 127)) && rc=$r2
        fi
        SUITE_PID=
        ((NIGHT_INT)) && rc=130
        suites+=(--model "$model" --exit "$rc")
        case $rc in
            0 | 11) ;;                      # done (11: some runs not final, re-done next night)
            10) result=deadline; break ;;
            21) result=gpu-busy; break ;;   # the owner works on the GPU: leave the night alone
            130) result=interrupted; break ;;
            *) result=error; m0_log "suite $model failed (exit $rc); going on with the next model" ;;
        esac
    done
    m0_py plan.py campaign-record "$CDIR" --date "$date" --result "$result" --started "$started" "${suites[@]}"
    lock_release
    m0_log "night of $date ended: $result"
    m0_py plan.py campaign-status "$CDIR" | while IFS= read -r l; do m0_log "$l"; done || true
    [[ $result != interrupted ]]
}

while :; do
    run_night || exit 130
    ((DRY_RUN)) && exit 0
    if m0_py plan.py campaign-status "$CDIR" > /dev/null; then
        m0_log "campaign $CAMPAIGN complete"
        { m0_py summarize_run.py --table "$CDIR" || true; } | while IFS= read -r l; do m0_log "$l"; done
        exit 0
    fi
    ((UNTIL_DONE)) || exit 0
    [[ -n $DEADLINE_IN ]] && exit 0   # test mode: one night per call
    while (($(date +%s) < W_E)); do
        ((NIGHT_INT)) && exit 130
        sleep 10
    done
done
exit
}
