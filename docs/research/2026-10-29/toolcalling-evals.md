# toolcalling-evals: Making tool calling robust with small local models (gpt-oss-20b, Gemma 4 26B A4B on llama.cpp), KV-cache-friendly context engineering, and how to evaluate it

## Executive summary
1) At llama.cpp 5e03a1ff4 (2026-10-16) both reference models have their own parsers with lazy grammars. A grammar switches on only at the tool-call trigger, and it is switched off while the model is inside its reasoning block, so "reason freely, constrain only the call" is already the default. gpt-oss arguments are constrained by the tool's JSON Schema. Gemma 4 arguments are only checked for syntax: the schema check is a TODO in the code, and PR #23247 that adds it is still OPEN.
2) The OpenAI-compatible endpoint is a black box for this kind of work. It rejects custom grammars when tools are present, drops client grammar triggers, and accepts only auto/none/required for tool_choice. When parsing fails, the whole turn errors out and the client never gets the raw text. Masks that depend on agent state, schema checks for Gemma arguments, patch payloads constrained by a Lark grammar, and agent-side repair therefore all need raw /completion. That endpoint takes token arrays plus grammar, grammar_lazy, grammar_triggers, preserved_tokens and reasoning_budget_* fields.
3) Research from 2024-2026 converges: most of the reported quality loss from constraints came from changed prompts or formats. Constraining after free reasoning (CRANE, "Thinking Before Constraining", a 2026 apples-to-apples study) matches or beats unconstrained output, and constraints that only block invalid answers gave up to +17.4%.
4) With these models the main failures are not JSON syntax but round-trip and format errors. gpt-oss-20b produced malformed Harmony headers in 7/120 turns when reasoning_content was not sent back, and in 0/120 when it was (llama.cpp #27720). Gemma 4 garbles deeply nested object arguments (#21680), and with Aider's SEARCH/REPLACE format it produced malformed edits in 51.6% of 225 cases. The chat templates also differ between the HF repos, the Unsloth GGUFs and llama.cpp's bundled copies.
5) Both models use sliding-window attention (SWA). llama.cpp cannot rewind a SWA prompt without a context checkpoint. Checkpoints are created at user-message starts, at the last user message and just before each prompt end; the defaults are 32 per slot and 8192-token spacing, and the oldest is evicted first. Without a usable checkpoint the server re-processes the whole prompt.
6) Things that break the cached prompt prefix in these templates: gpt-oss puts reasoning effort and the current date at the very top, and Gemma puts the <|think|> token at the top of its system turn. Both drop chain-of-thought after a final answer. Gemma sorts argument keys when it re-renders a call, and llama.cpp re-serializes arguments. So session settings must stay fixed, per-turn reasoning should be limited with reasoning_budget/reasoning_control, and history should keep the exact generated token IDs.
7) Re-prefill cost: desktop RTX 5090 measurements are gpt-oss-20b prompt processing 9.8k→6.3k tok/s (2k→32k context) and Gemma 4 26B Q4_K_M ~11.4k→9.3k tok/s. The laptop 5090 has 10496 CUDA cores and 896 GB/s (vs 21760 and 1792 on desktop), so I estimate ~3-5k tok/s. That puts a 64k re-prefill at roughly 15-25 s against sub-second time-to-first-token on a cache hit.
8) gpt-oss-20b can afford --swa-full (~+3 GB at 128k), which makes any rewind cheap. Gemma 4 cannot (~25 GB at 128k by config arithmetic), and each Gemma checkpoint costs roughly 0.3 GB of host RAM (estimate). Strict append-only history therefore matters most for Gemma.
9) Published scores. gpt-oss-20b (high): SWE-bench Verified 60.7, τ-bench retail 54.8, Aider polyglot 34.2 (OpenAI); Terminal-Bench 2.1 13.9% and 4.0 0% (Artificial Analysis); τ²-airline 53.6% using 84.7k output tokens per task (OpenRouter, 2026-10-29). Gemma 4 26B A4B: LiveCodeBench v6 77.1, τ2 average 68.2, τ2 retail 85.5 (Google); Terminal-Bench 2.1 39.0% (Artificial Analysis); τ²-airline 68.9% at 20.4k tokens per task; Terminal-Bench 2.0 32.6% on local llama.cpp; long-context MRCR v2 at 128k only 44.1%. Neither model is on the official BFCL v4 leaderboard.
10) Benchmark landscape: Terminal-Bench is now a continuous benchmark. Version 2.1 (89 tasks) is the right difficulty for 20-26B models; 3.0 and 4.0 are frontier-hard. It runs on Harbor, which supports custom agents and a standard trajectory format (ATIF-v1.7).
11) OpenAI stopped reporting SWE-bench Verified in February 2026 because of contamination. It is still usable for comparing profiles on the same model. SWE-bench Pro V2 (642 tasks) was released today. τ-bench 2.0 needs a user-simulator LLM, and BFCL can run against any OpenAI-compatible endpoint.
12) Recommendation: own the chat format in the agent (one Rust codec per model family), use llama.cpp as a token engine through raw /completion, and keep the OpenAI-compatible path as a baseline. Build a Rust eval crate with four parts: golden tests for the format code, replay of recorded sessions, a live suite with mock tools that also tracks KV-cache metrics, and adapters for Harbor, BFCL, τ2 and mini-SWE-agent. The adapters go through a 'profile proxy' that serves each profile as an OpenAI-compatible endpoint, so the agent's own layer can be compared against llama.cpp's built-in one.
13) Rust building blocks exist: openai-harmony 0.0.8, llguidance 1.9.1, xgrammar 0.6.0 bindings, smg-symphony and smg-response-template 0.1.0 (table-driven parsers, including Gemma 4), tokenizers 0.23.2 and rmcp 3.8.0.
14) Gotchas found in the code: the OpenAI-compatible path flattens the prompt into one string and tokenizes it with special tokens enabled, so special-token text inside files or tool output may become real control tokens. One bad tool schema can break the grammar for every tool (#25923, fixed after the local checkout). Combining tools with response_format makes gpt-oss loop (#30377). A q4_0 KV cache degrades tool calling.
15) Process note: in five read-only crates.io API requests I mistakenly put the user's email address in the User-Agent header. No other personal data was sent anywhere.

