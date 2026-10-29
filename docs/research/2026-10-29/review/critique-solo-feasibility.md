# critique: solo-feasibility

## Overall
VERDICT. The language and process choices hold up for one person and should stay: Rust core, TS/React UI packaged in Electron, Python dev-only, llama-server as the only engine, no FFI, the Generic path first and then Native codecs per family, exec in-process. Also keep: per-model edit formats, KV discipline, plan recitation, T0 goldens, early ACP.

The plan around them does not hold up:
- Only M0–M1 have durations; M2–M6 have none, and no milestone has exit criteria.
- M5 bundles ~10–15 weeks of security-critical subsystems (my estimate).
- The eval design needs several times the GPU hours one shared laptop GPU provides, and at realistic n it cannot detect the effects it is supposed to decide (±11 pts CI at 80 runs).
- Read literally, the draft reaches a v1 in roughly 7–11 months, with no point where anyone can say it is good.

Calibration from the owner's own history: cforge went from an empty repo to ~75K inserted lines (46K Rust LOC) and 23 ADRs in ~1.5 days (commits 2026-10-19/20) without running inference, and the tree was wiped 9 days later. Code volume is not the constraint. The binding constraints are:
1. GPU hours for real-model validation: one 24 GB GPU, one model at a time, shared by dogfooding, development and evals.
2. The owner's review and judgment.

Therefore: a thin vertical slice first, measurement from week 1, data gates on expensive work (Native Gemma, schema-GBNF, sandbox depth), and the GUI built by background agents against recorded traces. The gain is order, bounded scope and measurable progress, not raw speed.

CORRECTED PLAN
Calendar weeks at ~70 h/wk on the agent project only; multiply by 1.7–2 if cforge continues. A milestone more than 30% over its upper bound is re-scoped, not pushed through (cforge ADR-0019). Thresholds are proposals to calibrate after the M0 baseline.

M0 Measure + baseline (1–1.5 wk)
- Pinned llama.cpp CUDA sm_120 build; Gemma 4 GGUF; llama-bench at depths 0/16/32/64K for both models.
- One KV experiment per model with the planned CoT policy: re-prefill cost of a new user turn, checkpoint hits.
- llama-server router mode with an INI preset per model, no own supervisor. Verify /completion token arrays, return_tokens, grammar_lazy, /tokenize and /slots through the router.
- Vetted templates plus Jinja2 goldens.
- Baseline night: OpenCode (plus Claude Code --bare or the llama.cpp WebUI agent) on 10 own-repo tasks × 2 models, giving a failure catalog.
- At most 6 ADRs.
- Exit: perf/VRAM/KV table, baseline table, failure catalog.

M1 Walking skeleton (3–4 wk)
- Generic loop with two TOML profiles.
- Eight tools: read; per-model edit (apply_patch via vendored codex-apply-patch for gpt-oss, edit{file,find,replace} for Gemma); write; shell over pipes with output budget and spill to file; search; glob; update_plan with recitation; ask_user.
- Approvals with a read-only allowlist; edit-journal undo; loop guards; step-level compaction; special-token escaping; oom_score_adj; AGENTS.md.
- SQLite event log with seq and a nullable raw-token column; JSONL/ATIF traces.
- Headless exec, a minimal REPL, and --dump-prompt.
- T0, T2-mini (60–100 scenarios) and own-repo suite v0.
- Exit: one week of at least one real task per day; T2-mini parse_ok ≥98% and first-try edit-apply ≥90% on both models; 0 truncated calls executed; own-repo success at or above the OpenCode baseline; fixed prefix plus tools ≤~5K tokens.

M1b ACP v1 minimal (0.5–1 wk)
- Exit: daily use from VS Code (community ACP client) or Zed, with diffs, thoughts and permissions.

M2 Native gpt-oss + eval at scale (3–4 wk)
- oss-harmony rendering, tolerant token-level parser, verbatim token replay, frozen date.
- A/B of system-namespace vs functions tool layout; alias router; repair ladder.
- Harbor TB2.1 stratified 15–20-task subset with a local Terminus-2 baseline. T2 grows to ~250 scenarios.
- The event model freezes at exit.
- Exit: format failures ≤0.5%, in-turn cache hit ≥95%, no success regression beyond CI. Then Native becomes the gpt-oss default.

