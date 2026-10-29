# Review: Security of the approval policy and command classifier (crates/ognivo-tools/src/lib.rs classify_command, crates/ognivo-core/src/approval.rs) and the special-token injection guard design

v1 has no sandbox, so this approval policy is the only security boundary. The current code does not hold that boundary. The doc comment at crates/ognivo-tools/src/lib.rs:63-67 says the classifier "can only be too cautious, never too lax"; that is false.

Four root causes:
- `Effect::EditsFiles` carries no path, and `decide()` allows it unconditionally (approval.rs:31). There is no workspace confinement and no protected-paths rule.
- The four git read verbs are always `ReadOnly`, even in a dirty turn (the test at approval.rs:49 pins this), although git runs programs defined in repo config and attributes.
- The `ExecutesProjectCode` arms ignore their arguments. In any clean turn, `go run <module>@latest`, `--manifest-path /abs`, `make -C /abs` and `-toolexec` run silently.
- The "agent changed code" taint is a per-turn, per-agent bool whose default is clean.

Combined, these allow zero-prompt code execution inside a single turn, before the end-of-turn review. Example: an auto-applied edit to `.git/config` adding a `core.fsmonitor` hook, then a silent `git status`. Same pattern with a hook or `permissions.allow` rule in `.claude/settings.json`, or with `~/.bashrc`.

Further problems:
- The classifier has no cwd input, and cwd persists between commands.
- The path check is lexical, so symlinks and attached option values get through.
- The rg/find/wc denylists have gaps.
- Read-only commands that block forever or stream without limit are auto-run.

Special-token guard:
- The Generic sanitizer breaks only `<|`. All Qwen tokens and Gemma's closing tokens (such as `<tool_response|>`) stay injectable.
- The sanitizer applies only to tool output and files. Declarations, MCP descriptions, AGENTS.md, hook output and subagent reports are not covered.
- The Native table and the doctor probe skip specials such as Gemma `<eos>`/`<bos>`.
- There is no `Network` effect for WebFetch or MCP tools.

Cross-cutting recommendation: run auto-approved commands as argv via execve without a shell. Only plain words are accepted anyway, and this removes rc-file functions, aliases and BASH_ENV. Resolve argv[0] to an absolute path outside the workspace, use a fixed PATH, and scrub `*_API_KEY`, `RIPGREP_CONFIG_PATH`, `GIT_*`, `BASH_ENV` and `LD_PRELOAD` from the environment. Add each bypass below as a negative test.

Verification: `cargo test --workspace` passes. Verdicts were confirmed with a scratch harness outside the repo (/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/probe) that calls `classify_command` and `decide()`. Git and ripgrep behaviour was checked against the local man pages and `rg --help` (git 2.55, ripgrep 14.1.1). The Go claims come from documented Go behaviour; Go is not installed locally.

## [critical] Agent writes to any path are auto-allowed: no workspace confinement, no protected paths
`crates/ognivo-core/src/approval.rs:31`

**Problem.** `decide()` maps `Effect::EditsFiles` to Allow unconditionally. `Effect::EditsFiles` (crates/ognivo-tools/src/lib.rs:53-54) carries no target path, so the policy cannot tell `src/lib.rs` from `.git/config`, `.claude/settings.json`, `.mcp.json` or `~/.bashrc`. "Journaled, so reversible" holds for file content. It does not hold for what other programs do with those files before the end-of-turn review. The owner's "file writes ask" is not represented either: Edit and Write both map to EditsFiles, and the test at approval.rs:57 pins this.

**Failure scenario.** A README the agent reads carries a prompt injection. In one turn, with no prompt: (1) Edit `.git/config` to add a `core.fsmonitor` hook command; (2) run `git status`, which is ReadOnly and allowed even with agent_changed_code=true (approval.rs:49). Git runs the hook. Variants: (a) add a PreToolUse hook or a `permissions.allow` rule to `.claude/settings.json`. Both are loaded per architecture §8/§9, and settings are live-reloaded per §10, so the next tool call runs code or every command becomes allowed. (b) Add a stdio server to `.mcp.json`. (c) Append to `~/.bashrc` or `~/.gitconfig` outside the project. That change never shows in the project's Changes review and runs at the user's next shell or git command.

