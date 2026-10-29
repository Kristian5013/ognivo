# agents-landscape: Architecture of current coding agents (Oct 2026) and lessons for a llama.cpp-only agent running small local models (gpt-oss-20b, Gemma 4 26B A4B)

## Executive summary
By Oct 2026 the serious coding agents have converged on one skeleton: a headless agent core that owns the loop, tools, sandbox and an append-only session log, exposed through a versioned event protocol to many thin clients. Examples: Codex's Rust app-server (JSON-RPC thread/turn/item over stdio JSONL, used by its TUI, exec, VS Code, the desktop app, JetBrains and Xcode); OpenCode 2.0's single background service shared by TUI, web and an Electron desktop (Kilo Code rebuilt both its CLI and its VS Code extension on it in Feb-Apr 2026); Cline v4's host-independent SDK behind a 224-RPC gRPC bridge; OpenHands' event-sourced SDK; and ACP (registry live 2026-01-28 in Zed and JetBrains) for embedding in editors. Native tool calling has won (Roo removed XML in Jan 2026; Cline v4, Codex, Claude Code and OpenCode are native). Every harness that works well with local models still carries model-specific repair paths (OpenCode toolParser, nanocoder XML/JSON modes, Goose toolshim, Continue system-message tools), because servers and small models deviate from the expected formats. Frontier harnesses carry 15-30K tokens of fixed prefix (Claude Code: about 4-6K of system prompt plus 11-21K of tool schemas; the TodoWrite description alone is 2,037 tokens). Local-model guides instead recommend minimal prompts: pi (4 tools, under 1K tokens), Cline's compact prompt (about 10% of the full one), and Unsloth's `claude --bare` with Gemma 4 26B A4B (Oct 2026). Tool count matters: tool-selection accuracy collapses as catalogs grow (RAG-MCP: 13.6% without retrieval vs 43.1% with it; Anthropic: degradation beyond 30-50 tools, and tool search lifts Opus 4 from 49% to 74%). Cursor, Claude Code and Qwen Code now lazy-load MCP tools, and Cursor also its own built-in tools (-46.9% tokens). With llama.cpp, however, the chat template renders tools into the system turn, so changing the tool list re-prefills the whole context; lazy exposure has to go through append-only meta-tools. KV-cache reuse is the dominant local-performance factor (Manus: input:output is about 100:1). Claude Code's per-request attribution header made local inference about 90% slower until it was disabled. Both reference models' templates drop the previous turn's reasoning, which forces one partial re-prefill per user turn. Both also require passing reasoning back inside a tool-calling turn (OpenAI harmony docs, Google Gemma 4 docs). For edits, the evidence favours exact old/new replacement with layered tolerant matching plus lint/LSP gating: Diff-XYZ; Gemini CLI Smart Edit (edit errors about 20% -> 6%); SWE-agent lint gating (+3 points); llama.cpp's own agent replaced apply_diff with exact edits plus unicode normalization (Jul 2026). gpt-oss was trained around a Codex-like exec + apply_patch harness, yet in aider it produced 23% malformed search/replace responses vs 0.4% malformed whole-file responses. Reasoning effort is a big lever for gpt-oss (SWE-bench Verified 37.4% at low vs 60.7% at high). On terminal-heavy work, third-party Terminal-Bench 2.1 (Terminus-2 harness) puts Gemma 4 26B A4B at about 39% and gpt-oss-20b at about 14%. Community reports say Gemma 4 tends to stop early or plow ahead on assumed defaults, so continuation nudges and clarifying-question tools matter. Best terminal practice is a PTY session manager (Codex exec_command/write_stdin with yield timers and output budgets), spilling long output to files with previews (Claude Code, Cursor), and a PTY shared with the user (Warp Full Terminal Use). Safety has converged on OS sandboxes (Codex: bwrap + Landlock + seccomp + network proxy; Claude Code: bubblewrap/socat), because users approve 93% of prompts. Churn in the ecosystem is high: Roo sunset 2026-05-15, Continue acquired by Cursor 2026-06-16, Void archived 2026-06-02, Warp open-sourced 2026-04-28. Codex itself is now a poor base for local models: Responses-only since Feb 2026, freeform-only apply_patch since May 2026, and generic fallback metadata for unknown models.

## Recommendation
The 15 architectural lessons for our agent:

