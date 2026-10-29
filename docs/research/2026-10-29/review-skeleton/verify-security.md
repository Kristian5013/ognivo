# Verification: security

## [REAL · major] Agent writes to any path are auto-allowed: no workspace confinement, no protected paths

**Reason.** Holds against the code. Effect::EditsFiles (crates/ognivo-tools/src/lib.rs:53-54) has no target, and decide() (crates/ognivo-core/src/approval.rs:31) returns Allow for it unconditionally. The test at approval.rs:57 pins this even for a dirty turn. The policy therefore cannot tell src/lib.rs from .git/config, .claude/settings.json, .mcp.json or a file outside the workspace. The journal can revert file content, but it cannot undo what git, hooks or the user's shell do with those files before the end-of-turn review. git status/diff stay Allow in a dirty turn, so a silent config edit followed by a silent git verb needs no prompt at all. Downgraded from critical because no tool is wired up in M0 (only `doctor` runs). It is still the root cause of several silent-execution chains, and the Effect shape is the API M1 builds on, so it should change now. One part is overstated: the live reload in §10 applies to ognivo's own TOML settings, not to .claude/settings.json. The underlying point stands: hooks and MCP config edited after trust need re-trust.

**Refined fix.** 1) Make the effect carry its targets, e.g. `EditsFiles { targets: Vec<PathBuf> }`, including apply_patch add, delete and move destinations.
2) In decide(), return Ask if any target, resolved against the workspace root without following symlinks out of it, lies outside the root or matches one const protected list. For the resolution use rustix openat2 with RESOLVE_BENEATH, or canonicalize the parent and reject a symlink leaf. The protected list:
- any `.git` path component (file or dir) and everything below it, `.gitmodules`, `.gitattributes`;
- `.claude/**`, `.mcp.json`, `.ognivo/**`, `AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md`;
- `.vscode/**`, `.idea/**`, `.devcontainer/**`, `.envrc`;
- hook-manager configs: `.husky/**`, `.pre-commit-config.yaml`, `lefthook.yml`.
3) Ask the owner whether "file writes ask" means the Write tool. lib.rs:53 currently documents both edit and write tools as EditsFiles. If it does, map Write to Ask.
4) Tests: `.git/config`, `.claude/settings.json`, `../x`, an absolute path and an in-repo symlink pointing outside must each return Ask; `src/lib.rs` must return Allow.
5) M3 note: snapshot hooks, permissions and .mcp.json at trust time (pin a hash), so changes made during a session do not take effect without re-trust.

## [REAL · major] Project-code commands ignore their arguments: remote and out-of-project code auto-runs

**Reason.** Confirmed with a scratch probe crate (outside the repo) linked against the real ognivo-tools and ognivo-core. Each of these classifies as ExecutesProjectCode, so decide() returns Allow on a clean turn:
- `go run <module>@latest`;
- `cargo test --manifest-path /tmp/x/Cargo.toml`;
- `make -C /`;
- `npm run --prefix ../other x`;
- `pytest --rootdir ..` and `pytest ../sibling`;
- `go test -exec P`, `go build -toolexec P`, `go vet -vettool P`;
- `cargo clippy --fix --allow-dirty`;
- `just -f /tmp/justfile`.
The arms at lib.rs:109-116 match only the first one or two words and skip the absolute/`..` check that read_only() applies. That breaks the doc comment's promise at lib.rs:63-67 that the classifier is "never too lax". The `--flag=value` forms are blocked, but only incidentally, because `=` is in SHELL_SYNTAX. These commands run remote modules, code in other directories or substituted toolchain programs, none of which is the project's own code. `clippy --fix` also rewrites sources outside the edit journal. The owner's rule allows only the project's own code to run without asking. Major rather than critical because the exec layer does not exist yet.

**Refined fix.** In the ExecutesProjectCode arms, check every word before a `--` separator. Reject absolute paths and `..` exactly as read_only() does. Return SideEffects for flags that change the root, load foreign config or substitute a program:
- cargo: `--manifest-path`, `--config`, `-Z`, `--target-dir`; `clippy --fix` and `cargo fix`, which write files;
- make: `-C`, `--directory`, `-f`, `--file`, `--makefile`, `-I`, `--eval`;
- just: `-f`, `--justfile`, `-d`, `--working-directory`;
- npm/pnpm/yarn: `--prefix`, `-C`, `--dir`, `--cwd`;
- pytest: `--rootdir`, `-c`, `-p`, `--confcutdir`, `--pyargs`;
- go: `-exec`, `-toolexec`, `-vettool`, `-modfile`, `-C`, plus any positional argument that contains `@` or is not a relative pattern (`.`, `./...`, or a relative .go file).
Words after `--` for `cargo test` and `cargo run` go to the project's own binary and stay free. Better still: a per-tool allowlist of known-safe flags. Add a negative test for each case; this class currently has only positive tests.