## Recommendation
(a) PER-MODEL TOOL-CALLING PROFILE ('ModelProfile')

Principle: the agent, not llama.cpp, owns the chat format; llama.cpp is used as a token engine. A profile combines four things:
- pinned artifacts;
- a Rust format codec, one per family (Harmony, Gemma4; later Qwen-XML, Hermes);
- declarative knobs in a TOML file;
- provenance for every knob value from evals.
Two wire modes: 'raw' is the default for the two reference models; 'oai' (llama.cpp /v1/chat/completions with a vetted --chat-template-file) is the baseline and generic fallback.

1. Pins: GGUF sha256 and quant; llama.cpp commit; template file sha256 (a vetted copy, never taken implicitly from the GGUF); tokenizer hash; special and stop tokens.
   - gpt-oss stops: <|call|> 200012, <|return|> 200002, <|endoftext|> 199999.
   - Gemma stops: <turn|> 106, <|tool_response> 50, <eos> 1.

2. Raw transport:
   - Render the prompt to token IDs. Template parts become special-token IDs; untrusted text (files, tool output, user text) is tokenized with special-token parsing off.
   - POST /completion with: prompt as a token-ID array, grammar, grammar_lazy=true, grammar_triggers, preserved_tokens, reasoning_budget_tokens / start_tag / end_tags / message (or reasoning_control), explicit samplers, seed, return_tokens=true, cache_prompt=true.
   - Append the returned token IDs to history verbatim.
   - Track cache_n vs expected; alert when prompt_n exceeds the expected new suffix.

