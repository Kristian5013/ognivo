#!/usr/bin/env bash
# Stand-in for `podman` in tests/selftest.sh. It runs the image's entrypoint (container/m0-entry.sh
# with the pinned OpenCode from cache/, or verify.sh) straight on the host, in a throw-away
# directory, with the container paths (/work, /out, /tmp, /root, the -v mounts) mapped to host
# paths and the image's environment recreated (harness config/opencode.json as the OpenCode
# config, the image's git identity m0-agent <agent@localhost>).
# It implements just what run-task.sh and verify-task.sh use: image inspect, run, cp, rm, stop.
# It isolates nothing by itself: selftest.sh runs it inside bwrap with / read-only, a
# loopback-only network namespace and no GPU device nodes.
#
# Env: FAKE_ENGINE_STATE  (required) directory for the "containers"
#      FAKE_ENGINE_LOG    append every invocation (argv) here
#      FAKE_ENGINE_NO_IMAGE=1   `image inspect` fails (no image built)
#      FAKE_OPENCODE      the opencode binary (default <m0-baseline>/cache/opencode/2.2.1/bin/opencode)
#      FAKE_CLAUDE        a Claude Code binary, put on PATH as `claude` (HARNESS=claude runs)
#      FAKE_OGNIVO        an ognivo binary, put on PATH as `ognivo` (HARNESS=ognivo runs)
set -euo pipefail
HERE=$(cd "$(dirname "$(realpath "$0")")" && pwd)
H=$(dirname "$HERE")
# shellcheck source=../lib/common.sh
source "$H/lib/common.sh"
STATE=${FAKE_ENGINE_STATE:?set FAKE_ENGINE_STATE}
mkdir -p "$STATE"
if [[ -n ${FAKE_ENGINE_LOG:-} ]]; then
    { printf '%q ' "$@"; echo; } >> "$FAKE_ENGINE_LOG"
fi
OC_BIN=${FAKE_OPENCODE:-$M0_CACHE_DIR/opencode/$M0_OC_VERSION/bin/opencode}

kill_tree() {   # kill_tree SIG PID: the process and all its descendants (no PID namespace here)
    local sig=$1 pid=$2 kids k
    kids=$(ps -o pid= --ppid "$pid" 2> /dev/null || true)
    for k in $kids; do kill_tree "$sig" "$k"; done
    kill "-$sig" "$pid" 2> /dev/null || true
}

cmd=${1:-}
shift || true
case $cmd in
    image)   # image inspect --format '{{.Id}}' IMAGE
        if [[ ${FAKE_ENGINE_NO_IMAGE:-0} == 1 ]]; then
            echo "Error: fake-engine: no such image" >&2
            exit 125
        fi
        echo "sha256:fake-engine-image"
        exit 0
        ;;
    cp)   # cp NAME:/out/. DEST
        src=$1 dst=$2
        name=${src%%:*} path=${src#*:}
        [[ -d $STATE/$name/root ]] || { echo "Error: fake-engine: no container $name" >&2; exit 125; }
        cp -a "$STATE/$name/root${path%/.}/." "$dst"
        exit 0
        ;;
    rm)   # rm -f NAME
        [[ ${1:-} == -f ]] && shift
        name=$1
        if [[ -f $STATE/$name/pid ]]; then kill_tree KILL "$(cat "$STATE/$name/pid")"; fi
        rm -rf "${STATE:?}/$name"
        exit 0
        ;;
    stop)   # stop -t N NAME
        [[ ${1:-} == -t ]] && shift 2
        name=$1
        if [[ -f $STATE/$name/pid ]]; then kill_tree TERM "$(cat "$STATE/$name/pid")"; fi
        exit 0
        ;;
    run) ;;
    *)
        echo "fake-engine: unsupported command $cmd" >&2
        exit 125
        ;;
esac

name= rm=0 timeout_s= entrypoint=
envs=() msrc=() mdst=()
while (($#)); do
    case $1 in
        --name) name=$2; shift ;;
        -i) ;;
        --rm) rm=1 ;;
        --network | --cpus | --memory | --memory-swap | --pids-limit | --cap-drop | --security-opt | --pull) shift ;;
        --timeout) timeout_s=$2; shift ;;
        -v)
            IFS=: read -r s d _ <<< "$2"
            msrc+=("$s") mdst+=("$d")
            shift
            ;;
        -e) envs+=("$2"); shift ;;
        --entrypoint) entrypoint=$2; shift ;;
        -*) echo "fake-engine: unsupported run option $1" >&2; exit 125 ;;
        *) break ;;
    esac
    shift
