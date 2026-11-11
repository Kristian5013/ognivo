# ognivo

A coding agent for **local models only**, served by llama.cpp. ognivo owns each model's
native format: prompt rendering, tokenization, tool-call parsing, reasoning and sampling
are tuned per model family instead of being squeezed through a generic chat API.

It works in your project from the terminal (`ognivo chat`, or `ognivo run` with no user
at all) or in its own window, on a llama-server that you run: ognivo never starts one.

**Status:** pre-release, Linux only. On the way to v1: Gemma 4 on the Native path,
terminal sessions in the window, packages (.deb, AppImage) and documentation in English
(the architecture, [docs/architecture.md](docs/architecture.md), is in Russian for now).

## Models

| Profile | Model | Tool calls | Native path |
|---|---|---|---|
| `gpt-oss-20b` | gpt-oss-20b | Harmony | yes |
| `gemma-4-26b-a4b` | Gemma 4 26B A4B | Gemma 4's call syntax | not yet |
| `gemma-4-31b` | Gemma 4 31B | Gemma 4's call syntax | not yet |
| `qwen3.8-27b` | Qwen3.8-27B | XML | yes |
| `qwen3.6-35b-a3b` | Qwen3.6-35B-A3B | XML | yes |

ognivo finds the model on the server (`/props`, `/tokenize`), or `--profile` names it. A
profile ([profiles/](profiles/), built into the binary) holds the model's sampling,
reasoning levels, context budget, the shapes of its tools and the chat templates it was
checked against.

- **Generic path** (the default): `/v1/chat/completions`. The server renders the prompt
  and parses the tool calls; every model works this way, on ik_llama.cpp too.
- **Native path** (`--native`): `/completion`. ognivo renders the model's format itself,
  tokenizes content so that it cannot pass for the model's special tokens, and
  constrains tool calls with its own grammars; the server only tokenizes and generates. With `--priors`, gpt-oss gets the
  tools of its training (`container.exec`, `repo_browser` with `apply_patch`) instead of
  ognivo's own.

## What it does

- **Tools:** read, grep, glob, bash, edit, write and apply_patch, the project's skills,
  and the tools of its MCP servers.
- **Approvals.** Reads and searches inside the project run on their own. The agent's edits
  apply at once and go into the edit journal; edits outside the project or to files that
  make something run (`.git`, `.claude`, `.github/workflows`, `.mcp.json`, Makefiles, shell
  and tool configs) ask. Commands that run the project's code (`cargo test`, `npm test`)
  run on their own only while nothing the agent changed waits for your review; network
  access and everything else asks. An answer is `y`, `n` (with what to do instead) or `a`
  (that exact action again, for the rest of the session).
- **Modes**, switched at any time, a running turn too (Shift+Tab in the window, `/mode` in
  `chat`): *Manual* asks before every edit and every command that does more than read;
  *Edit automatically*, the default, is the above; *Plan* reads and plans, refuses every
  change without asking, and offers to carry the plan out; *Auto* asks for nothing but
  edits of protected files or outside the project, and what your rules ask for. Rules hold
  in every mode.
- **Rules** in Claude Code's syntax, deny > ask > allow: `~/.ognivo/config.toml`
  (`[permissions]`) and `~/.claude/settings.json`, the project's `.ognivo/config.toml`
  and `.claude/settings*.json`. A project's allow rules count once you trust them: a
  cloned repository must not let itself run commands.
- **The edit journal:** every change is kept with what it replaced. `/changes`, `/accept`
  and `/revert` in `chat`, the Changes panel in the window.
