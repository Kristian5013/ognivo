# mcp-acp: Protocols a local-model coding agent must speak in Oct 2026: MCP (client), ACP (Agent Client Protocol), related conventions (AGENTS.md, Agent Skills, hooks, slash commands), Codex app-server and other reference protocols, plus the recommended internal core-UI protocol

## Executive summary
The current MCP spec is 2026-07-28 (stable since 2026-07-28). It is a breaking 'stateless core' release. It drops the initialize handshake and Mcp-Session-Id. Every request now carries its protocol version and capabilities in _meta. Servers must implement server/discover. subscriptions/listen replaces the GET stream and resources/subscribe. Multi Round-Trip Requests (MRTR) replace server-initiated sampling, elicitation and roots requests. Tasks moved into an official extension. The release also adds cache hints (ttlMs/cacheScope) and the Mcp-Method/Mcp-Name headers. Roots, Sampling, Logging, HTTP+SSE and Dynamic Client Registration are deprecated, with a deprecation window of at least 12 months. Most MCP servers in the wild still use the initialize-based protocol, and a modern-only client fails against them. Our client must therefore support both protocol eras. The official Rust SDK rmcp does this: v3.x, Tier 1, 3.8.0 released 2026-10-26, client conformance 32/32. It uses ClientLifecycleMode::Auto and adds automatic MRTR retry, a response cache, OAuth, and stdio, child-process and Streamable HTTP transports. It deliberately ships no legacy HTTP+SSE transport. llama.cpp at 5e03a1ff4 has its own experimental server-side tools and an MCP client. That client is stdio-only and hard-coded to protocol 2024-11-05, so the agent must own the MCP client and the tool loop and only send normalized tool definitions to /v1/chat/completions. For small local models the real limits are the prompt budget, how each chat template renders tools, and llama.cpp grammar limits. gpt-oss renders tools as TypeScript inside `namespace functions` and turns anyOf/$ref into `any`. Gemma 4 renders only an OpenAPI-like subset. llama.cpp's JSON-Schema-to-GBNF conversion silently skips unsupported keywords. Tool-selection accuracy drops as tool count grows: for gpt-oss-20b it fell from 0.85 to 0.65 going from 4 to 128 tools. This calls for progressive tool discovery, with a switch threshold of 1-5% of the context per the official MCP client best practices. It also calls for sanitized namespaced names, per-model schema normalization with client-side validation, output caps with spill handles, and a tool list that stays stable so the KV cache can be reused. Tool descriptions and tool results are untrusted input (tool poisoning, rug pulls, shadowing). The spec requires showing the exact command before configuring a local server. ACP is governed jointly by Zed and JetBrains: v1 is stable and v2 has been a Draft since 2026-07-20. ACP gives editor integrations almost for free: Zed, JetBrains IDEs (2025.3.2+, with the ACP Registry), Neovim, Emacs, Obsidian and marimo, and VS Code only through community extensions. More than 50 agents implement it, including Gemini CLI, Copilot CLI, Cursor, Cline, and the Codex and Claude adapters. ACP v2 removes client-side file system and terminal methods, so the agent must own command execution and terminals regardless. ACP suits an external adapter but is too narrow for the internal core-GUI protocol. It has no multi-client snapshot/replay, no model/runtime management, no changesets or checkpoints, and it is moving from v1 to v2 while its Rust crate went 1.0 to 3.6 in four months. Better design references for the internal protocol are Microsoft's AHP 1.0 (2026-10-02, MIT: channels, reducer-based state, replay from sequence numbers, Rust crates, and the VS Code Agents window can attach to third-party hosts) and the Codex app-server (Thread/Turn/Item, item lifecycle, approvals as server requests, schemas generated from Rust). Conventions to support: AGENTS.md (now under the Agentic AI Foundation), Agent Skills SKILL.md with .agents/skills, Claude Code-format hooks, and slash commands built from skills, MCP prompts and ACP availableCommands. The MCP Registry is still a preview (API v0.1) aimed at aggregators, so use it only as an optional catalog.