3. Codec interface (sketch):
```rust
trait FormatCodec {
    fn render_prefix(&self, s: &SessionCfg, tools: &[ToolDecl]) -> Tokens;
    fn render_item(&self, h: &HistoryItem) -> Tokens;
    fn generation_prefill(&self, st: &TurnState) -> Tokens; // e.g. forced tool-call prefix
    fn constraint(&self, st: &TurnState, allowed: &ToolMask) -> Constraint; // GBNF + triggers
    fn parser(&self) -> Box<dyn StreamParser>; // events: Reasoning, Content, ToolBegin{name}, ArgsDelta, ToolEnd{json}, Stop
    fn encode_tool_result(&self, r: &ToolResult) -> Tokens;
    fn repair(&self, raw: &[Token], e: &ParseError) -> Repair;
}
```
   - Harmony can build on openai-harmony and its StreamableParser; Gemma4 is a custom codec (smg-symphony's gemma4 table is a reference).

4. Tool adapter:
   - A JSON-Schema normalizer: resolve $ref, drop unsupported keywords, clamp maxLength, fix empty objects, flatten.
   - Gemma: pass deep objects as JSON-string parameters and avoid enum on required fields until fixed upstream.
   - Present tools in each model's native syntax: Harmony TS namespace, Gemma declaration syntax.
   - Deterministic ordering and namespacing (fs_, sh_, git_, mcp_<server>_), descriptions written as for a new teammate, 0-3 native-syntax examples only for ambiguous tools.
   - A core set of ≤12 tools is always in the prefix. MCP tools sit behind a tool_search meta-tool whose results append definitions as messages, so the prefix does not change.
   - Tool availability per agent state is enforced by grammar masks over tool names ('mask, don't remove').

5. Constraint policy:
   - Default is a lazy GBNF that activates after the trigger and never runs during reasoning.
   - gpt-oss: arguments are schema-constrained.
   - Gemma (raw mode): the agent builds a schema-aware GBNF over Gemma syntax; otherwise syntax-only plus post-validation.
   - Optional two-phase decode: stop at the trigger, then continue from the cached prefix with an eager llguidance/Lark grammar (for example Codex's apply_patch grammar) and a lower temperature for arguments.

6. Sampling: set explicitly for each phase (reasoning, content, tool_args, edit_payload).
   - Vendor defaults: gpt-oss T=1.0, top_p=1.0, top_k=0, min_p=0; Gemma T=1.0, top_p=0.95, top_k=64, min_p=0.
   - Never greedy while reasoning. Lower temperature only where an eval shows a gain.

7. Reasoning policy:
   - The mode is fixed per session: gpt-oss 'Reasoning: medium|high'; Gemma <|think|> on.
   - Per-turn caps go through the reasoning budget or control endpoint; never toggle the system flags mid-session.
   - Always round-trip reasoning on tool-call turns.
   - Retention after a final answer is a calibrated knob: within_turn (the vendors' rule), until_compaction, or summarize-thoughts (Google's suggestion for long agents).

8. Edit protocol:
   - gpt-oss: apply_patch (V4A, Codex-compatible) as one raw string argument.
   - Gemma: str_replace(path, old_string, new_string) with <|"|>-delimited strings, plus write_file; whole-file rewrite only for small files.
   - Decide all of these by A/B eval.

9. Robustness ladder:
   - L0 prevent: grammar.
   - L1 lenient parse: family quirks such as stray 'commentary', repeated <|channel>, trailing <channel|>, a missing closer at end of stream, a JSON call leaked into content; coerce types to the schema.
   - L2 silent regeneration: discard the failed tokens (they never enter the model's context; the trace keeps them) and resample from the cached prefix, which is cheap thanks to the checkpoint just before the prompt end. Use a new seed or a forced tool-call prefix; at most 2 tries.
   - L3 visible error: a compact, actionable tool-result error. Semantic errors stay in context; format garbage does not.
   - L4 stop and show the raw output to the user.
   - A content-only reply is a valid end of turn; do not nag the model to use a tool.

10. Loop policy:
   - Fingerprint each call as (tool, canonical args) and (tool, args, observation hash).
   - 2 identical calls: annotate the result ('unchanged from previous call').
   - 3: refuse execution and suggest alternatives.
   - 4, or a ping-pong ×6, or 3 consecutive errors: stop and ask the user.
   - Also stop on no progress (no file or test delta in N steps) and on a token budget per task.
   - Start from OpenHands' thresholds and calibrate per model.

11. Context and KV policy:
   - Fixed prefix order: system/developer, then tools, then project rules. History is append-only. Dynamic data (time, git status, diagnostics) goes in appended user or tool messages.
   - Deterministic tool-result serialization. For Gemma, always send an object {exit_code, stdout, stderr, truncated} so llama.cpp does not reinterpret JSON-looking output.
   - Cap each tool output at about 4-8k tokens (head and tail kept), with the full output saved to a file and its path given.
   - Compaction happens at user-turn boundaries as a restart: static prefix, then summary plus todo, then the last turns.
   - Context budget ≤64k by default (Gemma 26B MRCR at 128k is only 44%).
   - Server flags per profile:
     - gpt-oss: -c 131072, --swa-full, -np 2, f16 or q8_0 KV.
     - Gemma: -c 65536-131072, no --swa-full, a tuned --ctx-checkpoints count, f16 or q8_0 KV (never q4_0), --cache-ram for subagents.
   - Save the static-prefix slot state to disk for an instant session start.

12. Capabilities and provenance:
   - parallel_tool_calls: off for gpt-oss (the template renders only the first call), optional for Gemma. Also record max recommended tools and vision support.
   - Every knob records the eval run id, metric delta and date; profiles are semver-versioned with a changelog.

(b) MINIMAL-BUT-SERIOUS EVAL HARNESS ('cforge-eval': a Rust crate and CLI, ATIF-v1.7 traces with extras for prompt token hashes, raw tokens and cache stats)

T0, codec goldens (every commit, CPU, under 1 minute):
- render goldens for each profile;
- a parse corpus of real captured outputs, including malformed ones (#27720-style headers, the Gemma edge cases);
- a KV-fidelity round trip: the next prompt must start with the previous prompt plus the generated tokens;
- grammar compilation for all bundled and popular MCP schemas;
- special-token injection cases.

T1, replay (every commit, CPU, about 1 minute):
- cassettes keyed by hash(token IDs + sampling) replay about 50 full sessions with mock tools;
- assert trajectories, UI events, compaction and loop decisions;
- any prompt-hash miss is reported as an unintended prefix change.

T2, live micro-suite (nightly, GPU, 45-90 minutes per profile):
- about 250 scenarios × 3 seeds, run against a mock filesystem, shell and MCP servers;
- tool selection with 5, 15 and 40 tools;
- argument fidelity: multi-line code, quotes, backslashes, unicode, special-token text, paths;
- nested, enum and optional parameters;
- irrelevance (no tool needed);
- multi-step dependencies, error recovery, loop bait;
- long context at 16k, 48k and 96k;
- edits applied to a fixture repo and verified by diff or tests;
- teacher-forced next-action accuracy on golden traces, a low-variance step-level metric.

T3, agentic tasks (nightly rotating subset, weekly full), run through Harbor with a cforge installed-agent adapter:
- Terminal-Bench 2.1: 30 stratified tasks nightly, 89 weekly; CPU-only tasks; resource floor and ceiling recorded;
- SWE-bench Verified/Multilingual: 30 nightly, for relative comparison only;
- an Aider polyglot subset (Rust, Python, Go);
- 20-50 tasks from the owner's own repos (revert a commit, give the issue text, hidden tests) as a contamination-free signal.

T4, comparability (monthly or on a model change):
- BFCL v4 without the web-search categories, τ-bench text-v1 (English), full Terminal-Bench 2.1, mini-SWE-agent bash-only;
- each run both through the profile proxy and against llama.cpp's own /v1/chat/completions, so the difference shows what the agent's layer adds or costs.

Metrics:
- parse_ok, schema_ok, tool_correct, args_match (exact / AST / semantic), irrelevance false positives and negatives;
- a histogram of repair levels, silent retries, visible errors, loop stops;
- success rate, pass@1, pass^3;
- turns, output and reasoning tokens, wall time, prompt and generation tok/s, time to first token;
- cache_hit_rate = Σcache_n / Σ(cache_n + prompt_n), re-prefill tokens per turn, full re-processing events, peak context, number of compactions;
- infrastructure errors counted separately.

Method:
- Paired comparisons on the same tasks and seeds, with Wilson confidence intervals plus paired bootstrap or McNemar; state the minimum detectable effect.
- k≥3 runs for agentic tasks; quarantine flaky tasks found by oracle runs.
- Pin and record the llama.cpp commit, flags, GGUF and template hashes, profile version, driver and power profile (reuse cforge's bench-conditions recorder).

Operations:
- The GPU is exclusive at night. Throughput runs use -np 2-4; debug runs use -np 1 with fixed seeds.
- 2-4 Docker task containers in parallel within 62 GB of RAM.
- Size the nightly sets with measured tokens per task (τ²-airline: gpt-oss-20b 84.7k vs Gemma 20.4k output tokens per task).
- Runs are resumable and produce a morning report with diffs and confidence intervals against the baseline.
- CI gates block a profile change if T0 or T1 fails, if T2 valid-call rate drops by more than 1 pt or task success drops beyond its confidence interval, or if cache-hit rate drops by more than 2 pts.

## Architecture implications
- The agent needs its own per-family format codecs: renderer, streaming parser, repair rules and grammar builder, in Rust. It also needs a tokenizer per model, either in-process (the tokenizers crate or the GGUF vocab) or via /tokenize with parse_special=false. llama.cpp then works as a token engine through raw /completion, with /v1/chat/completions kept as baseline and fallback.
- History must be stored as an event log of exact generated token IDs plus a structured view, and never re-rendered differently. The append-only prefix must be an invariant the code can test (prompt n starts with prompt n-1 plus generated n-1).
- Session-level invariants: system/developer prompt, tool set, reasoning mode and date are fixed per session. Per-turn behaviour is changed only through appended messages, grammar masks, prefills and the reasoning budget.
- The tool registry needs a JSON-Schema normalizer, namespacing, deterministic serialization and deferred loading for MCP tools (a tool_search meta-tool that appends definitions). State-dependent tool availability is enforced by grammar masks over tool names, not by editing the tool list.
- Edit tools should be a pluggable subsystem with several protocols (apply_patch V4A, str_replace, whole-file) chosen by profile, each with a strict parser and dry-run validation.
- The repair ladder, loop/stuck detector and no-progress detector belong in the agent loop itself, not in prompt text. Failed generations go to the trace but not to the model's context.
- A server orchestrator should own the llama-server lifecycle per profile: flags (--swa-full for gpt-oss, checkpoint count for Gemma, KV type, ctx, -np, --cache-ram, --slot-save-path), health checks and model swaps for nightly evals. It also reads timings.cache_n on every request.
- Context management: tool-output caps with files as spill-over, compaction as a restart at user-turn boundaries, a todo/notes file recited at the end of context, and subagents in separate slots so the main session's KV cache survives.
- Escape or tokenize untrusted text so that special-token strings in files or tool output cannot become control tokens.
- Make the eval harness a first-class crate. Ship a 'profile proxy' that serves any profile as an OpenAI-compatible endpoint, so Harbor, BFCL, τ2 and mini-SWE-agent (Python, development-time only) measure the agent's own layer and can be A/B'd against llama.cpp's built-in parsers.
- Use ATIF-v1.7 as the trace format, with extras for prompt hashes, raw tokens and cache stats. The same traces feed the UI timeline, replay tests and eval reports.
- The UI should expose model-specific channels: Harmony commentary preambles as progress messages, reasoning hidden or collapsible, and a per-turn cache-hit and prefill-cost indicator (an honest latency display for a local GPU).

## Risks and gotchas
- If the client drops reasoning_content on tool-call turns, gpt-oss parsing breaks (7/120 vs 0/120 malformed turns in #27720) and Gemma tends to emit tool calls as plain content (#22786). Many agent frameworks make this mistake.
- Template drift: the HF, Unsloth-GGUF and llama.cpp copies of both templates differ. The Unsloth gpt-oss template renders some assistant content in the analysis channel. The current HF Gemma template rejects string arguments. Pin vetted templates by hash and test them with /apply-template.
- In oai mode a parse failure errors the whole turn and the raw text is lost, so the agent can only retry blindly; there is no repair path.
- Special-token text inside tool output or files may be tokenized as real control tokens (the OAI path flattens the prompt and tokenizes it with special tokens enabled, per code reading). This can corrupt Harmony/Gemma structure or act as prompt injection.
- One unusual MCP schema (an empty object, a huge maxLength) can break the tool grammar for every tool (#25923, fixed 2026-10-21, after the local checkout). Every tool set needs normalization and a compile test.
- Never combine tools with response_format on gpt-oss (#30377: endless reasoning, no tool call).
- Toggling gpt-oss reasoning effort or Gemma <|think|> per turn re-prefills the entire context. Dropping CoT at a user-turn boundary re-prefills the whole previous turn: tens of thousands of tokens, roughly 5-15 s on the laptop GPU (estimate).
- Without a checkpoint at or before the divergence point, SWA models re-process the whole prompt. Checkpoints are evicted oldest-first (32 per slot). Gemma checkpoints cost roughly 0.3 GB of host RAM each (estimate). --swa-full is infeasible for Gemma at long context.
- --cache-reuse (KV shifting) is disabled with a multimodal projector loaded or when memory cannot shift. Don't design compaction around it.
- KV q4_0 degrades tool calling (llama.cpp docs); use f16 or q8_0.
- llama-server's default samplers (temp 0.8, top_k 40, min_p 0.05) are not the vendor settings; always set them explicitly. Greedy decoding causes loops in reasoning models, and Gemma at T=0 has leaked tool syntax into content.
- The gpt-oss template renders only tool_calls[0]. Sending several calls in one assistant message silently corrupts history.
- Gemma 4: nested object arguments are unreliable (#21680), array arguments containing braces get stringified (#21384), content is truncated at a literal <|tool_call> (#22371), and arguments are not schema-constrained (PR #23247 open; enum + required gives HTTP 500 in that branch).
- Streaming clients must accumulate tool-call deltas carefully: llama.cpp's first SSE chunk can carry name, id and '{' together (#22722).
- Aider-style SEARCH/REPLACE edits failed for Gemma 4 in about half the cases in a local test. Don't impose one edit format on all models.
- Few-shot examples and uniform context can trap the agent in repetitive action patterns over long loops (Manus); examples must be curated and A/B tested.
- Benchmark pitfalls: SWE-bench Verified is contaminated (use it only for relative comparisons). Terminal-Bench 3.0/4.0 give almost no signal for 20B models (gpt-oss-20b scored 0% on 4.0). Container resource caps shift Terminal-Bench scores by up to ~6 pts. The same model differs by ~10 pts across serving stacks.
- Overnight capacity is small: gpt-oss-20b used ~85k output tokens per τ²-airline task. At ~150 tok/s (estimated laptop decode) one task takes about 10 minutes, so suites must be subsetted and run with parallel slots.
- Nondeterminism: prompt caching and multi-slot batching change logits, so profile comparisons need several seeds and paired statistics; single runs are not evidence.

## Questions for owner
- Wire mode: should the agent own templating, tokenization and grammars (raw /completion, full control: schema-constrained Gemma arguments, tool masks, repair, exact cache reuse; costs two Rust codecs plus a GBNF generator), or rely on llama.cpp's /v1/chat/completions parsers? Recommendation: raw for the reference models, the OpenAI-compatible path as baseline and fallback.
- Are curated per-model profiles enough at launch, or is there a hard requirement to support arbitrary GGUFs from day one through llama.cpp's generic parser? The latter makes the oai path production-critical.
- May we ship and test against a pinned llama.cpp build with our own patches (for example Gemma schema-constrained arguments from PR #23247, safe tokenization of special tokens), or must we stay on upstream releases?
- Can cloud LLMs be used inside evaluation only (as the τ-bench user simulator, as an LLM judge, for task audits), or must the whole eval pipeline be local? Local-only changes which τ2 numbers are comparable and adds load to the GPU.
- How many GPU hours per night or week can go to evals, given the same RTX 5090 serves interactive work? Is running evals with 2-4 parallel slots acceptable even though it makes them non-deterministic?
- What context budget should a session target on the 24 GB card (32k / 64k / 128k)? And for Gemma 4, fully on GPU at Q4_K_M, or a higher quant with MoE experts partly on CPU? This sets KV policy, compaction thresholds and tok/s.
- Are Python-based harnesses (Harbor, BFCL, τ2-bench, mini-SWE-agent, the Aider benchmark) acceptable as development-only dependencies, or must the eval stack be pure Rust?
- Is Docker or Podman available and allowed for sandboxed task execution in evals (and, later, for the product's terminal sandbox)?

## Unknowns
- Actual prompt-processing and generation speed of both models on the RTX 5090 Laptop (the figures used are scaled from desktop 5090 results); needs llama-bench -d sweeps on the owner's machine.
- Real KV-cache size of Gemma 4 in llama.cpp (whether K and V are stored once when attention_k_eq_v=true) and the actual size of a context checkpoint; my figures come from config arithmetic.
- Whether special-token injection really happens at runtime in the OpenAI-compatible path; this comes from code reading only, with no runtime test.
- The runtime effect of the Unsloth gpt-oss template's analysis-channel branch when reasoning_content is round-tripped on final-answer turns; not tested.
- How much schema-constrained arguments, two-phase decoding or each edit format change quality for these two specific models; no published ablations found.
- BFCL v4 scores for gpt-oss-20b and Gemma 4 26B (not on the official board), and τ²-bench retail numbers for gpt-oss-20b (OpenAI published only τ-bench v1).
- Whether PR #23247 (schema-constrained Gemma 4 tool calls) or an equivalent was merged after 2026-06-30; the local 2026-10-16 checkout still has the TODO.
- The SWE-bench Verified score of Gemma 4 26B (25.8% appears only on an aggregator) and the exact harness Artificial Analysis used for its Terminal-Bench 2.1 numbers.
- Whether llama.cpp's grammar for the chat-format parsers can use the llguidance backend; my code reading suggests lazy triggers are GBNF-only.
- Which Terminal-Bench version the Harbor 'terminal-bench-cpu-only' dataset (63 tasks) derives from.
- Optimal tool-count thresholds and description styles specifically for Gemma 4 and gpt-oss; the published thresholds come from other models (Llama-3.1-8B, DeepSeek-v3, Claude).
