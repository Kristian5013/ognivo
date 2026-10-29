# gemma4: Gemma 4 (gemma-4-26B-A4B-it) as an agentic coding model: native format, tool calling, reasoning, sampling and llama.cpp integration. What the agent harness must get exactly right.

## Executive summary
Gemma 4 was announced on 2026-04-02 under Apache 2.0 (the ai.google.dev releases page gives 2026-03-31) in four sizes: E2B, E4B, 26B A4B (MoE) and 31B (dense). Google added MTP drafters on 2026-04-16 and an encoder-free 12B "Unified" model on 2026-06-03. The 26B A4B has 30 layers and 25.2B total / 3.8B active parameters: 128 experts with top-8 routing plus a dense shared FFN. 25 layers use sliding-window attention (window 1024) and 5 are global; the global layers have K=V, 2 KV heads, head_dim 512 and proportional RoPE. Context is 262,144 tokens and input is text plus image. The native tool protocol is not JSON. Tool declarations go in the system turn as <|tool>declaration:NAME{...}<tool|>. The model calls tools as <|tool_call>call:NAME{key:value,...}<tool_call|> and stops on <|tool_response>; the app then appends response:NAME{...}<tool_response|> inside the same model turn. Every string is wrapped in the single token <|"|> (id 52) and there is no escape mechanism. Keys are bare identifiers and the template sorts dicts alphabetically (Jinja dictsort), so the model emits arguments in alphabetical order. Parameter names therefore decide the generation order (an edit tool with path/oldText/newText makes the model write newText first). Stop tokens are 1 <eos>, 106 <turn|> and 50 <|tool_response>, and llama.cpp treats all three as end-of-generation (EOG). Thinking is switched on by <|think|> in the system turn and comes out as <|channel>thought\n…<channel|>. With thinking off the 26B still emits an empty thought block, so the template pre-fills one. The official template has changed six times. The latest version (published 2026-07-09, committed 2026-07-15) adds null handling and an optional preserve_thinking flag, forces a thought after tool responses, and supports images in tool results. The GGUFs from ggml-org, bartowski and Google's QAT repo embed it; the local NVIDIA copy is older. llama.cpp at commit 5e03a1ff4 has a dedicated Gemma 4 PEG parser and lazy grammar, but with gaps: it does not enforce per-tool argument schemas; its chat endpoint rejects custom grammars when tools are present; it tokenizes every prompt with special-token parsing on, so file contents can inject control tokens; its dictsort is case-sensitive; and it inherits a template bug that moves "preamble text + tool call" content after the tool responses. Because of the sliding-window cache, llama-server can only reuse KV after a prefix mismatch by restoring a checkpoint; otherwise it re-reads the whole prompt. --swa-full is not an option on 24 GB (about 200 KiB/token), so a byte- and token-exact append-only history is essential for agent latency. Google's numbers (thinking mode) for the 26B: LiveCodeBench v6 77.1, Tau2 average 68.2 (telecom 43.0), Terminal-Bench Hard 14.0 versus 36.0 for the 31B, and 44.1 on 128K multi-needle retrieval (MRCR); Google publishes no SWE-bench or BFCL score. Alibaba measured the 26B at 17.4 on SWE-bench Verified versus 73.4 for Qwen3.6-35B-A3B. OpenAI reports 60.7 SWE-bench Verified and 34.2 Aider Polyglot for gpt-oss-20b. A community BFCL v4 run gives 89.1 non-live, 63.8 live and 45.1 multi-turn. In community agents the 26B is fast (about 215–245 tok/s decode and 11–14K tok/s prefill on a desktop RTX 5090 at Q4) and does well on simple agent suites (Hermes 30/30). It also has recurring failure modes: duplicate calls, announcing a tool instead of calling it, invented CLI flags, shell-quoting errors, a bare <|tool_call>: opener, "="/JSON drift on long calls, a missing final brace, repetition at high context, and much weaker behaviour without thinking. On the 24 GB RTX 5090 Laptop, Google's QAT Q4_0 GGUF (14.44 GB) fits the full 256K context with f16 KV (about 5 GiB global plus 0.3 GiB sliding-window cache); UD-Q4_K_XL (17.0 GB) fits 128K. Recommendation: build a native token-level Gemma 4 adapter. It renders prompts as token ids with content tokenized with special-token parsing off, keeps raw model output verbatim, and parses with a token-level, lenient-but-safe state machine. Tool calls are optionally constrained by a schema-derived GBNF grammar with token-exact strings. Thinking stays on with a budget, sampling follows Google (temperature 1.0, top_p 0.95, top_k 64, min_p 0, no penalties), and edit tools use parameter names that sort into the right order: edit{file, find, replace}.