## [REAL · major] git status/diff/log/show are always ReadOnly although git runs config-defined programs

**Reason.** Holds. lib.rs:87-89 returns ReadOnly for these verbs with any arguments except `--output`, `-o`, absolute paths and `..`. decide() allows ReadOnly even in a dirty turn (approval.rs:48-51). The probe confirms ReadOnly for `git diff --ext-diff`, `git log --show-signature`, `git diff --no-index a b` and `git diff -O<file>`. Per git-config(1), these default verbs run programs that the repository config defines: the core.fsmonitor hook on index refresh, diff.external and diff drivers, textconv (on by default for diff/log/show), clean/process filters, and gpg.program when signatures are shown. Combined with finding 1, an auto-applied config edit plus an auto-allowed git verb gives silent code execution inside the very turn the owner's rule protects. Separately, §5.6 has ognivo run git status implicitly every turn. A workspace whose `.git` came from an archive would then run repository-defined programs with no consent, bypassing the §9 trust gate that covers hooks and .mcp.json. `git -c ...` and `git -C ..` are already blocked, by SHELL_SYNTAX and by the verb match.

**Refined fix.** 1) Classifier, now: add `--ext-diff`, `--textconv`, `--show-signature`, `--no-index` to the git denylist and reject any word starting with `-O`.
2) Exec layer (write it into §8/§9 now): every auto-approved or implicit git call, including the §5.6 dynamics, runs as `git -c core.fsmonitor=false --no-pager <verb> ...`. Add `--no-ext-diff --no-textconv` for diff/log/show, and unset GIT_EXTERNAL_DIFF.
3) Add `.git/**` (including `.git/modules/*/config`), `.gitattributes` and `.gitmodules` to the protected-path list from finding 1.
4) Optional: list exec-capable keys from `.git/config` in the trust dialog (core.fsmonitor, core.hooksPath, diff.*.command/textconv, filter.*, gpg.*program, include.path). Clean filters cannot be disabled per command, so before trust either skip the implicit git calls or keep them under these flags.

## [REAL · minor] Code-change taint is per turn, per agent, defaults to clean and misses non-edit writes

**Reason.** Partly real. The facts hold: TurnState (approval.rs:22-27) is a single bool, it derives Default (so the permissive state is the default), its doc limits it to "edits, writes", and nothing ties it to the journal or shares it across agents. The headline two-turn scenario, however, follows the owner's explicit rule ("ask if the agent changed code in this turn", architecture §1 third round and §8), with the end-of-turn review as the control. That is a policy question for the owner, not a code defect, as the finding itself concedes. The real defects are narrower:
- (a) a fail-open Default;
- (b) by the owner's own rule, code changed in this turn through an approved shell command (sed -i, git apply) or an MCP tool should count, but the documented scope excludes it;
- (c) §7 lets read-mode subagents run tests while a write-mode subagent edits in the same orchestrator turn, and a per-agent value would miss that.
There is no loop or subagent code yet, hence minor.

**Refined fix.** Remove `#[derive(Default)]`, or make the default dirty, so a forgotten propagation fails closed. Replace the by-value bool with a query on a workspace-level change record for the current turn, backed by the edit journal and shared by the orchestrator and all its subagents. Mark it on any edit or write, any executed non-ReadOnly shell command and any MCP tool call. Keep the per-turn reset the owner decided. Put two items to the owner as open questions instead of implementing them: "unreviewed changes across turns", and headless `ognivo run`, where nobody reviews (deny rather than ask when there is no TTY).

## [REAL · minor] Classifier is blind to the working directory that persists between commands

**Reason.** Holds. classify_command(&str) (lib.rs:68) takes neither cwd nor workspace root, and the in-project test (lib.rs:78) is a lexical check relative to whatever the current directory is. §9 keeps cwd between commands. The probe classifies `cd ..`, `cd /` and `cd sub` as SideEffects, so leaving the project costs one approval. After that, ReadOnly and ExecutesProjectCode commands with relative arguments run silently outside the project, including tests or make in another tree. Minor because it needs one explicit user approval and the exec layer does not exist yet. But a harmless-looking `cd` approval quietly ends confinement for the rest of the session, and fixing the signature now is cheap.

