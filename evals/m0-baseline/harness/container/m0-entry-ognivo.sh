#!/usr/bin/env bash
# ognivo under the M0 harness: one headless `ognivo run` on /work, artifacts in /out.
# Same contract as m0-entry.sh (stdin = prompt, /work = workspace, artifacts in /out).
#
# M0_OG_PROFILE  required   the ognivo profile id = the llama-server --alias
#                           (gpt-oss-20b | gemma-4-26b-a4b | qwen3.8-27b)
# M0_TASK_ID     required
# M0_OG_APPROVE  auto (default): every action that would ask is approved. ognivo allows that
#                only inside a container (/run/.containerenv), which this is. deny: refuse
#                them (tests/selftest.sh, whose sandbox is not a container)
# M0_AGENT_TIMEOUT          seconds (default 1200)
# M0_OG_TIME_LIMIT          ognivo's own time limit in seconds (default: the agent timeout minus
#                           10, so ognivo ends the run itself, with exit 124, before `timeout`
#                           kills it); 0: none, ognivo knows nothing of the timeout
# M0_NICE                   niceness (default 10)
# LLAMA_BASE_URL            http://127.0.0.1:8080/v1 (ognivo wants the server root: /v1 is cut)
# M0_OUT_DIR, M0_WORK_DIR   only for tests/fake-engine.sh (defaults /out and /work)
#
# Exit code: 0 the model answered, 1 it did not (step limit, stuck, a server error during the
# run: ognivo's own 3, kept in meta.json as ognivo_exit), 124 agent timeout (ognivo's own time
# limit or `timeout`), 2 no workspace or bad usage, 3 llama-server unreachable before the start.
set -uo pipefail
if [[ -z ${M0_NICED:-} ]]; then
    export M0_NICED=1
    exec nice -n "${M0_NICE:-10}" "$0" "$@"
fi
: "${M0_OG_PROFILE:?set M0_OG_PROFILE}" "${M0_TASK_ID:?set M0_TASK_ID}"
TIMEOUT=${M0_AGENT_TIMEOUT:-1200}
LIMIT=${M0_OG_TIME_LIMIT:-$(( TIMEOUT > 20 ? TIMEOUT - 10 : TIMEOUT ))}
APPROVE=${M0_OG_APPROVE:-auto}
OUT=${M0_OUT_DIR:-/out}
WORK=${M0_WORK_DIR:-/work}
mkdir -p "$OUT"
cat > "$OUT/prompt.txt"
cd "$WORK" || { echo 2 > "$OUT/exit_code"; exit 2; }

base=${LLAMA_BASE_URL%/v1}
if ! python3 -I -c 'import sys,urllib.request,time
for _ in range(60):
    try:
        urllib.request.urlopen(sys.argv[1], timeout=2); sys.exit(0)
    except Exception: time.sleep(1)
sys.exit(1)' "$base/health"; then
    echo "llama-server not reachable at $base/health" > "$OUT/infra_error"
    echo 3 > "$OUT/exit_code"
    exit 3
fi

head_commit=$(git rev-parse --verify -q HEAD 2> /dev/null) || head_commit=
[[ -n $head_commit ]] && echo "$head_commit" > "$OUT/base_commit"
# ognivo's logs (full command output) go under /out, never into /work: diff.patch takes
# every untracked file there
mkdir -p "$OUT/ognivo-data"
limit=()
[[ $LIMIT != 0 ]] && limit=(--time-limit "$LIMIT")
t0=$(date +%s.%N)
timeout -k 15 "$TIMEOUT" ognivo run --url "$base" --profile "$M0_OG_PROFILE" --approve "$APPROVE" \
    --slot 0 --data-dir "$OUT/ognivo-data" --jsonl "${limit[@]}" \
    < "$OUT/prompt.txt" > "$OUT/ognivo.jsonl" 2> "$OUT/ognivo.stderr"
og_rc=$?
t1=$(date +%s.%N)
case $og_rc in
    3) rc=1 ;;   # a server error during the run: the agent's failure, as OpenCode's exit 1
    *) rc=$og_rc ;;
esac
echo "$rc" > "$OUT/exit_code"
if [[ -n $head_commit ]]; then
    idx=$(mktemp)
    GIT_INDEX_FILE=$idx git read-tree "$head_commit" 2> /dev/null \
        && GIT_INDEX_FILE=$idx git add -A 2> /dev/null \
        && GIT_INDEX_FILE=$idx git diff --cached --binary "$head_commit" > "$OUT/diff.patch" 2> /dev/null
    rm -f "$idx"
    git log --format='%H %s' "$head_commit..HEAD" > "$OUT/agent_commits.txt" 2> /dev/null
fi
git status --porcelain=v1 -uall > "$OUT/git_status.txt" 2> /dev/null
M0_RC=$rc M0_OG_RC=$og_rc M0_T0=$t0 M0_T1=$t1 M0_TIMEOUT=$TIMEOUT M0_LIMIT=$LIMIT \
    M0_OGVER="$(ognivo --version 2> /dev/null | sed -n 1p)" \
    M0_OGBUILD="$(cat /usr/local/share/ognivo/build 2> /dev/null)" \
    python3 -I - "$OUT/meta.json" << 'PY'
import json, os, sys
e = os.environ
rc = int(e["M0_RC"])
build = dict(l.split("=", 1) for l in e.get("M0_OGBUILD", "").splitlines() if "=" in l)
json.dump({"task_id": e["M0_TASK_ID"], "model": e["M0_OG_PROFILE"], "harness": "ognivo",
           "ognivo": e["M0_OGVER"], "ognivo_build": build, "ognivo_exit": int(e["M0_OG_RC"]),
           "approve": e.get("M0_OG_APPROVE", "auto"), "exit_code": rc, "timed_out": rc == 124,
           "agent_timeout_s": int(e["M0_TIMEOUT"]), "ognivo_time_limit_s": int(e["M0_LIMIT"]),
           "wall_s": round(float(e["M0_T1"]) - float(e["M0_T0"]), 3),
           "llama_base_url": e.get("LLAMA_BASE_URL", "")},
          open(sys.argv[1], "w"), indent=1)
PY
exit "$rc"
