# M0 baseline harness (ognivo)

The M0 baseline of `docs/architecture.md` §11 and §13: OpenCode v2.2.1, run headless (and
optionally Claude Code 2.1.311 `--bare`), works on the 10 selected tasks of the suite × 3 local
models. All of them use the same pinned llama-server. The trajectories feed the failure
catalogue.

Everything lives under `/home/kristian/.local/share/ognivo/m0-baseline`. That path has no spaces,
so it can be bind-mounted into containers. All commands below run from there
(`cd /home/kristian/.local/share/ognivo/m0-baseline`):

| directory  | contents                                                                                             |
|------------|------------------------------------------------------------------------------------------------------|
| `harness/` | this directory, sources only (scripts, Containerfile, configs, tests)                                |
| `tasks/`   | the task packages (`task.json`, `setup.sh`, `verify.sh`, `hidden/` ...): never visible to the agent  |
| `cache/`   | pinned downloads (`cache/opencode/2.2.1/`) and the image build context                               |
| `runs/`    | results: `runs/<UTC stamp>/<model>/<task-id>/{result.json,trajectory/,logs/}`                       |
| `work/`    | workspaces, one fresh copy per run (finished ones are deleted)                                       |

## How one run works

1. **Setup on the host.** The task's `setup.sh` builds a fresh workspace (`work/<stamp>/<model>/<task>`).
   It runs with `GIT_CONFIG_GLOBAL=/dev/null`, so the owner's git identity cannot reach the
   workspace. The task scripts commit with their own anonymous identities (`ognivo-eval`,
   `flowcoin-task`, `m0-eval`). Then `scripts/normalize_ws.py` removes what would point at the
   injected bug: the builders apply the mutation diff after unpacking, so the touched files were
   the only ones with a fresh mtime and the highest inodes (`ls -lt` found the bug), and a tree
   written before the diff stayed in `.git` as an unreachable object (`git fsck` showed it). All
   tracked files are checked out again in index order with one mtime (2026-10-01), and
   `git prune --expire=now` drops unreachable objects.
2. **Agent container** (`run-task.sh`). This is a fresh rootless Podman container whose only mount
   is the workspace at `/work`. Its network is `pasta:--splice-only,-T,<port>`: the container has
   only `lo`, and `127.0.0.1:<port>` inside it is the host's `127.0.0.1:<port>`. Nothing else is
   reachable, DNS included. Limits are 8 CPUs, 16 GB RAM with no swap and 4096 pids, with
   `--cap-drop all` and `no-new-privileges`. The prompt goes in on stdin, and
   `opencode run --standalone --format json --thinking --auto --title <task> --model llama/<alias>`
   runs under `timeout`. After it the container exports the session(s), the diff against the base
   commit, the database and the logs to `/out`, which the host copies out; symlinks, FIFOs and
   devices the agent may have left there are dropped before any host tool reads them. A disk
   guard stops the container when the run has used 40 GB (`DISK_BUDGET_GB`, the agent's fault:
   `agent_error`) or when less than 30 GB is left and still falling (`DISK_FLOOR_GB`:
   `infra_error`); `/work` and the writable layer have no quota otherwise. An OOM kill in the
   container is recorded (`oom_killed`) and is never scored as a timeout.
3. **Recording proxy** (on by default). `scripts/llm_tap.py` on `127.0.0.1:<port+1>` sits between
   the container and llama-server. It saves every raw request, the SSE answer and llama-server's
   `timings` (`cache_n`, `prompt_n`, `predicted_per_second` ...) in `trajectory/llm.jsonl`. It
   writes into `logs/` during the run and the file is moved into `trajectory/` last, so a
   `/out/llm.jsonl` from the agent cannot replace it. It refuses request bodies over 64 MiB and
   more than 256 open connections: it runs on the host, outside the container's limits.
4. **Hidden check** (`verify-task.sh`). The agent's container is gone by now. A second container of
   the same image, with `--network none`, mounts the workspace at `/work` and the `tasks/` tree
   read-only at its own absolute path, then runs `verify.sh /work`: 0 = solved, 1 = not solved,
   2 = the check could not run.
