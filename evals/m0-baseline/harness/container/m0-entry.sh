#!/usr/bin/env bash
# Container entrypoint: one headless OpenCode session on /work, artifacts in /out.
# The host copies /out out of the stopped container (podman cp) and then removes it.
#
# stdin                 the task prompt (plain text; passed to `opencode run` on stdin, not argv:
#                       argv words with spaces reach the model wrapped in literal quotes)
# M0_MODEL   required   provider/model from /etc/opencode/opencode.json, e.g. llama/gpt-oss-20b
# M0_TASK_ID required   also the session title (a set title skips the extra title request)
# M0_AGENT_TIMEOUT      seconds for `opencode run` (default 1200)
# M0_NICE               niceness of the agent and everything it starts (default 10)
# LLAMA_BASE_URL        default http://127.0.0.1:8080/v1 (pasta -T forwards it to the host)
# M0_OUT_DIR, M0_WORK_DIR   only for tests/fake-engine.sh (defaults /out and /work)
#
# Exit code: that of `opencode run` (0 ok, 1 error event, 124 agent timeout), 3 = llama-server
# unreachable, 2 = no workspace.
set -uo pipefail
if [[ -z ${M0_NICED:-} ]]; then
    export M0_NICED=1
    exec nice -n "${M0_NICE:-10}" "$0" "$@"
fi
: "${M0_MODEL:?set M0_MODEL}" "${M0_TASK_ID:?set M0_TASK_ID}"
TIMEOUT=${M0_AGENT_TIMEOUT:-1200}
OUT=${M0_OUT_DIR:-/out}
WORK=${M0_WORK_DIR:-/work}
mkdir -p "$OUT"
cat > "$OUT/prompt.txt"
cd "$WORK" || { echo 2 > "$OUT/exit_code"; exit 2; }

ts() { date -u +%Y-%m-%dT%H:%M:%S.%3NZ; }
health="${LLAMA_BASE_URL%/v1}/health"
if ! python3 -I - "$health" << 'PY'
import sys, time, urllib.request
for _ in range(60):
    try:
        with urllib.request.urlopen(sys.argv[1], timeout=2) as r:
            if r.status == 200:
                sys.exit(0)
    except Exception:
        pass
    time.sleep(1)
sys.exit(1)
PY
then
    echo "llama-server not reachable at $health" > "$OUT/infra_error"
    echo 3 > "$OUT/exit_code"
    exit 3
fi

base=$(git rev-parse --verify -q HEAD 2> /dev/null) || base=
[[ -n $base ]] && echo "$base" > "$OUT/base_commit"
started=$(ts)
t0=$(date +%s.%N)
timeout -k 15 "$TIMEOUT" opencode run --standalone --print-logs --format json --thinking --auto \
    --title "$M0_TASK_ID" --model "$M0_MODEL" \
    < "$OUT/prompt.txt" > "$OUT/events.jsonl" 2> "$OUT/opencode.stderr"
rc=$?
t1=$(date +%s.%N)
finished=$(ts)
echo "$rc" > "$OUT/exit_code"

# Full transcript (reasoning, tool inputs/outputs, per-step tokens incl. cache reads, snapshots).
sid=$(python3 -I -c 'import json,sys
for line in open(sys.argv[1], encoding="utf-8", errors="replace"):
    try:
        s = json.loads(line).get("sessionID")
    except Exception:
        continue
    if s:
        print(s); break' "$OUT/events.jsonl" 2> /dev/null)
if [[ -z $sid ]]; then
    sid=$(timeout 60 opencode session list --standalone --format json 2> /dev/null \
        | python3 -I -c 'import json,sys; d=json.load(sys.stdin); print(d[0]["id"] if d else "")' 2> /dev/null)
fi
if [[ -n $sid ]]; then
    timeout 120 opencode session export "$sid" --standalone > "$OUT/session.json" 2> "$OUT/export.stderr"
    # Subagent runs are child sessions (not in events.jsonl, not in `session list`): export every
    # session id the transcripts reference, recursively, as session-<id>.json.
    python3 -I - "$OUT" "$sid" << 'PY'
import os, re, subprocess, sys
out, root = sys.argv[1], sys.argv[2]
seen, todo = {root}, [os.path.join(out, "session.json")]
while todo and len(seen) < 50:
    text = open(todo.pop(), encoding="utf-8", errors="replace").read()
    for s in sorted(set(re.findall(r'"sessionID":\s*"(ses_[A-Za-z0-9]+)"', text)) - seen):
        seen.add(s)
        path = os.path.join(out, f"session-{s}.json")
        with open(path, "w") as f:
            subprocess.run(["timeout", "120", "opencode", "session", "export", s, "--standalone"],
                           stdout=f, stderr=subprocess.DEVNULL)
        todo.append(path)
PY
fi
mkdir -p "$OUT/opencode-data"
cp -a "$XDG_DATA_HOME"/opencode/opencode.db* "$XDG_DATA_HOME"/opencode/log "$OUT/opencode-data/" 2> /dev/null

# Final change set against the base commit (also what the agent committed itself), without
# touching the workspace's own index: a temporary index, untracked files included, ignored
# files (target/, build/ ...) left out.
if [[ -n $base ]]; then
    idx=$(mktemp)
    GIT_INDEX_FILE=$idx git read-tree "$base" 2> /dev/null \
        && GIT_INDEX_FILE=$idx git add -A 2> /dev/null \
        && GIT_INDEX_FILE=$idx git diff --cached --binary "$base" > "$OUT/diff.patch" 2> /dev/null
    rm -f "$idx"
    git log --format='%H %s' "$base..HEAD" > "$OUT/agent_commits.txt" 2> /dev/null
fi
git status --porcelain=v1 -uall > "$OUT/git_status.txt" 2> /dev/null

M0_RC=$rc M0_SID=$sid M0_STARTED=$started M0_FINISHED=$finished M0_T0=$t0 M0_T1=$t1 \
    M0_TIMEOUT=$TIMEOUT M0_OCVER="$(opencode --version 2> /dev/null)" python3 -I - "$OUT/meta.json" << 'PY'
import json, os, sys
e = os.environ
rc = int(e["M0_RC"])
json.dump({"task_id": e["M0_TASK_ID"], "model": e["M0_MODEL"], "harness": "opencode",
           "session_id": e["M0_SID"], "opencode": e["M0_OCVER"], "exit_code": rc,
           "timed_out": rc == 124, "agent_timeout_s": int(e["M0_TIMEOUT"]),
           "started": e["M0_STARTED"], "finished": e["M0_FINISHED"],
           "wall_s": round(float(e["M0_T1"]) - float(e["M0_T0"]), 3),
           "llama_base_url": e.get("LLAMA_BASE_URL", "")},
          open(sys.argv[1], "w"), indent=1)
PY
exit "$rc"
