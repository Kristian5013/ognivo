#!/usr/bin/env bash
# Stand-in for llama-server in tests/selftest.sh (LLAMA_SERVER_BIN=this script). It accepts the
# full llama-server command line that run-suite.sh builds, writes it to FAKE_LLAMA_ARGV (one
# argument per line), and serves scripts/mock_llama.py on its --host/--port with its --alias,
# -m (reported as /props model_path) and --chat-template-file (reported as /props chat_template),
# so the server checks of run-suite.sh run unchanged.
# Env: FAKE_LLAMA_ARGV, FAKE_LLAMA_LOG (request log), FAKE_LLAMA_FAIL_START=1 (exit at once),
#      MOCK_ARGS (extra mock_llama.py options, e.g. "--delay 1" or "--fail slow")
set -euo pipefail
H=$(cd "$(dirname "$(realpath "$0")")/.." && pwd)
all=("$@")
host=127.0.0.1 port=8080 alias=model tmpl= model=
while (($#)); do
    case $1 in
        --host) host=$2; shift ;;
        --port) port=$2; shift ;;
        -a | --alias) alias=$2; shift ;;
        --chat-template-file) tmpl=$2; shift ;;
        -m | --model) model=$2; shift ;;
    esac
    shift
done
if [[ -n ${FAKE_LLAMA_ARGV:-} ]]; then printf '%s\n' "${all[@]}" > "$FAKE_LLAMA_ARGV"; fi
if [[ ${FAKE_LLAMA_FAIL_START:-0} == 1 ]]; then
    echo "fake-llama-server: failed to load the model" >&2
    exit 1
fi
opt=()
[[ -n $tmpl ]] && opt+=(--chat-template-file "$tmpl")
[[ -n $model ]] && opt+=(--model-path "$model")
# shellcheck disable=SC2086
exec python3 -I "$H/scripts/mock_llama.py" --host "$host" --port "$port" --alias "$alias" "${opt[@]}" \
    --log "${FAKE_LLAMA_LOG:-/dev/null}" ${MOCK_ARGS:-}