1. Use a headless Rust core with one versioned protocol, and treat every UI as a client. Copy the Codex app-server shape: JSON-RPC 2.0 over stdio/websocket, Thread/Turn/Item, item/started -> deltas -> item/completed, server-initiated approval requests that pause the turn, steer/interrupt/fork/rollback, generated TS/JSON-Schema types, and an append-only event log so clients can reconnect. GUI, TUI, headless exec, an ACP adapter (Zed/JetBrains) and an optional MCP-server mode all speak it. Codex, OpenCode 2.0, Kilo, Cline v4 and OpenHands all converged on this.

2. Store per-model behaviour as data in a 'model profile', never in scattered ifs. A profile covers:
   - chat transport (llama-server --jinja vs agent-rendered prompt over /completion);
   - reasoning policy;
   - sampling (gpt-oss T=1.0, top_p=1.0; Gemma 4 T=1.0, top_p=0.95, top_k=64);
   - edit-tool variant, prompt addendum and tool-description variants;
   - effective-context budget and compaction thresholds;
   - parallel_tool_calls;
   - known quirks and the repairs allowed for them.
   Ship tested profiles for gpt-oss-20b and Gemma 4 26B A4B, and refuse or warn rather than silently using generic metadata (Codex's fallback assumes 272K context and 10 KB truncation).

3. Keep the fixed prefix tiny and stable. Target at most ~2K tokens of system prompt plus at most ~2-3K of tool schemas for the core loop. Use progressive disclosure: skills and rules show only name and description, docs are fetched on demand. Claude Code spends 15-30K up front, while pi (under 1K), OpenCode 2.x (~200-token base) and Cline's compact prompt are what local users rely on.

4. Engineer context for the KV cache first.
   - Keep the prefix byte-identical: no timestamps, random IDs or headers; deterministic JSON key order; tool set fixed for the session.
   - Keep history append-only; deliver state changes (plan, diagnostics, environment, rules) as appended messages (OpenCode keyed instructions, pi section deltas).
   - Tune llama-server: cache_prompt, --ctx-checkpoints, checkpoints at user messages, a deliberate choice on --swa-full for Gemma's sliding-window attention.
   - Budget one partial re-prefill per user turn, because both templates drop the previous turn's reasoning.
   - Measure the cache-hit rate.

5. Use a small, stable core toolset and expose extras lazily without mutating the declared list. Core: read (paged, with line numbers), edit (per model), write, exec/write_stdin, search (ripgrep and glob), update_plan, ask_user. MCP and rarely used tools sit behind 1-2 meta-tools (tool_search / mcp_call) whose schemas come back as tool results. Changing the tools array re-renders the system turn in both templates and invalidates the cache. Evidence: RAG-MCP, Anthropic tool search, Cursor's -46.9% tokens, Claude Code dropping Glob/Grep, Manus 'mask, don't remove'.

6. Make the edit tool per model, with layered matching and actionable errors.
   - gpt-oss: the apply_patch envelope (its trained format) as a JSON function tool with a single 'input' string, plus interception of `apply_patch` heredocs run through the shell.
   - Default and Gemma 4: exact old/new replacement (single or edits[]). Match exactly first, then after stripping line-number prefixes, then after whitespace/unicode normalization, then with flexible indentation. Enforce uniqueness and reject overlapping or ambiguous edits.
   - Use whole-file write for new or small files (aider: gpt-oss-20b whole files were 99.6% well-formed vs 77.3% for search/replace).
   - On failure, return the nearest candidate lines with line numbers.
   - Measure edit-failure rate per model; Gemini CLI's Smart Edit went from about 20% to 6% edit errors.

7. Close the feedback loop inside the tool result. After every edit, parse with tree-sitter and pull LSP diagnostics (errors only, capped, as OpenCode v1 did); optionally reject edits that introduce syntax errors (SWE-agent: +3 points). Offer a cheap test/lint command, and cap reflection loops (aider uses 3).

8. Make the terminal a PTY process manager owned by the core.
   - Exec with an optional tty and session IDs; write_stdin/poll with yield timers (Codex defaults: 10 s yield, 10K-token output budget).
   - Write full output to a log file and return a head/tail preview with the path (Claude Code: 30K chars inline, otherwise file + 2K preview).
   - Know which exit codes are benign; auto-background long commands; kill process trees.
   - Snapshot the user's login-shell environment; apply a cgroup memory cap and run inside the OS sandbox.
   - Render the same PTY live in the GUI so the user can watch and take over (Warp Full Terminal Use).

9. Make the plan a tiny structure that the harness recites. Use one update_plan tool with Codex's schema (step + status, at most one in_progress). The harness re-appends the current plan near the end of the context every N steps and after compaction (Cline Focus Chain, Manus todo.md). Avoid giant tool descriptions such as Claude Code's 2,037-token TodoWrite. Offer plan/act modes in which planning only gets read-only tools.

10. Treat reasoning as a first-class control. Set effort per phase: high for planning and debugging, low for mechanical steps (gpt-oss SWE-bench Verified: 60.7% high vs 37.4% low). Use --reasoning-budget and reasoning_control to end runaway thinking. Always pass reasoning_content back inside a tool-calling turn and drop it at turn end. For long runs, feed back a short text summary of earlier thoughts, as Google advises for Gemma 4.

11. Build guardrails for the ways small models fail.
    - Detect loops: identical-call repetition (Cline 3/5, Qwen 5) and stagnation (Qwen's 8 shell inspections; Grok Build's 8/12 stationarity guard).
    - Cap retries of malformed tool calls; nanocoder hit OOM without a cap.
    - Nudge on empty responses or premature stops (Gemini CLI's auto-nudge; Gemma 4 'gives up' reports).
    - Never execute tool calls cut off by max-tokens (pi).
    - Return validation errors to the model as tool errors and keep failures in the context.
    - Never add meta-parameters such as security_risk to tool schemas; local models broke on it in OpenHands.

12. Design compaction for small effective windows.
    - Trigger early, against measured effective context rather than the nominal window.
    - Elide old tool outputs before summarizing.
    - Use a structured handoff summary (Codex's prompt).
    - Keep the last few observations verbatim (SWE-agent: last 5 beats full history).
    - Keep the full transcript on disk with a search_history tool (Cursor), and persist the plan across compaction.

13. Start code intelligence with agentic search plus a tree-sitter repo map (aider: ~1K tokens, PageRank) and LSP symbols. Add a local embedding index later, served by llama.cpp; Cursor measured +12.5% average accuracy, more on large repos.

14. Default to OS-level safety and cheap undo.
    - Sandbox the shell (bubblewrap + Landlock/seccomp + network proxy, as Codex and Claude Code do).
    - Use allow/ask/deny prefix rules (Codex execpolicy, Claude Code `Bash(npm run *)`).
    - Checkpoint into a shadow git repo that also captures shell side effects; Claude Code's checkpoints miss Bash changes.
    - Don't rely on prompts (93% get approved) or on an LLM judge built from the same small model.

15. Adopt the standards and evaluate from day one.
    - Standards: MCP client (stdio + streamable HTTP per 2026-07-28, lazy exposure), ACP agent mode plus registry entry, AGENTS.md and SKILL.md, hooks shaped like Claude Code's.
    - Evaluation: an eval harness (Terminal-Bench 2.x via Harbor with Terminus-2 parity, a SWE-bench Verified subset, aider polyglot, your own repo tasks), tracking tool-call validity, edit-failure rate, loop rate, tokens per turn, cache-hit rate and wall-clock time per model profile.
    - Results swing hugely with harness, template and format: gpt-oss-20b scores 34.2% on aider polyglot in OpenAI's run vs 55.6% in a community whole-format run; on TB2.1 it gets ~14% vs ~39% for Gemma 4 26B.

## Architecture implications
- Crate split for the Rust core:
- agent-core: the loop and state machine.
- protocol: JSON-RPC types and schema generation, modelled on codex app-server-protocol.
- model-profiles: data plus traits for prompt rendering and output parsing.
- llm-llamacpp: client for /v1/chat/completions, /completion, /tokenize, /apply-template, slots and metrics.
- tools: fs, edit engines (str_replace and apply_patch), PTY manager, ripgrep-based search, tree-sitter repo map, LSP client.
- sandbox: bwrap/Landlock/seccomp.
- checkpoints: shadow git via gix or git2.
- mcp: the rmcp crate.
- acp: the agent-client-protocol crate.
- store: SQLite plus JSONL rollouts.
- evals.
- Codex's crates are Apache-2.0, so their designs (and possibly code) are reusable: apply-patch parser and applier, execpolicy, the linux-sandbox/bwrap launcher, the unified-exec PTY session manager, and the app-server protocol design.
- Two model transports are needed. (a) llama-server --jinja chat completions, relying on server-side native parsers (Harmony, gemma4 PEG). (b) An agent-rendered raw /completion path for things the chat API cannot do: a custom grammar together with tools is rejected; grammar-constrained apply_patch; Manus-style constraints on which tools may be called; exact control over CoT passback.
- The context manager must own prompt determinism: canonical JSON serialization, a stable tool ordering, no volatile system-prompt sections, state injected only as appended messages, and cache-hit telemetry from llama-server timings.
- Tool exposure has two tiers: a fixed core set declared in the tools array, and a lazy tier reached only through meta-tools whose schemas come back in tool results. This keeps the system turn, where both the Gemma 4 and gpt-oss templates render tools, stable for the whole session.
- The edit subsystem is pluggable per profile: apply_patch for gpt-oss, str_replace/edits[] for Gemma 4 and the default. All variants share a matching cascade, ambiguity checks, tree-sitter syntax validation, LSP diagnostics injection, and diff events for the GUI (turn-level aggregated diff, as Codex's turn/diff/updated provides).
- The terminal subsystem is a PTY multiplexer shared by agent and user: session IDs, incremental reads, full logs written to files, ANSI-aware previews, a vt100 terminal model so the agent can read the state of TUIs, cgroup limits and sandbox wrapping. The GUI renders the same PTY.
- Protocol events must carry reasoning deltas (raw for local models, like Codex's reasoning item content), the plan, diffs, command output deltas, token usage and context-window fill (ACP usage_update), so a Cursor-level GUI can show live state without extra parsing.
- Optional llama-server lifecycle management, in the style of Hermes Agent / unsloth start: launch with per-profile flags (ctx size, reasoning flags, cache-ram, ctx-checkpoints, KV types), health/metrics probes, and router mode for model swaps.
- The evaluation harness is part of the architecture, not an afterthought: replay recorded sessions against new llama.cpp commits and profiles, and gate releases on Terminal-Bench-style and SWE-style subsets per model.

## Risks and gotchas
- Ways to lose the KV cache:
- volatile bytes at the top of the prompt (Claude Code's attribution header: ~90% slowdown);
- timestamps;
- non-deterministic JSON;
- changing the tool list, which both templates render into the system turn;
- editing or compacting history;
- Gemma 4's sliding-window attention, where reuse depends on context checkpoints (or --swa-full at a memory cost).
- Both reference templates drop the previous turn's reasoning when a new user message arrives, so the part of the prompt after the first dropped segment is re-prefilled every user turn; long tool-heavy turns make this expensive. Measure it; keep turns focused and compact early.
- Forgetting to pass reasoning_content back inside a tool-calling turn quietly degrades gpt-oss and Gemma 4 tool use (OpenAI and Google docs; reports of 5+ turn degradation for gpt-oss).
- llama.cpp's chat layer changes fast (Oct 2026 'chat : refactor API', 'replace output string with parts'; gemma4 required-tool grammar fixed 2026-09-19). Pin the llama.cpp commit per release and keep per-model template/parser regression tests.
- llama.cpp limits: parallel tool calls are disabled unless requested; custom grammars cannot be combined with tools; /v1/responses is only a conversion shim; freeform/custom tools (Codex's apply_patch) are not supported; server-side MCP is stdio-only.
- Gemma 4 behaviour: early 'I'm done' stops and claims of progress without doing it; it assumes defaults instead of asking (BFCL miss_param 31.5%); it may emit thoughts with thinking disabled; its tool calling depended on llama.cpp/Ollama parser fixes through Sept 2026.
- gpt-oss behaviour: quality depends heavily on reasoning effort (SWE-bench Verified 37.4% low vs 60.7% high), so latency and quality trade off sharply; it is weak on terminal benchmarks (~14% TB2.1); it is sensitive to its native apply_patch/Codex conventions; anecdotally, re-quantizing away from MXFP4 hurts.
- Small models break on schema bloat: extra required meta-parameters (OpenHands' security_risk), long tool descriptions, many similarly named tools.
- Recovery parsers can backfire: duplicated 'ghost echo' tool-call text in history, retry loops that never end (nanocoder OOM). Every repair needs caps, telemetry and a profile flag.
- Approval fatigue (93% of prompts approved) makes prompt-only safety illusory; an LLM safety judge built on the same 20-30B local model is slow and unreliable.
- Published scores vary enormously with harness, template and edit format (gpt-oss-20b aider polyglot: 34.2% in OpenAI's run vs 55.6% in a community whole-format run), so third-party leaderboards cannot replace evaluations on our own harness.
- Ecosystem churn: Roo sunset, Continue acquired and shut down, Void archived, Codex dropped Chat Completions. Basing the core on another product's internals is risky; borrow designs and permissively licensed code instead.
- Parallel subagents on one 24 GB GPU compete for slots: llama-server splits the KV cache per slot unless it is unified, and concurrency lowers per-request speed. Use subagents mainly to isolate context, mostly serially.
- Context windows are nominally 128K (gpt-oss) and 256K (Gemma 4), but reliability degrades with length (context rot); a large -c also costs VRAM. Set compaction thresholds from measurements, not from the model card.

## Questions for owner
- Product shape: an agent-first desktop workbench (chat, plan, diff review, file tree, terminals, like the Codex app or OpenCode desktop) or a full editor/IDE at Cursor level (code editing surface, inline edits, tab completion)? Should v1 also run inside existing editors via ACP (Zed/JetBrains) or as a VS Code extension?
- GUI technology: is a webview UI (Tauri + TS/Solid/React with Monaco/CodeMirror and xterm.js) acceptable on top of the Rust core, or must the UI be pure Rust (GPUI/egui/Slint/iced)? This decides how fast Cursor-level polish is reachable.
- Template and parsing ownership: rely on llama-server --jinja and its per-model parsers, implement our own Harmony/Gemma 4 renderers and parsers over raw /completion (full control, grammar-constrained tool calls and patches), or go hybrid?
- Should the agent own the llama-server lifecycle (spawn with per-model flags, context and KV-cache settings, model swaps via router mode, health and metrics), or only attach to user-managed, possibly remote, servers?
- Default safety posture: OS sandbox with auto-run inside the workspace (Codex/Claude Code style), approval-first (Cline), or full auto (pi)? And what is the default network policy for shell commands?
- VRAM policy on 24 GB: one main model only, or main model plus a small helper (summaries, titles, embeddings, maybe draft or MTP models) running concurrently? Are parallel subagents allowed at reduced per-request speed?
- Code intelligence in v1: tree-sitter repo map plus LSP only, or also a local embedding index (extra model, indexing pipeline, storage)?
- Model and evaluation scope: only gpt-oss-20b and Gemma 4 26B A4B for v1, or a general profile system for several families from day one? Which benchmarks or internal task suites should gate releases?
- Platform scope: Linux-only first (bwrap/Landlock, Unix PTYs), or cross-platform macOS/Windows from the start (Seatbelt/ConPTY/Windows sandbox)?

## Unknowns
- Actual prefill and decode throughput, KV memory and cache-hit behaviour of gpt-oss-20b and Gemma 4 26B A4B on the owner's RTX 5090 Laptop at 32-128K context. Not measured; GPU jobs were not allowed.
- Effective long-context reliability (RULER/NoLiMa-style) of both models was not verified.
- Which coding harness and edit format Gemma 4 was RL-trained on. Google publishes the tool-call tokens but not harness details, so the best edit format for Gemma 4 needs our own A/B tests.
- Whether gpt-oss-20b does better in multi-turn agent loops with apply_patch than with str_replace. Only indirect evidence exists: OpenAI's Codex-like eval harness, the gpt-oss reference tool, aider's whole-vs-diff data.
- The Terminal-Bench 2.1 figures come from an aggregator listing entries as 'Imported', most likely from Artificial Analysis; the primary per-model pages were not retrieved. The Gemma 4 BFCL numbers are a single community run.
- Claude Code and Cursor are closed source; details come from docs, extracted prompts and secondary analyses. Cursor's current apply pipeline in particular is described by third parties.
- Whether OpenCode 2.x has restored LSP diagnostics in edit results: the HEAD source shows a TODO, while the v1.18 teardown documents the feature.
- How large the benefit of todo recitation / Focus Chain is for small models: vendors describe it qualitatively, with no controlled numbers.
- How Gemma 4 and gpt-oss behave with parallel_tool_calls=true in llama.cpp, and how much MTP/DSpark speculative decoding (recently added for Gemma 4) helps agent workloads.
