# Review: architecture conformance and seams

The crate layout matches §12: core depends on models, protocol, store and tools. `cargo test` and `clippy --all-targets` are clean. Another agent changed profile.rs, doctor.rs and the profiles while I was reviewing (template pins and known-template verdicts); the findings below are against the current tree. The seams do not yet carry what §4–§5 and the M0 data require.

1. **Approval policy.** The approval policy has no path information. Commands with external paths are classed as project code, and `git status` stays Allow after the agent edits `.git/config`. Both bypass the owner's approval rules. I confirmed this by calling the real functions.
2. **TokenEngine is the right layer, but its contract is underspecified.**
   - Mid-stream server errors vanish without a trace.
   - On ik_llama.cpp every stop reason becomes "unknown".
   - `Done` drops `truncated` and the cache counters, so running out of slot context looks like an n_predict stop, and the §5.6 cache telemetry and the M2 cache-hit criterion cannot be computed.
   - The `Constraint` type cannot express Harmony's header triggers (llama.cpp itself uses 4 PATTERN triggers), `preserved_tokens`, or the M0 rule that a grammar must start with its trigger.
   - `slot` is a plain `Option<u32>` instead of a lease, there is no stop specification, and cancelling only means dropping the stream.
3. **EngineCaps and ServerClass are not expressive enough.**
   - A missing `build_info` is treated as proof of ik_llama.cpp. A server that needs an API key (or a vLLM server) is reported as "ik_llama.cpp class C", and doctor exits 0.
   - Router mode (every POST needs a `model` field) and `unix://` endpoints, both promised in §4, cannot work.
   - The §4 capability list (lazy grammar, trigger types, kv-unified, swa-full/checkpoints, rollback) has no fields, and an in-process engine has nothing true to report.
   - The committed-hash list for complete token-id streaming cannot cover upstream builds after the fix is merged.
   - The M0 matrix has no router column, and the old-build lazy-grammar result predates the probe fix.
4. **Profile schema.**
   - Reasoning is set in two places (`[reasoning]` and `[template].kwargs`) and nothing keeps them in sync; effort levels are not validated per model.
   - Per-phase sampling and penalties are impossible: the Qwen research's presence_penalty is rejected as an unknown field, and `completion_body` would force it to 0 anyway.
   - Provenance exists only as TOML comments, there is no minimum-build field, and `deny_unknown_fields` plus all-required fields rule out partial user overrides unless they are merged at the TOML-table level.
5. **Missing for M1.** M1 starts on the Generic path, but the ModelDriver seam with its history and event types does not exist yet. TokenEngine is not dyn-compatible, so ModelDriver has to be the dyn boundary.
6. **Store.** The store persists the client wire event: raw ids are stored as unsealed client deltas, there is nowhere to keep sealed ids or the codec version, and the seq numbering (per thread or global) contradicts itself.

## [major] Approval seam is path-blind: out-of-project code and git-config tricks bypass the owner's rules
`crates/ognivo-core/src/approval.rs:29`

**Problem.** `decide(Effect, TurnState)` never sees what a call touches. `Effect::EditsFiles` is always Allow, whether it writes inside the project, to ~/.bashrc or to .git/config. In `classify_command` (crates/ognivo-tools/src/lib.rs:77-117) the `leaves_project` check runs only for read-only commands; the ExecutesProjectCode arms (cargo/npm/pytest/make/just/go) accept any argument. `git status/diff/log/show` stay ReadOnly → Allow even after the agent changed files, and the test at approval.rs:48-51 enshrines that. `TurnState` is one per-turn bool, so it does not see edits made by parallel write-subagents (§7, decision 16).

**Failure scenario.** I called these exact functions from a scratch crate. In a clean turn, `pytest /tmp/x`, `make -f /tmp/evil.mk`, `make -C ../other`, `go run /tmp/x.go` and `cargo run --manifest-path /tmp/x/Cargo.toml` all return ExecutesProjectCode → Allow, so code outside the project runs with no prompt.

Second path: the agent's Edit/Write puts `[core] fsmonitor = ./x.sh` into .git/config (EditsFiles → Allow), then runs `git status` (ReadOnly → Allow even with agent_changed_code=true). git executes the hook: arbitrary code without any prompt. That defeats the owner's rule that project code asks after the agent's own edits (§8, decisions 7/12/13). A diff/textconv driver in .git/config does the same with `git diff/log/show`.

Third path: a write-subagent edits build.rs during the orchestrator's turn; the orchestrator's TurnState is still clean, so its `cargo test` auto-runs.

