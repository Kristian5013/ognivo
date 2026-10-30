#!/usr/bin/env bash
# Self-tests of the M0 baseline harness that need neither Podman nor the GPU nor the network.
#
#   tests/selftest.sh [--quick] [--keep] [--only STAGE]
#     --quick   stages 1-3 only (no OpenCode runs)
#     --keep    keep the sandbox directory (printed at the end)
#     --only    run one stage: static | profiles | dryrun | e2e | night
#
# 1 static    bash -n on every shell script; Python syntax (ast, no .pyc written); JSON configs;
#             executable bits; no path into a session scratchpad; tests/lint_bash.py (errexit,
#             pipefail/SIGPIPE and quoting hazards); shellcheck as well when it is installed;
#             tests/scoring_test.py (the scorer on synthetic run dirs)
# 2 profiles  scripts/llama_cmd.py turns the three ognivo profiles into the expected
#             llama-server flags; the served-template check on canonical and on non-canonical text
# 3 dryrun    run-task.sh / verify-task.sh DRY_RUN=1, run-suite.sh --dry-run on the fixtures and
#             on the real tasks/, run-night.sh --dry-run --new, prepare-image.sh --dry-run
# 4 e2e       run-suite.sh end to end without a container runtime: tests/fake-engine.sh runs the
#             container entrypoint on the host with the pinned OpenCode from cache/,
#             tests/fake-llama-server.sh (scripts/mock_llama.py) is the managed llama-server and
#             tests/fake-nvidia-smi.sh the GPU check: results, artifacts, server flags, resume,
#             GPU-busy refusal, deadline guard, deadline watchdog + resume, --agent reference/none
# 5 night     run-night.sh: a two-night campaign with the default plan (night 1 cut short by
#             --deadline-in, night 2 finishing it), then a third call with nothing left
# Stages 2-5 run inside bwrap: / read-only, only the sandbox writable, a network namespace with
# loopback only, a minimal /dev without GPU device nodes.
# Needs bash, python3 >= 3.11, git, curl, bwrap, util-linux; stages 4-5 the pinned OpenCode in
# <m0-baseline>/cache (harness/prepare-image.sh fetch).
set -uo pipefail
T=$(cd "$(dirname "$(realpath "$0")")" && pwd)
H=$(dirname "$T")
QUICK=0 KEEP=0 ONLY= INNER=
while (($#)); do
    case $1 in
        --quick) QUICK=1 ;;
        --keep) KEEP=1 ;;
        --only) ONLY=$2; shift ;;
        --inner) INNER=$2; shift ;;
        -h | --help) sed -n '2,/^set -uo/{/^set -uo/d;s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
        *) echo "selftest: unknown argument $1" >&2; exit 2 ;;
    esac
    shift
done