5. **result.json.** It holds the classified status plus a summary of the trajectory: steps, tool
   calls, failed calls, tokens, compactions, leaked tool-call markup and loop flags.

## One-time setup

1. **Podman and pasta.** `sudo apt install podman passt`.
   When this harness was drafted, Podman was **not** installed on this host. The dpkg log shows
   `podman 5.8.6+ds1-2` and `passt 0.0~git20260728.f8df3f1-1` installed at 2026-10-30 03:32 KST,
   so the command may already be a no-op. Check it:

   ```sh
   podman --version                        # 5.8.6
   pasta --help | grep -c -- --splice-only # 1: the network isolation depends on it
   grep kristian /etc/subuid /etc/subgid   # kristian:100000:65536 (already set)
   ```

   Rootless Docker 29.9.2 is installed too, but its rootlesskit runs with
   `--disable-host-loopback`: its containers cannot reach a llama-server bound to 127.0.0.1. It is
   good for smoke tests only (`ENGINE=docker NETWORK=<an --internal network>`); real runs need
   Podman.
2. **Linger**, only for runs started by a timer or cron without a login session:
   `loginctl enable-linger kristian`. It was already `Linger=yes` on 2026-10-30.
3. **Build the image** (CPU only, `nice -n 10`; the build needs the network for the Ubuntu
   archive, rustup and the PyTorch CPU index):

   ```sh
   harness/prepare-image.sh --dry-run  # what it will do; checks the inputs
   harness/prepare-image.sh            # = build full -> localhost/m0-opencode:2.2.1-full
   harness/prepare-image.sh claude     # optional: + Claude Code -> localhost/m0-opencode:2.2.1-claude
   ```

   - OpenCode comes from its pinned release URL, checked by sha256, into `cache/opencode/2.2.1/`.
     It is cached already: it was fetched once, offline, from an identical verified copy, so
     there is nothing to download.
   - The Rust crates are a copy of `~/.cargo/{registry/index,registry/cache,git/db}` (about 200 MB).
     They are baked into the image because every build in the tasks is `--offline`.
   - **Claude Code** (target `claude` only) is the owner's installed binary,
     `~/.local/share/claude/versions/2.1.311`, which is what `~/.local/bin/claude` points to.
     It must match the sha256 of the official 2.1.311 manifest (`86dc1fcc…`). It is copied
     only into the local build context and the local image, then blanked in the context. It is
     never downloaded and never uploaded anywhere. If that version is gone after an
     auto-update, point `CC_BIN` at another copy of 2.1.311; the checksum is still enforced.
   - Targets: `opencode` is OpenCode, git, ripgrep, python3 and tini. `full` adds gcc, cmake,
     ninja, clang-format 22, valgrind with its headers, libudev-dev, Rust 1.99.0 with clippy and
     rustfmt, pytest, torch 2.13.0 CPU and the crates. `claude` is `full` plus Claude Code.
4. **Check everything that needs no GPU:**

   ```sh
   harness/tests/selftest.sh           # ~3 min; no Podman, no GPU, no network (bwrap sandbox)
   harness/tests/container-smoke.sh    # Podman: network isolation probe + the fixture tasks
   harness/tests/container-smoke.sh --reference   # every task's reference passes and every
                                                  # base fails in the image (toolchains); 10-20 min
   ```

## Run one task

With the harness starting and stopping the pinned llama-server itself:

```sh
harness/run-suite.sh --server gpt-oss-20b cforge-timefmt-february
```

