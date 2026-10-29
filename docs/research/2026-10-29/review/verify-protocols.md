# verify: protocols

## [CONFIRMED] MCP spec revision 2026-07-28 is the latest stable revision; it is a stateless-core release removing the initialize handshake and Mcp-Session-Id and deprecating roots, sampling, logging and HTTP+SSE.
**Corrected:** Correct. 2026-07-28 is the 'Current' revision. It was tagged on GitHub 2026-07-28T16:47Z, after an RC on 2026-05-29. The previous revision is 2025-11-25.

Removed:
- the initialize / notifications/initialized handshake (SEP-2575). Each request now carries protocolVersion and clientCapabilities in _meta.
- protocol-level sessions and the Mcp-Session-Id header (SEP-2567).
- ping, logging/setLevel, notifications/roots/list_changed, resources/subscribe/unsubscribe, the HTTP GET stream, and SSE resumability (Last-Event-ID).

Added:
- server/discover, which servers MUST implement.
- subscriptions/listen.
- the MRTR pattern (InputRequiredResult replaces server-initiated roots/list, sampling/createMessage and elicitation/create).
- a required resultType field.
- ttlMs/cacheScope cache hints.
- required Mcp-Method/Mcp-Name headers.
- tasks, moved out of the core into the io.modelcontextprotocol/tasks extension.