## Recommendation
PROTOCOL ORDER (each step builds on the previous one):
P0. MCP client plus a 'tool gateway'. Build on rmcp 3.x with the transport-child-process, transport-streamable-http-client-reqwest, auth and elicitation features. Support both eras with ClientLifecycleMode::Auto, and cache the detected era per server config to avoid the 10 s fallback probe. Scope:
- tools/list and tools/call, with outputSchema validation and structuredContent.
- resources/read, with resource_link handling that never auto-fetches.
- prompts, exposed as slash commands.
- progress and cancellation (notifications/cancelled on stdio, closing the stream on HTTP).
- list_changed, via subscriptions/listen on modern servers or legacy notifications.
- elicitation in form and URL modes through MRTR.
- roots only on legacy-era sessions.
- no sampling and no logging (both deprecated).
Read the mcpServers JSON format (.mcp.json, Cursor, Claude, llama.cpp) and import the VS Code servers format. Do not use llama.cpp --tools or --mcp-servers-config: that client is 2024-11-05-only, stdio-only and experimental. llama-server stays a pure inference backend.
P0 in parallel: the internal core-GUI protocol v0 (design below).
P1. Conventions:
- AGENTS.md: nested, closest wins, CLAUDE.md fallback.
- Agent Skills: scan .agents/skills, the native dir and .claude/skills; three-tier disclosure.
- Slash commands built from skills, MCP prompts and built-ins.
- Hooks: a subset compatible with Claude Code's format (SessionStart/End, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, Stop, PreCompact; exit 2 blocks; JSON hookSpecificOutput). Give built-in tools Claude Code-compatible canonical IDs (Bash, Read, Edit, Write, Grep, Glob) so third-party hooks and skill allowed-tools match. The names shown to the model are mapped separately per model.
- Load repo-provided hooks, MCP servers and skills only after the user marks the workspace trusted.
P2. Remote and long-running MCP:
- OAuth 2.1: PKCE, RFC 9728/8414 discovery, RFC 8707 resource, RFC 9207 iss validation, issuer-bound credentials in the OS keyring, CIMD with a project-hosted metadata document, DCR fallback with application_type=native.
- The Tasks extension, rendered as background jobs.
- MCP Apps, only if the GUI embeds a webview (sandboxed iframe).
P3. ACP v1 agent adapter over stdio, using agent-client-protocol 3.x. Implement initialize with authMethods (the registry requires them; e.g. a terminal-auth flow that configures the local model endpoint), session/new|load|resume|list|close|delete, prompt and cancel, request_permission backed by our policy engine (ACP says the agent stores 'always' decisions), plan, availableCommands, config options (local model preset, reasoning effort, mode), usage_update, notices and compaction. Feed MCP passthrough (stdio and http) into the same gateway. Never depend on client fs or terminal methods (they are gone in v2); the agent owns execution and reports output as tool-call content. Run acp-tck in CI. Add v2 behind a feature flag once it stabilizes. This gives Zed, JetBrains, Neovim, Emacs and other clients.
P4 (optional):
- An AHP host over WebSocket, for the VS Code Agents window (protocol path proven by pi-ahp).
- An ACP-client mode in our GUI, to drive third-party agents.
- An MCP server mode that exposes our tools.
- ACP remote transport once its RFD lands.

