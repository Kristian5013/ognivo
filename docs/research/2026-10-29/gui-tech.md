# gui-tech: GUI technology for a Cursor-level, Linux-first (Ubuntu/Wayland) desktop coding-agent app with a Rust agent core, solo developer - state as of 2026-10-29

## Executive summary
As of 2026-10-29 the leading agent GUIs fall into two camps: VS Code forks (Cursor, Devin Desktop/ex-Windsurf, Kiro, Antigravity, Trae) and web UIs in Electron (Codex, now the ChatGPT desktop app, Claude Desktop, OpenCode, T3 Code, LM Studio). The main native exceptions are Zed (GPUI) and Warp (warpui), and large teams built both over several years. The fact that decides the Linux question is WebKitGTK. Tauri v2 renders through it, and its documented problems hit exactly what an agent IDE is made of: scroll and repaint lag, DMABUF/NVIDIA crashes, WebGL that silently runs on a slow path while reporting a masked renderer string, and an IME popup that lands in the wrong place (still open on Ubuntu 26.04/GNOME 50). OpenCode moved its desktop app from Tauri to Electron in April-May 2026 for these reasons. Tauri's fix is a Chromium/CEF runtime, but it exists only in Tauri 3.0 alpha (tauri-runtime-cef 3.0.0-alpha.6, 2026-10-14). It needs GTK4 and a ~1 GB CEF download per build cache, has open Linux bugs (100% idle CPU, #16189), and its maintainers say v3 stable is 'a long time off'. Readest shows it can ship on Linux, but a solo developer should not build on it today. Electron 45.1 (Chromium 156, released 2026-10-28, Wayland-native since 38.2) is the lowest-risk way to get a current Chromium on Linux, and Cursor itself is Electron, so Cursor-level polish has already been done on that stack. Forking VS Code is a trap for a solo dev in 2026: upstream went to weekly stable releases on 2026-03-09 (about 1.142 by October), Cursor was still on a 1.105 base in June 2026, and Void was archived on 2026-06-02. On top of that, the build pipeline is private and Microsoft's Marketplace terms lock forks out of its own extensions. A VS Code extension cannot reach Cursor-level UX for several reasons: webviews are sandboxed, proposed APIs (chat sessions, agent hosts) cannot be published, the native Agents window is closed to third-party harnesses, BYOK models are tied to Copilot plans, and terminal capture through shell integration has a long failure history (Cline). Cursor itself says it forked VS Code 'to shape our own surface', and for Cursor 3 (2026-04-02) it rebuilt an agent-first Agents Window from scratch: the target UX is now an agent workspace with sessions, diff review, terminal and worktrees, not a classic editor. Zed 1.0 (2026-04-29) is the best native reference, but forking it makes the whole app GPL-3.0, its extensions cannot add UI, and the official GPUI crate has had no release since 0.2.2 (2025-10-22), so the ecosystem runs on forks and mirrors. The Agent Client Protocol (ACP) has become the 'LSP for agents'. Zed and JetBrains develop it together, a shared registry has been live since 2026-01-28, Neovim, Emacs, Devin Desktop and others are clients, and the Rust SDK (agent-client-protocol 3.6.0, stable wire protocol v1) covers streamed thoughts, tool calls with diffs and terminals, permissions, plans, usage, and model/reasoning config options. The other Rust toolkits (egui 0.34, iced 0.14, Slint 1.15, Dioxus 0.7/Blitz 0.3 beta, Floem, Makepad, Xilem) lack production-grade code editor, diff, terminal and rich-text building blocks. Shipping agent apps have converged on a component set: CodeMirror 6 (official merge view and LSP client), Shiki, @pierre/diffs, xterm.js 6 with WebGL (canvas renderer removed) or ghostty-web, Streamdown-style streaming markdown and react-virtuoso. libghostty is production-grade (JetBrains 2026.3 uses it) and has Rust bindings. MCP Apps (stable 2026-01-26) render as sandboxed HTML iframes, which favors an HTML-capable GUI. Recommendation: build a headless Rust core that speaks ACP v1 plus extensions, give it a web UI in React/TypeScript that does not depend on its shell, package that UI in Electron, and ship the core as an ACP agent for Zed and JetBrains from the first week. Keep Tauri 3 with CEF as a later shell swap, and choose GPUI only if a pure-Rust UI is non-negotiable. Rough GUI effort for a solo developer with AI agents: Electron/web MVP 8-12 weeks, polished v1 another 6-9 months; GPUI MVP 4-6 months, v1 12-18 months; ACP-only integration 1-3 weeks.

