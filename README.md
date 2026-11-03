# ognivo

A coding agent for **local models only**, served by llama.cpp. ognivo owns each model's
native format: prompt rendering, tokenization, tool-call parsing, reasoning and sampling
are tuned per model family instead of being squeezed through a generic chat API.

Reference models: gpt-oss-20b (Harmony), Gemma 4 26B A4B (native tool-call DSL),
Qwen3.8-27B (XML tool calls).

**Status:** M1 in progress. The line-mode `ognivo chat` and `ognivo run` (one task, no
user) work on the Generic path (`/v1/chat/completions`), with the special-token guard, the
repetition guard and a detector of repeated calls; `run` has an optional `--time-limit`.
The edit journal lets `chat` show, accept and revert what the agent changed: `/changes`,
`/accept`, `/revert`. AGENTS.md and CLAUDE.md files go to the model as Codex and Claude
Code give them: from the user, from every directory down to the project, and from a
subdirectory once the agent works there, with `@path` imports. Sessions are kept in SQLite
(`~/.ognivo/data/ognivo.db`) as they go: `chat --resume ID` or `--continue` picks one up
with its conversation and edit journal, and `ognivo sessions` lists them or finds them by
words. When the context fills up, the history is compacted in the middle of a turn: the
model's summary, ognivo's facts and the last steps replace the rest. Permission rules come
from `~/.ognivo/config.toml` and the project, in Claude Code's syntax and from its
`settings.json` files too (deny > ask > allow); a project's allow rules count once you
trust them. Shell commands are parsed with tree-sitter-bash. Every session can be
exported as an ATIF trajectory (`ognivo sessions --atif ID`, `ognivo run --atif FILE`),
the `trajectory.json` Harbor reads.

## Layout

| Path | What |
|---|---|
| `crates/ognivo-cli` | the `ognivo` binary (`doctor`, `chat`, `run`, `sessions`, and `core`, which the GUI talks to) |
| `crates/ognivo-models` | model families, profiles, server capabilities, llama-server client, the router (whose step goes next at a server's slots), the model driver and its guards, Harmony rendering and reading for the Native path |
| `crates/ognivo-core` | agent loop and approvals |
| `crates/ognivo-tools` | the tools (read, edit, write, grep, glob, shell, apply_patch) and their per-model shapes |
| `crates/ognivo-patch` | the apply_patch (V4A) format, after OpenAI Codex (Apache-2.0) |
| `crates/ognivo-protocol` | what the core and its clients say: the event model and the core's JSON-RPC methods, with their TypeScript |
| `crates/ognivo-store` | sessions, the event log, the edit journal's contents |
| `crates/ognivo-evals` | evaluation harness: T0 (the recorded completions replayed, failure and injection corpora, the prompt-cache check) and T2-mini (`t2`: scenarios on fixture projects against a live server) |
| `apps/ui` | the React UI of the GUI (React 19, Vite, Tailwind): sessions on the core, and recorded ones (`fixtures/`) |
| `apps/desktop` | the Electron main process around it, which joins the page to the core |
| `profiles/` | per-model profiles (TOML) |
| `scripts/` | `check.sh` (lints, tests and T0 before each commit), `night.sh` (T2-mini for each model in the night GPU window), pinned llama.cpp build, benchmarks, server probes |
| `docs/` | architecture (Russian for now), M0 measurements, research |

## Try it

```sh
cargo run -p ognivo-cli -- doctor --url http://127.0.0.1:8080 --probe
cargo run -p ognivo-cli -- chat --url http://127.0.0.1:8080
echo "Which file implements the edit tool?" | cargo run -p ognivo-cli -- run --url http://127.0.0.1:8080
```

`doctor` connects to a running llama-server (or ik_llama.cpp), detects the model family,
classifies the server and checks the properties the agent depends on. `chat` works in the
current directory: the answer streams, each tool call shows as one line and each change as a
short diff, an action that needs approval asks `y`, `n` (or `n` and what to do instead) or
`a` (that exact action again without asking, for the rest of the session), and ^C stops a
turn; `/help` lists the rest. `run` takes the prompt on stdin;
actions that would need approval are refused and explained to the model. With `--native`
(gpt-oss so far) ognivo renders the model's format itself and the server only tokenizes and
generates: the Native path of M2, in progress. In `chat` on that path, ^\ ends the model's
reasoning where it stands and asks for the answer, and `--priors` gives gpt-oss the tools of
its training (container.exec, repo_browser with apply_patch) instead of ognivo's own.

llama-server in router mode works as well: every request names the model, which is
`--model` (for `doctor` too), else the one model of `--profile`, else the one loaded; one
that is not loaded the router is asked to load. Requests are not pinned to a slot unless
`--slot` says so: llama-server's prompt cache keeps each conversation's KV and puts it back
into whichever slot the conversation comes to. When the server's slots are all busy, the
next step goes by priority, the user's agent first, across ognivo sessions too.

`run --events` writes the session in the clients' event model (`ognivo-protocol`, frozen as
version 1): what the GUI gets of a session, one JSON event a line.

With `--subagents` the agent may hand tasks to subagents (`delegate`): each works on its
task in a context of its own, at the same time as the others, at medium reasoning, and
reports back. A read task changes no file; a write task only the files it lists. What
they change is the session's to review, and each one's conversation is kept as a session
of its own. gpt-oss-20b does not delegate on its own: ask for subagents in the task. They pay
off for independent parts that each take long (slow test suites), not for quick lookups.

The GUI works on the core: `ognivo core` runs the sessions, one core per user, on a Unix
socket only that user can open (`$XDG_RUNTIME_DIR/ognivo/core.sock`), and the window starts
it when none runs. The window lists the sessions of every project, starts new ones, streams
turns, asks what needs approval in the thread, and reviews and reverts what changed. A turn
goes on when the window closes; the core quits a minute after its last client and turn.

```sh
cargo build --release -p ognivo-cli             # the window runs the newest of target/{release,debug}/ognivo
npm ci                                          # runs no install scripts (.npmrc)
npm run install-electron -w @ognivo/desktop     # so Electron's binary comes here
npm run build -w @ognivo/ui
npm start -w @ognivo/desktop
```

`npm run dev -w @ognivo/ui` serves the UI with hot reload at http://127.0.0.1:5180; start the
window with `OGNIVO_UI_DEV=http://127.0.0.1:5180` to use it. In a browser, or with
`OGNIVO_UI_QUERY=recorded`, the UI shows the recorded sessions instead. `npm run e2e -w
@ognivo/desktop` runs a session on a real core against a fake llama-server in a real window,
and `npm run perf -w @ognivo/desktop` measures the GUI's perf budgets there for about ten
seconds; the screen must be on for both.

## License

MIT