- **Context.** A session works with the server's window (llama-server's `-c`), up to what
  its model works with: 256K for Gemma 4, Qwen3.6 and Qwen3.8, 128K for gpt-oss. Where a
  model writes nothing at all in a long context, as llama.cpp is reported to make Qwen3.8
  do past about 130K ([#27756](https://github.com/ggml-org/llama.cpp/issues/27756)), the
  conversation is compacted under that point from then on, and the step runs again.
  `--context 64k`, `OGNIVO_CONTEXT` or the window's Context field set another.
  At the profile's share of it, 65–90%, the conversation is compacted in the middle of a
  turn: the model's summary, ognivo's facts and the last steps replace the rest.
- **Images** for a model whose llama-server runs its projector (`--mmproj`): Gemma 4 and
  Qwen3.6/3.8 see them, gpt-oss does not. Paste, drop or add them with a message in the
  window, `/image FILE` in `chat`, `--image FILE` in `run`; a read of an image file gives
  the model the image. PNG, JPEG, GIF and BMP, up to 10 MB each and 8 or 32 MB a message;
  the window turns other formats into PNG. A request carries up to 48 MB of the newest
  images (llama-server takes 100 MB), and the model reads what the older ones were. On the Native path a step with images goes to the server
  as text with its media marker, content guarded as on the Generic path.
- **Guards** against special tokens in content, repeated output, repeated calls and calls
  cut off by the token limit.
- **Subagents** (`--subagents`): the agent hands tasks to subagents with `delegate`. They
  work at the same time on the server's slots, each in a context of its own, and report
  back. A read task changes no file, a write task only the files it lists; templates in
  `.claude/agents` give them roles. They pay off for independent parts that each take
  long, such as slow test suites, not for quick lookups.
- **Sessions** are kept in SQLite (`~/.ognivo/data`) as they go. `chat --resume ID` or
  `--continue` picks one up with its conversation and edit journal; `ognivo sessions`
  lists them or finds them by words; `--atif` exports one as an ATIF trajectory (the
  `trajectory.json` Harbor reads).
- **Servers:** upstream llama-server, ik_llama.cpp (Generic path), servers on your LAN,
  and llama-server in router mode (`--model`; one that is not loaded the router is asked
  to load). When the server's slots are all busy, the next step goes by priority, the
  user's agent first, across ognivo sessions too.

### Projects set up for Claude Code work as they are

- `AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md` and `~/.claude/CLAUDE.md`, with `@path`
  imports: the user's, those of every directory down to the project, and a
  subdirectory's once the agent works there;
- the permissions and hooks of `.claude/settings.json`;
- skills (`.claude/skills`), commands (`.claude/commands`) and subagents
  (`.claude/agents`), the project's and the user's;
- the MCP servers of `.mcp.json` and `~/.claude.json`.

### MCP

Servers come from the project's `.mcp.json`, `~/.ognivo/mcp.json` and Claude Code's
`~/.claude.json`, over stdio or Streamable HTTP, with OAuth sign-in for remote ones
(`ognivo mcp login NAME`). Their tools are `mcp__<server>__<tool>` to the model, as in
Claude Code, and each call asks unless a rule such as `mcp__github` allows it. With many
tools, `--mcp-tools search` gives the model `tool_search` in their place (`auto`, the
default, declares them while they are few and short). Their prompts are commands,
`/mcp__<server>__<prompt>`, and the model knows of them too. A project's servers start
once you trust exactly their commands. `ognivo mcp` shows what a session here gets, and
`ognivo mcp call` calls one tool, as a session would.

## Requirements

- Linux (x86_64; tested on Ubuntu).
- A llama-server that runs the model: upstream llama.cpp from b6096 for gpt-oss, b7973
  for Qwen3.5 and 3.8, b8637 for Gemma 4, and a recent build is better; ik_llama.cpp on
  the Generic path.
- To build: Rust 1.99 or newer and a C compiler; Node.js 24 or newer for the window.

## Build and try

```sh
git clone https://github.com/Kristian5013/ognivo.git
cd ognivo
cargo install --locked --path crates/ognivo-cli      # ognivo into ~/.cargo/bin
```

Start llama-server with your model. `ognivo doctor` prints the flags its profile
recommends; for gpt-oss-20b, from the repository's root:

```sh
llama-server -m gpt-oss-20b-mxfp4.gguf --port 8080 \
  -c 131072 -np 2 --kv-unified --swa-full -fa on -ngl 99 -b 2048 -ub 2048 \
  --chat-template-file profiles/templates/gpt-oss-20b/canonical.jinja
```

Then, in your project:

```sh
ognivo doctor --url http://127.0.0.1:8080     # the model, the server, what to fix
ognivo chat --url http://127.0.0.1:8080       # /help lists the commands
echo "Which file implements the edit tool?" | ognivo run --url http://127.0.0.1:8080
```

`doctor` detects the model family, classifies the server and checks the properties the
agent depends on (`--probe` also runs a short generation). `chat` streams the answer;
each tool call shows as one line and each change as a short diff, and ^C stops a turn.
On the Native path, ^\ ends the model's reasoning where it stands and asks for the
answer. `run` takes the prompt on stdin, refuses what would need approval and tells the
model why; `--events` writes the session in the clients' event model (`ognivo-protocol`,
version 1), one JSON event a line. For a server started with `--api-key`, pass
`--api-key` or set `OGNIVO_API_KEY`.

## The window

The GUI works on the core: `ognivo core` runs the sessions, one core per user, on a Unix
socket only that user can open (`$XDG_RUNTIME_DIR/ognivo/core.sock`), and the window
starts it when none runs. The window lists the sessions of every project and starts new
ones (server, model, reasoning level, context, Native path), streams turns, asks in the
thread what needs approval, shows subagents as lanes, reviews and reverts what changed,
shows the model's reasoning and each call's code in a window as the model writes them,
switches a thread's mode,
and shows a thread's MCP servers with their tools and prompts, signs in to remote ones
and edits the files that configure them. A turn goes on when the window closes; the core
quits a minute after its last client and turn.

```sh
cargo build --release -p ognivo-cli             # the window runs the newest of target/{release,debug}/ognivo, else PATH's
npm ci                                          # runs no install scripts (.npmrc)
npm run install-electron -w @ognivo/desktop     # so Electron's binary comes here
npm run build -w @ognivo/ui
npm start -w @ognivo/desktop
```

On Ubuntu 23.10 and newer, `kernel.apparmor_restrict_unprivileged_userns` keeps
Chromium's sandbox in Electron from user namespaces, and the window does not start
(`chrome-sandbox` is not owned by root). Give Electron a profile like the system's Chrome
one, from the repository's root:

```sh
sudo tee /etc/apparmor.d/ognivo-electron > /dev/null <<EOF
abi <abi/4.0>,
include <tunables/global>

profile ognivo-electron "$PWD/node_modules/electron/dist/electron" flags=(unconfined) {
  userns,
}
EOF
sudo apparmor_parser -r /etc/apparmor.d/ognivo-electron
```

The renderer's sandbox stays on; `--no-sandbox` would take it off.

## Layout

| Path | What |
|---|---|
| `crates/ognivo-cli` | the `ognivo` binary: `doctor`, `chat`, `run`, `sessions`, `mcp`, and `core`, which the GUI talks to |
| `crates/ognivo-core` | the agent loop, approvals and rules, instructions, skills, hooks, subagents, compaction |
| `crates/ognivo-models` | model families and profiles, server capabilities, the llama-server client, the router, the model driver and its guards, the Native path's renderers and readers |
| `crates/ognivo-tools` | the tools and their per-model shapes |
| `crates/ognivo-patch` | the apply_patch (V4A) format, after OpenAI Codex (Apache-2.0) |
| `crates/ognivo-mcp` | the MCP client and gateway: stdio, Streamable HTTP, OAuth |
| `crates/ognivo-protocol` | what the core and its clients say: the event model and the core's JSON-RPC methods, with their TypeScript |
| `crates/ognivo-store` | sessions, the event log, the edit journal's contents |
| `crates/ognivo-evals` | evaluation: T0 (recorded completions replayed, failure and injection corpora, the prompt-cache check) and T2-mini (scenarios on fixture projects against a live server) |
| `apps/ui` | the React UI of the GUI (React 19, Vite, Tailwind) |
| `apps/desktop` | the Electron main process around it, which joins the page to the core |
| `profiles/` | per-model profiles (TOML), checked templates and their goldens |
| `scripts/` | `check.sh`, `night.sh`, the pinned llama.cpp build, benchmarks, server probes |
| `docs/` | the architecture (Russian for now), M0 measurements, research |

## Development

`scripts/check.sh` runs before each commit: formatting, clippy, the workspace's tests,
the UI's types and tests, and T0's replay of recorded completions where the recordings
are (they stay on the machine that made them). CI runs it on every push, and the window
end to end.

`npm run dev -w @ognivo/ui` serves the UI with hot reload at http://127.0.0.1:5180; start
the window with `OGNIVO_UI_DEV=http://127.0.0.1:5180` to use it. In a browser, or with
`OGNIVO_UI_QUERY=recorded`, the UI shows recorded sessions instead. `npm run e2e -w
@ognivo/desktop` runs a session on a real core against a fake llama-server in a real
window, and `npm run perf -w @ognivo/desktop` measures the GUI's perf budgets there for
about ten seconds; the screen must be on for both. `scripts/night.sh` runs T2-mini for
each model against a live server.

## License

MIT