**Fix.** Give the policy the targets and the workspace root, e.g. `decide(&Action { effect, paths, argv }, &WorkspaceState)`.
- Ask for writes outside the root and for exec-relevant paths: .git/**, .gitattributes, .claude/**, .mcp.json, .envrc, .vscode/**.
- Apply `leaves_project` to every classify_command arm, including the project-code arms.
- Keep the dirty flag per workspace: set by any agent or subagent write, cleared only by user review.
- Run the agent's git with `-c core.fsmonitor=false --no-ext-diff --no-textconv`, or classify git as ExecutesProjectCode once git config or attributes changed.
- Add these cases to the owner_rules test.

## [major] RawEngine stream has no reliable terminal event: mid-stream server errors vanish, ik stop reasons become "unknown"
`crates/ognivo-models/src/engine.rs:194`

**Problem.** `token_events` reads only tokens/content/stop/stop_type. After the HTTP 200 header has gone out, llama-server reports failures as an SSE event `data: {"error":{...}}` and then closes without [DONE]:
- pinned upstream: tools/server/server-context.cpp:5434-5439;
- old build: server-context.cpp:5044, 5114;
- ik: examples/server/server.cpp:1266-1268.

That payload yields no TokenEvent, and llama.rs:193-197 simply ends the stream, so the consumer gets neither Done nor Err.

ik_llama.cpp's final payload has no stop_type, only stopped_eos/stopped_word/stopped_limit (ik server-context.cpp:2620-2623; bench/m0/probes/ik-*.json record stop_type null), so on class C every Done is `Stop::Other("unknown")`.

The TokenEngine contract (engine.rs:92-107) does not say how a stream must end.

**Failure scenario.** M0 already produced a generation-time exception: old-gpt-oss.server.log:32-33, "Unexpected empty grammar stack" thrown in slot iterate. With stream:true, a Native step under a lazy tool-call grammar delivers part of a call's ids and then just ends. That looks like a normal end of stream; only a consumer that notices the missing Done avoids sealing or executing a truncated call, which §6 forbids ("исполняется только после закрытия вызова и EOG").

On ik the loop cannot tell n_predict truncation from EOG at all, so the degraded Native mode either never executes calls or executes cut ones. `doctor --probe` also degrades silently to NotProbed instead of reporting the error.

**Fix.** - In token_events, map a payload with `error` to `Err(EngineError::Status { status: error.code (default 500), body: error.message })`.
- Derive Stop from stop_type, falling back to stopped_eos/stopped_word/stopped_limit.
- In RawEngine::complete, track whether a terminal payload arrived; if the byte stream closes first, emit `Err(EngineError::Protocol("stream ended without stop"))`.
- Document on TokenEngine::complete that every stream ends with exactly one terminal item, Done or Err.
- Add unit tests using real upstream and ik final and error payloads.

## [major] Server identity is guessed from a missing build_info; the router and unix:// endpoints of §4 cannot work
`crates/ognivo-models/src/caps.rs:18`

**Problem.** `ServerKind::from_build_info(None)` returns IkLlama, and doctor.rs:130-140 feeds it `Props::default()` whenever /props fails for any reason. Absence of evidence therefore becomes "ik_llama.cpp / class C".

ServerKind has no Router or Unknown variant, and neither LlamaServer nor CompletionRequest can select a model, although §4 lists `llama-router` as an endpoint type.

LlamaServer (llama.rs:66-88) concatenates base and path, so the `unix://…` endpoints of §4 fail in reqwest with "URL scheme is not allowed"; reqwest 0.13.5 does have ClientBuilder::unix_socket.

The M0 matrix (measurements.md:59-71, run-matrix.sh:46-52) has no router column, although §13 M0 requires one and architecture.md:158 says "проверить в M0".

**Failure scenario.** (a) A server started with --api-key, ognivo run without the key. /health is public (pinned server-http.cpp:252-258) and returns 200; /props and /tokenize return 401. doctor prints "ik_llama.cpp · class C" and "server streams no token ids (ik_llama.cpp)", and exits 0 because every finding is a Warn.
(b) A non-llama.cpp server on the LAN that answers /health but has no /props, such as vLLM, gets the same ik / class C label.
(c) llama-server in router mode answers /props without ?model= with role "router", model_path "none", n_ctx 0 (server-models.cpp:2164-2190). Every proxied POST without "model" fails with "model name is missing from the request" (server-models.cpp:2093-2095, 2226-2233). Family detection, /tokenize and /completion all fail, yet doctor says only the Generic path is available, which needs `model` just the same.

**Fix.** - Introduce `Endpoint { url, api_key, model: Option<String>, kind: LlamaServer | IkLlama | Router | Unknown }`.
- Infer IkLlama only from a 200 /props whose build_info is null, and Router from `role == "router"`.
- Report 401/403 as Error ("API key required/invalid") and stop there.
- For router endpoints, inject "model" into every POST body and `?model=` into GET /props and /slots.
- Map `unix://path` to ClientBuilder::unix_socket with a fixed http://localhost base.
- Add a router column to the M0 matrix: token-array /completion, id_slot, /tokenize and /slots through the proxy.

## [major] The M1 seam (ModelDriver and its history/event types) is missing while the M2 seam exists
`crates/ognivo-models/src/lib.rs:11`

**Problem.** §5.1 (architecture.md:186) makes ModelDriver the only seam the loop sees, with ChatCompletionsDriver in M1 and NativeDriver<Codec, TokenEngine> in M2+. The skeleton has only the M2 TokenEngine and postpones ModelDriver (lib.rs:11-12). There is no DriverEvent and no history-item type, and the SSE reader is hard-wired to /completion (llama.rs:167), so the Generic path has no client at all. TokenEngine uses `impl Future` methods and is not dyn-compatible: a trait of the same shape gives E0038 on rustc 1.99. So per-endpoint dynamic dispatch has to happen one level up.

**Failure scenario.** The M1 loop gets written directly against /v1/chat/completions messages (role/content/reasoning_content/tool_calls) and HTTP finish_reason. M2's NativeDriver needs per-item sealed token ids, the codec semver written to the transcript (architecture.md:180), placement() of dynamic context and its own stop reasons. The loop and the store schema then have to be rewritten. That contradicts "Цикл не знает, какой транспорт под ним" and the raw-id column "nullable с M1 — без миграции на Native" (architecture.md:388).

**Fix.** Before writing the M1 loop, define a dyn-compatible trait in ognivo-models and hold it as `Box<dyn ModelDriver>` per session:
`ModelDriver::step(&self, history: &[HistoryItem], tools: &ToolView, phase: Phase) -> BoxFuture<Result<BoxStream<DriverEvent>>>`
- `DriverEvent = ReasoningDelta | TextDelta | ToolCall { canonical, args, raw } | Usage | Finish { reason: Eog | Length | ContextFull | Error }`.
- `HistoryItem` keeps reasoning verbatim (the §5.2 replay rule) plus `raw: Option<SealedTokens { ids, codec }>`.
- Make `completion_stream` take the path, so ChatCompletionsDriver reuses the SSE reader.
- Keep TokenEngine a generic parameter inside NativeDriver.

## [major] Profile schema: reasoning set twice, no per-phase sampling, provenance only in comments, no min build, no partial overrides
`crates/ognivo-models/src/profile.rs:113`

**Problem.** The schema (deny_unknown_fields, every field required) cannot hold what §4, §5.1, §7, §11 and the new research need:
1. Reasoning lives in two places that nothing reconciles: `[reasoning].default/history` and `[template].kwargs`. gpt-oss has default "high" and kwargs.reasoning_effort "high"; Qwen has default "medium" plus kwargs reasoning_effort/enable_thinking/preserve_thinking. `default` is a free String although the valid levels differ per model: the Qwen profile itself says "high" is an error in its template (qwen3.8-27b.toml:28).
2. Sampling is only `[sampling.main]` with 4 knobs (profile.rs:98-109), and completion_body forces every penalty to neutral (engine.rs:163-169). §5.1 needs main/retry/utility/summary; the Qwen research wants prose vs code presets with presence_penalty 1.5 vs 0 (qwen38.md:280-281); §7 wants per-role effort, subagents at medium and utility at low (architecture.md:292).
3. Provenance (§5.1: run id, delta, date per value) exists only as TOML comments.
4. There is no min_build, so §4's "verified требует известную сборку" and the b6096/b7973/b8637 minimums cannot be checked.
5. User overrides in ~/.ognivo/ (§11) must be complete files.

**Failure scenario.** - An override `[reasoning] default = "low"` for gpt-oss leaves kwargs.reasoning_effort at "high", so the Native renderer and the Generic path run different efforts.
- For Qwen, `default = "high"` (the gpt-oss spelling) parses without complaint.
- The research's `presence_penalty = 1.5` for prose fails as an unknown field. Once the field is added, engine.rs:166 still overwrites it with 0.
- Retrofitting provenance later as Sourced<T> wrappers rewrites every field and every TOML file.
- An override containing only `[sampling.main] temperature` fails with "missing field id".
- recommended_flags point at repo-relative profiles/templates/… files that an installed binary or a LAN host does not have.

**Fix.** - Make `[reasoning]` the single typed source: control, validated levels, default, and per-phase overrides `{ subagent, utility }`. The Generic driver derives chat_template_kwargs from it per request; `[template].kwargs` keeps only keys that do not come from reasoning. Add a consistency check in the builtin test.
- Use `[sampling.<phase>]` tables as partial overrides of main, with Option presence/frequency/repeat penalties and n_predict, carried through SamplerSpec instead of being hard-coded.
- Add `[server].min_build`.
- Add `[provenance]` keyed by dotted path (`{ source, run, delta, date }`) under `#[serde(default)]`; this is purely additive.
- Load overrides by deep-merging the user's toml::Table into the builtin table before deserializing, keeping deny_unknown_fields for typo detection, plus a `schema = 1` key.
- Compile the canonical templates into the binary and add `ognivo template <id>` to write them out.

## [major] Constraint seam: one Token/Word trigger, no PATTERN or preserved_tokens, raw GBNF, trigger invariant not enforced
`crates/ognivo-models/src/engine.rs:51`

**Problem.** `Constraint { gbnf: String, trigger: Trigger }` carries exactly one trigger, and only TOKEN or WORD. llama.cpp accepts a list of TOKEN/WORD/PATTERN/PATTERN_FULL triggers (common/sampling.cpp:225-258). With PATTERN the grammar starts at the first capture group, so a trigger can require context the grammar does not contain (src/llama-grammar.cpp:384-398). llama.cpp's own gpt-oss parser needs four such patterns to cover both call forms, the role header (`<|start|>assistant to=…`) and the channel header (`<|channel|>commentary to=functions…`) (common/parsers/gpt-oss.cpp:145-148).

preserved_tokens cannot be sent, and a WORD trigger that is a single token is rejected without it: "Grammar trigger word should be marked as preserved token" (common/sampling.cpp:1068-1075).

The M0 rule that a grammar must start with its trigger lives only in a doc comment (engine.rs:48-49). And a raw GBNF string ties the seam to llama.cpp, while §5.7 plans to compile Grammar IR to llguidance or masks for the own engine.

**Failure scenario.** In the first M2 deliverable, Native gpt-oss:
- A single WORD trigger "to=functions" also fires inside the analysis channel whenever the model mentions a recipient, constraining reasoning against §5.7.
- Picking just one header form leaves the other form unconstrained.
- A Qwen codec that writes `Trigger::Word("<tool_call>")` gets an HTTP error on every request.
- A GBNF whose root does not begin with the trigger aborts mid-generation with "Unexpected empty grammar stack", the M0 error in old-gpt-oss.server.log:32. Through the previous finding it reaches the loop as a silent end of stream.

**Fix.** - Change the type to `Constraint { grammar: GrammarIr (lowered per engine via caps; Gbnf(String) only as an escape hatch), triggers: Vec<Trigger>, preserved_tokens: Vec<String> }` with `Trigger::{Token { id, text }, Word, Pattern, PatternFull }`.
- Allow construction only through a constructor that prefixes the root with the trigger alternatives (or uses capture groups), so the invariant holds by construction.
- Serialize all triggers and preserved_tokens.
- Add body tests for the Harmony pattern set and for a single-token Word trigger.

## [major] EngineCaps describes server identity, not capabilities: no room for lazy-grammar, KV-rollback or slot facts, or an in-process engine
`crates/ognivo-models/src/caps.rs:74`

**Problem.** EngineCaps is `{ kind, class, n_ctx, total_slots }`, and class is derived from stream-id completeness alone (caps.rs:58-71).

None of the caps listed in §4 (architecture.md:110-116) has a field: grammar_lazy and trigger types, reasoning-budget fields, --swa-full/checkpoints, --kv-unified. Neither do n_probs ids, /slots availability (`endpoint_slots` in /props) or the rollback model. Upstream /props exposes neither kv_unified, swa_full nor checkpoints (pinned server-context.cpp:5550-5598), so these values must be Option, probed or declared, never assumed.

`kind: ServerKind` (LlamaCpp | IkLlama) leaves a future in-process engine (§5.1) nothing true to report.

COMPLETE_ID_COMMITS (caps.rs:55) is a list of commit hashes. Once PR #31244 is merged, every later upstream build has a new hash, stays class B without --probe, and doctor warns "lossy" falsely, despite the comment saying upstream builds will join the list.

bench/m0/probes/old-gpt-oss.json predates the last edit of server_caps.py (mtime 23:24:58 vs 23:26:39) and still records HTTP 500 for the lazy-grammar test. Its "Unexpected empty grammar stack" error is the signature of a grammar that does not start with its trigger, so the "да" for old upstream at measurements.md:65 has no committed raw data behind it.

**Failure scenario.** In M2 the Harmony codec and the KV policy need to know, for this particular server, whether a rewind is free (gpt-oss with --swa-full), bounded by checkpoints (Gemma SWA) or impossible (Qwen hybrid, append-only), and whether lazy grammars and PATTERN triggers work. EngineCaps cannot say, so both fall back to profile constants that are wrong for a server started with different flags. A cforge in-process engine has to pretend to be `LlamaCpp { build: None }` and gets labelled class B.

**Fix.** - Make EngineCaps a `#[non_exhaustive]` record of `Probed<T>`/Option fields: stream_ids, n_probs_ids, lazy_grammar (probed by its effect, not by HTTP 200), trigger_types, logit_bias, slots_endpoint, total_slots, n_ctx_slot, kv_unified, `rollback: Free | Checkpoints | AppendOnly | Unknown`.
- Derive the A/B/C label from these fields and move ServerKind to the endpoint.
- Replace COMPLETE_ID_COMMITS with patched_commits plus `fixed_since_build: Option<u32>`.
- Cache probe results per (build, commit, model sha).
- Re-run the old-build probe and correct measurements.md:65.

## [minor] TokenEvent::Done drops `truncated` and the cache/timing stats: running out of slot context looks like an n_predict stop
`crates/ognivo-models/src/engine.rs:89`

**Problem.** Done keeps only stop and predicted_n. The final /completion payload also carries truncated, tokens_cached, timings.cache_n, timings.prompt_n, prompt_ms and predicted_ms (pinned server-task.cpp:332-350, server-common.cpp:143-150).

Upstream stops a generation that runs out of slot context with stop_type "limit" plus truncated:true (server-context.cpp:2145-2148), which looks exactly like an n_predict stop.

On the request side, §5.1's StopSpec and SlotLease are also missing (`slot: Option<u32>`), and cancelling exists only as dropping the stream.

**Failure scenario.** The Qwen profile recommends -c 98304 with compaction at 0.90 (≈88.5K), and the research uses n_predict 32768 (qwen38.md:272), so any step that starts above ~65K can hit the end of the slot. RawEngine reports `Stop::Limit`, exactly as for an agent-side thinking-budget stop. The loop then appends the end-of-thought and resubmits instead of compacting, and the server rejects the next request for exceeding the context.

Separately, three things cannot be computed from engine output: the per-step expected vs actual cache_n (architecture.md:262), the HUD cache %, and the M2 exit metric "попадание в кеш внутри хода ≥ 95%".

**Fix.** - Change Done to `{ stop, truncated, usage: Usage { prompt_n, cache_n, predicted_n, prompt_ms, predicted_ms } }` with Option fields.
- Add `Stop::ContextFull` for limit + truncated.
- Mark TokenEvent, Stop and Usage `#[non_exhaustive]`.
- Introduce StopSpec and a router-issued SlotLease newtype now, so M2 does not have to change every call site.
- Document that dropping the stream is the cancel.

## [minor] Store persists the client wire type: raw ids travel as unsealed client deltas, and the seq scope contradicts itself
`crates/ognivo-protocol/src/lib.rs:72`

**Problem.** ognivo-store depends on ognivo-protocol and stores `protocol::Event` as is. Raw ids reach the log only as `Delta::Tokens` inside the client stream, which freezes at M2 and is generated into TS. Replay needs the sequence after `seal()` (`<|return|>` → `<|end|>`, canonical re-render after an L1 repair) and the codec semver (architecture.md:180, 228); neither has a place in the event model.

`Seq` is documented as a position in one thread's log (protocol lib.rs:24-25), but MemoryLog numbers events globally across threads (store lib.rs:31, 50; the replay test uses seq 1/2/3 across threads a/b/a).

**Failure scenario.** - A SQLite store written to the protocol doc (per-thread seq) rejects the first event of a second thread as OutOfOrder. One written to match MemoryLog forces clients to track a global seq.
- Rebuilding a prompt by concatenating `Delta::Tokens` replays the unsealed `<|return|>` and the model's own drift, which §5.4 forbids.
- Every GUI and VS Code client receives token arrays it never uses.

**Fix.** - Add a store-level record next to the wire event, e.g. `ItemRecord { event, sealed_ids: Option<Vec<u32>>, codec: Option<String> }`, matching §11's nullable column. Write the sealed ids when the item completes.
- Remove `Delta::Tokens` from the client protocol; the Inspector can fetch tokens on demand.
- Choose global seq and fix the doc comment, or make append/last_seq per thread.
