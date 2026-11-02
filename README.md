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
model's summary, ognivo's facts and the last steps replace the rest.

## Layout

| Path | What |
|---|---|
| `crates/ognivo-cli` | the `ognivo` binary (`doctor`, `chat`, `run`) |
| `crates/ognivo-models` | model families, profiles, server capabilities, llama-server client, the model driver and its guards |
| `crates/ognivo-core` | agent loop and approvals |
| `crates/ognivo-tools` | the tools (read, edit, write, grep, glob, shell) and their per-model shapes |
| `crates/ognivo-protocol` | core ⇄ client event types |
| `crates/ognivo-store` | event log |
| `crates/ognivo-evals` | evaluation harness |
| `profiles/` | per-model profiles (TOML) |
| `scripts/` | pinned llama.cpp build, benchmarks, server probes |
| `docs/` | architecture (Russian for now), M0 measurements, research |

## Try it

```sh
cargo run -p ognivo-cli -- doctor --url http://127.0.0.1:8090 --probe
cargo run -p ognivo-cli -- chat --url http://127.0.0.1:8090
echo "Which file implements the edit tool?" | cargo run -p ognivo-cli -- run --url http://127.0.0.1:8090
```

`doctor` connects to a running llama-server (or ik_llama.cpp), detects the model family,
classifies the server and checks the properties the agent depends on. `chat` works in the
current directory: the answer streams, each tool call shows as one line and each change as a
short diff, an action that needs approval asks `y`, `n` (or `n` and what to do instead) or
`a` (that exact action again without asking, for the rest of the session), and ^C stops a
turn; `/help` lists the rest. `run` takes the prompt on stdin;
actions that would need approval are refused and explained to the model.

## License

MIT