## Recommendation
Ranking for this owner: Rust core, Linux/Wayland first, solo developer working with AI agents about 70 h/week, Cursor-level polish.

Effort figures cover the GUI only, in calendar time. They assume the Rust core is built in parallel and already streams ACP-style events. They are judgment-based ranges.

1) Best option: a headless Rust core plus a web UI packaged in Electron 45.x.
- The UI is TypeScript with React 19 and React Compiler; SolidJS is acceptable. It must not depend on its shell.
- Why:
  - It is the only option that gives a guaranteed, current Chromium on Linux/Wayland today.
  - It is literally what Cursor/VS Code, Codex/ChatGPT desktop (Electron + React + bundled Rust app-server), Claude Desktop, OpenCode, T3 Code and LM Studio ship.
  - Every heavy component already exists.
  - MCP Apps (sandboxed HTML iframes) render natively.
  - Your coding agents can drive, inspect and screenshot the real app through Playwright or CDP.
- Stack spec:
  - Electron main process is a thin supervisor: it spawns and monitors the core, passes a socket path and token, and handles the updater, menus and xdg-portal dialogs.
  - The renderer runs sandboxed with contextIsolation and a strict CSP. It talks to the core directly over WebSocket or a Unix socket (the T3 Code pattern). That way the same UI also runs in a browser tab for fast development and remote use.
  - CodeMirror 6 with @codemirror/merge and @codemirror/lsp-client; the core spawns the LSP servers and proxies them.
  - @pierre/diffs for review.
  - xterm.js 6 with the WebGL addon (DOM fallback), or ghostty-web, to display PTYs that the core owns.
  - Streamdown-style streaming markdown, with Shiki running in workers.
  - react-virtuoso or TanStack Virtual for threads and lists.
  - Radix/shadcn, cmdk and Tailwind for UI primitives.
- MVP, about 8-12 weeks:
  - session sidebar
  - streamed chat with collapsible reasoning
  - tool-call cards with live terminal output
  - permission prompts (once / always / deny)
  - plan and todo view
  - context and usage meter
  - per-file diff review with hunk accept/reject
  - file view and edit
  - terminal panel
  - llama-server and model manager with per-family profiles (gpt-oss reasoning_effort, Gemma 4 enable_thinking, sampling presets)
  - MCP server manager
  - settings
  - .deb/.rpm packaging
- Polished v1, a further 6-9 months:
  - streaming that holds 60 fps on threads with 10k messages
  - inline edit and ghost-text completion in the editor (llama.cpp FIM)
  - checkpoint timeline and undo
  - worktree and multi-agent dashboard
  - @-mention context picker
  - git panel
  - diff line comments fed back as next-turn context
  - command palette and a full keymap
  - themes, ru/en i18n, a11y, onboarding
  - crash reporting and auto-update
  - a Playwright end-to-end suite
  - AppImage and Flatpak
- Built from scratch (any web option): all of the agent-specific UX listed above.
- Reused: editor, diff renderer, terminal emulator, markdown, virtualization and UI primitives.

2) Alternative shell for the same UI: Tauri.
- Do not use Tauri v2 on Linux. WebKitGTK has documented scroll and repaint lag, DMABUF/NVIDIA crashes, and WebGL that silently runs slow. Its IME popup lands in the wrong place on Ubuntu 26.04, and ibus composition bugs corrupt input in xterm.js. OpenCode left Tauri for these reasons.
- Tauri 3 with tauri-runtime-cef fixes rendering; Readest ships every Linux build on it. The catches:
  - it is alpha (3.0.0-alpha.6, 2026-10-14)
  - it needs GTK4 and MSRV 1.95
  - each build cache downloads about 1 GB of CEF
  - bundles grow by hundreds of MB, so there is no size win over Electron
  - Linux bugs are open: idle CPU at 100% (#16189) and X11 focus/drag
  - the maintainers say v3 stable is 'a long time off'
- Plan: put the UI behind a Platform interface (as OpenCode did) and re-evaluate when Tauri 3 reaches beta. Swapping the shell then costs about 2-4 weeks.
- Starting on the Tauri 3 alpha today adds about 3-6 weeks of Linux/CEF plumbing, plus ongoing alpha churn.

3) Pure Rust: GPUI with gpui-component, only if a TypeScript UI is unacceptable.
- Upside:
  - the highest ceiling: Zed-class latency, Wayland-native, wgpu on Linux
  - Apache-2.0 licensing and AccessKit
  - in 2026 many agent-orchestrator apps already run on it