FAILS=0 PASSES=0
ok() { PASSES=$((PASSES + 1)); printf '  ok    %s\n' "$*"; }
bad() { FAILS=$((FAILS + 1)); printf '  FAIL  %s\n' "$*"; }
check() {   # check DESCRIPTION CMD...
    local d=$1
    shift
    if "$@" > /dev/null 2>&1; then ok "$d"; else bad "$d"; fi
}
want() { [[ -z $ONLY || $ONLY == "$1" ]]; }
jget() { python3 -I -c 'import json,sys
d = json.load(open(sys.argv[1]))
for k in sys.argv[2].split("."):
    d = d.get(k) if isinstance(d, dict) else None
print("" if d is None else d)' "$1" "$2" 2> /dev/null; }

# --- stage 1: static (outside the sandbox) --------------------------------------------------
stage_static() {
    echo "== 1 static"
    local f n=0 before=$FAILS
    while IFS= read -r -d '' f; do
        n=$((n + 1))
        bash -n "$f" 2> /dev/null || bad "bash -n $f"
        [[ -x $f || $f == */lib/* || $f == */_common/* ]] || bad "not executable: $f"
        [[ $(sed -n 1p "$f") == '#!/usr/bin/env bash' || $f == */lib/* || $f == */_common/* ]] \
            || bad "no bash shebang: $f"
    done < <(find "$H" -name '*.sh' -type f -print0)
    ((FAILS == before)) && ok "bash -n, shebang and exec bit on $n shell scripts"
    n=0
    while IFS= read -r -d '' f; do
        n=$((n + 1))
        python3 -I -c 'import ast,sys; ast.parse(open(sys.argv[1], encoding="utf-8").read(), sys.argv[1])' "$f" \
            || bad "python syntax $f"
    done < <(find "$H" -name '*.py' -type f -print0)
    ok "python syntax of $n scripts (ast only)"
    for f in "$H/config/opencode.json" "$H/config/models.json" "$T"/fixtures/tasks/*/task.json; do
        python3 -I -c 'import json,sys; json.load(open(sys.argv[1]))' "$f" || bad "JSON $f"
    done
    ok "JSON configs parse"
    local p1="/tmp/clau""de-" p2="scratch""pad/m0-tasks"   # split: this file must not match itself
    if grep -rIl -e "$p1" -e "$p2" "$H" > /dev/null 2>&1; then
        bad "paths into a session scratchpad: $(grep -rIl -e "$p1" -e "$p2" "$H" | tr '\n' ' ')"
    else
        ok "no path into a session scratchpad"
    fi
    local big
    big=$(find "$H" \( -name __pycache__ -o -name '*.pyc' -o -size +5M \) -print)
    if [[ -n $big ]]; then
        bad "binaries or caches in harness/: $(tr '\n' ' ' <<< "$big")"
    else
        ok "harness/ holds sources only (no __pycache__, nothing over 5 MB)"
    fi
    for f in "$H"/*.sh; do
        grep -q '^set -euo pipefail' "$f" || bad "no 'set -euo pipefail' in $f"
    done
    ok "top-level scripts use set -euo pipefail"
    local lint shfiles=()
    # NUL-separated: the repository copy lives under "Рабочий стол", a path with a space
    mapfile -d '' shfiles < <(find "$H" -name '*.sh' -type f -print0 | sort -z)
    if lint=$(python3 -I "$T/lint_bash.py" "${shfiles[@]}"); then
        ok "tests/lint_bash.py: no errexit/pipefail/quoting hazards"
    else
        bad "tests/lint_bash.py:"$'\n'"$lint"
    fi
    local sdir scoring
    sdir=$(mktemp -d "${TMPDIR:-/tmp}/m0-scoring.XXXXXX")
    if scoring=$(python3 -I "$T/scoring_test.py" "$sdir" 2>&1); then
        ok "tests/scoring_test.py: $(grep -c '^  ok' <<< "$scoring") scorer cases (forged /out files, OOM, disk guard, stack errors, attempt cap)"
    else
        bad "tests/scoring_test.py:"$'\n'"$(grep -v '^  ok' <<< "$scoring")"
    fi
    rm -rf "$sdir"
    if command -v shellcheck > /dev/null; then
        if (cd "$H" && shellcheck -x -S warning ./*.sh lib/*.sh container/*.sh tests/*.sh); then
            ok "shellcheck -S warning"
        else
            bad "shellcheck -S warning"
        fi
    else
        echo "  skip  shellcheck is not installed (sudo apt install shellcheck)"
    fi
}

if [[ -z $INNER ]]; then
    want static && stage_static
    if [[ $ONLY == static ]]; then
        echo "== $PASSES passed, $FAILS failed"
        exit $((FAILS > 0))
    fi
    command -v bwrap > /dev/null || { echo "selftest: bwrap is needed for stages 2-5" >&2; exit 1; }
    SB=$(mktemp -d "${TMPDIR:-/tmp}/m0-selftest.XXXXXX")
    mkdir -p "$SB/tmp"
    bwrap --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp --bind "$SB" "$SB" \
        --unshare-net --unshare-ipc --die-with-parent --setenv TMPDIR "$SB/tmp" \
        -- bash "$0" --inner "$SB" ${ONLY:+--only "$ONLY"} $( ((QUICK)) && echo --quick)
    rc=$?
    if ((KEEP)); then echo "sandbox kept: $SB"; else rm -rf "$SB"; fi
    if ((FAILS)); then echo "== static stage had $FAILS failure(s)"; rc=1; fi
    exit "$rc"
fi

# --- inside the sandbox ---------------------------------------------------------------------
SB=$INNER
cd "$SB" || exit 1
export TMPDIR=$SB/tmp
export M0_RUNS_ROOT=$SB/runs M0_WORK_ROOT=$SB/work M0_TASKS_ROOT=$T/fixtures/tasks
export ENGINE=$T/fake-engine.sh FAKE_ENGINE_STATE=$SB/engine FAKE_ENGINE_LOG=$SB/engine.log
export LLAMA_SERVER_BIN=$T/fake-llama-server.sh NVIDIA_SMI=$T/fake-nvidia-smi.sh
export FAKE_LLAMA_LOG=$SB/llama-requests.jsonl GPU_RETRIES=1 GPU_RETRY_SLEEP=0
export M0_SETUP_EST=5 M0_VERIFY_EST=5 M0_GUARD_OVERHEAD=5 TAP_PORT=18081
unset M0_ROOT M0_LOG_FILE
mkdir -p "$SB/models"
# models.json for the tests: the real profiles (real flags, canonical templates), dummy model files
python3 -I - "$H/config/models.json" "$SB/models" "$SB/models.json" << 'PY'
import json, os, sys
src, mdir, dst = sys.argv[1:]
c = json.load(open(src))
for k, m in c["models"].items():
    p = os.path.join(mdir, os.path.basename(m["gguf"]))
    open(p, "w").close()
    m["gguf"] = p
json.dump(c, open(dst, "w"), indent=1, ensure_ascii=False)
PY
export M0_MODELS_JSON=$SB/models.json
OC_BIN=$H/../cache/opencode/2.2.1/bin/opencode
REAL_MODELS=$H/config/models.json
HOME_REAL=$HOME
REAL_TASKS=$H/../tasks

stage_profiles() {
    echo "== 2 profiles"
    local m out
    for m in gpt-oss-20b gemma-4-26b-a4b qwen3.8-27b; do
        if out=$(M0_MODELS_JSON=$REAL_MODELS python3 -I "$H/scripts/llama_cmd.py" show "$m" --no-file-check); then
            ok "llama_cmd show $m"
        else
            bad "llama_cmd show $m"
            continue
        fi
        grep -q -- "--alias $m " <<< "$out" || bad "$m: --alias"
        grep -q -- "--host 127.0.0.1 --port 8090\$" <<< "$out" || bad "$m: host/port last"
        grep -q -- "--jinja --metrics --no-webui --log-timestamps" <<< "$out" || bad "$m: fixed flags"
        grep -q -- "--chat-template-file '/home/kristian/Рабочий стол/cforge/profiles/templates/" <<< "$out" \
            || bad "$m: absolute --chat-template-file"
    done
    out=$(M0_MODELS_JSON=$REAL_MODELS python3 -I "$H/scripts/llama_cmd.py" show gpt-oss-20b --no-file-check)
    check "gpt-oss: profile flags -c 131072 -np 2 --kv-unified --swa-full, effort high" \
        grep -q -- "-c 131072 -np 2 --kv-unified --swa-full -fa on -ngl 99 -b 2048 -ub 2048 .* --temp 1.0 --top-p 1.0 --top-k 0 --min-p 0.0 --reasoning-effort high --jinja" <<< "$out"
    out=$(M0_MODELS_JSON=$REAL_MODELS python3 -I "$H/scripts/llama_cmd.py" show gemma-4-26b-a4b --no-file-check)
    check "gemma: checkpoints/cache-ram, top-k 64, --reasoning on" \
        grep -q -- "--ctx-checkpoints 16 --checkpoint-min-step 2048 --cache-ram 12288 .* --top-p 0.95 --top-k 64 --min-p 0.0 --reasoning on --jinja" <<< "$out"
    out=$(M0_MODELS_JSON=$REAL_MODELS python3 -I "$H/scripts/llama_cmd.py" show qwen3.8-27b --no-file-check)
    check "qwen: -c 98304 -ub 1024, top-k 20, --reasoning on --reasoning-effort medium" \
        grep -q -- "-c 98304 -np 1 -fa on -ngl 99 -b 2048 -ub 1024 .* --top-k 20 --min-p 0.0 --reasoning on --reasoning-effort medium --jinja" <<< "$out"
    M0_MODELS_JSON=$REAL_MODELS python3 -I "$H/scripts/llama_cmd.py" argv gpt-oss-20b --port 18099 --no-file-check > "$SB/argv.bin"
    check "argv output is NUL-separated with the template path as one argument" \
        python3 -I -c 'import sys; a = open(sys.argv[1], "rb").read().split(b"\0"); i = a.index(b"--chat-template-file"); assert a[i+1].decode().endswith("gpt-oss-20b/canonical.jinja") and b" " in a[i+1]; assert a[-2] == b"18099"' "$SB/argv.bin"
    check "unknown model is refused" bash -c "! python3 -I '$H/scripts/llama_cmd.py' show no-such-model"
    # served-template check: canonical file -> ok; a modified template -> problem (or warning)
    python3 -I - "$SB" << 'PY'
import json, os, sys, tomllib
sb = sys.argv[1]
repo = json.load(open(os.environ["M0_MODELS_JSON"]))["ognivo_repo"]
for name, sub in (("gpt-oss-20b", "gpt-oss-20b"), ("qwen3.8-27b", "Qwen3.8-27B")):
    tpl = open(os.path.join(repo, "profiles/templates", sub, "canonical.jinja"), encoding="utf-8").read()
    gguf = json.load(open(os.environ["M0_MODELS_JSON"]))["models"][name]["gguf"]
    json.dump({"chat_template": tpl, "model_path": gguf, "build_info": "b11924-12cc59ba4"}, open(f"{sb}/props-{name}.json", "w"))
    json.dump({"chat_template": tpl + "{# changed #}", "model_path": gguf}, open(f"{sb}/props-{name}-bad.json", "w"))
    json.dump({"data": [{"id": name}]}, open(f"{sb}/models-{name}.json", "w"))
PY
    for m in gpt-oss-20b qwen3.8-27b; do
        check "check: $m canonical served template accepted" \
            python3 -I "$H/scripts/llama_cmd.py" check "$m" "$SB/props-$m.json" "$SB/models-$m.json"
        check "check: $m modified template refused" \
            bash -c "! python3 -I '$H/scripts/llama_cmd.py' check $m '$SB/props-$m-bad.json' '$SB/models-$m.json'"
        check "check: $m modified template only warned with TEMPLATE_CHECK=warn" \
            env TEMPLATE_CHECK=warn python3 -I "$H/scripts/llama_cmd.py" check "$m" "$SB/props-$m-bad.json" "$SB/models-$m.json"
    done
    check "check: wrong alias refused" \
        bash -c "! python3 -I '$H/scripts/llama_cmd.py' check gpt-oss-20b '$SB/props-gpt-oss-20b.json' '$SB/models-qwen3.8-27b.json'"
}

stage_dryrun() {
    echo "== 3 dry runs"
    local out
    out=$(DRY_RUN=1 "$H/run-task.sh" st-hello llama/gpt-oss-20b "$SB/ws-x" "$SB/p.txt" "$SB/r") || bad "run-task.sh DRY_RUN"
    check "run-task.sh DRY_RUN: pasta splice-only, one rw mount, limits, timeouts" grep -q -- \
        "--network pasta:--splice-only,-T,8090 --cpus 8 --memory 16g --memory-swap 16g --pids-limit 4096 --timeout 1500 --cap-drop all --security-opt no-new-privileges -v $SB/ws-x:/work " <<< "$out"
    check "run-task.sh DRY_RUN: host-side timeout = agent + 420 s" grep -q -- "^timeout --preserve-status -k 30 1620 " <<< "$out"
    out=$(env -u TAP_PORT DRY_RUN=1 TAP=1 HARNESS=claude "$H/run-task.sh" st-hello gemma-4-26b-a4b "$SB/ws-x" "$SB/p.txt" "$SB/r") || bad "run-task.sh DRY_RUN claude"
    check "run-task.sh claude+tap: tap port, claude image, M0_CTX 65536" grep -q -- \
        "pasta:--splice-only,-T,8091 .*M0_CC_MODEL=gemma-4-26b-a4b -e M0_CTX=65536 localhost/m0-opencode:2.2.1-claude -- /usr/local/bin/m0-entry-claude" <<< "$out"
    out=$(DRY_RUN=1 TASKS_ROOT=$M0_TASKS_ROOT VERIFY_ENV="FC_SKIP_FORMAT=1" "$H/verify-task.sh" "$M0_TASKS_ROOT/st-hello" "$SB/ws-x" "$SB/r") || bad "verify-task.sh DRY_RUN"
    check "verify-task.sh DRY_RUN: --network none, tasks root ro at its own path, verify.sh /work" grep -q -- \
        "--network none .* -v $SB/ws-x:/work -v $M0_TASKS_ROOT:$M0_TASKS_ROOT:ro .* -e FC_SKIP_FORMAT=1 --entrypoint nice localhost/m0-opencode:2.2.1-full -n 10 bash $M0_TASKS_ROOT/st-hello/verify.sh /work" <<< "$out"
    check "run-task.sh refuses a workspace path with ':'" bash -c "! '$H/run-task.sh' t m '$SB/a:b' '$SB/p' '$SB/r'"
    check "run-task.sh usage on wrong argument count" bash -c "'$H/run-task.sh' a b; [[ \$? == 2 ]]"

    out=$("$H/run-suite.sh" --dry-run --server --port 18080 gpt-oss-20b 2>&1) || bad "run-suite.sh --dry-run (fixtures)"
    printf '%s\n' "$out" > "$SB/dry-suite.txt"
    check "run-suite --dry-run lists the 3 selected fixtures, not the reserve one" \
        bash -c "grep -q '^st-hello ' '$SB/dry-suite.txt' && grep -q '^st-inline ' '$SB/dry-suite.txt' && grep -q '^st-unsolvable ' '$SB/dry-suite.txt' && ! grep -q '^st-reserve' '$SB/dry-suite.txt'"
    check "run-suite --dry-run shows the managed llama-server command and the GPU guard" \
        bash -c "grep -q 'fake-llama-server.sh -m .*--alias gpt-oss-20b' '$SB/dry-suite.txt' && grep -q 'GPU guard' '$SB/dry-suite.txt'"
    check "run-suite --dry-run shows the agent and verify container commands" \
        bash -c "grep -q 'run --name m0-opencode-st-hello-gpt-oss-20b' '$SB/dry-suite.txt' && grep -q -- '--network none' '$SB/dry-suite.txt'"
    check "run-suite --dry-run wrote nothing" test ! -e "$SB/runs"
    check "run-suite: unknown model refused" bash -c "! '$H/run-suite.sh' --dry-run no-such-model"
    check "run-suite: unknown task refused" bash -c "! '$H/run-suite.sh' --dry-run gpt-oss-20b no-such-task"

    if [[ -d $REAL_TASKS ]]; then
        out=$(M0_TASKS_ROOT=$REAL_TASKS M0_MODELS_JSON=$REAL_MODELS NVIDIA_SMI=nvidia-smi LLAMA_SERVER_BIN= \
            "$H/run-suite.sh" --dry-run --server gpt-oss-20b 2>&1)
        rc=$?
        printf '%s\n' "$out" > "$SB/dry-real.txt"
        if ((rc == 0)); then
            ok "run-suite --dry-run on the real tasks/: $(grep -c ' run$' "$SB/dry-real.txt") selected tasks planned"
        else
            bad "run-suite --dry-run on the real tasks/ (exit $rc)"
        fi
        out=$(M0_TASKS_ROOT=$REAL_TASKS M0_MODELS_JSON=$REAL_MODELS M0_RUNS_ROOT=$SB/night-dry \
            "$H/run-night.sh" --dry-run --new --no-inhibit 2>&1)
        rc=$?
        printf '%s\n' "$out" > "$SB/dry-night.txt"
        if ((rc == 0)) && grep -q 'suite    gpt-oss-20b' "$SB/dry-night.txt" && grep -q 'suite    gemma-4-26b-a4b' "$SB/dry-night.txt" \
            && ! grep -q 'suite    qwen3.8-27b' "$SB/dry-night.txt" && grep -q '03:00 - .*06:00' "$SB/dry-night.txt"; then
            ok "run-night --dry-run --new: night 1 = gpt-oss-20b + gemma-4-26b-a4b in 03:00-06:00"
        else
            bad "run-night --dry-run --new (exit $rc)"
        fi
        check "run-night --dry-run wrote nothing" test ! -e "$SB/night-dry"
        out=$(M0_TASKS_ROOT=$REAL_TASKS M0_RUNS_ROOT=$SB/night-create "$H/run-night.sh" --new --create-only 2>/dev/null)
        check "run-night --new --create-only: a campaign of the 10 selected tasks, no night run" python3 -I -c 'import json,sys
c = json.load(open(sys.argv[1])); assert len(c["tasks"]) == 10 and c["nights"] == [] and c["plan"] == [["gpt-oss-20b", "gemma-4-26b-a4b"], ["qwen3.8-27b"]], c' "$SB/night-create/$out/campaign.json"
    else
        echo "  skip  no real tasks/ next to the harness"
    fi
    out=$("$H/prepare-image.sh" --dry-run full 2>&1) || bad "prepare-image.sh --dry-run"
    check "prepare-image --dry-run: target full, pinned base digest, podman build" bash -c \
        "grep -q 'localhost/m0-opencode:2.2.1-full' <<< \"\$1\" && grep -q 'ubuntu@sha256:262df65c' <<< \"\$1\" && grep -q 'build --target full' <<< \"\$1\"" _ "$out"
    out=$("$H/prepare-image.sh" --dry-run claude 2>&1) || bad "prepare-image.sh --dry-run claude"
    check "prepare-image --dry-run claude: the local Claude Code binary is found with its pinned sha256" \
        grep -q 'claude   .*2.1.311 (sha256 ok)' <<< "$out"
}

result_status() { jget "$M0_RUNS_ROOT/$1/result.json" status; }

stage_e2e() {
    local rc d out ver
    export HOME=$SB/home
    mkdir -p "$HOME"
    # OpenCode logs even for --version: only ever with an isolated HOME/XDG tree
    ver=$(env -i PATH=/usr/bin:/bin HOME="$SB/oc-home" XDG_CONFIG_HOME="$SB/oc-home/.config" \
        XDG_DATA_HOME="$SB/oc-home/.local/share" XDG_CACHE_HOME="$SB/oc-home/.cache" \
        XDG_STATE_HOME="$SB/oc-home/.local/state" OPENCODE_DISABLE_AUTOUPDATE=1 "$OC_BIN" --version 2> /dev/null | sed -n 1p)
    echo "== 4 end to end (fake engine, mock llama-server, pinned OpenCode $ver)"
    # A: managed server, the three selected fixtures
    FAKE_LLAMA_ARGV=$SB/argv-A.txt nice -n 10 "$H/run-suite.sh" --server --port 18080 --stamp A --timeout 120 \
        --verify-env FC_SKIP_FORMAT=1 gpt-oss-20b > "$SB/suite-A.log" 2>&1
    rc=$?
    ((rc == 0)) && ok "suite A exit 0" || bad "suite A exit $rc (see $SB/suite-A.log)"
    d=$M0_RUNS_ROOT/A/gpt-oss-20b
    [[ $(result_status A/gpt-oss-20b/st-hello) == solved ]] && ok "st-hello solved" || bad "st-hello: $(result_status A/gpt-oss-20b/st-hello)"
    [[ $(result_status A/gpt-oss-20b/st-inline) == solved ]] && ok "st-inline (inline prompt) solved" || bad "st-inline: $(result_status A/gpt-oss-20b/st-inline)"
    [[ $(result_status A/gpt-oss-20b/st-unsolvable) == failed ]] && ok "st-unsolvable failed" || bad "st-unsolvable: $(result_status A/gpt-oss-20b/st-unsolvable)"
    check "no run for the reserve task" test ! -e "$d/st-reserve"
    for f in result.json trajectory/prompt.txt trajectory/events.jsonl trajectory/session.json trajectory/diff.patch \
        trajectory/meta.json trajectory/base_commit trajectory/git_status.txt trajectory/llm.jsonl \
        logs/run.env logs/suite.env logs/setup.log logs/verify.log logs/verify_exit logs/opencode.stderr logs/opencode-data/opencode.db; do
        [[ -s $d/st-hello/$f ]] || bad "st-hello/$f missing or empty"
    done
    ok "st-hello artifacts: result.json, trajectory/*, logs/*"
    check "diff.patch adds hello.txt" grep -q '^+hello from mock' "$d/st-hello/trajectory/diff.patch"
    check "prompt reached the agent unchanged" cmp "$d/st-hello/trajectory/prompt.txt" "$M0_TASKS_ROOT/st-hello/prompt.txt"
    check "result.json: final, agent exit 0, verify exit 0, prompt sha matches task.json" python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1]))
assert r["final"] and r["agent"]["exit"] == 0 and r["verify"]["exit"] == 0 and r["prompt_matches_task_json"] is True
assert r["summary"]["steps"] == 3 and r["summary"]["tools"].get("write") == 1 and r["server"]["managed"]' "$d/st-hello/result.json"
    check "tap recorded 3 chat requests with llama timings" python3 -I -c 'import json,sys
rows = [json.loads(l) for l in open(sys.argv[1])]
chat = [r for r in rows if r["path"].endswith("/chat/completions")]
assert len(chat) == 3 and all(r["timings"] for r in chat)' "$d/st-hello/trajectory/llm.jsonl"
    check "the agent saw no task files (no hidden/verify paths in its requests)" bash -c "! grep -q 'fixtures/tasks' '$FAKE_LLAMA_LOG'"
    check "requests carry the gpt-oss sampling body and reasoning_effort" python3 -I -c 'import json,sys
b = [json.loads(l)["body"] for l in open(sys.argv[1]) if "chat/completions" in l][0]
assert b["model"] == "gpt-oss-20b" and b["temperature"] == 1.0 and b["top_k"] == 0 and b["reasoning_effort"] == "high" and b["max_tokens"] == 32768' "$FAKE_LLAMA_LOG"
    check "llama-server started with the profile flags" bash -c "grep -qx -- '--kv-unified' '$SB/argv-A.txt' && grep -qx -- '--swa-full' '$SB/argv-A.txt' && grep -A1 -x -- '--port' '$SB/argv-A.txt' | grep -qx 18080"   # lint: ok (bash -c: no pipefail)
    check "server.json: canonical template, alias served" python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1])); assert r["ok"] and r["template_verdict"] == "canonical"' "$d/server.json"
    check "llama-server stopped at the end (port closed)" bash -c "! (exec 3<>/dev/tcp/127.0.0.1/18080) 2>/dev/null"
    check "finished workspaces removed" bash -c "! ls '$M0_WORK_ROOT/A/gpt-oss-20b/' 2>/dev/null | grep -q st-"   # lint: ok (bash -c: no pipefail)
    check "results.jsonl has 3 rows" bash -c "[[ \$(wc -l < '$d/results.jsonl') == 3 ]]"
    check "summarize_run.py --table" bash -c "python3 -I '$H/scripts/summarize_run.py' --table '$M0_RUNS_ROOT/A' | grep -q 'st-hello .*PASS'"   # lint: ok (bash -c: no pipefail)
    check "summarize_run.py rows for the failure catalogue" bash -c "python3 -I '$H/scripts/summarize_run.py' '$d/st-hello' | grep -q '\"flags\": \\[\"solved\"\\]'"   # lint: ok (bash -c: no pipefail)
    check "agent containers left no state behind" bash -c "[[ -z \$(ls -A '$FAKE_ENGINE_STATE' 2>/dev/null) ]]"

    # B: resume a finished stamp -> nothing to run, no server start
    "$H/run-suite.sh" --server --port 18080 --stamp A --timeout 120 gpt-oss-20b > "$SB/suite-B.log" 2>&1
    rc=$?
    ((rc == 0)) && grep -q '0 of 3 tasks to run' "$SB/suite-B.log" && ! grep -q 'starting llama-server' "$SB/suite-B.log" \
        && ok "resume of a finished stamp: nothing re-run, no server start" || bad "resume B (exit $rc)"
    check "no _attempts after a clean resume" test ! -e "$d/_attempts"

    # C: GPU busy -> refused before anything starts
    FAKE_GPU_BUSY=1 FAKE_LLAMA_ARGV=$SB/argv-C.txt "$H/run-suite.sh" --server --port 18080 --stamp C gpt-oss-20b st-hello > "$SB/suite-C.log" 2>&1
    rc=$?
    ((rc == 21)) && ok "GPU busy: refused with exit 21" || bad "GPU busy: exit $rc"
    check "GPU busy: llama-server never started, no result" bash -c "[[ ! -e '$SB/argv-C.txt' && ! -e '$M0_RUNS_ROOT/C/gpt-oss-20b/st-hello/result.json' ]]"
    check "GPU busy: the busy process is named in the log" grep -q '4242, /usr/bin/python3, 1536 MiB' "$SB/suite-C.log"
    FAKE_NVSMI_FAIL=1 "$H/run-suite.sh" --server --port 18080 --stamp C2 gpt-oss-20b st-hello > "$SB/suite-C2.log" 2>&1
    rc=$?
    ((rc == 21)) && ok "nvidia-smi failing: refused as well (fail safe)" || bad "nvidia-smi failing: exit $rc"
    # port taken -> refused
    python3 -I -c 'import socket,time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1",18080)); s.listen(); time.sleep(20)' &
    local holder=$!
    sleep 0.5
    "$H/run-suite.sh" --server --port 18080 --stamp C3 gpt-oss-20b st-hello > "$SB/suite-C3.log" 2>&1
    rc=$?
    kill "$holder" 2> /dev/null
    wait "$holder" 2> /dev/null
    ((rc == 21)) && grep -q 'already in use' "$SB/suite-C3.log" && ok "port taken: refused with exit 21" || bad "port taken: exit $rc"
    # a server that cannot start -> 20
    FAKE_LLAMA_FAIL_START=1 "$H/run-suite.sh" --server --port 18080 --stamp C4 gpt-oss-20b st-hello > "$SB/suite-C4.log" 2>&1
    rc=$?
    ((rc == 20)) && ok "llama-server failing to load: exit 20" || bad "llama-server failing to load: exit $rc"
    # unmanaged and no server -> 22
    "$H/run-suite.sh" --port 18080 --stamp C5 gpt-oss-20b st-hello > "$SB/suite-C5.log" 2>&1
    rc=$?
    ((rc == 22)) && ok "no --server and no llama-server running: exit 22" || bad "unmanaged without server: exit $rc"

    # D: deadline guard -> nothing starts
    "$H/run-suite.sh" --server --port 18080 --stamp D --timeout 60 --deadline $(($(date +%s) + 20)) gpt-oss-20b > "$SB/suite-D.log" 2>&1
    rc=$?
    ((rc == 10)) && ! grep -q 'starting llama-server' "$SB/suite-D.log" && ok "deadline guard: no task fits, exit 10, no server start" \
        || bad "deadline guard: exit $rc"

    # E: watchdog at the deadline -> interrupted (non-final); resume -> solved
    MOCK_ARGS="--fail slow" M0_START_GUARD=0 "$H/run-suite.sh" --server --port 18080 --stamp E --timeout 120 \
        --deadline $(($(date +%s) + 15)) gpt-oss-20b st-hello st-inline > "$SB/suite-E1.log" 2>&1
    rc=$?
    ((rc == 10)) && ok "watchdog: the suite stops at the deadline with exit 10" || bad "watchdog: exit $rc"
    [[ $(result_status E/gpt-oss-20b/st-hello) == interrupted ]] && ok "watchdog: the running agent was interrupted" \
        || bad "watchdog: st-hello is $(result_status E/gpt-oss-20b/st-hello)"
    check "watchdog: interrupted run is not final and kept its workspace" python3 -I -c 'import json,sys
r = json.load(open(sys.argv[1])); assert r["final"] is False and r["deadline_hit"] and r["paths"]["workspace_kept"]' "$M0_RUNS_ROOT/E/gpt-oss-20b/st-hello/result.json"
    check "watchdog: the next task was not started" test ! -e "$M0_RUNS_ROOT/E/gpt-oss-20b/st-inline"
    check "watchdog: partial trajectory copied out (events.jsonl)" test -s "$M0_RUNS_ROOT/E/gpt-oss-20b/st-hello/trajectory/events.jsonl"
    check "watchdog: llama-server stopped, no container left" bash -c "! (exec 3<>/dev/tcp/127.0.0.1/18080) 2>/dev/null && [[ -z \$(ls -A '$FAKE_ENGINE_STATE' 2>/dev/null) ]]"
    "$H/run-suite.sh" --server --port 18080 --stamp E --timeout 120 gpt-oss-20b st-hello st-inline > "$SB/suite-E2.log" 2>&1
    rc=$?
    ((rc == 0)) && ok "resume after the deadline: exit 0" || bad "resume E: exit $rc"
    [[ $(result_status E/gpt-oss-20b/st-hello) == solved && $(result_status E/gpt-oss-20b/st-inline) == solved ]] \
        && ok "resume: both tasks solved" || bad "resume: $(result_status E/gpt-oss-20b/st-hello) $(result_status E/gpt-oss-20b/st-inline)"
    check "resume: the interrupted attempt kept under _attempts/st-hello.1" test -f "$M0_RUNS_ROOT/E/gpt-oss-20b/_attempts/st-hello.1/result.json"
    check "resume: the re-run is attempt 2" bash -c "[[ \$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[\"attempt\"])' '$M0_RUNS_ROOT/E/gpt-oss-20b/st-hello/result.json') == 2 ]]"

    # F: the no-model agents through the real verify path
    "$H/run-suite.sh" --agent reference --stamp F > "$SB/suite-F1.log" 2>&1
    rc=$?
    ((rc == 0)) && [[ $(result_status F/reference/st-hello) == solved && $(result_status F/reference/st-unsolvable) == solved ]] \
        && ok "--agent reference: every fixture solved by its solve.sh" || bad "--agent reference: exit $rc"
    "$H/run-suite.sh" --agent none --stamp F > "$SB/suite-F2.log" 2>&1
    rc=$?
    ((rc == 0)) && [[ $(result_status F/unsolved/st-hello) == failed && $(result_status F/unsolved/st-unsolvable) == failed ]] \
        && ok "--agent none: every fixture fails on its base" || bad "--agent none: exit $rc"
    check "reference diff.patch recorded" grep -q '^+bye' "$M0_RUNS_ROOT/F/reference/st-unsolvable/trajectory/diff.patch"

    # H: the optional Claude Code --bare harness, with the owner's local binary when it is there
    local cc=$HOME_REAL/.local/share/claude/versions/2.1.311
    if [[ -x $cc ]] && echo "86dc1fcc69498159eaf15f446613e847010f9036bfbbdffd4c49350be8489ada  $cc" | sha256sum -c --status - 2> /dev/null; then
        FAKE_CLAUDE=$cc "$H/run-suite.sh" --server --port 18080 --stamp H --harness claude --timeout 120 gpt-oss-20b st-hello > "$SB/suite-H.log" 2>&1
        rc=$?
        d=$M0_RUNS_ROOT/H/gpt-oss-20b.claude-code/st-hello
        ((rc == 0)) && [[ $(result_status H/gpt-oss-20b.claude-code/st-hello) == solved ]] \
            && ok "--harness claude: Claude Code 2.1.311 --bare solved st-hello via /v1/messages" || bad "--harness claude: exit $rc"
        check "claude: stream.jsonl, transcripts, meta and the Bash tool in the summary" python3 -I -c 'import json,sys,os
d = sys.argv[1]; r = json.load(open(os.path.join(d, "result.json")))
assert os.path.getsize(os.path.join(d, "trajectory/stream.jsonl")) > 0 and r["harness"] == "claude"
assert r["summary"]["tools"].get("Bash", 0) >= 2, r["summary"]' "$d"
        check "claude: only /v1/messages traffic, model = the alias" python3 -I -c 'import json,sys
rows = [json.loads(l) for l in open(sys.argv[1])]
paths = {r["path"].split("?")[0] for r in rows}
assert paths <= {"/v1/messages", "/health", "/v1/messages/count_tokens"}, paths
assert all(r["request"]["model"] == "gpt-oss-20b" for r in rows if r["path"].startswith("/v1/messages"))' "$d/trajectory/llm.jsonl"
    else
        echo "  skip  --harness claude: no Claude Code 2.1.311 with the pinned sha256 in ~/.local/share/claude/versions"
    fi

    # I: llama-server crashes during a run -> infra_error, restart, one more attempt -> solved
    rm -f "$SB/died"
    MOCK_ARGS="--die-after 1 --die-marker $SB/died" "$H/run-suite.sh" --server --port 18080 --stamp I \
        --timeout 25 gpt-oss-20b st-hello > "$SB/suite-I.log" 2>&1
    rc=$?
    ((rc == 0)) && [[ $(result_status I/gpt-oss-20b/st-hello) == solved ]] \
        && ok "server crash mid-run: restarted, task re-run and solved" || bad "server crash: exit $rc, $(result_status I/gpt-oss-20b/st-hello)"
    [[ $(result_status I/gpt-oss-20b/_attempts/st-hello.1) == infra_error ]] \
        && ok "server crash: the broken attempt is kept as infra_error" || bad "server crash: first attempt $(result_status I/gpt-oss-20b/_attempts/st-hello.1)"
    check "server crash: restart logged" grep -q 'restart 1 of 2' "$SB/suite-I.log"

    # J: TERM to a running suite -> agent stopped, run interrupted, server stopped, exit 130
    MOCK_ARGS="--fail slow" "$H/run-suite.sh" --server --port 18080 --stamp J --timeout 120 gpt-oss-20b st-hello st-inline \
        > "$SB/suite-J.log" 2>&1 &
    local sp=$! i
    for ((i = 0; i < 60; i++)); do grep -q 'agent: gpt-oss-20b' "$SB/suite-J.log" 2> /dev/null && break; sleep 0.5; done
    sleep 3
    kill -TERM "$sp"
    rc=0
    wait "$sp" || rc=$?
    ((rc == 130)) && [[ $(result_status J/gpt-oss-20b/st-hello) == interrupted ]] \
        && ok "TERM to the suite: exit 130, run interrupted" || bad "TERM to the suite: exit $rc, $(result_status J/gpt-oss-20b/st-hello)"
    check "TERM to the suite: server stopped, no container, next task not started" bash -c \
        "! (exec 3<>/dev/tcp/127.0.0.1/18080) 2>/dev/null && [[ -z \$(ls -A '$FAKE_ENGINE_STATE' 2>/dev/null) ]] && [[ ! -e '$M0_RUNS_ROOT/J/gpt-oss-20b/st-inline' ]]"
    check "no stray agent processes" bash -c "! pgrep -f -- '[-]-title st-hello' > /dev/null"   # [-]: not this command

    # G: a setup failure is recorded as setup_error (not final)
    mkdir -p "$SB/tasks-bad/st-broken"
    cp -r "$M0_TASKS_ROOT/_common" "$SB/tasks-bad/"
    printf '#!/usr/bin/env bash\necho "cannot build" >&2\nexit 1\n' > "$SB/tasks-bad/st-broken/setup.sh"
    cp "$M0_TASKS_ROOT/st-hello/verify.sh" "$M0_TASKS_ROOT/st-hello/prompt.txt" "$SB/tasks-bad/st-broken/"
    printf '{"id": "st-broken", "role": "selected", "verify_seconds": 1}\n' > "$SB/tasks-bad/st-broken/task.json"
    "$H/run-suite.sh" --agent reference --tasks-root "$SB/tasks-bad" --stamp G > "$SB/suite-G.log" 2>&1
    rc=$?
    ((rc == 11)) && [[ $(result_status G/reference/st-broken) == setup_error ]] && ok "setup failure: setup_error, exit 11" \
        || bad "setup failure: exit $rc, $(result_status G/reference/st-broken)"
}

stage_night() {
    echo "== 5 night plan (two nights of a campaign with the default plan)"
    local rc c
    export HOME=$SB/home
    mkdir -p "$HOME"
    export M0_RUNS_ROOT=$SB/night-runs M0_WORK_ROOT=$SB/night-work
    # night 1: 40 s of window, each run ~4-6 s, guard = timeout 15 + 15 s of estimates
    MOCK_ARGS="--delay 1" "$H/run-night.sh" --new --no-inhibit --night-date 2026-11-01 --deadline-in 40 \
        -- --port 18080 --timeout 15 --no-tap > "$SB/night-1.log" 2>&1
    rc=$?
    c=$(ls "$M0_RUNS_ROOT" | sed -n 1p)
    ((rc == 0)) && [[ -f $M0_RUNS_ROOT/$c/campaign.json ]] && ok "night 1 ran (campaign $c)" || bad "night 1: exit $rc"
    local n1 qn
    n1=$(find "$M0_RUNS_ROOT/$c" -name result.json | wc -l)
    qn=$(find "$M0_RUNS_ROOT/$c/qwen3.8-27b" -name result.json 2> /dev/null | wc -l)
    ((n1 >= 1 && n1 < 6)) && ok "night 1 stopped at its deadline after $n1 of 6 phase-1 runs" || bad "night 1 made $n1 runs"
    ((qn == 0)) && ok "night 1 did not start phase 2 (qwen3.8-27b)" || bad "night 1 started qwen ($qn runs)"
    check "night 1 recorded as 'deadline'" python3 -I -c 'import json,sys
c = json.load(open(sys.argv[1])); assert c["nights"][-1]["result"] == "deadline" and not c["complete"]' "$M0_RUNS_ROOT/$c/campaign.json"
    check "night 1: no llama-server left running" bash -c "! (exec 3<>/dev/tcp/127.0.0.1/18080) 2>/dev/null"
    # night 2: generous window; resumes phase 1, then phase 2
    MOCK_ARGS="--delay 0" "$H/run-night.sh" --no-inhibit --night-date 2026-11-02 --deadline-in 900 \
        -- --port 18080 --timeout 15 --no-tap > "$SB/night-2.log" 2>&1
    rc=$?
    ((rc == 0)) && ok "night 2 exit 0" || bad "night 2: exit $rc"
    check "campaign complete after night 2: 9 final results (3 models x 3 tasks)" python3 -I -c 'import json,sys,glob,os
c = json.load(open(sys.argv[1])); assert c["complete"], c
rs = [json.load(open(p)) for p in glob.glob(os.path.join(os.path.dirname(sys.argv[1]), "*", "st-*", "result.json"))]
assert len(rs) == 9 and all(r["final"] for r in rs), len(rs)
assert sorted(r["status"] for r in rs).count("solved") == 6' "$M0_RUNS_ROOT/$c/campaign.json"
    check "no task ran twice to a final result" bash -c "! find '$M0_RUNS_ROOT/$c' -path '*_attempts*' -name result.json | xargs -r grep -l '\"final\": true' | grep -q ."   # lint: ok (bash -c: no pipefail)
    "$H/run-night.sh" --status --campaign "$c" > "$SB/night-status.txt" 2>&1
    check "run-night --status shows every model complete" bash -c "[[ \$(grep -c '3/3 final' '$SB/night-status.txt') == 3 ]]"
    "$H/run-night.sh" --no-inhibit --deadline-in 60 > "$SB/night-3.log" 2>&1
    rc=$?
    ((rc == 0)) && grep -q 'no unfinished campaign' "$SB/night-3.log" && ok "a third night finds nothing to do (exit 0)" || bad "night 3: exit $rc"
    # TERM to a running night -> forwarded to the suite, the night recorded as interrupted
    export M0_RUNS_ROOT=$SB/night-runs-2 M0_WORK_ROOT=$SB/night-work-2
    MOCK_ARGS="--fail slow" "$H/run-night.sh" --new --no-inhibit --night-date 2026-11-03 --deadline-in 600 \
        -- --port 18080 --timeout 120 --no-tap > "$SB/night-4.log" 2>&1 &
    local np=$! i
    for ((i = 0; i < 60; i++)); do grep -q 'agent: gpt-oss-20b' "$SB/night-4.log" 2> /dev/null && break; sleep 0.5; done
    sleep 3
    kill -TERM "$np"
    rc=0
    wait "$np" || rc=$?
    c=$(ls "$M0_RUNS_ROOT" | sed -n 1p)
    ((rc == 130)) && ok "TERM to the night: exit 130" || bad "TERM to the night: exit $rc"
    check "TERM to the night: recorded as interrupted, server stopped" bash -c "python3 -I -c 'import json,sys
c = json.load(open(sys.argv[1])); assert c[\"nights\"][-1][\"result\"] == \"interrupted\"' '$M0_RUNS_ROOT/$c/campaign.json' && ! (exec 3<>/dev/tcp/127.0.0.1/18080) 2>/dev/null"
}

[[ -z $ONLY || $ONLY == profiles ]] && stage_profiles
[[ -z $ONLY || $ONLY == dryrun ]] && stage_dryrun
if ((!QUICK)); then
    if [[ ! -x $OC_BIN ]]; then
        echo "  skip  stages 4-5: no pinned OpenCode at $OC_BIN (run harness/prepare-image.sh fetch)"
    else
        [[ -z $ONLY || $ONLY == e2e ]] && stage_e2e
        [[ -z $ONLY || $ONLY == night ]] && stage_night
    fi
fi
echo "== $PASSES passed, $FAILS failed (sandbox $SB)"
exit $((FAILS > 0))