**Fix.** Make the effect carry its targets: `Effect::EditsFiles { paths: Vec<PathBuf> }`, including apply_patch Add/Delete/Move-to targets. decide() returns Ask when any target, resolved against the workspace root with openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS) (rustix, no unsafe), either (a) leaves the workspace, or (b) matches the protected set:
- `**/.git` (file or dir) and everything below it, `.gitmodules`, `**/.gitattributes`;
- `.claude/**`, `.mcp.json`, `.ognivo/**`, `AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md`;
- `.vscode/**`, `.idea/**`, `.devcontainer/**`;
- `.envrc`, `mise.toml`, `.mise.toml`, `.tool-versions`;
- `.husky/**`, `.pre-commit-config.yaml`, `lefthook.yml`.
Outside-workspace targets ask with a loud warning. These include ~/.bashrc, ~/.profile, ~/.zshrc, ~/.config/**, ~/.gitconfig, ~/.cargo/**, ~/.npmrc, ~/.claude/**, ~/.ognivo/**, ~/.local/bin/** and an ancestor `.cargo/config.toml`. Hooks, permissions and MCP config edited in-session must not be live-reloaded until the user re-trusts them; pin a hash at trust time. Add tests: Edit of `.git/config`, `.claude/settings.json`, `../x`, and a symlink pointing outside must each return Ask.

## [critical] Project-code commands ignore their arguments: remote and out-of-project code auto-runs
`crates/ognivo-tools/src/lib.rs:109`

**Problem.** The ExecutesProjectCode arms (lines 109-116) match only the first one or two words and never inspect the rest. read_only()'s absolute/`..` check and flag denylist are not applied. A turn is clean at its start, which is the normal case, so these commands auto-run code that is neither the project's nor reviewed.

**Failure scenario.** With no edits in the turn, the harness confirmed each of these as ExecutesProjectCode, so Allow:
- `go run <module path>@latest` downloads a module from the internet and runs it.
- `cargo test --manifest-path /tmp/x/Cargo.toml`, `make -C /`, `npm run --prefix ../other x` and `pytest --rootdir ..` run code outside the project. `pytest <dir outside>` loads that directory's conftest.py.
- `go test -exec P`, `go build -toolexec P` and `go vet -vettool P` swap in another program.
- `cargo clippy --fix --allow-dirty` rewrites sources outside the edit journal.
Two-turn variant: turn 1 writes `/tmp/x/Makefile` (EditsFiles, Allow); turn 2 runs `make -C /tmp/x` (Allow).

**Fix.** Apply read_only()'s argument checks to these arms (no absolute paths, no `..`). Return SideEffects for:
- go: `go run|build|test` with any argument containing `@` or not starting with `.`/`./`; `-exec`, `-toolexec`, `-vettool`, `-modfile`, `-C`;
- cargo: `--manifest-path`, `--config`, `-Z`, `--target-dir` outside the root; `clippy --fix`;
- make: `-C`, `-f`/`--file`/`--makefile`, `-I`, `--eval`;
- just: `-f`/`--justfile`, `-d`/`--working-directory`;
- npm/pnpm/yarn: `--prefix`, `-C`, `--dir`;
- pytest: `--rootdir`, `-p`, `--pyargs`, `-c`, `--confcutdir`, and any path argument outside the root.
Only words after `--` for `cargo run|test` stay free-form. Add a negative test for each.

## [major] git status/diff/log/show are always ReadOnly although git runs config-defined programs
`crates/ognivo-tools/src/lib.rs:87`

**Problem.** The arm treats these verbs as plain reads whatever the repo config holds, and decide() allows ReadOnly even when agent_changed_code is set (approval.rs:48-51 asserts this). Git runs external programs on exactly these verbs:
- a core.fsmonitor hook on index refresh (status, diff);
- diff.external on `git diff` (git-config(1));
- `diff.<driver>.textconv`, on by default for diff/log/show;
- `filter.<driver>.clean`/`process` when status/diff re-hash worktree files;
- gpg.program with `--show-signature` or log.showSignature.
The `--output`/`-o` denylist only covers file writes.

**Failure scenario.** (a) No agent edit at all: the user unpacks an archive, or opens a directory that ships its own `.git` directory. git's safe.directory check passes because the user owns the files. The agent's first silent `git status` runs that repo's core.fsmonitor command. (b) Chained with the EditsFiles finding: an auto-applied edit of `.git/config` or `.git/modules/<m>/config`, then an automatic `git diff`. In both cases execution happens before the end-of-turn review.

**Fix.** Auto-run these verbs only in a hermetic argv that ognivo builds itself: `git -c core.fsmonitor=false --no-pager <verb> ...`. For diff/log/show also add `--no-ext-diff --no-textconv`; for status and diff add `--ignore-submodules=all`. Environment: unset `GIT_EXTERNAL_DIFF`, set `GIT_PAGER=cat` and `GIT_CONFIG_NOSYSTEM=1`. Return SideEffects for `--ext-diff`, `--textconv`, `--show-signature`, `--no-index`, and `-O<file>`. Clean filters cannot be switched off per command, so treat git verbs like ExecutesProjectCode once `.git/config`, `.git/info/attributes` or any `.gitattributes` has changed since workspace trust. At trust time, scan `.git/config` for exec-capable keys and list them in the trust dialog: core.fsmonitor, core.pager, core.editor, core.sshCommand, core.hooksPath, diff.*.command, diff.*.textconv, filter.*, merge.*.driver, gpg.*program, credential.helper, include.path, includeIf.*.

## [major] Code-change taint is per turn, per agent, defaults to clean and misses non-edit writes
`crates/ognivo-core/src/approval.rs:26`

**Problem.** TurnState is one bool that every caller must keep correct. It resets each turn, and `Default` is the permissive state (false). It is not derived from the edit journal and not shared between the orchestrator and subagents. Its doc limits it to "edits, writes", so files changed by an approved shell command (`sed -i`, `git apply`), by an MCP tool, or by an ExecutesProjectCode run (clippy --fix, a code generator) do not set it. The name agent_changed_code also invites a source-files-only reading, which would skip build and runner configs: `.cargo/config.toml`, `rust-toolchain.toml`, `.npmrc`, `.yarnrc.yml` (yarnPath), conftest.py, *.pth.

**Failure scenario.** (1) Turn N: the agent appends a payload to build.rs or a test. Changes shows it, but review is optional and the user just types the next message. Turn N+1: `cargo test` is allowed and the payload runs. That is exactly the hole architecture §8 says this rule closes. (2) Per §7, a write-mode subagent edits tests while a read-mode subagent, which §7 lets run tests, has its own TurnState::default() and auto-runs `cargo test`. (3) The user approves `git apply fix.patch`; the next `cargo test` in the same turn runs silently. (4) Headless `ognivo run` has no review step at all.

**Fix.** Replace the bool with a workspace-level query on the edit journal: `journal.unreviewed_changes(workspace)`.
- It becomes true on any file change by any agent or subagent, after any approved non-ReadOnly command (via the planned watcher, or by marking the workspace dirty), and after an MCP call.
- Only an explicit Accept in Changes, or a revert, clears it. Persist it in SQLite across turns and sessions.
- Remove `Default`, or make the default dirty.
- In headless mode with unreviewed changes, ask, or deny when there is no TTY.
This goes beyond the literal "in this turn" in the owner's decision and needs the owner's sign-off. Without it, the two-turn bypass stays open.

## [major] Classifier is blind to the working directory that persists between commands
`crates/ognivo-tools/src/lib.rs:68`

**Problem.** `classify_command(&str)` takes no cwd and no workspace root, and the in-project test (line 78) is a lexical check on relative paths only. Architecture §9 keeps cwd between commands. `cd` is classified SideEffects, so leaving the project costs one approval, and approving a `cd` looks harmless.

**Failure scenario.** The agent asks to `cd ..` or `cd ~` "to look at the sibling crate", and the user approves because cd changes nothing. From then on all of these run silently as if they were inside the project: `cat .ssh/id_ed25519`, `rg -uuu token`, `git status` in another repo with its own config, and `cargo test`/`make` in that directory.

**Fix.** Change the signature to `classify_command(cmd, &ExecContext { workspace_root, cwd })`. Return ReadOnly or ExecutesProjectCode only when the canonical cwd is under workspace_root, and resolve every path argument against cwd before the in-root check. Implement `cd` in the exec layer: silent inside the root, ask outside it. Reset cwd to the root at the start of each turn.

## [major] In-project path check is lexical: symlinks and attached option values read outside the project
`crates/ognivo-tools/src/lib.rs:78`

**Problem.** `leaves_project` only rejects words that start with `/` or contain `..`. It misses three cases. (a) Symlinks inside the workspace: git stores them, so any cloned repo can ship one. (b) Recursive tools that follow links, all ReadOnly in the harness: `grep -R`, `rg -L/--follow`, `find -L/-H/-follow`. (c) Path-valued options with an attached value, which do not start with `/`: `grep -f/abs/file`, `git diff -O/abs/file`. A future Edit/Write that uses the same lexical approach would follow a symlink out of the project.

**Failure scenario.** A repo contains `docs/env -> /proc/self/environ` or `notes -> ~/.ssh`. A silent `cat docs/env` puts the exec layer's inherited environment into the context. That includes OGNIVO_API_KEY (main.rs:29 takes it from the env) and any provider keys in the user's shell. A silent `grep -R -n PRIVATE .` walks ~/.ssh through the link. Any network-capable step then exfiltrates the data: WebFetch, an HTTP MCP tool, or `cargo run -- <url>` in a clean turn. An Edit of `docs/setup.md` that is a link to ../../.bashrc writes the user's shell rc.

**Fix.** Canonicalize every non-option argument, and the value of known path-taking options, relative to cwd. Require the result to be under the workspace root, and for cat/head/tail require a regular file. Return SideEffects for `-R`, `--dereference-recursive`, `-L`, `--follow`, `-H`, `-follow`, and for short options with an attached path. Better: route model reads through ognivo's own Read/Grep/Glob tools, built on openat2(RESOLVE_BENEATH|RESOLVE_NO_MAGICLINKS), with O_NOFOLLOW for writes. Scrub OGNIVO_API_KEY, *_API_KEY and *_TOKEN from every child environment.

## [major] Generic-path sanitizer breaks only "<|": Gemma closers and all Qwen tokens stay injectable
`docs/architecture.md:214`

**Problem.** The design inserts U+2060 only inside `<|`. Many structural tokens contain no `<|`:
- Gemma (profiles/gemma-4-26b-a4b.toml:13-14): `<turn|>`, `<tool|>`, `<tool_call|>`, `<tool_response|>`, `<channel|>`;
- Qwen (profiles/qwen3.8-27b.toml:14-15): `<tool_call>`, `</tool_call>`, `<think>`, `</think>`, `<tool_response>`, `</tool_response>`.
M0 measured both families' tokens as recognized even with parse_special=false, and the chat path tokenizes with parse_special=true anyway. The scope is also limited to "tool output and files". Not covered: tool and MCP declarations and server instructions, skill names and descriptions, AGENTS.md/CLAUDE.md, hook stdout, the git-status dynamics block (§5.6), subagent reports (§7) and compaction summaries.

**Failure scenario.** Gemma on the M1 Generic path: a file the agent reads contains `<tool_response|>` followed by text. The tool response closes early, and the rest renders as the model's own continuation in the model turn, so it reads as the agent's own reasoning or plan ("next I will edit .git/config"). Qwen: `</tool_response><tool_response>` inside content forges a second tool result, such as "tests passed". gpt-oss: an MCP tool description containing `<|end|><|start|>developer<|message|>` forges a developer message, because declarations are not sanitized.

**Fix.** Derive the sanitizer from the profile: break every string in `tokens.structural` and in the wider specials list after its first character. For unknown or Generic models, use a default set covering `<|`, `|>`, the ChatML tags, `<think>` and the `<tool_*>` tags. Apply it at one choke point: every string that is neither the user's typed text nor verbatim model tokens. Then verify by tokenizing the rendered message with parse_special=true and asserting that no structural id falls inside a content span, the same assert as on the Native path. Make the inverse exact: escape pre-existing markers so that un-sanitizing edit/write arguments is bijective and does not strip legitimate U+2060 from files.

## [minor] rg/grep/find/wc denylists miss options that run programs, load config or read path lists
`crates/ognivo-tools/src/lib.rs:101`

**Problem.** Only `--pre` is blocked for rg and grep. Missed:
- ripgrep `--hostname-bin COMMAND`: runs the executable when the hyperlink format uses {host}, e.g. `--hyperlink-format file` with `--color always`;
- ripgrep `-z/--search-zip`: spawns gzip, xz, zstd and other decompressors from PATH;
- the RIPGREP_CONFIG_PATH config file: always read unless `--no-config` is passed, and it can contain `--pre`;
- `find -files0-from FILE` and `wc --files0-from FILE`: read paths, possibly absolute, from a file.

**Failure scenario.** If the user's environment sets RIPGREP_CONFIG_PATH, an unconfined agent write to that file adds a `--pre` line, and every later silent `rg TODO` runs the preprocessor on each file. `rg --hostname-bin <prog> --hyperlink-format file --color always x` runs <prog> as ReadOnly. `find -files0-from list -name id_ed25519` lists files outside the project from a list file inside it.

**Fix.** rg: SideEffects for `--hostname-bin`, `--hyperlink-format`, `-z`, `--search-zip`, `--pre-glob`, `-L`, `--follow`, and `--ignore-file` outside the root. The exec layer always passes `--no-config` and unsets RIPGREP_CONFIG_PATH. find: block `-files0-from`, `-L`, `-H`, `-follow`, and `-newer`/`-samefile` with outside paths. wc: block `--files0-from`. grep: block `-R`, `--dereference-recursive`, `-d recurse`. Better: switch to a per-tool allowlist of known-safe flags, with everything else SideEffects.

## [minor] Read-only commands can block forever or stream unbounded output
`crates/ognivo-tools/src/lib.rs:97`

**Problem.** `tail -f/-F/--follow/--pid` is ReadOnly and runs silently (harness-confirmed), and so is cat/head/tail on a FIFO or on a repo symlink to /dev/zero or /dev/urandom. Architecture §9 says a command that hits its timeout is moved to the background, not killed, and that the full log is written to a file.

**Failure scenario.** The model "checks the log" with `tail -f build.log` at every step. Each call times out and lingers as an invisible background process. `cat data/sample.bin`, where the file is a link to /dev/zero, writes gigabytes into the full-log file until the disk is full; ~/.ognivo/data shares that disk with the GGUF models.

**Fix.** Return SideEffects for tail `-f`, `-F`, `--follow*`, `--pid`, `--retry`, `-s`, and require regular files for cat/head/tail. In the exec layer, ReadOnly commands get a hard wall-clock limit with kill (no backgrounding) and a byte cap that kills the process group. Cap the persisted full log, e.g. 64 MiB keeping head and tail.

## [minor] Native guard token table and doctor probe skip specials such as Gemma <eos>/<bos>
`profiles/gemma-4-26b-a4b.toml:12`

**Problem.** The Native guard splits and asserts only `tokens.structural`, and doctor (crates/ognivo-models/src/doctor.rs:225) probes only that list.
- Gemma's table omits `<eos>` (id 1, which the same file lists in `eog`) and `<bos>` (id 2).
- gpt-oss omits `<|endoftext|>`, `<|startoftext|>`, `<|endofprompt|>` and the reserved ids.
M0 found that ik_llama.cpp (class C) ignores parse_special for every token it checked. Such text therefore probably becomes a control id inside a content span (not verified for these specific tokens), and neither the split, the assert nor doctor would notice.

**Failure scenario.** The agent works on an ML repo, or on ognivo itself, whose fixtures mention `<eos>`. On ik_llama.cpp that text becomes id 1 mid-prompt. The model's view of the conversation degrades or ends, with no error anywhere.

**Fix.** Add `tokens.specials` to each profile: every CONTROL and USER_DEFINED string in the vocab, including bos, eos, pad, media and reserved tokens (or id ranges). Use it for both the split and the assert. Have doctor probe it and report which specials the server recognizes without parse_special. Add a profile test that every `eog` and `bos` id is covered.

## [minor] No Network effect and no effect mapping for WebFetch and MCP tools
`crates/ognivo-tools/src/lib.rs:50`

**Problem.** Effect has no network variant, and nothing maps ToolId to Effect: classify_command covers Bash only, while WebFetch (line 18) and Mcp (line 24) exist. ReadOnly is documented as "reads", so the natural M1 mapping makes WebFetch silent. The MCP spec says tool annotations such as readOnlyHint are untrusted.

**Failure scenario.** After reading `.env` or a symlinked secret, the model calls WebFetch on a URL that carries the data in its query string, or calls a remote MCP tool. Both run silently if mapped to ReadOnly or trusted by annotation.

**Fix.** Add `Effect::Network`. Add an exhaustive `fn effect_of(tool: &ToolId, args) -> Effect` with no wildcard arm, so any new tool must be classified before it compiles. WebFetch and every MCP tool map to Network or SideEffects and Ask. Allow only per-server rules the user wrote, and never honour readOnlyHint from project `.mcp.json` servers.