Deprecated in 2026-07-28:
- Roots, Sampling and Logging (SEP-2577). The earliest removal is the first revision released on or after 2027-07-28.
- OAuth Dynamic Client Registration, in favour of Client ID Metadata Documents (PR #2858).

Two details are off: HTTP+SSE was already deprecated in 2025-03-26 and is only reclassified under the new lifecycle policy (SEP-2596). The includeContext values were likewise reclassified. Elicitation is not deprecated.
**Evidence:** https://modelcontextprotocol.io/specification/2026-07-28/changelog ; https://modelcontextprotocol.io/specification/versioning (says 'The current protocol version is 2026-07-28') ; https://modelcontextprotocol.io/specification/2026-07-28/deprecated ; GitHub API repos/modelcontextprotocol/modelcontextprotocol/releases (tag 2026-07-28 published 2026-07-28T16:47:49Z, 2026-07-28-RC 2026-05-29)

## [CONFIRMED] rmcp (official Rust MCP SDK) is at v3.x (3.8.0 around 2026-10-26) and supports both 2026-07-28 and legacy initialize-based servers (ClientLifecycleMode::Auto), stdio + Streamable HTTP, OAuth.
**Corrected:** Correct.
- **Release:** rmcp 3.8.0 reached crates.io on 2026-10-26T22:03Z. Its CHANGELOG dates it 2026-10-23. 3.0.0 came out on 2026-07-28. The repo is modelcontextprotocol/rust-sdk, MSRV is 1.88 and the edition is 2024.
- **Lifecycle modes:** ClientLifecycleMode has three variants: Initialize, Discover{preferred_versions} and Auto{preferred_versions, legacy_version}. Auto probes server/discover. It falls back to the legacy initialize handshake if the peer reports it is legacy or does not answer within 10 s (DEFAULT_AUTO_DISCOVER_TIMEOUT). A legacy server that ignores the probe therefore costs about 10 s at startup.
- **Default is still legacy:** plain ServiceExt::serve() uses Initialize. To get Auto you must call ClientServiceExt::serve_with_lifecycle.
- **Versions:** KNOWN_VERSIONS covers 2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25 and 2026-07-28. LATEST is 2026-07-28 and LATEST_WITH_INITIALIZE is 2025-11-25.
- **Transports:** stdio() and stdio_pipes() (feature transport-io), TokioChildProcess, a Streamable HTTP client and server, and a Unix-socket HTTP client.
- **OAuth:** the auth feature, plus auth-client-credentials-jwt and auth-enterprise-managed.
- **Not supported:** there is no WebSocket transport (the ws module is commented out) and no legacy HTTP+SSE client transport.
- **License:** Cargo says Apache-2.0. The LICENSE file adds that the project is moving from MIT to Apache-2.0, and contributions without relicensing consent remain MIT.
**Evidence:** crates.io API /api/v1/crates/rmcp/versions (3.8.0 created 2026-10-26T22:03:56Z, Apache-2.0) ; downloaded crate source https://static.crates.io/crates/rmcp/rmcp-3.8.0.crate, unpacked at /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/dl-rmcp-3.8.0/rmcp-3.8.0 (src/service/client.rs:699-719 enum and 10 s timeout ; :757-783 default serve uses Initialize ; :861-900 Auto fallback ; src/model.rs:167-207 version constants ; src/transport.rs:85-122 transports, with line 111 '// pub mod ws;' ; Cargo.toml:67,142-169 auth/transport features ; CHANGELOG.md:10,227 ; LICENSE:1-5)

## [CONFIRMED] ACP (Agent Client Protocol): v1 stable, v2 draft since about 2026-07-20; Rust crate agent-client-protocol 3.x (3.6.0); clients include Zed, JetBrains (ACP registry live since 2026-01-28), Neovim, Emacs; v2 removes client-side fs/terminal methods.
**Corrected:** Correct.
- **Versions:** ACP protocol version 1 is the stable surface. The latest v1 schema is schema-v1.27.0, and the agent-client-protocol-schema crate is at 1.13.0; both were released 2026-10-27. 'ACP v2 is Available in Draft' was announced 2026-07-20, the same day as schema-v2.0.0-alpha.1. v2 is now at 2.0.0-alpha.10 (2026-10-27) and is still labelled draft. The migration guide says to gate it behind version negotiation and feature flags.
- **Rust SDK:** agent-client-protocol 3.6.0 was published 2026-10-28T08:10Z (Apache-2.0, repo agentclientprotocol/rust-sdk, MSRV 1.88). v2 support sits behind the unstable_protocol_v2 feature. The SDK's 3.x major version is separate from the protocol version.
- **Clients:** Zed, JetBrains, Neovim (CodeCompanion, avante.nvim, agentic.nvim, hermes.nvim plugins) and Emacs (agent-shell.el) are all on the official clients list, among many others.
- **Registry:** Zed and JetBrains launched the ACP Registry together on 2026-01-28. JetBrains called it a beta release.
- **v2 removals:** fs/read_text_file, fs/write_text_file and terminal/create, output, release, wait_for_exit and kill. v2 also removes session/load (use session/resume with replayFrom) and session/set_mode (now config options). Agent-owned terminal output becomes a display-only surface (terminal_update, terminal_output_chunk). For client-side tools, the guide says to use client-provided MCP servers.
**Evidence:** https://agentclientprotocol.com/updates (July 20, 2026 'ACP v2 is Available in Draft') ; https://agentclientprotocol.com/protocol/v2/migration (method table marks fs/* and terminal/* as Removed) ; https://agentclientprotocol.com/get-started/clients ; GitHub API repos/agentclientprotocol/agent-client-protocol/releases (schema-v2.0.0-alpha.1 2026-07-20T19:39Z ; schema-v2.0.0-alpha.10 2026-10-27) ; crates.io API agent-client-protocol 3.6.0 (2026-10-28) ; https://zed.dev/blog/acp-registry (Jan 28, 2026) ; https://blog.jetbrains.com/ai/2026/01/acp-agent-registry/

## [CONFIRMED] Microsoft's Agent Host Protocol (AHP) 1.0 was released 2026-10-02 (MIT) and the VS Code Agents window can attach to third-party hosts.
**Corrected:** Correct.
- **Release:** AHP Spec v1.0.0 was released 2026-10-02T23:35Z, together with the Swift, Go and TypeScript v1.0.0 releases. The microsoft/agent-host-protocol repo is MIT-licensed. The docs site already lists 1.1.0 as a draft.
- **Third-party hosts:** the Agents window has the command 'Add Remote Agent Host...' (sessions.remoteAgentHost.add). It accepts a host, host:port or WebSocket URL, such as ws://127.0.0.1:8080?tkn=..., and stores entries in chat.remoteAgentHosts. The AHP site lists third-party hosts (pi-ahp, ahpd), and pi-ahp documents and tests interop with the VS Code Agents window.
- **Caveats:** Microsoft's VS Code docs only describe connecting to VS Code's own Agent Host: local, SSH, dev tunnel, WSL, GitHub Environments, or a standalone `code agent host`. They do not present third-party hosts as a supported scenario. Connecting without signing in to GitHub is experimental and desktop-only, and its setting (chat.agentHost.allowSignedOutWhenUsable) is off by default.
- **Transport:** remote connections use AHP JSON-RPC over WebSocket; local connections use a MessagePort.
**Evidence:** GitHub API repos/microsoft/agent-host-protocol/releases (spec/v1.0.0 2026-10-02T23:35:43Z ; license MIT) ; https://microsoft.github.io/agent-host-protocol/guide/implementations.html ; VS Code source src/vs/sessions/contrib/providers/remoteAgentHost/browser/remoteAgentHostActions.ts:59,74,90 (raw.githubusercontent.com/microsoft/vscode/main/...) ; VS Code commit 5c16c31 (#301224, 2026-03-13) 'Connect to remote agent hosts via setting chat.remoteAgentHosts' ; https://code.visualstudio.com/docs/agents/concepts/agent-host ; https://code.visualstudio.com/docs/agents/run/agents-window ; https://github.com/Qusic/pi-ahp (README, VS Code section)

## [CONFIRMED] AGENTS.md is stewarded by the Agentic AI Foundation; the Agent Skills (SKILL.md) format is adopted by several agents beyond Claude Code (name them).
**Corrected:** Correct.
- **AGENTS.md:** OpenAI released it in August 2025. It was one of three founding projects of the Linux Foundation's Agentic AI Foundation (AAIF), announced 2025-12-09; the other two were Anthropic's MCP and Block's goose. agents.md says it is 'now stewarded by the Agentic AI Foundation under the Linux Foundation'.
- **Agent Skills:** a skill is a folder with a SKILL.md file holding name/description frontmatter, plus optional scripts/, references/ and assets/. Anthropic originally developed the format and published it as an open standard at agentskills.io.
- **Adopters beyond Claude Code** listed on agentskills.io:
  - OpenAI Codex / ChatGPT
  - Google Gemini CLI
  - GitHub Copilot and VS Code
  - Cursor
  - OpenCode
  - Block Goose
  - Amp
  - Factory
  - OpenHands
  - Roo Code
  - JetBrains Junie
  - Kiro
  - TRAE
  - Mistral Vibe
  - Letta
  - pi
  - Mux (Coder)
  - Firebender
  - Tabnine
  - Spring AI
  - Snowflake Cortex Code
  - Databricks Genie Code
  - Hermes Agent (Nous)
  - OpenClaw
  - Google AI Edge Gallery
  - and others
**Evidence:** https://agents.md/ (About section) ; https://www.linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation (2025-12-09) ; https://agentskills.io/ (client showcase and 'Open development' section)

## [CONFIRMED] Codex app-server protocol is JSON-RPC Thread/Turn/Item over stdio JSONL; Codex requires the Responses API (no Chat Completions) since Feb 2026; Codex crates (apply-patch, execpolicy, linux-sandbox) are Apache-2.0.
**Corrected:** Correct.
- **Protocol:** app-server uses bidirectional JSON-RPC 2.0, but leaves out the "jsonrpc":"2.0" field on the wire, so it is not strictly spec-compliant. The core primitives are Thread, Turn and Item. The default transport is stdio (--listen stdio://) with newline-delimited JSON (JSONL). Other options are WebSocket (--listen ws://, marked experimental and unsupported), WebSocket over a Unix socket (--listen unix://), and off. A connection starts with initialize, then the initialized notification. TS/JSON schemas come from `codex app-server generate-ts|generate-json-schema`.
- **Responses API:** the Chat Completions deprecation was announced 2025-12-09 (Discussion #7782). Support was removed by PR #10157 'nuke chat/completions API' and PR #10498 'drop wire_api from clients', both merged 2026-02-03, and shipped in rust-v0.95.0 on 2026-02-04. `wire_api = "chat"` is now a hard config-load error, and the WireApi enum only has Responses.
- **Licenses:** the codex-rs workspace license is Apache-2.0. codex-apply-patch, codex-execpolicy and codex-linux-sandbox inherit it through license.workspace.
- **Not on crates.io:** the workspace version is 0.0.0, so reusing these crates means a git dependency or vendoring.
- **Sandbox licensing:** codex-linux-sandbox runs bubblewrap plus seccomp. It uses the system bwrap or a bundled bwrap binary built from the vendored codex-rs/vendor/bubblewrap, which is LGPL-2.0-or-later. Today it is shipped as a separate binary; current build.rs no longer compiles it in.
**Evidence:** https://developers.openai.com/codex/app-server (Protocol / Core primitives sections) ; https://github.com/openai/codex/discussions/7782 ; GitHub API repos/openai/codex/pulls/10157 and /pulls/10498 (merged 2026-02-03) ; releases/tags/rust-v0.95.0 (published 2026-02-04T04:54Z, lists #10157 and #10498) ; raw openai/codex main: codex-rs/model-provider-info/src/lib.rs:108-118 ; codex-rs/Cargo.toml:171-178 ; codex-rs/{apply-patch,execpolicy,linux-sandbox}/Cargo.toml:5 ; codex-rs/vendor/bubblewrap/bubblewrap.c:3 (SPDX LGPL-2.0-or-later) ; crates.io API (all three crates return 'does not exist')

## Other corrections
- rmcp's default ServiceExt::serve() still runs the legacy initialize handshake. A client that should talk to both 2026-07-28 and older servers must explicitly call serve_with_lifecycle(ClientLifecycleMode::Auto{..}). Auto can add up to 10 s of startup latency on legacy servers that silently ignore server/discover. rmcp also cannot reach servers that only speak the deprecated HTTP+SSE transport, and it has no WebSocket transport (rmcp-3.8.0 src/transport.rs:111; src/service/client.rs:719,757-783).
- MCP 2026-07-28 also deprecates OAuth Dynamic Client Registration in favour of Client ID Metadata Documents (PR #2858). It removes ping, resources/subscribe and SSE resumability, and moves tasks out of the core into an extension. An MCP client designed today should implement server/discover, per-request _meta, MRTR (InputRequiredResult) and subscriptions/listen. Roots and Sampling should not be relied on: they become eligible for removal in the first revision released on or after 2027-07-28.
- ACP v2 drops the Client file system, terminal and session-mode APIs. An agent exposed over ACP v2 must therefore own its file system access and terminal execution, which fits a design where the terminal lives agent-side. v2 is still a draft, so the migration guide says to support v1 and v2 side by side. The Rust SDK exposes v2 only behind the unstable_protocol_v2 feature, and the SDK's 3.x version is separate from the protocol version.
- The AHP docs site already shows 1.1.0 as a draft, one month after 1.0.0, so an AHP implementation should pin the spec version. VS Code's local agent host also exposes a discoverable AHP WebSocket endpoint on a Unix domain socket for external local clients (src/vs/platform/agentHost/LOCAL_ENDPOINT.md in microsoft/vscode).
- The Codex Rust crates are not on crates.io, so reuse means a git dependency on openai/codex or vendoring. codex-linux-sandbox also depends at runtime on a bubblewrap binary, either the system bwrap or one built from vendored LGPL-2.0-or-later source. That is fine for an MIT/Apache project as long as bwrap stays a separate executable.
- Cross-check with the llama.cpp cluster: the local llama.cpp at 5e03a1ff4 (2026-10-16) serves the OpenAI Responses API at /v1/responses and /responses (tools/server/server.cpp:276-277). Codex-style Responses-only clients can therefore talk to llama-server directly; whether it fully matches the Responses API was not checked here.