## Recommendation
GEMMA 4 PROFILE v1 (target gemma-4-26B-A4B-it; also applies to 31B and 12B. E2B/E4B differ: sliding window 512 and no empty-thought prefill.)

0. DETECTION
Apply the profile when all of these hold:
- general.architecture == gemma4
- vocab[48] == <|tool_call>
- vocab[52] == <|"|>
Do not use the GGUF's embedded Jinja for rendering. Keep it only as a test oracle, and log its version (July header: "Published: 2026-07-09").

1. TRANSPORT
Primary: llama-server POST /completion with:
- stream:true, return_tokens:true, cache_prompt:true, a pinned id_slot
- prompt = an array of token ids built by the agent
Compat fallback: /v1/chat/completions with the July template. In that mode you must send reasoning_content back on tool-call turns, and you accept the bugs listed below.

2. TOKEN CONSTANTS
BOS 2, EOS 1, <|turn> 105, <turn|> 106, <|think|> 98, <|channel> 100, <channel|> 101, <|tool> 46, <tool|> 47, <|tool_call> 48, <tool_call|> 49, <|tool_response> 50, <tool_response|> 51, <|"|> 52, <|image|> 258880.
Text pieces such as "system\n", "user\n", "model\n" and "thought\n" come from the tokenizer.

3. TOKENIZATION RULE
- Template/control structure is emitted as ids.
- All system, user, tool and file text is tokenized with special-token parsing OFF: Rust tokenizers set_encode_special_tokens(true) on the tokenizer.json that matches the GGUF (verify parity against /tokenize), or /tokenize with parse_special:false.
- Assert that no content span produces ids 1, 2, 46-52, 98, 100, 101, 105, 106 or media ids.

4. PROMPT LAYOUT
Byte-identical to the July template wherever the template is sane.
- System turn: [2][105]"system\n" ([98]"\n" if thinking) SYSTEM.trim() {[46]DECL[47]}* [106]"\n"
- User turn: [105]"user\n" TEXT.trim() [106]"\n"
- Model turn start: [105]"model\n", then:
  - thinking off: [100]"thought\n"[101] (the official empty-thought prefill);
  - thinking on: nothing, or an optional forced [100]"thought\n".
- Model output: the generated ids stored verbatim, excluding the terminating EOG. This keeps preamble-before-call order and avoids re-serializing past output.
- Tool step: for each call in emission order, append [50]"response:"NAME"{"BODY"}"[51]. Then append [100]"thought\n" if thinking is on, and generate again in the same model turn (no new <|turn>).
- Final answer: [106]"\n" before the next user turn.
- BODY:
  - string result: value:[52]TEXT[52];
  - structured result: sorted k:v using format_argument rules (strings in [52], true/false, null, numbers, bare-key nested dicts, lists);
  - error: error:[52]MSG[52];
  - images: [258880] after [51] (vision mode only).