- Downside:
  - There is no official GPUI on crates.io since 0.2.2 (2025-10-22). You must pin a mirror or fork (gpui-unofficial, Longbridge gpui-pre, or gpui-ce) and track its churn.
  - Zed's reusable pieces are GPL-3.0: the multibuffer diff, git_ui, terminal_view, agent_ui and markdown.
  - It cannot render HTML, so MCP Apps would need a separate webview.
  - Your AI agents have far fewer examples to learn from.
- Effort: MVP about 4-6 months; polished v1 about 12-18 months.
- Built from scratch:
  - streaming markdown with selection and copy across blocks
  - a virtualized thread with variable-height items
  - tool cards
  - the diff review UI (on top of gpui-component's editor)
  - a terminal element: libghostty-vt or alacritty_terminal, plus GPUI painting, selection, links and IME
  - file tree, LSP glue, portals and dialogs, packaging

4) Do this in parallel whatever you choose: expose the core as an ACP agent.
- Use the agent-client-protocol crate and wire protocol v1.
- About 1-3 weeks of work gives you Zed, JetBrains (2025.3.2+ registry), Neovim, Emacs and Devin Desktop as free clients.
- Use them to dogfood gpt-oss-20b and Gemma 4 tool calling before your own GUI exists. The UX there belongs to the host.

5) Fork Zed only if GPL-3.0 for the whole app is acceptable and you want an editor-first product.
- MVP 1-2 months (swap in your own core); v1 6-12 months.
- You pay a permanent weekly rebase onto a very large, fast-moving Rust monorepo.

6) Fork VS Code/Code-OSS: no, not for a solo developer in 2026.
- Upstream releases weekly since 2026-03-09, and Cursor was still on a 1.105 base in June 2026.
- Void is archived, the build pipeline is private, Microsoft's extensions are locked out, and Open VSX is rate-limited.
- MVP 2-3 months, v1 9-12+ months, plus about one rebase every week forever.

7) VS Code extension: only as a secondary channel, a thin client of the same core.
- It cannot reach Cursor-level UX:
  - webviews are sandboxed
  - third parties get no inline widgets beyond decorations
  - proposed APIs cannot be published
  - the native Agents window is closed to third-party harnesses
  - BYOK models are tied to Copilot plans
  - terminal capture is fragile

Not recommended for this app in 2026: egui, iced, Slint, Dioxus (its desktop target is WebKitGTK and Native/Blitz is still beta), Floem, Makepad, Xilem, and warpui (unpublished, git-pin only).

Decision rule: choose (1) unless a pure-Rust UI is a hard requirement, in which case choose (3). Do (4) in every case.

