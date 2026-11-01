# ognivo

A coding agent for **local models only**, served by llama.cpp. ognivo owns each model's
native format: prompt rendering, tokenization, tool-call parsing, reasoning and sampling
are tuned per model family instead of being squeezed through a generic chat API.

Reference models: gpt-oss-20b (Harmony), Gemma 4 26B A4B (native tool-call DSL),
Qwen3.8-27B (XML tool calls).

**Status:** M1 in progress. `ognivo run` (one task, no user) works on the Generic path
(`/v1/chat/completions`), with the special-token guard, the repetition guard, a detector of
repeated calls and an optional `--time-limit`; the line-mode `chat` comes next.

## Layout

| Path | What |
|---|---|
| `crates/ognivo-cli` | the `ognivo` binary (`doctor`, `run`) |
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
echo "Which file implements the edit tool?" | cargo run -p ognivo-cli -- run --url http://127.0.0.1:8090
```

`doctor` connects to a running llama-server (or ik_llama.cpp), detects the model family,
classifies the server and checks the properties the agent depends on. `run` takes the
prompt on stdin and works in the current directory; actions that would need approval are
refused and explained to the model.

## License

MIT
