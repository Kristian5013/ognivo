#!/usr/bin/env bash
# Optional second baseline: Claude Code --bare headless against llama-server's /v1/messages.
# Same contract as m0-entry.sh (stdin = prompt, /work = workspace, artifacts in /out).
#
# M0_CC_MODEL   required   the llama-server --alias, e.g. gpt-oss-20b | gemma-4-26b-a4b | qwen3.8-27b
# M0_TASK_ID    required
# M0_CTX        context window to assume (default 98304; gemma 65536)
# M0_AGENT_TIMEOUT          seconds (default 1200)
# M0_NICE                   niceness (default 10)
# LLAMA_BASE_URL            http://127.0.0.1:8080/v1 (the /v1 suffix is stripped for Anthropic clients)
# M0_OUT_DIR, M0_WORK_DIR   only for tests/fake-engine.sh (defaults /out and /work)
set -uo pipefail
if [[ -z ${M0_NICED:-} ]]; then
    export M0_NICED=1
    exec nice -n "${M0_NICE:-10}" "$0" "$@"
fi
: "${M0_CC_MODEL:?set M0_CC_MODEL}" "${M0_TASK_ID:?set M0_TASK_ID}"
TIMEOUT=${M0_AGENT_TIMEOUT:-1200}
OUT=${M0_OUT_DIR:-/out}
WORK=${M0_WORK_DIR:-/work}
mkdir -p "$OUT"
cat > "$OUT/prompt.txt"
cd "$WORK" || { echo 2 > "$OUT/exit_code"; exit 2; }

base=${LLAMA_BASE_URL%/v1}
export ANTHROPIC_BASE_URL=$base                       # Claude Code appends /v1/messages itself
export ANTHROPIC_API_KEY=${LLAMA_API_KEY:-sk-local}   # --bare reads only ANTHROPIC_API_KEY (x-api-key)
export ANTHROPIC_MODEL=$M0_CC_MODEL
export ANTHROPIC_DEFAULT_HAIKU_MODEL=$M0_CC_MODEL ANTHROPIC_DEFAULT_SONNET_MODEL=$M0_CC_MODEL
export ANTHROPIC_DEFAULT_OPUS_MODEL=$M0_CC_MODEL CLAUDE_CODE_SUBAGENT_MODEL=$M0_CC_MODEL
export CLAUDE_CODE_MAX_CONTEXT_TOKENS=${M0_CTX:-98304}
export CLAUDE_CODE_MAX_OUTPUT_TOKENS=${M0_MAX_OUT:-16384}   # counts against the window before auto-compact
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 DISABLE_AUTOUPDATER=1
export CLAUDE_CODE_ATTRIBUTION_HEADER=0                # no per-request block at the top of the prompt
export CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1
export CLAUDE_CODE_DISABLE_THINKING=1                  # no `thinking` budget: reasoning stays as the server sets it
export IS_SANDBOX=1                                    # root in a rootless container: allow --dangerously-skip-permissions

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
t0=$(date +%s.%N)
timeout -k 15 "$TIMEOUT" claude --bare -p --output-format stream-json --verbose \
    --dangerously-skip-permissions --model "$M0_CC_MODEL" \
    < "$OUT/prompt.txt" > "$OUT/stream.jsonl" 2> "$OUT/claude.stderr"
rc=$?
t1=$(date +%s.%N)
echo "$rc" > "$OUT/exit_code"
mkdir -p "$OUT/claude-projects"
cp -a "$HOME"/.claude/projects/. "$OUT/claude-projects/" 2> /dev/null   # <session>.jsonl transcripts
if [[ -n $head_commit ]]; then
    idx=$(mktemp)
    GIT_INDEX_FILE=$idx git read-tree "$head_commit" 2> /dev/null \
        && GIT_INDEX_FILE=$idx git add -A 2> /dev/null \
        && GIT_INDEX_FILE=$idx git diff --cached --binary "$head_commit" > "$OUT/diff.patch" 2> /dev/null
    rm -f "$idx"
    git log --format='%H %s' "$head_commit..HEAD" > "$OUT/agent_commits.txt" 2> /dev/null
fi
git status --porcelain=v1 -uall > "$OUT/git_status.txt" 2> /dev/null
M0_RC=$rc M0_T0=$t0 M0_T1=$t1 M0_TIMEOUT=$TIMEOUT M0_CCVER="$(claude --version 2> /dev/null | sed -n 1p)" \
    python3 -I - "$OUT/meta.json" << 'PY'
import json, os, sys
e = os.environ
rc = int(e["M0_RC"])
json.dump({"task_id": e["M0_TASK_ID"], "model": e["M0_CC_MODEL"], "harness": "claude-code-bare",
           "claude": e["M0_CCVER"], "exit_code": rc, "timed_out": rc == 124,
           "agent_timeout_s": int(e["M0_TIMEOUT"]),
           "wall_s": round(float(e["M0_T1"]) - float(e["M0_T0"]), 3),
           "llama_base_url": e.get("LLAMA_BASE_URL", "")},
          open(sys.argv[1], "w"), indent=1)
PY
exit "$rc"