Against a llama-server that already runs on 127.0.0.1:8090 (the default port in config/models.json; 8080 is taken by the owner's Open WebUI) with `--alias gpt-oss-20b`, leave out
`--server`. The suite checks `/props` and `/v1/models` first: the alias and the model file must
match, and the served chat template must be the profile's canonical one.

Lower level, a single agent run, with or without its hidden check:

```sh
VERIFY_DIR=tasks/cforge-timefmt-february AGENT_TIMEOUT=480 \
  harness/run-task.sh cforge-timefmt-february llama/gpt-oss-20b <fresh workspace> <prompt file> <new run dir>
harness/verify-task.sh tasks/<id> <workspace> <run dir>      # the hidden check on its own
```

## Run the suite

```sh
harness/run-suite.sh --dry-run --server gpt-oss-20b   # the plan: tasks, server and container commands
harness/run-suite.sh --server gpt-oss-20b             # the 10 tasks whose task.json role is "selected"
harness/run-suite.sh --server --stamp <stamp> gpt-oss-20b   # resume: final results are kept
```

- **Models.** `gpt-oss-20b`, `gemma-4-26b-a4b` and `qwen3.8-27b`, defined in `config/models.json`.
  The llama-server command is built from `/home/kristian/Рабочий стол/cforge/profiles/<model>.toml`
  by `scripts/llama_cmd.py`:
  - `[server].recommended_flags`, with `--chat-template-file` made absolute under `profiles/templates/`
    (that file's sha256 must equal `[template].file_sha256`);
  - sampling from `[sampling.main]` and reasoning from `[reasoning]`, repeated on the server
    because Claude Code sends none;
  - the fixed `--jinja --metrics --no-webui --log-timestamps --host 127.0.0.1 --port 8090` (config/models.json `port`).
  The binary is `~/.cache/llama-pinned/build-12cc59ba4914/bin/llama-server`. The model files are on
  `/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e` (that disk must be mounted).
- **GPU guard.** `--server` refuses to start when
  `nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader` lists any
  process, or when nvidia-smi itself fails. It also refuses when the port is taken. The server
  runs under `setpriv --pdeathsig TERM`, so it dies with the suite even after a SIGKILL. A dead
  server is restarted (at most 2 times per suite) and the task that hit it is re-run once.
- **Timeouts** are per task: `--timeout`, else `task.json` `agent_timeout_s`, else the model's
  value (480 s for gpt-oss and Gemma, 1500 s for Qwen). The verify limit is 900 s and setup's is
  900 s.
- `--deadline HH:MM|epoch`: a task starts only if its agent timeout plus setup and verify
  estimates plus 60 s fit before the deadline. An agent still running at the deadline is stopped
  and its run marked `interrupted`; the server stops then too.
- `--no-tap` (no `llm.jsonl`), `--keep-ws` (keep finished workspaces), `--verify-env K=V`
  (for example `FC_SKIP_FORMAT=1`), `--harness claude` (results under `<model>.claude-code/`),
  `--agent reference|none`: the task's `solve.sh`, or no change, followed by the real verify
  container. No model is involved; results go under `reference/` and `unsolved/`.
- **Exit codes:**

  | code | meaning                                                           |
  |------|-------------------------------------------------------------------|
  | 0    | all final                                                         |
  | 10   | stopped at the deadline (tasks left)                              |
  | 11   | all ran, some results not final                                   |
  | 20   | llama-server did not start, or does not match the profile         |
  | 21   | GPU or port busy                                                  |
  | 22   | llama-server down and not restartable                             |
  | 3    | another suite is running                                          |
  | 130  | interrupted                                                       |

## Nights (03:00–06:00 local time)

`run-night.sh` runs the plan inside the GPU window:

- **night 1:** `gpt-oss-20b`, then `gemma-4-26b-a4b`, over the 10 selected tasks;
- **night 2:** `qwen3.8-27b`.

A campaign is one `runs/<stamp>/`, and every night writes into it. Night *k* first finishes what
the earlier nights left, then runs its own phase (`--greedy` lets it start later phases too).

```sh
harness/run-night.sh --dry-run --new   # what tonight would run, with the server commands
harness/run-night.sh --new             # evening, e.g. in tmux: waits for 03:00, runs night 1
harness/run-night.sh                   # the next evening: resumes the newest unfinished campaign
harness/run-night.sh --new --until-done   # or: one process that sleeps through the days
harness/run-night.sh --new --create-only  # only create the campaign (for a timer, below)
harness/run-night.sh --status          # progress and the result table
```

- **Clean stop at 06:00.** No agent starts that cannot finish in time. An agent that is still
  running at 06:00 is stopped: its partial trajectory is kept, the run is marked `interrupted`
  and is re-run on the next night. The llama-server is shut down. A verify that is still running
  (CPU only) may finish a few minutes later.
- **Budget.** The worst case for night 1 is about 200 min for a 180 min window (20 runs × agent
  timeout + ~2 min). What does not fit moves to the next night.
- **Started inside the window** (for example at 04:20), a night starts at once and ends at 06:00.
  **Started outside it**, the night waits; the wait follows the wall clock, so it survives suspend.
  A night whose window passed while the machine slept is recorded as `missed`, not as an
  ordinary `deadline` night.
- **Keeping the machine awake.** From `--lock-lead` hours before the window (default 8, i.e.
  from 19:00) until the window ends, a background `systemd-inhibit` holds
  `sleep:idle:handle-lid-switch`. The lid-switch part matters: with logind's default
  `LidSwitchIgnoreInhibited=yes` a plain `sleep` lock does not stop `HandleLidSwitch=suspend`,
  and this laptop suspends on a closed lid also on AC. With the lock a closed lid does not
  suspend it, but three hours of full GPU load with the lid closed run hotter: keep it open if
  you can, and stay on AC (the budget is wall-clock). The lock is never held across days, so a
  closed lid during the day suspends the laptop as usual, also under `--until-done`.
- **Pre-flight.** At launch and again when the window opens, one `PRE-FLIGHT OK|FAILED` line
  in `night.log` checks the image, pasta, the model files, ports 8090/8091, the GPU, AC, the
  lid, the load and the free disk; a failure also shows a desktop notice (`M0_NOTIFY=0` turns
  it off). Started in the evening, that leaves time to fix it before 03:00.
- If the GPU or the port is still busy when a suite starts (another model server, a forgotten
  smoke run), the suite waits up to 30 min for it (`GPU_RETRIES` × `GPU_RETRY_SLEEP`, never past
  the deadline); only then the night stops with `gpu-busy` and touches nothing.
- **Other jobs on this machine** that can collide with the window: the old
  `cforge-nightly.timer` (03:00, `WakeSystem=true`) still fires every night; its `run.sh` is
  gone with the old tree, so it fails at once, but `systemctl --user disable --now
  cforge-nightly.timer` removes the question. `vllm-muse-glimmer.service` and
  `vllm-gpt-oss-20b.service` (`--gpu-memory-utilization 0.95`) must not run during a night;
  Open WebUI holds 127.0.0.1:8080, which is why the suite uses 8090.
- **Unattended, with a user timer** (needs linger):

  ```ini
  # ~/.config/systemd/user/m0-night.service
  [Unit]
  Description=ognivo M0 baseline night
  [Service]
  # --max-wait 900: a timer that fires late (on resume at 08:00) exits instead of waiting 19 h
  ExecStart=/home/kristian/.local/share/ognivo/m0-baseline/harness/run-night.sh --max-wait 900
  # the default KillMode=control-group sends TERM to every process of the night at once
  # (systemd-inhibit does not forward signals); each script then stops its part cleanly
  TimeoutStopSec=240

  # ~/.config/systemd/user/m0-night.timer
  [Timer]
  OnCalendar=*-*-* 02:55:00
  AccuracySec=1s
  [Install]
  WantedBy=timers.target
  ```

  Create the campaign once with `harness/run-night.sh --new --create-only`, then
  `systemctl --user enable --now m0-night.timer`. A timer started at 02:55 takes the lock only
  then, so with the timer the laptop must be awake at 02:55 (lid open, or started from an
  evening `run-night.sh` instead). Once the campaign is complete, the timer's runs report that no
  unfinished campaign is left and exit 0.

## Reading results

```
runs/<stamp>/
  campaign.json, night.log                 (nights only)
  <model>/
    suite.log                              what the suite did, in order
    llama-server.cmd / .argv / .log        the exact server command and its log
    server.json, server-props.json         /props check: build, n_ctx, slots, template verdict
    results.jsonl                          one result.json per line
    _attempts/<task>.<n>/                  earlier, unfinished attempts (interrupted, infra ...)
    <task-id>/
      result.json
      trajectory/  prompt.txt events.jsonl session.json session-<child>.json llm.jsonl
                   diff.patch diff.src.patch (without new build output) agent_commits.txt
                   base_commit git_status.txt meta.json exit_code
                   (Claude Code: stream.jsonl claude-projects/)
      logs/        run.env suite.env setup.log run-task.log container.stdout/.stderr
                   opencode.stderr export.stderr opencode-data/ (db + logs) tap.stderr
                   verify.log verify_exit verify-task.log
```

```sh
python3 -I harness/scripts/summarize_run.py --table runs/<stamp>          # model x task matrix
python3 -I harness/scripts/summarize_run.py --table runs/<stamp> --json   # the same as JSON lines
python3 -I harness/scripts/summarize_run.py runs/<stamp>/*/[!_]*/ > catalogue.jsonl   # one row per run
jq -r '[.task_id, .status, .agent.exit, .verify.exit, .summary.steps, ((.summary.flags // []) | join(","))] | @tsv' \
  runs/<stamp>/gpt-oss-20b/results.jsonl
```

`result.json` (`schema: ognivo.m0.result/1`) holds `status`, `cause` (for `abandoned`),
`final`, `attempt`, `setup`/`agent`/`verify` (exit, wall time, timeout; `agent.disk_limit`,
`agent.oom_killed`), `server` (managed, healthy after the run), `versions` (`suite_sha256`:
harness + ognivo profiles/templates + llama-server build + image, listed file by file in
`<model>/manifests/<hash>.json`, a second file there means the suite changed between nights;
`task_sha256`: the task dir and its `_common/<group>`), `host` (AC, platform profile, GPU
temperature/power limit/clock/pstate and load average at the start), `image`, `container`,
`network`, `prompt_sha256`, `prompt_matches_task_json` and `summary`. The summary has steps,
tools, `failed_tools`, finish reasons, tokens including cache reads, `compactions`,
`last_error`, `format_leaks`, `max_identical_calls`, `llm` (requests, failed requests with the
first and last error, whether the last one failed, the system prompt's sha256 and the tool
names OpenCode sent: in 2.2.1 the system prompt depends on the model name, the 10 tools do not; `model_s`, the time spent in LLM requests, and `predicted_n`, the generated tokens) and `flags` (timeout, infra, overflow,
format_leak, loop, server_errors, solved).

| status | meaning | on resume |
|---|---|---|
| `solved` | verify passed (whatever the agent's exit code) | kept |
| `failed` | verify failed, the agent finished normally | kept |
| `timeout` | verify failed, the agent hit its time limit | kept |
| `agent_error` | verify failed, and the agent stopped on an error event, wrote `DISK_BUDGET_GB`, or was killed (137/143) before its budget ran out, e.g. an OOM kill by its own builds (`agent.oom_killed`) | kept |
| `stack_error` | verify failed and the run ended on a failed LLM request (HTTP ≥ 400 or an SSE error in `llm.jsonl`): the OpenCode–llama.cpp pairing broke, not the model's code | kept |
| `server_crash` | still `infra_error` with llama-server down after the run after `M0_MAX_ATTEMPTS` (3) attempts: the run's own trajectory takes the server down; scored as unsolved | kept |
| `abandoned` | still not final after `M0_MAX_ATTEMPTS` (3) attempts that were not interrupted, for any other reason; `cause` has the last status; not counted in solved/final | kept |
| `setup_error` | `setup.sh` failed, no agent ran | re-run |
| `infra_error` | llama-server unreachable or gone, the tap died, or the disk floor | re-run |
| `harness_error` | the container could not run (image, engine) | re-run |
| `verify_error` | `verify.sh` could not run (exit 2) or its container failed | re-run |
| `interrupted` | stopped at the deadline or by a signal | re-run |

When reading trajectories, classify by exit code first: 124 is the agent timeout, and the
`events.jsonl` of a timed-out run ends with a spurious ECONNRESET error. Take tokens and subagent
child sessions from `session*.json`, which come from `opencode session export`.

## Files

| file | role |
|---|---|
| `run-suite.sh` | one model over a task list: setup, agent, verify, `result.json`; optional managed llama-server |
| `run-night.sh` | the night plan, the 03:00-06:00 window, campaigns and resume |
| `run-task.sh` | one agent run in its container (OpenCode or Claude Code), artifacts out |
| `verify-task.sh` | one hidden check in its `--network none` container |
| `prepare-image.sh` | pinned OpenCode into `cache/`, the build context, `podman build`, smoke test |
| `container/Containerfile`, `container/m0-entry.sh`, `container/m0-entry-claude.sh` | the image and its entrypoints |
| `config/opencode.json`, `config/models.json` | the OpenCode provider config; the models (profile, gguf, agent timeout) |
| `lib/common.sh` | shared paths and helpers |
| `scripts/llama_cmd.py` | llama-server argv from the ognivo profile; the `/props` check |
| `scripts/plan.py` | task selection, pending runs, campaign state |
| `scripts/summarize_run.py` | rows for the failure catalogue, `result.json`, the result table |
| `scripts/llm_tap.py` | the recording proxy |
| `scripts/normalize_ws.py` | after setup: one mtime, index-order checkout, `git prune`: the workspace does not show where the bug was injected |
| `scripts/manifest.py` | content hashes of the suite (harness, profiles, llama-server build, image) and of each task, for `result.json` `versions` |
| `scripts/mock_llama.py` | a stand-in llama-server (OpenAI and Anthropic APIs, scripted agent) for tests |
| `tests/selftest.sh`, `tests/lint_bash.py`, `tests/fake-*.sh`, `tests/fixtures/` | the self-tests |
| `tests/container-smoke.sh` | the Podman smoke test (network isolation, fixtures, `--reference`) |
| `tests/test-pasta-splice.sh`, `tests/oc-host-test.sh` | network-isolation emulation; one `opencode run` on the host |

## Task contract

`tasks/<id>/`:

- **`task.json`**: a JSON object with `id` (= the directory name) and `role` (`selected` /
  `reserve`). Optional: `prompt`/`prompt_file`/`prompt_sha256`, `verify_seconds`,
  `agent_timeout_s`, `setup_seconds`.
- **`setup.sh <new dir>`**: builds a git workspace with a base commit, offline, with its own
  anonymous identity.
- **`verify.sh <workspace>`**: exit 0 = solved, 1 = not solved, 2 = the check could not run. It
  runs as root in the image with `--network none`; `../_common/<group>` resolves because the whole
  `tasks/` tree is mounted read-only at its own path.
- **`solve.sh <workspace>`**: the reference solution, used by `--agent reference`.
- **`prompt.txt`**.

Only the agent container ever sees the workspace; `tasks/` is mounted for the verify step only.

## Pins

| what | pin |
|---|---|
| OpenCode | v2.2.1 `opencode-linux-x64.tar.gz` sha256 `3179551ddf69877ca24a960b0175e94405cf3f52147f1eec705094374bef49f2`, binary `32e95ec126d416ab833094447cf590290e17dd028a68b122b94d3539795c0dae` (also checked in the image) |
| base image | `docker.io/library/ubuntu@sha256:262df65c0ec6bf8d9dcc87454f4330a05fae4b97825f45854af3b136666584c2` (26.10, 2026-10-30) |
| toolchains | Rust 1.99.0 (forced with `RUSTUP_TOOLCHAIN`: both Rust repos pin channel `stable`, which rustup would try to download offline), clang-format-22 22.1.8, torch 2.13.0 CPU |
| Claude Code | 2.1.311, sha256 `86dc1fcc69498159eaf15f446613e847010f9036bfbbdffd4c49350be8489ada` (official manifest), local copy only |
| llama-server | b11924-12cc59ba4 (`~/.cache/llama-pinned/build-12cc59ba4914`), port passed explicitly (its default is 9931 since b11521) |
| OpenCode config | `config/opencode.json` (v2 schema), installed as `/etc/opencode/opencode.json`: provider `llama`, one entry per alias with the profile limits (98304/65536/98304 context, 32768 output) and sampling, `webfetch`/`websearch` denied, `instructions: ["CLAUDE.md"]` |

## Configuration knobs

| where | variables |
|---|---|
| paths (`lib/common.sh`) | `M0_ROOT`, `M0_TASKS_ROOT`, `M0_RUNS_ROOT`, `M0_WORK_ROOT`, `M0_CACHE_DIR`, `M0_MODELS_JSON` |
| containers (`run-task.sh`) | `ENGINE`, `IMAGE`, `CPUS`, `MEMORY`, `PIDS`, `M0_NICE`, `NETWORK`, `TAP_PORT`, `DRY_RUN=1` |
| server (`run-suite.sh`) | `LLAMA_SERVER_BIN`, `OGNIVO_REPO`, `NVIDIA_SMI`, `TEMPLATE_CHECK=warn`, `SERVER_LOAD_TIMEOUT`, `SETUP_TIMEOUT`, `MAX_SERVER_RESTARTS`, `GPU_RETRIES`/`GPU_RETRY_SLEEP` |
| deadline guard | `M0_SETUP_EST`, `M0_VERIFY_EST`, `M0_GUARD_OVERHEAD` |

## Notes and pitfalls

- Inside containers, always use 127.0.0.1 (never `localhost`): ::1 is forwarded too, and
  llama-server listens on IPv4.
- OpenCode needs `--title`, or it sends an extra title request that evicts the KV cache. It needs
  `--standalone`, or it leaves a detached `opencode serve` behind. It needs `rg` on PATH, or it
  downloads ripgrep. Prompts go on stdin. On the host, run it only with an isolated
  HOME/XDG tree: it writes a log line even for `--version`.
- Infrastructure errors look like model errors. With the server down, OpenCode retries for about
  85 s and then exits 1 with `provider.transport`. The suite tells them apart by checking
  `/health` after every run.
- OpenCode compacts at context − max(15 %, 16K) (82.3K for 98304, 49.5K for 65536), which is not the
  profiles' `compact_at`. Claude Code `--bare` offers only Bash/Edit/Read and needs `IS_SANDBOX=1`
  as root (both set by `container/m0-entry-claude.sh`).
- The shell tool's default timeout is 2 minutes. A cold `cargo test` with 8 CPUs can come close
  to that.
- **Permissions.** `config/opencode.json` only denies `webfetch` and `websearch`; `--auto`
  approves everything else that is not explicitly denied. An extra `"*": allow` rule must not
  come back: OpenCode appends config rules after each agent's own, so it would lift the stock
  limits of the `explore` (read-only) and `general` (no nested subagents) subagents and even of
  the title/summary agents (`opencode debug agents` shows the effective rules). A `question` in
  a headless run returns the error "This non-interactive run cannot ask the user questions ...
  make reasonable assumptions" at once: it does not hang the run. Network stays possible through
  the `execute` tool (a JavaScript "Code Mode" runtime with `fetch`), which only the container's
  network confines.
- **OpenCode picks the system prompt by model name**: gpt-oss gets its GPT prompt (10.4K chars,
  with commentary-channel, "Final Answer" and "do not spawn subagents unless ..." sections), Gemma
  and Qwen the generic one (8.6K). All three get the same 10 tools (edit, glob, grep, question,
  read, shell, skill, subagent, write, execute). Part of a cross-model difference is OpenCode's,
  not the model's; `summary.llm.system_prompt_sha256` and `summary.llm.tools` record what each
  run got.
- **No formatter runs.** Formatters in OpenCode 2.2.1 are opt-in (the `formatter` config key) and
  off here: after edits it spawns only git and rg. An ognivo comparison must not auto-format
  either, or flowcoin's format gate is no longer like for like.
- **Single samples.** Each (model, task) cell runs once, at temperature 1.0 with no seed: a
  difference of one or two tasks between models (or later between OpenCode and ognivo) is
  within noise. Compare per-call metrics from `llm.jsonl`/`session.json`, or run important
  cells twice.
- **A context overflow** that ends the run as HTTP 400 now reads as `stack_error` (the run ended
  on a failed LLM request), not `agent_error`.
- **Budgets are wall-clock and set for equal decode capacity** (owner's decision 2026-10-30). At
  the measured generation rates (gpt-oss ~105-130 tok/s, Gemma ~110-125, Qwen3.8 ~40-45) 480 s
  buys the MoE models ~56K tokens; Qwen's first budget of 900 s bought only ~37K (two thirds), and
  it took 791 s of 900 on the easiest task in the smoke run. With 1500 s (~61K) a Qwen `timeout`
  means about the same as a MoE one. The agent never sees its budget, so "solved within N s" can
  still be read off solved runs' `agent.wall_s` afterwards. Qwen's phase (10 x 1500 s worst case,
  about 270 min with setup and verify) spreads over two nights; run-night.sh resumes it.
- **misc-ai-chat-multiturn (and the files of misc-ai-moe-balance) contain the models' own special
  tokens as text:** Qwen's `<think>`/`</think>` (USER_DEFINED 248068/248069) in the prompt and in
  `model/byte_tokenizer.py`, and Gemma's `<bos>`/`<pad>` (CONTROL ids 2/0) in the same files and
  in `spec/model-spec.md`. llama-server tokenizes the whole prompt with special tokens parsed, so
  these become real control tokens in the user turn and in every tool result that shows those
  files. That is a hazard of the OpenCode + llama.cpp stack (exactly what ognivo's special-token
  guard targets), not only a coding test: `task.json` `token_hazard` lists them; report those
  cells separately and check their `llm.jsonl` for reasoning/content splits at `</think>` and for
  turns after a read of `byte_tokenizer.py`.
- **flowcoin's lint-files also lints untracked files**: scratch files an agent leaves outside
  `build/` fail an otherwise correct fix. The prompts ask to keep `ctest --preset dev` green, so
  this stays; `logs/verify.log` names the failing steps, which separates such runs afterwards.
- **cforge-timefmt-february does not check the "add tests" part of its prompt** (lenient by
  design); all three models added tests in the smoke runs.
- Testing during drafting left images in the owner's rootless Docker (`localhost/m0-opencode:2.2.1`,
  `:2.2.1-claude`, `ubuntu:26.10`, about 1.4 GB). Remove them with `docker rmi` if unwanted.

## What was tested here, and what was not

State of 2026-10-30, after the review fixes (all on the final tree and the rebuilt image
`localhost/m0-opencode:2.2.1-full`, id 04045ea92a15):

- `tests/selftest.sh`: 0 failures. Stage 1 runs `bash -n`, `tests/lint_bash.py` (shellcheck-style
  errexit, pipefail/SIGPIPE and quoting checks; shellcheck itself is not installed here) and
  `tests/scoring_test.py`. The scoring test has 16 scorer cases: forged `/out` files, OOM and early
  kills, the disk guard, stack errors and the agent's own curl, the attempt cap, and the table.
  Stages 2-5 run inside bwrap, with `/` read-only, loopback-only networking and no GPU nodes:
  - the three llama-server command lines and the served-template check;
  - every `--dry-run`;
  - full suites with the pinned OpenCode through `tests/fake-engine.sh` and
    `scripts/mock_llama.py`: artifacts, resume, GPU/port-busy refusals, the deadline guard and
    watchdog, a server crash with restart, TERM, `--agent reference/none`, Claude Code `--bare`;
  - a two-night campaign, run from its frozen copy.
- `tests/container-smoke.sh` with real rootless Podman (5.8.6, pasta 0.0~git20260728):
  - The network probe reaches only the forwarded port. Host port 22, another listening host
    port, Open WebUI's 8080, 169.254.1.2, 1.1.1.1, IPv6 and DNS all fail.
  - The fixture suite gives solved, solved, failed, with the session exported and no container
    left behind.
- `--agent reference` / `--agent none` over all 18 tasks in the image: every reference solution
  passes and every base fails, offline, with `normalize_ws.py` and the final verifiers.
- The disk guard (budget, floor, no trigger on a small tmpfs), the tap's limits and its
  orphan-safe start, and the frozen campaign re-exec were tested by hand with real Podman.
- Real models, once each, on cforge-timefmt-february before most of the fixes (04:52-05:15,
  `runs/smoke-real-20261030/`):
  - gpt-oss-20b solved it in 251 s, Gemma 4 in 172 s and Qwen3.8 in 791 s;
  - sampling and reasoning reached the server as configured, and reasoning was sent back on
    every tool-call turn;
  - no HTTP or template errors;
  - the prefix cache held, also on the hybrid Qwen.

Not run here:

- A real-model run of the final code. The GPU window had ended; the first night's first task is
  the canary.
- Claude Code in a real container.