**Refined fix.** Change the signature to `classify_command(cmd, &ExecContext { workspace_root, cwd })`. If the canonical cwd is not under the canonical root, return SideEffects for every command. Resolve relative path arguments against cwd before the in-root check. Implement `cd` in the exec layer: silent inside the root; outside it, ask and say that later commands there will ask too. Optionally reset cwd to the root at the start of each turn. Add tests with cwd outside the root.

## [REAL · minor] In-project path check is lexical: symlinks and attached option values read outside the project

**Reason.** Holds. leaves_project (lib.rs:78) rejects only words that start with '/' or contain '..'. The probe confirms ReadOnly for `grep -R`, `rg -L`, `rg --follow`, `find -L`, and for attached option values such as `grep -f/abs/file` and `git diff -O/abs/file`. A path that resolves through a symlink committed in the repo (git checks symlinks out as-is) also passes. That contradicts the doc comment's claim that paths able to leave the project are rejected. Practical impact is limited for now: -f and -O consume files without echoing them, a repo that plants symlinks can already run code through a clean-turn `cargo test` under the owner's rule, and the model is local. The write-through-symlink variant is covered by the fix for finding 1.

**Refined fix.** Classifier, now: treat any option word containing '/' as leaving the project. This catches attached values such as `-f/...` and `-O/...`, and is merely over-cautious for relative ones. Return SideEffects for symlink-following flags: grep `-R`/`--dereference-recursive`, rg `-L`/`--follow`, find `-L`/`-H`/`-follow`. Exec layer (M1): canonicalize every path argument relative to cwd and require it to be under the workspace root; for cat/head/tail also require a regular file. Writes resolve without following symlinks out of the root (shared with finding 1). Drop OGNIVO_API_KEY from child process environments.

## [REAL · major] Generic-path sanitizer breaks only "<|": Gemma closers and all Qwen tokens stay injectable

**Reason.** Holds against the design M1 will implement. docs/architecture.md:214 breaks only `<|`, but the vetted templates depend on markers that contain no `<|`:
- Gemma closes with `<turn|>`, `<tool_response|>`, `<tool_call|>` and `<channel|>`, and renders tool responses inside the model turn (profiles/templates/gemma-4-26B-A4B-it/canonical.jinja:168-180, 370-387);
- Qwen wraps tool output in `<tool_response>`…`</tool_response>` and uses `<tool_call>` and `<think>` (profiles/templates/Qwen3.8-27B/canonical.jinja:151-153).
M0 measured these as special tokens even with parse_special=false (docs/m0/measurements.md:64), and §5.3 itself says the chat path tokenizes with parse_special=true. So file or tool content can close or forge tool-response structure on 2 of the 3 reference models. The stated scope ("tool output and files") also leaves out several inputs that all go through the server template on the Generic path: tool and MCP declarations, server instructions, skill descriptions, AGENTS.md/CLAUDE.md, hook output, the §5.6 dynamics, subagent reports and compaction summaries. Rated major: the sanitizer is an M1 deliverable, and fixing the spec now costs only a doc change.

**Refined fix.** Rewrite the Generic bullet of §5.3:
- Break-set: derive it from the profile, i.e. every string in `tokens.structural` plus the extra specials from finding 10. Insert U+2060 after the first character of each occurrence.
- Default set for unknown models: `<|`, `|>`, `<tool_call>`, `</tool_call>`, `<tool_response>`, `</tool_response>`, `<think>`, `</think>`.
- Scope: apply it at one choke point to every string that is neither the user's typed text nor verbatim model output. That includes declarations and MCP metadata, instruction files, hook output, the dynamics block, subagent reports and summaries.
- Verification: tokenize each content span with parse_special=true and assert that no structural id appears, the same assert as on the Native path.
- Exact inverse: strip U+2060 only where the sanitizer inserted it (inside a break-set marker) and escape occurrences that were already there, so files that legitimately contain U+2060 survive edit/write.

## [REAL · minor] rg/grep/find/wc denylists miss options that run programs, load config or read path lists

**Reason.** Holds. The probe confirms ReadOnly for `rg --hostname-bin prog --hyperlink-format file --color always x`, `rg -z`, `rg --search-zip`, `find -files0-from list`, `find . -files0-from list` and `wc --files0-from list`. lib.rs:101 denies only `--pre` for rg and grep, and the find denylist at lib.rs:102-108 lacks -files0-from.
- ripgrep's --hostname-bin executes the named program.
- -z spawns decompressors from PATH.
- A RIPGREP_CONFIG_PATH file is read unless --no-config is given.
- find -files0-from and wc --files0-from read path lists from a file, which bypasses the lexical path check.
The `=` forms are already blocked by SHELL_SYNTAX. Minor: two of these depend on a writable config file or PATH, and the others only widen reads.