## Architecture implications
- Split the product into a headless Rust core daemon (sessions, llama-server lifecycle, model-family adapters, tools, MCP client, PTYs, git/worktrees/checkpoints, SQLite) and thin clients. With that split the GUI choice is reversible: OpenCode swapped Tauri for Electron in weeks, and the Codex app-server, Cline Core and Kilo-on-OpenCode all use this pattern.
- Protocol: use ACP v1 as the outer contract. Serve it over stdio for third-party editors and over the same JSON-RPC on WebSocket or a Unix socket, with token auth, for your own GUI. Add underscore-prefixed extension methods and _meta for what ACP lacks: llama-server control, model download and profiles, worktrees, checkpoint and restore, core-owned terminal streams, and a multi-session dashboard. Pin the Rust crate, which went from 1.0 to 3.6 in four months.
- The core should own the PTYs. ACP terminals are client-owned, and IDE terminal APIs are unreliable (VS Code shell integration, Cline's history). For a flawless terminal: use portable-pty; inject your own OSC 133/633-style markers with a nonce; run a headless VT (libghostty-vt or alacritty_terminal) so the model gets clean screen and scrollback text plus exit codes; stream raw bytes and snapshots to any UI for rendering and rehydration. Use ACP client terminals only when the agent is hosted in Zed or JetBrains.
- Model-family UI contract. Stream reasoning separately (ACP agent_thought_chunk; llama-server reasoning_content). Map per-family controls to ACP configOptions: thought_level for gpt-oss reasoning_effort and Gemma 4 enable_thinking, model_config for sampling and context size. Show usage_update as context fill, which matters with local ctx limits. Persist reasoning per message so the core can re-inject it where a template needs it (Gemma 4 re-renders thoughts only for tool-call turns).
- Streaming performance budget. Coalesce token deltas in the core (about 30-60 UI updates/s). Memoize markdown per block (the Streamdown/remend approach). Defer syntax highlighting to a worker pool (as Pierre does). Virtualize threads, diff lists and file trees. Long transcripts full of diffs are where Cursor 3.0 stuttered.
- MCP belongs in the core: stdio plus streamable HTTP, OAuth, per-tool permissions and elicitation. Do not route agent tools through llama-server's server-side, stdio-only MCP. The GUI needs MCP management screens, OAuth browser and deep-link flows, and MCP Apps rendering (sandboxed iframes with a sandbox proxy), which requires HTML capability.
- Keep the diff and checkpoint engine in the core (git worktrees or a shadow repo, per-hunk apply and revert); the GUI only renders and approves. Line comments on diffs should become next-turn context, as in the Codex and T3 Code UX.
- Build the UI as a web app behind a Platform interface (dialogs, notifications, updater, clipboard, deep links). It then runs in a browser tab during development and for remote use with the core on the GPU box, and inside Electron or later Tauri for distribution.
- Linux packaging is Wayland-first, since Ubuntu 26.04/GNOME 50 is Wayland-only. Ship .deb/.rpm first because they include the setuid chrome-sandbox, then AppImage with its known sandbox caveats on Ubuntu 23.10+, then Flatpak. Test rendering on both the Intel iGPU and NVIDIA dGPU paths of hybrid laptops.
- Pick a stack your coding agents can verify themselves: Playwright with _electron, CDP screenshots and DOM queries. Readest reports that CDP on CEF let its agents inspect, click and screenshot the real app, which WebKitGTK never allowed.

## Risks and gotchas
- WebKitGTK (Tauri v2, Dioxus desktop, wry):
- NVIDIA DMABUF crashes; the workarounds force slow CPU repaint (40-90 ms frames measured).
- WebGL silently runs on a slow path while reporting a masked renderer string, which hurts xterm.js WebGL.
- The IME candidate window is mispositioned on Ubuntu 26.04 Wayland.
- ibus emits orphan compositionend events, so xterm.js duplicates characters.
- No WebRTC.
- Tauri 3 with CEF:
- Alpha APIs that are still changing.
- GTK4 only; one process cannot load GTK3 wry and GTK4 CEF together.
- About 1 GB CEF download per build cache, and bundles hundreds of MB larger.
- Idle 100% CPU on Linux (#16189) and X11 focus/drag/dialog crashes.
- Windows is unsandboxed, and AppImages lose the sandbox on Ubuntu 23.10+.
- Development happens largely in-house at the vendor, and v3 stable is far off.
- Electron:
- Chromium plus Node footprint.
- A major every 8 weeks with only 3 majors supported, so upgrades are routine work.
- Needs security hardening (contextIsolation, sandbox, CSP, no remote content in privileged windows).
- Residual fcitx5 IME quirks on some Wayland compositors.
- AppImage sandbox limitations.
- On hybrid laptops, check that the GUI's GPU process does not take VRAM that llama.cpp needs on the 24 GB dGPU.
- GPUI:
- Distribution is fragmented: the official crate is frozen at 0.2.2 while forks and mirrors diverge, so you must pin and track churn.
- Zed's best reusable crates are GPL-3.0.
- X11/XIM IME bugs.
- No HTML for MCP Apps.
- AI agents generate GPUI code less reliably than React/TS (judgment).
- VS Code fork:
- Upstream rebases every week.
- Marketplace terms forbid Microsoft extensions (C/C++, Pylance, Remote) in forks.
- Open VSX is rate-limited, with a paid managed tier for heavy use.
- The build, sign and update pipeline is private and must be rebuilt (Void's experience).
- VS Code extension:
- Proposed-API features cannot be published.
- The native agent UI is closed to third-party harnesses.
- BYOK models are tied to Copilot plans and admins can disable them.
- Shell-integration output capture is flaky.
- ACP:
- The Rust crate has had several major versions in four months, and protocol v2 is in progress.
- Clients implement subsets, so negotiate capabilities.
- The client-owned fs/terminal semantics conflict with a core-owned PTY design unless you add extension methods.
- Terminal libraries:
- ghostty-web lacks some hooks and is about 5x the size of xterm.js.
- libghostty-vt is pre-1.0, needs the Zig toolchain, and its types are !Send/!Sync.
- xterm.js 6 removed the canvas fallback.
- Streaming markdown: naively re-parsing the whole message on every token is O(n^2), and Streamdown is React-only. Long agent transcripts with many diffs and code blocks are the hardest performance case.
- Monaco is heavy and hard to restyle. monaco-vscode-api tracks weekly VS Code, so its major version breaks often (v38 by Oct 2026).
- Licensing traps: a Zed fork is GPL-3.0; Warp is AGPL-3.0 except the unpublished MIT warpui; Slint is GPL-3.0, royalty-free or commercial. CM6, xterm.js, ghostty/libghostty-vt, @pierre/diffs and Electron are permissive.
- Vendors in this space disappear or rebrand quickly: Void archived, Roo Code shut down, PearAI reportedly defunct, Windsurf rebranded. Do not depend on any third-party agent UI surviving; ACP limits that lock-in.

## Questions for owner
- Is a TypeScript/React web UI acceptable, with all agent logic in Rust, or is a pure-Rust GUI a hard requirement? An Electron/web MVP takes about 2-3 months; a GPUI MVP about 4-6 months, and a GPUI v1 roughly twice as long.
- What should v1 be: an agent-first workspace (sessions, diff review, terminal and a light editor, like the Codex app, Cursor 3's Agents Window or Conductor), or an editor-first IDE replacement (Tab/next-edit completion, full LSP editing, extensions)? Editor-first pushes toward deeper CM6+LSP work, Monaco, or a Zed fork.
- What license will the app use: permissive (MIT/Apache, like cforge), or is GPL-3.0/AGPL acceptable? A Zed fork, or reusing Zed's editor, terminal or diff crates, forces GPL-3.0.
- Is v1 Linux-only, or do macOS and Windows need to follow within about 12 months? That changes the shell decision (Tauri's system webviews are fine on macOS and Windows) and adds signing, notarization and packaging work.
- Should Zed, JetBrains and Neovim (via ACP) be first-class clients from day one, which makes ACP the core's primary protocol? Or do you prefer a custom app-server protocol (Codex-style) with an ACP adapter added later?
- Can you accept a Chromium-class desktop app (hundreds of MB on disk, plus Chromium renderer and GPU processes in RAM) running next to llama.cpp on the same machine?
- Must the GUI work remotely, with the core on the GPU workstation and the UI on another machine or in a browser? If so, the WebSocket transport, auth and a browser build become MVP requirements.
- Do you need an embedded browser or live app preview (like Cursor's Design Mode) in v1? That needs Chromium webviews (Electron WebContentsView or CEF) and rules out WebKitGTK or GPUI for that feature.

## Unknowns
- Whether tauri-runtime-cef supports native Wayland on Linux (every reported pitfall was on X11), and when Tauri 3 will reach beta or stable.
- Whether Zed will resume official GPUI releases on crates.io, and which community fork or mirror will become canonical.
- Cursor's VS Code base in Oct 2026; the latest data point found is 1.105 in June 2026.
- Claude Desktop's exact stack; Electron is inferred from Linux repackaging of the Windows Electron build, not from a primary technical source.
- No rigorous benchmarks of WebKitGTK 2.52/2.54 vs Chromium 156 vs GPUI for chat, diff and terminal workloads were found; only anecdotal measurements (tauri-explorer, Readest, OpenCode).
- How production-ready ghostty-web is, and whether xterm.js will adopt libghostty (issue #5686 is open).
- Whether VS Code will open Agent Host provider APIs to third-party extensions (#325827).
- Whether the Language Model Chat Provider API still requires a Copilot plan in Oct 2026; the cited restriction is from Oct 2025.
- PearAI's status; there is no company statement.
- How Streamdown/remend performs on very long streams; no benchmarks found.
- All effort estimates are judgment-based. They assume a solo developer with AI agents at about 70 h/week and a Rust core built in parallel behind a stable event API.