5. DECLARATIONS
Compile once per tool and cache:
declaration:NAME{description:[52]D[52],parameters:{properties:{P},required:[[52]r[52],…],type:[52]OBJECT[52]}}
- P is sorted case-insensitively and stably (Jinja2 dictsort semantics, NOT llama.cpp's case-sensitive sort).
- Each property: name:{description:[52]..[52],enum:[…](STRING only) | items:{sorted keys} | nullable:true | properties:{…},required:[…],type:[52]UPPER[52]}
Normalize JSON Schema first:
- resolve $ref;
- [T,null] becomes T + nullable;
- other unions become STRING plus a prose description of the alternatives;
- default, min/max, pattern and format go into the description text;
- free-form objects become a STRING parameter that holds JSON.
Names:
- tool names match [A-Za-z_][A-Za-z0-9_]* (map MCP names, e.g. server__tool);
- property names are lowercase snake_case identifiers;
- keep a reversible name map.

6. PARSER
A token-level state machine over the returned ids.
- TOP state:
  - 100 followed by the token 'thought' enters THOUGHT; any other 100 is stray and dropped;
  - stray 101 is dropped;
  - 48 enters CALL;
  - EOG ends the step;
  - anything else is CONTENT.
- THOUGHT: skip one leading "\n" and read until 101. Merge consecutive thought blocks for display but keep them raw in the transcript.
- CALL:
  - accept "call:" or a bare ":" opener; NAME is \w+;
  - then "{" ARGS "}" followed by 49;
  - key is an identifier or [52]-quoted; value is [52]…[52] (decode the exact tokens up to the next 52), a number, true/false/null, […] or {…}.
- Lenient mode (only when no grammar was applied):
  - accept '=' separators;
  - accept JSON "…" and '…' strings, decoding escapes;
  - accept bare words coerced by schema type;
  - repair a missing final '}' only if 49 arrived.
- Abort a call if 100, 101, 105 or 48 appears inside it.
- Never execute before 49 plus an EOG.
- END handling:
  - 50 means a tool step.
  - 106 or 1 means a final answer, unless complete calls were parsed; then treat it as a tool step.
  - stop_type=limit inside THOUGHT: resume, or force [101].
  - stop_type=limit inside CALL: discard the call and tell the model the output was truncated and should be split.
- Validation and execution:
  - validate each call against the original schema; send violations back as error results, never silently coerced;
  - dedupe identical (name, args) calls within a step; the duplicate gets an error result;
  - cap at 8 calls per step;
  - run read-only calls concurrently and mutating calls sequentially in emission order;
  - always emit responses in emission order.

7. CONSTRAINED DECODING
Default ON in native mode. Send:
- grammar
- grammar_lazy:true
- grammar_triggers:[{"type":0,"value":"<|tool_call>","token":48}]
Grammar compiled from the normalized schemas, for example:
  root ::= call{1,8}
  call ::= <[48]> "call:" ( t-bash | t-edit | t-read | t-write ) <[49]>
  t-read ::= "read{" "file:" s ( ",limit:" i )? ( ",offset:" i )? "}"
  t-edit ::= "edit{" "file:" s ",find:" s ",replace:" s ( ",replace_all:" b )? "}"
  t-write ::= "write{" "file:" s ",text:" s "}"
  t-bash ::= "bash{" "command:" s ( ",timeout_s:" i )? "}"
  s ::= <[52]> !<[52]>* <[52]>
  i ::= "-"? [0-9]{1,12}
  n ::= "-"? [0-9]+ ("." [0-9]+)? ([eE] [-+]? [0-9]+)?
  b ::= "true" | "false"
Rules:
- enums are alternations inside [52]; arrays are "[" (v ("," v)*)? "]"; nested objects are ordered key lists;
- keys appear in case-insensitive sorted order, matching training. Optional keys that sort before the first required key need alternation rules;
- no whitespace outside strings.
Keep the lenient parser for compat mode. Verify that grammar works together with MTP and the reasoning budget, and measure sampler cost on 50K-token string arguments.

8. REASONING
- Thinking ON for agent turns: <|think|> in the system turn, and a forced [100]"thought\n" after every tool response (as the July template does).
- Per-step budget through reasoning_budget_tokens, about 4-8K, with start '<|channel>thought', end '<channel|>' and a short budget message.
- Offer 'think less' through a LOW-thinking system instruction (Google: about 20% fewer thought tokens).
- History:
  - keep tool-step thoughts verbatim for the whole task. This equals preserve_thinking=true in the July template and the llama.cpp default, and it causes no prefix breaks;
  - drop final-answer thoughts when the next user message arrives;
  - drop all thoughts at compaction and summarize them as text, as Google suggests;
  - provide an 'official strip at every user turn' flag and A/B test it.
- The UI shows the thought channel collapsed and never treats it as content.

9. SAMPLING
Send explicitly with every request:
- temperature 1.0, top_k 64, top_p 0.95, min_p 0.0, typical_p 1.0
- repeat_penalty 1.0, presence_penalty 0, frequency_penalty 0, dry_multiplier 0, xtc_probability 0
- default sampler order; a fixed seed for evals
Never use repetition or DRY penalties: edits legitimately repeat text. Optional, unvalidated experiment: temperature 0.6-0.8 for retries of failed edits.

10. STOP CONDITIONS
- EOG ids {1, 106, 50}; no stop strings.
- n_predict per step of 16-32K, plus a wall-clock limit.
- Loop guard: the same call and arguments 3 times, or repeated thought lines, triggers an injected nudge.

11. TOOLS FOR GEMMA
- Flat string arguments, with parameter names whose alphabetical order equals the order you want the model to generate:
  - read{file, limit?, offset?}
  - edit{file, find, replace, replace_all?}: one edit per call; several edits are several call blocks in one step, applied in order
  - write{file, text}
  - bash{command, timeout_s?}
- No arrays of objects and no free-form objects; use a JSON-string argument if unavoidable.
- Results: a status line plus a unified diff; reads come back with line numbers; long outputs are truncated with paging hints.
- System-prompt rules aimed at known 26B failure modes:
  - call tools, don't announce them;
  - verify CLI flags and paths with tools before relying on them;
  - write scripts to files instead of python -c or heredocs;
  - read the error before retrying a command;
  - run code instead of simulating it;
  - stop and ask when blocked.

12. CACHE AND CONTEXT (24 GB RTX 5090 Laptop)
Model options:
- Google QAT Q4_0 (14.44 GB) with -c 262144 and f16 KV, or
- UD-Q4_K_XL (17.0 GB) with -c 131072.
Server flags:
- -np 1 per agent session, -fa on, --ctx-checkpoints 32
- --cache-ram 8-16 GiB, watching host RAM (#24265)
- no --swa-full, no context shift, KV quantization no lower than q8_0
- optional --spec-type draft-mtp with the MTP GGUF (verify the exact flags)
Agent invariant: each request's tokens == previous prompt + generated tokens. Assert this in debug builds and log tokens_cached on every step. Any history mutation (compaction, thought stripping) is batched into one deliberate re-prefill. Keep the working context at or below about 64-96K tokens and compact at 60-70% of n_ctx (MRCR-128K is 44%; repetition at high context, #21799).

13. GOLDEN TESTS
a) Render conversations with our renderer and with Python Jinja2 using HF settings (trim_blocks, lstrip_blocks, case-insensitive dictsort) on the July template. Output must be byte-identical except for deliberate deviations: preamble order and empty-thought retention.
b) Tokenizer parity against /tokenize on the same GGUF.
c) Round-trip fuzz of string arguments: ", """, \, {}, '<|' fragments, CRLF, tabs, emoji, 100 KB files, and files that contain literal <turn|>.
d) A parser corpus built from the cited issues: bare opener, '=' separators, JSON quotes, quoted keys, missing '}', duplicates, double <|channel>, trailing <channel|>.
e) Prefix-stability checks over 50-step runs.
f) Agentic task suites comparing thinking on/off, preserve vs strip, grammar on/off, and the 26B vs 31B and the quant choices.