TOOL EXPOSURE FOR SMALL LOCAL MODELS (gpt-oss-20b, Gemma 4 26B-A4B):
- Budget: keep upfront tools within 1-5% of context or about 12 tools: roughly 8 built-ins plus tool_search. Measure the budget in real template tokens with llama-server /apply-template and /tokenize for each model profile.
- Defer MCP tools behind tool_search(query, detail=name|summary|schema), ranked by BM25, optionally plus embeddings from llama.cpp /embedding. Return the top N grouped by server, and refresh on list_changed.
- Keep the KV prefix stable. Either append promoted definitions as tool-result text and call them through a stable mcp_call(tool, args) meta-tool, or accept one re-prefill per promotion. Pick per model by measurement. Never re-sort the tools array.
- Names: '<server_key>__<tool>', sanitized to [a-z0-9_] and at most 64 chars, with a deterministic collision suffix and a reverse map. No '.' (it breaks gpt-oss's to=functions.<name>) and no '-' (not a valid TS identifier).
- Per-dialect schema normalizer:
  - dereference $ref, merge allOf, turn anyOf[T,null] into nullable or a type list;
  - drop keywords the dialect or GBNF cannot express, keep enums, shorten descriptions to the first sentence, and sort keys deterministically;
  - for gpt-oss, emit a TS-friendly flat shape; for Gemma 4, emit the OpenAPI subset and map outputSchema to its 'response' declaration.
- Validate arguments against the original schema with a full JSON Schema 2020-12 validator, and return corrective isError-style messages on failure.
- Results: cap per model (for example 2-4k tokens at 32k context). Truncate head+tail with an explicit marker and a read_output(handle, range) tool. Strip ANSI codes. Send images only to Gemma 4; give gpt-oss a text or resource link instead.
- Code mode is a phase-2 experiment, run in a sandbox (bubblewrap + Landlock + seccomp, or Wasmtime/Deno). The broker re-checks permissions on every call. For gpt-oss, evaluate its native python tool channel.
- Security:
  - pin a SHA-256 of each tool definition and re-prompt on change (rug-pull defense);
  - show full AI-visible descriptions in the UI and treat annotations as hints only;
  - run local servers sandboxed with no network unless granted, after showing the exact command;
  - once untrusted content has entered context, require approval for egress-capable tools;
  - keep an audit log of every call.

INTERNAL CORE-UI PROTOCOL:
- Process and transport: a Rust host process (daemon) owns sessions, the llama-server lifecycle and model presets, MCP connections, PTYs, file edits and checkpoints, and policy. The GUI, CLI/TUI, IDE adapters and tests are all clients. Wire format is JSON-RPC 2.0 over three transports: newline-delimited stdio for embedded and headless use; WebSocket over a Unix domain socket as the GUI default; and loopback TCP with a capability token, rejecting Origin-bearing requests, following Codex and pi-ahp. The same message set runs on every transport.
- Versioning: semver protocol versions negotiated in initialize, capability objects, and an x-/_ namespace for extensions.
- State: AHP-style channels (host://, session/<id>, chat/<id>, terminal/<id>, changeset/<id>, runtime://models). Subscribing returns a snapshot plus a sequence number, followed by ordered actions. On reconnect the client sends lastSeenSeq and receives a replay or a fresh snapshot. This supports multiple windows and crash recovery.
- Conversation model: Codex's Thread -> Turn -> Item, with ACP-v2 semantics (stable IDs; patch rules where omitted means unchanged, null clears and chunks append; foreground state running/requires_action/idle with stopReason; steer and queue during a turn; interrupt; fork at an item; checkpoint and restore). Item kinds:
  - user_message, agent_message (commentary/final), reasoning (raw/summary, mapped from Harmony analysis or the Gemma thinking channel);
  - tool_call (canonical id, model-facing name, server, raw and validated args, status, progress, content blocks, structured result, truncation handle);
  - command_execution (pty id, cwd, exit code, duration, output deltas, user/agent control claim);
  - file_change (structured add/delete/modify/move plus unified diff);
  - plan, approval_request (subject command/file_write/mcp_tool/network; scope once/session/always), elicitation, compaction, notice/error;
  - usage (context fill, prefill/decode tok/s, KV cache reuse).
- Server-to-client requests are reserved for human decisions (approval, elicitation). Everything is cancellable ($/cancel_request).
- Model-debug surface, needed to tune each model to 'ideal': the rendered prompt, the token stream tagged by channel, parser and grammar diagnostics, and the active model profile.
- Source of truth: one Rust crate with serde types plus schemars (JSON Schema 2020-12) plus ts-rs/specta, publishing generated schemas the way Codex does (generate-ts/json-schema). Golden-transcript tests. ACP and AHP adapters are thin projections of the internal items.

## Architecture implications
- The agent, not llama-server, must own the whole tool loop: an MCP client (rmcp 3.x, dual-era), built-in tools, permission policy, sandboxing, per-model tool rendering and argument parsing/validation; llama-server is used only as an OpenAI-compatible inference backend with --jinja (its server-side MCP is 2024-11-05/stdio-only and its /tools API is internal).
- A 'tool gateway' layer sits between MCP servers and the model: namespacing/sanitization, per-model-dialect schema normalization (gpt-oss TypeScript rendering vs Gemma 4 OpenAPI subset vs llama.cpp GBNF subset), deterministic ordering, progressive discovery (tool_search), output capping/spill, definition hashing/pinning, audit log.
- Canonical tool identity (for hooks, permissions, skills allowed-tools, audit) must be decoupled from model-facing tool names, because model families need different naming/format and hooks/skills ecosystems expect Claude Code-style names (Bash, Read, Edit, mcp__server__tool).
- The core must own terminals (PTY) and file I/O itself: ACP v2 removes client fs/terminal execution and AHP/Codex treat command execution as agent/host-owned items; the GUI only renders and optionally claims a PTY for user takeover.
- The internal protocol should be a superset of ACP v2 concepts (stable IDs, patch semantics, idle/running/requires_action, structured file changes, subject-based permissions) and AHP sync concepts (channels, snapshot+seq replay) so that ACP and AHP adapters are thin projections rather than rewrites.
- KV-cache economics in llama.cpp make the tools array part of the prompt prefix: changing tools mid-session forces re-prefill, so tool promotion strategy (append vs stable meta-tool vs re-prefill) is a per-model-profile setting measured on the RTX 5090.
- Workspace trust is a first-class concept: repo-provided AGENTS.md is instructions-only, but repo-provided hooks, .mcp.json servers and skills with scripts are code execution and must be gated behind explicit trust and an exact-command consent UI.
- Protocol schemas live in one Rust crate (serde + schemars + ts-rs/specta); GUI tech choice affects only the client binding (Rust-native client vs generated TS types for a webview), but MCP Apps support requires a webview.
- Remote MCP with OAuth implies project infrastructure: a stable HTTPS-hosted Client ID Metadata Document (CIMD) with localhost redirect URIs, plus OS keyring storage keyed by issuer.
- ACP registry listing requires authMethods even for a local-only agent, so the ACP adapter needs a terminal/agent auth flow (e.g., configuring the local model endpoint) rather than 'no auth'.

## Risks and gotchas
- MCP era split: modern-only clients fail against legacy (initialize-era) servers and vice versa; rmcp Auto mode may wait up to 10 s on silent legacy stdio servers before falling back, so cache the era per server config.
- rmcp intentionally has no legacy HTTP+SSE transport; old SSE-only remote servers need a proxy (or a custom transport).
- MCP 2026-07-28 allows arbitrary JSON Schema 2020-12 (incl. $ref/composition), but llama.cpp GBNF conversion silently skips unsupported keywords and both model templates silently degrade them (gpt-oss -> 'any', Gemma ignores anyOf/$ref); without normalization plus validation the model sees wrong types and the grammar does not enforce them.
- Tool names with '.' or '-' (legal in MCP) break gpt-oss's TypeScript 'type <name>' and 'to=functions.<name>' conventions; server display names are not unique; llama.cpp itself silently skips colliding <server>_<tool> names.
- Tool selection degrades with tool count (gpt-oss-20b 0.85 -> 0.65 from 4 -> 128 tools); naive 'expose every MCP tool' configurations will fail on local models long before the context is full.
- Large tool outputs can flood a 32-64k local context; Claude Code's defaults (25k tokens) are far too large for local models; truncation must be explicit and recoverable via handles.
- Tool poisoning, rug pulls and shadowing: descriptions/annotations/results are untrusted; servers can change definitions after approval; results can carry prompt injection that triggers exfiltration (lethal-trifecta conditions: private data + untrusted content + egress).
- Repo-provided hooks (.claude/settings.json is read by Claude Code, Cursor, Copilot CLI and VS Code) and .mcp.json commands execute code on clone-and-open unless trust-gated.
- ACP churn: v2 Draft breaks prompt lifecycle, diffs, permissions and removes fs/terminal/modes/session load; the Rust crate went 1.0 -> 3.6 between 2026-06-24 and 2026-10-28; pin versions and isolate ACP behind an adapter.
- ACP remote transport is not standardized yet (RFD); Streamable HTTP variant requires HTTP/2; do not build remote GUI on ACP.
- AHP is young (repo 2026-03-12, v1.0.0 2026-10-02) and Microsoft/VS Code-driven; third-party host attachment in VS Code relies on settings described as experimental (signed-out mode desktop-only).
- Deprecated MCP features (Roots, Sampling, Logging) are still used by legacy servers; dropping roots entirely can break servers that derive allowed directories from roots.
- MCP Registry is preview with no durability guarantees and is not meant for direct host consumption; treat it as an optional catalog and still require exact-command consent.
- Hook compatibility is shallow: event names, tool names and payloads differ across harnesses (VS Code explicitly warns); claim 'Claude Code-compatible subset' rather than full compatibility.
- MCP Apps requires a web runtime; a pure Rust-native GUI cannot render them without embedding a webview.
- OAuth for desktop clients: CIMD cannot prove which local process owns a localhost redirect; DCR is deprecated but still what many authorization servers support; RFC 9207 iss must be validated when present.

## Questions for owner
- Process model: should the GUI talk to a separate long-running core host (daemon) over a socket (enables multiple windows, CLI/TUI, IDE adapters, remote GPU box, crash isolation) or embed the core in-process? This decides transport, the snapshot/replay design and whether an AHP-style protocol is worth it.
- External integrations priority: is running inside Zed/JetBrains/Neovim via ACP an MVP goal or a later phase, and do you want VS Code coverage via an AHP host (experimental path) at all?
- Should our GUI also act as a universal ACP client for third-party agents (OpenCode, Codex/Claude adapters pointed at llama.cpp /v1/messages or /v1/chat/completions), or strictly host only our own agent? If yes, the internal protocol should be modeled even more closely on ACP.
- Remote MCP scope: only local stdio MCP servers in MVP, or also remote OAuth-protected servers (GitHub, Linear, etc.)? The latter requires hosting a CIMD document on a project HTTPS domain and keyring integration.
- Sandboxing level for shell commands and local MCP servers on Linux: Landlock/seccomp/bubblewrap per process, rootless containers (podman), or none by default with approvals only? This also decides whether code-mode (model-written scripts calling MCP tools) is allowed.
- Ecosystem compatibility: should the agent read Claude Code-specific files (.claude/skills, .claude/commands, .claude/settings.json hooks, CLAUDE.md) in addition to the vendor-neutral AGENTS.md and .agents/skills, and should built-in tool IDs follow Claude Code names for hook/skill compatibility?
- GUI runtime: will the GUI embed a webview (Tauri/wry) or be fully native (GPUI/egui/Slint)? Only a webview can render MCP Apps (ui:// iframes) and reuse the TS SDKs of ACP/AHP/MCP.

## Unknowns
- Share of MCP servers in the wild that already implement 2026-07-28 (modern or dual-era) versus legacy-only; not measured anywhere I found.
- Real tool-selection accuracy of gpt-oss-20b and Gemma 4 26B-A4B on llama.cpp quantized GGUFs with our normalized schemas; the only gpt-oss data point is a bachelor thesis via OpenRouter with synthetic tools, and none was found for Gemma 4.
- How llama.cpp's PEG parsers for Harmony and Gemma 4 handle tool names containing '.' or '-' (inferred from the Jinja templates, not tested).
- Whether llama.cpp passes gpt-oss 'builtin_tools' (python/browser) through chat_template_kwargs and parses 'to=python' calls, which would enable a native code-mode channel for gpt-oss.
- ACP v2 stabilization date and how long Zed/JetBrains will keep v1 support.
- Whether VS Code will keep allowing arbitrary third-party AHP hosts in the Agents window (documented by third-party projects, settings marked experimental), and whether a Rust server-side AHP SDK exists beyond the client crates.
- MCP Registry GA date (listed as 'ideating' with no target).
- Exact JetBrains ACP behaviour with local-only agents that expose a no-cost auth method, and whether the ACP registry CI accepts such authMethods for a local-model agent.