M3 Gemma 4 + MCP (4–5 wk; gates can remove 1–3 wk)
- One week of Gemma harness fixes on Generic.
- Native Gemma codec only if T2 or KV telemetry shows Generic losses; schema-GBNF only if invalid args stay above 1–2%.
- MCP stdio (rmcp Auto) with the per-dialect gateway and tool_search/mcp_call; T2 at 5/15/40 tools.
- Exit: Gemma scorecard vs baseline; MCP tool_correct ≥90% and no prefix change when a server attaches.

M4 Own GUI MVP (6–8 wk calendar, mostly agent-built with ~20–30% of the owner's time, overlapping M3)
- Component kit on mocks and recorded traces from wk ~5.
- Thin JSON-RPC and wiring after the M2 freeze; Electron spawns the core as a sidecar; trimmed MVP.
- Exit: the owner moves daily use to it.

M5 Safety + terminal depth (4–6 wk)
- bwrap + seccomp sandbox v1: network off/on, fail-closed, auto-run.
- PTY sessions with vt100 and write_stdin/poll; GUI attach and takeover; optional memory caps via systemd-run --user --scope.
- Exit: escape tests pass (AF_UNIX to docker.sock, D-Bus and ssh-agent; secret dirs; network).

M6 Public v0.1 (3–5 wk)
- Packaging (.deb/AppImage), docs, onboarding, rewind of code and conversation, hunk-level review, @-mentions, ACP registry entry.

Resulting dates:
- Daily CLI: wk ~5–6. In-editor via ACP: wk ~6–7.
- Native gpt-oss default: wk ~10. Gemma + MCP: wk ~14–15.
- Own GUI daily: wk ~16–17. Sandboxed auto-run: wk ~20–21.
- Public v0.1: wk ~24–27 (~6 months).

Swap M2 and M3 if the owner's first-focus model is Gemma.

## Problems
- **[critical] The milestone plan is unbounded. Only M0 (1–2 wk) and M1 (3–5 wk) have durations; M2–M6 have none, and no milestone has exit criteria. The GUI is declared 'parallel with M2–M3', M5 lumps sandbox + checkpoints + full MCP gateway + PTY/takeover together, and M6 is open-ended polish. The two outcomes that matter, 'usable daily' and 'measurably good', are never defined. Risk 11 lists 'объём работ для соло' with no mitigation.**
  - why: Code volume is not this owner's limit: cforge produced ~75K inserted lines (46K Rust LOC) and 23 ADRs in ~1.5 days without running inference, then the tree was wiped. Without exit criteria the same throughput produces breadth without validation. The real serial resources are the single 24 GB GPU (one model at a time) and the owner's review, so 'parallel GUI' is only partly parallel. By my estimate M5 alone is 10–15 weeks: sandbox 2–3, proxy 1–2, Landlock ~1, PTY/VT/takeover 2–3, shadow snapshots/restore/fork 2–3, MCP gateway 2–3. Read literally, the draft gives a v1 at ~7–11 months, the first safety boundary at month ~4–6, and no point where it can be called 'good'.
  - fix: Replace section 10 with the corrected plan in overall_assessment. Every milestone gets an upper-bound duration and numeric exit criteria, and a milestone running more than 30% over its upper bound is re-scoped (cforge ADR-0019 rule). Define 'usable daily' as at least one real task per day on the owner's repos for a full week. Define 'measurably good' as a per-profile scorecard: T2 micro-metrics plus own-repo and TB2.1-subset success against baselines run on the same pinned server. Each milestone ends with a usable artifact, not a layer.
- **[critical] The eval plan (T0–T4: a nightly live suite, Harbor/TB2.1 + SWE-bench subset + own repos, and BFCL/τ2 via a 'profile proxy') has no size. The research it builds on proposes 30 TB2.1 + 30 SWE tasks nightly and 89 TB tasks weekly at k≥3 for two models, all on one laptop GPU that also serves dogfooding and development.**
  - why: gpt-oss-20b on high used ≈85K output tokens per τ²-airline task, about 10 min of pure decode at ≈150 tok/s; Gemma used ≈20K. TB2 per-task agent timeouts are 900 s (some 3600 s), and models at 14–39% pass rates often run into them. 60 tasks per model per night ≈ 20–30 GPU-h for both models against a ~7 h night. Full TB2.1 for both models at k=3 ≈ 90–135 GPU-h/week. The design is also underpowered: 40 tasks × 2 seeds gives a 95% CI of about ±11 pts, so the Native-vs-Generic A/B that gates M2/M3 cannot be decided on task success. Running parallel slots to save time stretches wall-clock per task, which causes more timeouts and makes TB scores incomparable with single-slot Terminus-2 baselines.
  - fix: (1) Add a GPU budget: a ~7 h night window, one model loaded during the day, and a lock file so background coding agents never start GPU jobs while the owner dogfoods. (2) Metric hierarchy: transport and profile decisions rest on high-n T2 micro-metrics with paired seeds (parse_ok, schema_ok, tool_correct, first-try edit-apply, truncated-call rate, loop rate, cache-hit %, re-prefill tokens, tokens/turn). Task success counts only for effects ≥10 pts. (3) Cadence: T0 per commit on CPU. T2 nightly at k=1 for both models, k=3 weekly. T3 rotates ~15–25 tasks per night into a weekly scorecard: own-repo 20 tasks × 2 models × k=2 plus a TB2.1 stratified 20-task subset × focus model × k=2 ≈ 120 runs ≈ 25–30 GPU-h/week, with -np fixed and recorded. (4) Defer T1 prompt cassettes until after M2. Defer T4, the profile proxy, BFCL, τ2 (it needs a user-simulator LLM on the same GPU) and SWE-bench until after v0.1.
- **[major] M1 front-loads infrastructure and leaves out what makes daily use possible. M0 includes 'протокол', M1 includes T1 cassettes and the ACP adapter, and section 5's exec design (EXIT-trap env persistence, per-session cgroups) has no phasing. M1's scope line, meanwhile, does not schedule an approval policy (only the M5 sandbox), compaction (section 4 compacts only at turn boundaries), loop guards, special-token handling on the Generic path, or protection of llama-server from agent-spawned builds.**
  - why: (a) The Generic chat path flattens the prompt and tokenizes it with parse_special=true. Dogfooding the agent on its own repo, whose codec sources, tests and goldens are full of Harmony/Gemma control strings, will inject control tokens from week one. (b) Autonomous tasks overflow inside a single user turn: gpt-oss on high overflowed 128K in the harmonyagent SWE-bench runs, and 50 tool steps exceed Gemma's 64–96K working budget. (c) On 62 GB RAM, llama-server holds --cache-ram 8 GiB plus Gemma checkpoints (~200 MiB each, up to 32 per slot), so a parallel cargo or npm build can get it OOM-killed. (d) Without approvals there is no safe daily use until month ~5.
  - fix: M1 is the Generic loop for both profiles, with:
- vetted templates, reasoning_content round-trip, one retry with a new seed on HTTP 500 parse errors, and a finish_reason=length guard;
- reversible escaping of special-token strings in tool output, un-escaped in edit/write arguments, plus a repo convention never to write <|…|> literals contiguously in the agent's own sources and tests (split them or escape them);
- step-level compaction at 60–70% of the profile budget (elide old tool outputs, then a structured summary plus the plan);
- loop and stuck guards;
- approvals (a read-only allowlist via tree-sitter-bash, ask for everything else, remembered prefix rules) and an edit journal for undo;
- oom_score_adj=+500 on spawned commands, and cwd persistence only.
Move the protocol out of M0, T1 to after M2, and ACP to M1b.
- **[major] There is no competitive baseline and no early check of the model ceiling. The only comparison is T4 (our layer vs llama.cpp's built-in path via a profile proxy), and it comes late. 'Model ceiling' is listed as a risk with nothing to test it.**
  - why: The product claim is 'a better agent for these models than what exists'. Public numbers are thin and depend on the harness: TB2.1 under Terminus-2 is gpt-oss 13.9% and Gemma 39%; Gemma SWE-V is 17.4 (Alibaba); gpt-oss reaches 60.7 only with a native-Harmony harness. A one-night baseline also yields the failure catalog that should set priorities. If Gemma's losses are early stops and 'announcing instead of calling' rather than malformed args, the 1–2 weeks of schema-GBNF work are wasted.
  - fix: In M0, run OpenCode, plus Claude Code --bare and the llama.cpp WebUI agent if practical, against the same pinned llama-server on 10 own-repo tasks × 2 models. Classify failures as format, wrong tool, edit-apply, early stop, loop, context overflow or wrong code. At the start of M2, run Harbor's Terminus-2 locally on the same TB2.1 subset. Every milestone exit reports deltas against these baselines.
- **[major] The GUI is sequenced too early and scoped too wide. The protocol is designed in M0, before the item model exists. The GUI MVP runs parallel with M2–M3 and includes file view/edit, hunk-level accept/reject, terminal, model manager, MCP manager, debug view and Playwright. The draft also carries CLI, TUI, a browser tab 'for remote access', Electron, and snapshot+seq reconnection.**
  - why: A core-to-GUI protocol fixed before the loop exists will churn in two languages at once. During M2–M3 the owner's attention is on the codecs. A TUI is a third full frontend: Codex's Ratatui TUI crate is ~390K lines of Rust, against ~6.5K for its headless exec. Remote access implies auth and TLS hardening. The research's 8–12-week GUI MVP estimate already assumes a stable event API.
  - fix: In M1, define only the event model, as Rust types (Thread/Turn/Item with item kinds as in the Codex app-server), with ts-rs TS generation; traces need it anyway. From week ~5, background agents build the GUI component kit against mocks and recorded M1 traces, verified with Playwright screenshots and no GPU: streaming markdown, virtualized thread, tool card, @pierre/diffs, read-only xterm.js, CM6 viewer, approval dialog, plan, context/cache meter.

Freeze the event model at the M2 exit, then add a thin JSON-RPC. Electron main spawns the core as a sidecar, and reconnect means replaying events from seq out of SQLite: no AHP-style snapshots, no multi-client, no daemon.

Trimmed MVP (6–8 wk): sessions, chat with reasoning, tool cards, approvals, plan, meters, file-level accept/revert, a read-only viewer and agent-terminal view, a model/profile picker over router /models, settings, and a debug pane. Cut the TUI. The browser tab stays a localhost dev mode only.
- **[major] The safety and checkpoint stack is one monolithic M5: bwrap + seccomp + Landlock, cgroups v2, a host proxy with allowlist, the tree-sitter classifier, shadow-git snapshots before every non-read-only command, and restore of code, dialog, both or fork.**
  - why: This is exactly what cforge's ADR-0019 lists as where AI agents do worst (security, sandbox, concurrency) and where owner review is mandatory. Codex's linux-sandbox is 12.8K LOC and depends on codex-protocol, codex-sandboxing and the 35K-LOC network-proxy built on rama 0.3.0-alpha.4. That makes it a design reference, not a cheap vendor; codex-apply-patch, by contrast, needs only its ExecutorFileSystem shim replaced. Shadow-git snapshots are what Codex enabled by default in November 2025 and removed in January 2026 because they caused 'significant problems', and Cline's shadow repo has corruption reports. Fork and rewind on SWA models cost re-prefills.
  - fix: Phase it.
- MVP (M1): approvals, the edit journal and the user's git, plus a warning on irreversible commands.
- Sandbox v1 (M5): the system /usr/bin/bwrap with ro /, rw workspace and session tmp, tmpfs over ~/.ssh, ~/.gnupg, ~/.aws, ~/.config/gh and $XDG_RUNTIME_DIR. Seccomp denies AF_UNIX sockets, io_uring, ptrace and the mount/unshare family. Network is --unshare-net off or on only. A fail-closed health check, and auto-run inside the sandbox. Write it by hand (~1.5–2K LOC) from the Codex and Zed designs, with the owner reviewing every line.
- After v0.1: allowlist proxy, Landlock RESOLVE_UNIX, a persistent per-thread sandbox with port publishing, cgroup scopes.
- Cut shadow-git entirely. Add 'rewind code+conversation' in M6 and fork later.
- **[major] M3 fixes 'Native Gemma 4 + schema-GBNF + A/B' as a deliverable whatever M1–M2 show.**
  - why: The benefit is unmeasured; the research found no ablations. Gemma's dominant reported failures are behavioural: describing a call instead of making it, an early 'done', invented CLI flags, quoting errors, repetition at high context. The Generic path already uses llama.cpp's dedicated PEG_GEMMA4 parser and lazy grammar, and one community suite went from 59 to 70 out of 90 just by using a proper Gemma parser. A schema compiler over the Gemma DSL is real work: case-insensitive sorted keys, alternation for optional keys that sort before required ones, token-exact strings, all validated on GPU.
  - fix: Open M3 with a week of Gemma harness work on Generic, measured on T2 and T3: flat string args, edit{file,find,replace}, rules aimed at its failure modes, early-stop nudges and a per-step thinking budget. Build the Native Gemma codec (renderer, token parser, verbatim replay) only if T2 or KV telemetry shows Generic losses such as preamble reordering, dictsort or serialization prefix breaks, or injection. Build the schema-GBNF only if schema-invalid args stay above ~1–2% after validation plus one retry. Native gpt-oss stays unconditional in M2, because the validated container/repo_browser system-namespace layout cannot be expressed through chat-completions tools.
- **[major] The full MCP gateway is deferred to M5 (month ~5), although MCP is one of the owner's top-priority items and its hard part is model-layer work.**
  - why: The hard part is normalizing schemas per dialect (gpt-oss TypeScript, the Gemma OpenAPI subset, the GBNF subset) and exposing tools lazily without breaking the cached prefix, since the tools array sits in the cached prefix in both templates. With small models MCP is mainly a risk: gpt-oss-20b tool selection falls from 0.85 at 4 tools to 0.65 at 128. Left until M5, the profiles and T2 scenarios freeze without MCP coverage.
  - fix: Move MCP into M3, next to Gemma: rmcp 3.x stdio with ClientLifecycleMode::Auto and the detected era cached per server; namespacing and sanitization; the per-dialect normalizer with full JSON-Schema validation; output caps with spill handles; tool_search and mcp_call meta-tools; T2 scenarios at 5, 15 and 40 tools, checking that the prefix does not change when a server attaches. Defer Streamable HTTP, OAuth/CIMD (it needs a hosted HTTPS metadata document), Tasks, MCP Apps, hash pinning and elicitation. Do not auto-load a repo's .mcp.json until workspace trust exists.
- **[major] The plan assumes the full ~70 h/week, but the relation to cforge is unresolved: its working tree was wiped on 10-29, while cforge ADR-0019 still commits ~72 h/week to the CPU-engine roadmap.**
  - why: If both projects continue, every duration grows by ×1.7–2 under cforge's own rule. And if cforge ever becomes the engine, a backend boundary that is not token-level would force the codecs to be rewritten.
  - fix: Get an explicit owner decision; the corrected plan assumes 100% on the agent. Keep the Native backend interface token-level (complete(tokens, sampling, constraint, slot) returning a token stream, plus tokenize), so a future cforge backend plugs in without touching the codecs.
- **[major] Daily dogfooding drives the plan, but both reference models are weak or slow at long agentic work, and the draft lists this risk with no hedge.**
  - why: Under Terminus-2, TB2.1 is gpt-oss-20b 13.9% and Gemma 4 26B 39%. Gemma scores SWE-V 17.4–25.8 and MRCR-128K 44.1. gpt-oss on high, the draft's main-loop setting, uses ≈85K output tokens per τ² task, about 10 min of decode, which makes interactive loops slow. If the owner cannot get real work done, the cheapest feedback loop stalls. The community's local picks score far higher: Qwen3.6-35B-A3B at TB2.1 ~45% and SWE-V 73.4, Qwen3.6-27B at TB2.1 ~61%.
  - fix: Ask the owner. The cheap hedge is a third Generic profile for a stronger model as the daily driver: 2–3 days for template vetting plus TOML, after checking it fits in VRAM. gpt-oss and Gemma stay the eval targets the profiles are tuned for. Pick one reference model as the first focus; on latency grounds that is Gemma, at ≈4× fewer output tokens per task. Consider medium effort for interactive gpt-oss sessions and high for autonomous runs, measured on T2.
- **[minor] The ACP adapter is folded into an already-large M1 with undefined scope. The research's full ACP list (registry authMethods, load/resume/list/close/delete, config options, notices, acp-tck) is ~2–3 weeks.**
  - why: Early-dogfooding value is real for this owner, who works in VS Code. Community VS Code ACP clients exist: formulahendry.acp-client shows reasoning, tool calls, permissions and protocol traffic, and Multicoder adds per-file diffs and registry installs; Zed is native. But the full surface is not needed, and the crate churns: 1.0 to 3.6 in four months, with v2 in draft.
  - fix: Make it M1b, 0.5–1 wk, after the CLI works. Implement initialize, session/new, prompt, cancel and request_permission. Send session/update with message and thought chunks, tool_call and tool_call_update (diff content; command output as text content), and plan. Terminals stay agent-owned; skip the client fs and terminal methods, which v2 removes anyway. Pin the crate. Defer registry, load/resume and config options to M6.
- **[minor] M0 plans a hand-written llama-server supervisor: spawn, /health + /props handshake, restart with backoff, Unix socket.**
  - why: Router mode already provides INI presets with every CLI flag (including chat-template-file), --models-max 1, load/unload/autoload, crash detection with exit_code, and routing (POST by the 'model' field, GET by ?model=). It also gives one shared server to the CLI, ACP sessions and the GUI. Otherwise two frontends each spawning a llama-server will OOM a 24 GB GPU.
  - fix: In M0, run router mode with a presets INI under systemd --user, or spawn it if absent behind a lock. Spend an hour verifying /completion token arrays, return_tokens, grammar_lazy, /tokenize and /slots through the router. The agent only checks /props against the pinned build and template hash. Write an own supervisor only if a requirement fails.
- **[minor] Section 4 puts LSP diagnostics into every edit result.**
  - why: An LSP client means managing a server per language, document sync and async diagnostic timing; rust-analyzer alone takes minutes to index a large workspace. Making that robust is 2–4 weeks. The evidence is for lint-gating (SWE-agent +3 pts), not for LSP as such.
  - fix: In M1, run a tree-sitter syntax check on every edit, plus an optional per-project check command (cargo check, tsc --noEmit, ruff) after batches of edits, returning errors only and capped. LSP comes after v0.1.
- **[minor] Thirteen crates and two pnpm apps from day one.**
  - why: Early boundaries are guesses, and moving them is churn. Under ADR-0019's 'one task = one crate' rule for AI agents, wrong boundaries are expensive.
  - fix: Start with ~6 crates: core, models (profiles, codecs, llama client), tools (fs/edit/exec/search), store, evals, cli (with an acp module). Split out protocol, mcp and sandbox when they actually arrive.

## missing_from_draft
- Numeric exit criteria for every milestone, upper-bound durations for M2–M6, and a re-scope trigger (cforge ADR-0019's +30% rule).
- A GPU-hour budget and schedule: a ~7 h night window, one model loaded during the day, a lock or lease so background AI coding agents never start GPU jobs while the owner dogfoods, and a fixed, recorded -np for evals.
- A competitive baseline on the same pinned server and tasks (OpenCode, Claude Code --bare, llama.cpp WebUI agent, Harbor's Terminus-2), plus a failure catalog by class to seed T2 scenarios and set priorities.
- A metric hierarchy and power analysis. Micro-metrics decide transports and profiles; task success only for effects of 10 pts or more (n≈80 gives a CI of ±11 pts). Note that gpt-oss on high costs ~4× Gemma's output tokens per task.
- Step-level (mid-turn) compaction for long autonomous tasks: gpt-oss on high overflows 128K within one task, and Gemma's working budget is 64–96K.
- Special-token safety on the Generic path in M1: reversible escaping in tool output, un-escaping in edit/write arguments, and a repo rule never to write <|…|> literals contiguously in the agent's own codec sources and tests, so the agent can work on itself.
- Protecting llama-server from agent-spawned builds: oom_score_adj +500 on child processes now, memory caps via systemd-run --user --scope later.
- A nullable raw-token-IDs column per assistant segment in the M1 store, so Native M2 needs no migration. The token-level backend trait is also the plug point for a future cforge engine.
- An explicit non-goals list for v0.1: multi-user, remote access/TLS, macOS/Windows, vision, subagents/parallel slots, FIM/Tab completion, embeddings index.
- The split between work AI coding agents can own (GUI kit, CLI, store, ACP adapter, MCP client) and work the owner must own and review (codec tolerance rules, KV invariants, sandbox, permission engine), reusing ADR-0019's review gates.
- Recorded-trace replay as GUI fixtures and e2e input, so GUI work never needs the GPU.
- The Terminal-Bench integration approach: run the static agent binary inside Harbor task containers as an installed agent, pointing at the host llama-server. This removes any need for a remote exec backend; verify container-to-host networking early in M2.
- A definition of which model is the first tuning focus and daily driver; it decides the M2/M3 order.

## things_to_cut_or_defer
- TUI: cut. The GUI, the CLI (exec plus a minimal REPL) and ACP cover the needs; Codex's TUI crate alone is ~390K lines.
- Internal core-to-GUI JSON-RPC and snapshot+seq replay: defer from M0 to M4. Until then only Rust event types plus JSONL traces. Reconnect replays events from seq out of SQLite; no AHP-style snapshots, multi-client claims or reducers.
- Long-lived agentd daemon and multi-window: defer past v0.1. The CLI and ACP processes embed the core; Electron spawns it as a sidecar.
- Separate execd and a ProcessBackend for remote or devcontainer backends: defer past v0.1. The draft is right to defer execd; also drop the remote abstraction, since Harbor runs the binary inside the container.
- Hand-written llama-server supervisor: replace with router mode plus INI presets. Write one only if the router fails a requirement.
- Sandbox depth beyond v1: allowlist network proxy (Codex's is 35K LOC), Landlock, persistent per-thread sandbox with port publishing, cgroup scopes via zbus. All post-v0.1. v1 in M5 is bwrap + seccomp with network off/on only.
- Shadow-git workspace snapshots before commands: cut. Codex removed its version after shipping it, and Cline's has corruption reports; use the edit journal plus the user's git.
- Conversation fork, /slots save/restore to NVMe, cache-epoch UI: defer until a measured need.
- EXIT-trap env persistence across commands: defer; persist cwd only.
- LSP diagnostics after edits: defer past v0.1. Use a tree-sitter syntax check plus a per-project check command instead.
- MCP Streamable HTTP + OAuth/CIMD, Tasks extension, MCP Apps, definition hash pinning, elicitation: defer past v0.1. M3 is stdio only, with no auto-loading of a repo's .mcp.json without workspace trust.
- ACP beyond a minimal v1 subset (registry/authMethods, load/resume/list, config options, v2): defer to M6 or later.
- Eval: T1 prompt-hash cassettes until after M2. T4, the profile proxy, BFCL, τ2 (needs a simulator LLM on the same GPU) and SWE-bench subsets past v0.1. Full TB2.1 runs at most monthly.
- Native Gemma codec, and especially the schema-aware Gemma GBNF: a data-gated option in M3, not a fixed deliverable.
- GUI MVP extras: CM6 editing with LSP, hunk-level accept/reject (do file-level first), MCP manager UI, model download manager, remote browser access, inline edit/FIM, worktrees and a multi-agent dashboard. All post-MVP.
- Hooks, Agent Skills, plan/act modes: defer past v0.1. AGENTS.md stays in M1.
- 13-crate split upfront: start with ~6 crates.
- MTP/EAGLE3 speculative-decoding tuning, Gemma vision, and carrying a llama.cpp patch set: defer or avoid; the Native path removes the need for patches.

## questions_to_add_for_owner
- Is cforge paused? What share of the ~70 h/week goes to the agent? All durations assume 100%; multiply by 1.7–2 if the time is shared.
- Until your own GUI exists (~wk 16), will you dogfood via ACP inside VS Code (community ACP client extensions) or Zed, or only in a terminal CLI?
- What does 'Cursor-level' mean for v1: an agent workspace like Cursor 3's Agents Window or the Codex app (sessions, diffs, terminal), or a full editor with inline edit and Tab/FIM completion? FIM needs a second model, and 24 GB holds one.
- Which reference model is the first tuning focus and your daily driver? gpt-oss-20b on high uses ≈85K output tokens per τ² task and is slow; Gemma 4 26B uses ≈20K and is better on terminal tasks. This decides the M2/M3 order.
- If both reference models turn out too weak for your real tasks, may a stronger local model (e.g., Qwen3.6-27B or 35B-A3B via a Generic profile) be the daily driver while gpt-oss and Gemma remain the eval targets?
- GPU budget: how many night hours can evals use? Can they also run in the daytime when you are not dogfooding? Are parallel slots acceptable, given that they make TB scores incomparable with single-slot baselines?
- Is approval-per-command without an OS sandbox acceptable for your own daily use until ~wk 20? Can the repo go public before the sandbox lands?
- What 'measurably good' result would satisfy you: beating OpenCode / Claude Code --bare on your own repo tasks by 10 pts or more, beating Terminus-2 on a TB2.1 subset, or both? Is it OK to run those competitor harnesses against your local llama-server for baselines?