done
(($#)) || { echo "fake-engine: no image" >&2; exit 125; }
shift   # the image
args=("$@")
[[ -n $name ]] || name=fake-$$
root=$STATE/$name/root
[[ -e $STATE/$name ]] && { echo "Error: fake-engine: container $name exists" >&2; exit 125; }
mkdir -p "$root/out" "$root/tmp" "$root/root/.config/opencode" "$root/root/.local/share" \
    "$root/root/.cache" "$root/root/.local/state"
cp "$H/config/opencode.json" "$root/root/.config/opencode/opencode.json"
printf '[user]\n\tname = m0-agent\n\temail = agent@localhost\n[safe]\n\tdirectory = *\n' > "$root/gitconfig"

map() {   # a container path -> its host path
    local p=$1 i d
    for i in "${!mdst[@]}"; do
        d=${mdst[$i]}
        if [[ $p == "$d" || $p == "$d"/* ]]; then
            printf '%s\n' "${msrc[$i]}${p#"$d"}"
            return
        fi
    done
    case $p in
        /out | /out/* | /tmp | /tmp/* | /root | /root/*) printf '%s\n' "$root$p" ;;
        *) printf '%s\n' "$p" ;;
    esac
}
prog() {
    case $1 in
        /usr/local/bin/m0-entry) echo "$H/container/m0-entry.sh" ;;
        /usr/local/bin/m0-entry-claude) echo "$H/container/m0-entry-claude.sh" ;;
        /usr/local/bin/m0-entry-ognivo) echo "$H/container/m0-entry-ognivo.sh" ;;
        opencode | /usr/local/bin/opencode) echo "$OC_BIN" ;;
        *) echo "$1" ;;
    esac
}
if [[ -z $entrypoint ]]; then
    argv=("$H/container/m0-entry.sh" "${args[@]}")
elif [[ $entrypoint == /usr/bin/tini ]]; then
    [[ ${args[0]:-} == -- ]] && args=("${args[@]:1}")
    argv=("$(prog "${args[0]}")" "${args[@]:1}")
else
    argv=("$(prog "$entrypoint")" "${args[@]}")
fi
for i in "${!argv[@]}"; do argv[i]=$(map "${argv[i]}"); done
menv=()
for kv in "${envs[@]}"; do menv+=("${kv%%=*}=$(map "${kv#*=}")"); done
work=$(map /work)
[[ -d $work ]] || work=$root

# the claude target: FAKE_CLAUDE (a Claude Code binary) appears as `claude` on PATH
mkdir -p "$root/bin"
[[ -n ${FAKE_CLAUDE:-} ]] && ln -s "$FAKE_CLAUDE" "$root/bin/claude"
[[ -n ${FAKE_OGNIVO:-} ]] && ln -s "$FAKE_OGNIVO" "$root/bin/ognivo"
base_env=(PATH="$root/bin:$(dirname "$OC_BIN"):/usr/local/bin:/usr/bin:/bin" HOME="$root/root"
    XDG_CONFIG_HOME="$root/root/.config" XDG_DATA_HOME="$root/root/.local/share"
    XDG_CACHE_HOME="$root/root/.cache" XDG_STATE_HOME="$root/root/.local/state"
    OPENCODE_DISABLE_AUTOUPDATE=1 OPENCODE_DISABLE_MODELS_FETCH=1 OPENCODE_DISABLE_FREE_MODELS=1
    LLAMA_BASE_URL=http://127.0.0.1:8080/v1 TERM=dumb NO_COLOR=1 LANG=C.UTF-8 TMPDIR="$root/tmp"
    GIT_CONFIG_GLOBAL="$root/gitconfig" GIT_CONFIG_NOSYSTEM=1
    M0_OUT_DIR="$root/out" M0_WORK_DIR="$work")
prefix=()
[[ -n $timeout_s ]] && prefix=(timeout -k 5 "$timeout_s")

child=
on_signal() {   # the sig-proxy of `podman run`
    [[ -n $child ]] && kill_tree TERM "$child"
    return 0
}
trap on_signal TERM INT HUP
exec 3<&0
(cd "$work" && exec "${prefix[@]}" env -i "${base_env[@]}" "${menv[@]}" "${argv[@]}") <&3 &
child=$!
echo "$child" > "$STATE/$name/pid"
while :; do
    rc=0
    wait "$child" || rc=$?
    if ((rc > 128)) && kill -0 "$child" 2> /dev/null; then continue; fi
    break
done
rm -f "$STATE/$name/pid"
((rm)) && rm -rf "${STATE:?}/$name"
exit "$rc"