**Refined fix.** Add to the denylists:
- rg: `--hostname-bin`, `-z`, `--search-zip`, `-L`, `--follow`;
- grep: `-R`, `--dereference-recursive`, `-d`;
- find: `-files0-from`, `-L`, `-H`, `-follow`;
- wc: `--files0-from`.
In the exec layer, always pass `--no-config` to rg, or unset RIPGREP_CONFIG_PATH. Longer term, move to a per-tool allowlist of known-safe flags with everything else SideEffects. Add one test per flag.

## [REAL · minor] Read-only commands can block forever or stream unbounded output

**Reason.** Holds. The probe confirms ReadOnly for `tail -f`, `tail -F`, `tail --follow` and `tail --pid 1 -f x`; lib.rs:97-100 checks no flags for tail. cat, head and tail on a FIFO, or on a repo path that resolves to an endless device, also pass the lexical check. §9 says a command that hits its timeout is moved to the background instead of being killed, and that the full log is written to a file. So a never-ending ReadOnly command lingers indefinitely, and an endless one can fill the disk that ~/.ognivo/data shares. This is a reliability problem more than a confidentiality one; minor.

**Refined fix.** Classifier: for tail, return SideEffects on `-f`, `-F`, `--follow*`, `--pid` and `--retry`, or allow only `-n N` and `-c N` with relative files. Record in §9 for the M1 exec layer:
- ReadOnly commands get a hard wall-clock limit and are killed, not backgrounded;
- an output byte cap kills the process group;
- the persisted full log is capped (head plus tail, e.g. 64 MiB total).
For cat, head and tail, require a regular file after canonicalization (finding 6).

## [REAL · minor] Native guard token table and doctor probe skip specials such as Gemma <eos>/<bos>

**Reason.** Holds for both the data and the code. The specials that `structural` leaves out:
- Gemma: profiles/gemma-4-26b-a4b.toml:12-17 lists id 1 `<eos>` in eog and id 2 `<bos>` as bos;
- Qwen: eog id 248044 `<|endoftext|>` (profiles/qwen3.8-27b.toml:13-17);
- gpt-oss: `<|endoftext|>`, `<|startoftext|>`, `<|endofprompt|>` and the reserved ids.
The Native split and assert (§5.3) and doctor's probe (crates/ognivo-models/src/doctor.rs:226) use only `structural`. Upstream servers do not match CONTROL tokens with parse_special=false. The exposure is therefore mainly ik_llama.cpp (class C, Native only in degraded mode), where M0 saw the flag ignored for every token it checked, plus the Generic path, where everything is parsed. Whether ik recognizes these specific tokens was not measured, and the Native path is M2, so minor. It is a cheap data fix.

**Refined fix.** Add `tokens.specials = [{ text, id }]` to each profile. Cover bos, eos, pad, endoftext and endofprompt, plus the other CONTROL/USER_DEFINED entries from the GGUF vocab (or id ranges for the gpt-oss reserved tokens). Use `structural ∪ specials` for the Native split, the content-span assert and the Generic sanitizer (finding 7). Have doctor probe the union and report which entries the server recognizes without parse_special. Add a profile test that every `eog` id and the `bos` id are in the table; it fails today for Gemma id 1 and Qwen id 248044.

## [NOT REAL · minor] No Network effect and no effect mapping for WebFetch and MCP tools

**Reason.** Does not hold against the code. Nothing maps ToolId to Effect yet, so WebFetch and MCP calls are not classified as anything and do not run. The claimed failure needs a future mis-mapping of WebFetch to ReadOnly, which would contradict the existing doc: ReadOnly means "reads and searches inside the project" (crates/ognivo-tools/src/lib.rs:51). The existing SideEffects variant already yields Ask, so no new variant is needed for correct behaviour. This is a design recommendation for when the mapping lands in M1, not a defect in the M0 skeleton.

**Refined fix.** No change needed in M0. When the ToolId→Effect mapping lands in M1:
- write it as an exhaustive match with no wildcard arm, so every new tool must be classified before it compiles;
- map WebFetch and every `Mcp { .. }` call to SideEffects (Ask) unless a user-written rule allows that server or tool;
- never relax approval on readOnlyHint from project `.mcp.json` servers, because the MCP spec treats annotations from untrusted servers as untrusted.
`Effect::Network` is optional; add it only if the UI needs to show network calls differently.