## Architecture implications
- Model adapters must work at token level. Each family profile owns: a renderer that emits token ids, a streaming parser over token ids, a JSON-Schema-to-declaration-to-GBNF compiler, a validator, and a reasoning-history policy. llama.cpp's Jinja/PEG chat path is only a compatibility fallback.
- Use llama-server /completion as the primary transport: token-array prompt, return_tokens, grammar plus grammar_lazy/grammar_triggers, reasoning budget fields. /v1/chat/completions cannot combine a custom grammar with tools, and it re-serializes history: reasoning newlines, case-sensitive key order, content moved after tool responses. That re-serialization breaks KV prefixes and the model's native ordering.
- Make an append-only raw-token transcript a core invariant (sliding-window cache plus checkpoints). Store every model segment's generated ids verbatim and render only new segments. Treat compaction, thought stripping and elision of old tool output as explicit, rare events with a known re-prefill cost; telemetry should show tokens_cached per step.
- The agent needs the exact tokenizer: the Rust tokenizers crate with the tokenizer.json that matches the GGUF, cross-checked against /tokenize. It is needed for tokenization that cannot be injected (encode_special_tokens), exact context accounting, and diffing. Tokens, not strings, are the unit of history.
- The tool registry needs one canonical internal schema plus a projection per model. Gemma requires identifier names, flattened schemas (no anyOf/$ref/free-form objects) and parameter names whose alphabetical order matches the intended generation order. Plan for per-model aliases and reversible renaming of MCP tool names.
- Execute tools only after a call is closed and the generation has ended with an end-of-generation token. The UI can stream thoughts and content live, but should show tool calls as pending until they parse and validate, because the 26B sometimes backs out of a call mid-way.
- If the agent manages llama-server, it should own the launch profile: model file, context, -np, checkpoints, cache-ram, MTP, sampling. It should verify that profile through /props. If it attaches to arbitrary servers, it must detect and warn about unsafe settings (min_p 0.05 default, --swa-full, context shift, low-bit KV, a missing --jinja in compat mode).
- Golden-test and eval infrastructure is part of the product, not an afterthought: template-oracle tests run on the official templates with Python Jinja2 semantics, round-trip fuzzing, a parser corpus built from real model drift, prefix-stability tests, and small agentic task suites per model and quant.
- Vision (screenshots in tool results) in llama.cpp takes a different prompt path (strings with media markers). That path gives up token-array safety and cache-reuse shifting, so keep vision optional and isolated in the adapter.
- The 26B's weaker long-horizon agent skills (Terminal-Bench Hard 14, MRCR-128K 44) mean the harness has to supply structure: a todo/plan tool, mandatory verification steps (run tests or linters), duplicate and loop detection, tool errors that explain what went wrong, tight context budgets, and model-specific system-prompt rules.

