# ognivo

A coding agent for **local models only**, served by llama.cpp. ognivo owns each model's
native format: prompt rendering, tokenization, tool-call parsing, reasoning and sampling
are tuned per model family instead of being squeezed through a generic chat API.

Reference models: gpt-oss-20b (Harmony), Gemma 4 26B A4B (native tool-call DSL),
Qwen3.8-27B (XML tool calls).

**Status:** M0 (measurements and skeleton). Nothing here is usable yet.

## Layout

| Path | What |
|---|---|
| `crates/ognivo-cli` | the `ognivo` binary (`doctor` works today) |
| `crates/ognivo-models` | model families, profiles, server capabilities, llama-server client |
| `crates/ognivo-core` | agent loop, sessions, approvals |
| `crates/ognivo-tools` | tool identities and effects |
| `crates/ognivo-protocol` | core ⇄ client event types |
| `crates/ognivo-store` | event log |
| `crates/ognivo-evals` | evaluation harness |
| `profiles/` | per-model profiles (TOML) |
| `scripts/` | pinned llama.cpp build, benchmarks, server probes |
| `docs/` | architecture (Russian for now), M0 measurements, research |

## Try it

```sh
cargo run -p ognivo-cli -- doctor --url http://127.0.0.1:8080 --probe
```

`doctor` connects to a running llama-server (or ik_llama.cpp), detects the model family,
classifies the server and checks the properties the agent depends on.

## License

MIT