## Risks and gotchas
- Template drift between GGUF vintages (April/May/July) changes the prompt: null rendered as 'None', how reasoning is gated, a forced thought after tool responses, whether string arguments are allowed. Never trust the embedded template blindly; detect its version or render it yourself.
- Official template bug: an assistant message with a preamble plus tool_calls has its content moved after the tool responses and the turn closed. With thinking on, the next generation prompt is malformed ('<|channel>thought' with no '<|turn>model'). llama.cpp only patches the thinking-off case.
- Control-token injection: llama-server parses special tokens inside all prompt text, so file contents and tool output containing <turn|>, <|tool_call>, <|"|> or <|think|> become real control tokens. Such text appears in llama.cpp sources, Gemma docs, and our own Gemma adapter code.
- <|"|> cannot appear inside a string argument (no escaping exists), so the model cannot natively write a file that contains that exact text. Keep the literal split in our own source code, and document the limitation.
- llama.cpp's built-in Gemma grammar enforces syntax only, not argument schemas. Nested or complex object arguments produce malformed calls (#21680), so keep arguments flat.
- Known 26B output drift: bare '<|tool_call>:' opener; '=' separators and JSON/Python-quoted strings in containers on long calls; <|"|>-quoted keys; missing final '}'; duplicate calls (sometimes 20-50 per turn in early runtimes); several <|channel> tokens; a trailing <channel|>; consecutive thought blocks; backing out of a call mid-way.
- A call cut off by the n_predict limit can look complete (for example a write with partial content). Never execute a call unless its <tool_call|> arrived and generation stopped on an end-of-generation token rather than the limit.
- Prefix breaks with sliding-window attention cost a re-prefill from the nearest checkpoint, or of the full prompt. A desktop 5090 manages about 2.8K tok/s prefill at 128K, so a full re-read is tens of seconds; the laptop is likely slower. Causes: re-serialized reasoning (extra newline), re-sorted keys, trimmed content, stripped thoughts, a moved preamble. --swa-full is infeasible on 24 GB. Checkpoints cost about 300 MiB of host RAM each, and #24265 reports system hangs near the RAM limit.
- llama.cpp's sampling defaults (min_p 0.05, top_k 40, temp 0.8) silently differ from Google's recommended settings unless they are set explicitly.
- llama.cpp's dictsort is case-sensitive but Jinja2's is not, so mixed-case property names get a different order. Use lowercase snake_case names.
- Arguments come out alphabetically, so edit tools named path/old_string/new_string make the model write the new text before the old one (one observed degenerate edit). Choose parameter names deliberately (file/find/replace).
- Long-context weakness (MRCR-128K 44.1, repetition at high context in #21799) and weak long-horizon agent scores (Terminal-Bench Hard 14, SWE-bench Verified 17.4 per Alibaba): do not plan on more than 128K of effective working context or long unsupervised runs.
- With thinking off the 26B degrades sharply in agent tasks. With thinking on, latency grows and runaway thought loops are possible. Use per-step budgets and a UI control to end reasoning.
- System messages in the middle of a conversation render as '<|turn>system' turns, which the model was not trained on. Put reminders inside user turns or tool-response text instead.
- The native format has no call ids, so parallel responses are matched by order and name. Always emit responses in call order with correct names; two calls to the same tool can be told apart only by order.
- MCP tools with hyphenated or dotted names, non-identifier property names, anyOf/$ref/additionalProperties schemas render badly or not at all in Gemma declarations, so the agent must normalize them.
- Vision: loading the mmproj disables cache_reuse; multimodal prompts are strings, so the injection protection is lost; Gemma 4 vision fixes in llama.cpp landed as late as 2026-09-04.
- The larger models emit ghost thought channels even with thinking off, so always prefill the empty thought block, and make the parser accept thoughts anywhere.
- For legacy templates llama.cpp parses tool output that looks like JSON into objects, which changes the representation the model sees between two tool calls with the same content.
- Blackwell CUDA toolchain issues (13.x MMQ segfault; 13.2 garbage output with Gemma 4): pin a toolkit version that has been verified for the RTX 5090 Laptop build.

## Questions for owner
- Should the agent own prompt rendering and parsing for each model (raw /completion with token-id prompts and our own GBNF grammar), accepting more adapter code, or delegate to llama-server's Jinja/PEG chat endpoint for simplicity? Recommended: own it, and keep the chat endpoint only as a fallback.
- Will the agent launch and manage llama-server itself (pinned build, flags, model files, MTP), or must it attach to any user-run llama.cpp server and version? This decides whether we can rely on /completion features, checkpoints, and our sampling and cache settings.
- Is Gemma 4 26B A4B the primary default despite weak long-horizon agent scores (Terminal-Bench Hard 14 vs 36 for the 31B; SWE-bench Verified 17.4 vs 52.0 per Alibaba)? Or should the profile also treat Gemma 4 31B (dense, about 3x slower) and/or the 12B as first-class in the eval matrix?
- What is the default memory plan on the 24 GB GPU: Google QAT Q4_0 with 256K context, or UD-Q4_K_XL with 128K (or the local NVFP4 converted to GGUF, about 18.8 GB)? The choice changes compaction thresholds and the context-budget UX.
- Is vision (screenshots or UI images in tool results) required in v1? It costs about 0.8-1.2 GB of VRAM and forces a separate, less safe prompt path in llama.cpp.
- Are we willing to carry llama.cpp patches or upstream PRs? Examples: tokenizing message content with parse_special=false, case-insensitive dictsort, a Gemma schema-aware grammar, exposing the stop token id. Or must the agent work with unmodified upstream releases only?
- Latency versus quality by default: always-on thinking with per-step budgets (slower, more reliable), or adaptive thinking (off for trivial steps)? This shapes the agent loop and the UI controls.

## Unknowns
- Whether the 26B emits keys in sorted order in every case; only a handful of raw samples were observed, all of them sorted.
- The model's exact whitespace conventions, for example whether it writes a newline before <channel|>. This decides whether llama.cpp's reasoning_content round trip breaks the prefix.
- The quality effect of preserve_thinking=true versus stripping thoughts at each user turn; no published evaluation was found.
- Real throughput and prefill speed on the RTX 5090 Laptop GPU; only desktop RTX 5090 figures were found.
- How grammar-constrained sampling interacts with MTP speculative decoding and the reasoning-budget sampler in llama.cpp, and how much the grammar sampler costs on very long string arguments.
- Whether /completion includes the terminating end-of-generation token id in tokens (to tell 50, 106 and 1 apart), and the exact llama-server flag combination for the Gemma 4 MTP drafter.
- Whether Google's multi-step training trajectories include preamble text before tool calls and thoughts preserved across user turns.
- No official Google SWE-bench, BFCL or Aider numbers exist for the 26B. Third-party numbers come from heterogeneous harnesses, and Alibaba's 17.4 SWE-bench Verified may understate it.
- The exact memory per context checkpoint for the 26B in llama.cpp and the practical --cache-ram sizing (about 300 MiB per checkpoint is an estimate).
- Whether llama-server will start honoring the prompt-part is_input flag, which would close the special-token injection hole on the chat endpoint.
- Whether Rust tokenizers set_encode_special_tokens(true) behaves exactly like the Python flag tested here; docs.rs does not state the semantics.
- How much quantization (QAT Q4_0 vs UD-Q4_K_XL vs NVFP4 vs Q8_0) affects tool-call syntax fidelity; the '=' drift reports came from an AWQ INT4 build.
