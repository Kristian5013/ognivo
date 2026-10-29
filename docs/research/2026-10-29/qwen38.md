# qwen38: qwen3.8: Qwen3.8-27B as an agentic coding model in a llama.cpp-only agent: ChatML + Qwen3-Coder XML tool format, preserved thinking and reasoning effort, hybrid Gated DeltaNet caching, MTP, measured performance, and a Qwen3.8 profile spec. What the agent harness must get exactly right.

## Executive summary
Qwen3.8-27B (Qwen/Qwen3.8-27B) is the dense, Apache-2.0 member of the Qwen3.8 family. It went public on HF and ModelScope on 2026-08-14 (repo created 2026-08-05); the official FP8 build appeared on 2026-08-13. It followed the API launch of Qwen3.8-Max on 2026-08-02/03. Siblings:
- Qwen3.8-2.4T-A95B: the open "Max" weights, under a custom license with a $50M-revenue clause.
- Qwen3.8-Flash-Next: 125B-A6B plus n-gram embeddings and Qwen Sparse Attention; llama.cpp arch qwen4exp.

Architecture: a native vision-language model on the Qwen3.5 backbone.
- 64 layers as 16 × (3 Gated DeltaNet + 1 gated full-attention layer): 48 linear-attention layers (48 V / 16 QK heads × 128) and 16 GQA layers (24 Q / 4 KV × 256, 64-dim partial mRoPE).
- Hidden 5120, FFN 17408, vocabulary 248,320, one MTP layer.
- Context 262,144 tokens native, about 1M with YaRN.
- KV cache is small: 64 KiB/token at f16, about 34 KiB at q8_0.
- Every sequence also carries a fixed 149.6 MiB fp32 recurrent state that cannot be rewound. That one fact drives the whole caching design.

The owner's unsloth Q4_0 GGUF (16.06 GB, arch qwen35, 65 blocks) already contains the MTP head (blk.64). Its embedded template is an Unsloth variant: it merges system/developer messages and maps "high" to "xhigh".

Format:
- Turns: ChatML; <|im_start|>/<|im_end|> are CONTROL tokens.
- Tool declarations: JSON lines inside <tools> in the system turn. Order: an optional effort sentence, then the tools block, then fixed XML instructions, then the user's system prompt.
- Calls: <tool_call> (a single USER_DEFINED token), then plain-text <function=NAME> and <parameter=NAME> tags. Values are raw and unescaped and end at "\n</parameter>\n".
- Several calls per turn are separated by a newline. Results are grouped as <tool_response> blocks inside one user turn; there are no call ids, so results match by order.
- Thinking: "<think>\n" is prefilled; thinking-off prefills "<think>\n\n</think>\n\n".
- reasoning_effort: xhigh (the default, injects a sentence), medium (injects nothing), low (injects a sentence). "high" raises an error in the official template.
- preserve_thinking now defaults to on: all past reasoning stays in the prompt. Qwen3.6 had it off.
- Generation stops on <|im_end|> 248046 or <|endoftext|> 248044.

Official sampling:
- Thinking: temperature 1.0, top_p 0.95, top_k 20, min_p 0, presence 0.
- Non-thinking: 0.7, 0.8, 20, 0, presence 1.5.

Tokenizer, verified with llama-tokenize on the owner's GGUF:
- The six tag tokens (<tool_call>, </tool_call>, <tool_response>, </tool_response>, <think>, </think>) are USER_DEFINED (special=false in HF). llama.cpp matches them even with parse_special=false, so file contents can inject them.
- The XML tag text does not align with tokens ("=read" and "=path" are single tokens).
- Emoji and box-drawing characters split into incomplete UTF-8 tokens, so PR #31244 matters here too.

llama.cpp at 5e03a1ff4 routes this template to the qwen3-coder PEG parser. The chat path has open defects for this model:
- Whitespace that cannot be round-tripped (#30357) forces full re-prefill on this hybrid.
- A stray trailing <think> after tool calls cancels the whole turn (#27733).
- A literal "</think>" in reasoning is matched as a string (won't fix).
- Mid-conversation system messages return HTTP 500.

On the hybrid arch, llama-server reuses only an exact prefix:
- A divergence needs a checkpoint (149.6 MiB each). Checkpoints are created only near prompt ends, never during generation; otherwise the whole prompt is re-processed.
- Partial recurrent rollback exists only inside speculative decoding.
- The server reasoning budget cannot see a prefilled <think> on raw /completion.
- MTP works (+30% on a desktop RTX 5090 at n=3; about 48 tok/s on a 5090 Laptop per community reports), but it is fragile. Reports include decode degradation in long agent sessions, 0% acceptance after updates, and checkpoint bloat of about 4 KiB/token.
- Long contexts are suspect: silent instant EOS beyond about 130K tokens (#27756, closed as not planned).

Benchmarks:
- Vendor (Claude Code harness): Terminal-Bench 2.1 73.0, SWE-bench Pro 61.7, LiveCodeBench v6 90.3. No official SWE-bench Verified, tau2 or BFCL numbers.
- Independent: vals.ai SWE-bench Verified 86.0% (bash-only mini-swe-agent, about 2M tokens per task); vals Terminal-Bench 2.1 58.4%.
- Artificial Analysis rates the xhigh setting as very verbose (130M output tokens).
- Reasoning-effort studies agree: medium matches xhigh on agentic coding at a fraction of the tokens (VulcanBench 82.6% vs 73.9%; xhigh binges up to 80K tokens). Low behaves like medium.

Community failure modes: tool calls inside the open thinking block, "announce, then stop" turns, trailing <think>, Hermes-JSON drift, duplicate calls, and overthinking.

Estimated speed on the RTX 5090 Laptop (Q4_0, llama.cpp, not measured):
- Decode about 36–42 tok/s at short context and 21–25 tok/s at about 120K.
- Prefill about 1.1–1.6K tok/s.
- So a medium-effort step with about 3K thinking tokens costs about 1.5 minutes of decode.
- Fits 24 GB at 131K context with q8_0 KV (about 21 GiB total).

Recommendation: a native token-level Qwen adapter.
- Transport: /completion with token-ID prompts, plus PR #31244.
- A content encoder that never emits added-token IDs.
- A byte-exact renderer for the official template.
- Verbatim replay of the model's own token IDs, with canonical re-rendering only for anomalous turns.
- A two-level parser: a token-ID state machine plus a schema-aware byte-level XML reader with explicit disambiguation for "</parameter>" inside values.
- An agent-side thinking budget.
- reasoning_effort fixed per session at medium.
- Official thinking sampling with min_p sent explicitly as 0.
- Claude-Code-shaped tools: str_replace Edit with old_string/new_string; raw XML values carry code without escaping.
- q8_0 KV at 131K, MTP off until a soak test passes, and compaction near 90K.

## Recommendation
QWEN3.8 PROFILE v1
Target: Qwen3.8-27B, GGUF arch qwen35, on llama-server pinned at 5e03a1ff4 plus PR #31244.
- Also valid for Qwen3.5/3.6 dense and MoE, with the template deltas in §0/§4.
- Not valid for Qwen3.8-Flash-Next (qwen4exp, QSA) or the 2.4T model.

0. DETECTION
Apply the profile when all of these hold:
- general.architecture ∈ {qwen35, qwen35moe}, tokenizer.ggml.pre == qwen35, n_vocab == 248320
- vocab[248045] == <|im_start|>, vocab[248046] == <|im_end|>, vocab[248058] == <tool_call>, vocab[248068] == <think>, vocab[248069] == </think>

Template version:
- Contains "reasoning_effort|default('xhigh')" and "preserve_thinking is undefined": 3.8 rules.
- Contains "preserve_thinking is defined and preserve_thinking is true": 3.6 rules (no effort sentence, history stripped by default, think extraction from content).

Never render with the GGUF's Jinja. Keep it only as a test oracle and log its sha256:
- official: c3cf9e34abf4f9e3 (Flash-Next uses the same file)
- Unsloth main GGUF: 12827f24b742ea4e (merges system/developer messages, maps high→xhigh)
- Unsloth MTP sidecar: 701ba13a085c0c1b (official minus "| safe")

The 65-block GGUF (64 layers + nextn blk.64) needs a build with qwen35 MTP support; older builds fail on a missing blk.64.ssm_conv1d.

1. TRANSPORT
- Primary: POST /completion with prompt = a token-ID array rendered by the agent.
  - Request fields: stream:true, return_tokens:true, cache_prompt:true, id_slot pinned per session, n_probs:0, explicit sampler fields (§9).
  - Build the pin with PR #31244. This tokenizer splits emoji, box-drawing and rare CJK into incomplete-UTF-8 byte tokens (🚀 = 3, └ = 2, ⚠ = 3), so unpatched streaming silently drops IDs.
- Parse token IDs only. Build text from the token bytes; the server's "content" is for display only.
- Compat or diagnostics fallback: /v1/chat/completions --jinja --chat-template-file <official template>.
  - Send reasoning_content back on every assistant turn.
  - reasoning_effort ∈ {xhigh, medium, low, none}; never "high" (the official template raises).
  - Never send a system or developer message after the first turn (raises, HTTP 500: #27367, #29947).
  - Known losses on this path:
    - #30357: whitespace normalization causes a prefix miss and a re-prefill on the hybrid model;
    - #27733: a trailing <think> kills the whole turn;
    - '</think>' is detected as a string (#27661, #28679);
    - a custom grammar is rejected when tools are present.

2. TOKEN CONSTANTS
CONTROL tokens:
- <|endoftext|> 248044: end of generation; listed as GGUF "bos" but never added
- <|im_start|> 248045
- <|im_end|> 248046: EOS
- <|vision_start|> 248053, <|vision_end|> 248054, <|vision_pad|> 248055 (GGUF pad), <|image_pad|> 248056, <|video_pad|> 248057
- FIM/repo/file_sep 248060-248065: 248063/248064/248065 are EOG in llama.cpp
- audio/tts 248070-248076
- [PAD] 248077-248319: unused

USER_DEFINED tokens (HF special=false):
- <tool_call> 248058, </tool_call> 248059
- <tool_response> 248066, </tool_response> 248067
- <think> 248068, </think> 248069

Text pieces: "\n" 198, "\n\n" 271, "\n\n\n" 1358, "assistant" 74455, "user" 846, "system" 8678.

Fixed sequences:
- generation prompt, thinking: [248045, 74455, 198, 248068, 198]
- generation prompt, non-thinking: [248045, 74455, 198, 248068, 271, 248069, 271]
- canonical end of reasoning: [198, 248069, 271]
- separator between parallel calls: [248059, 198, 248058]
- tool-result turn: [248045] + bpe("user\n") + for each result i: (i>0 ? [198] : []) + [248066] + bpe("\n" + OUT_i.trim() + "\n") + [248067]; then [248046, 198]

Tag text inside calls is NOT token-aligned: "<function=read>" = 27, 1628, 86779 "=read", 29, but "<function=edit>" = 27, 1628, 28, 3468, 29. Parse the XML on bytes, never on fixed token patterns.

3. TOKENIZATION RULE
- Structure is emitted as constant IDs. Each text run between two structure IDs is BPE-encoded as one string, which reproduces HF's canonical merges.
- Content uses a content-safe encoder: tokenizer.json with the added-token pass disabled.
  - The six USER_DEFINED tags are special=false. HF matches them unconditionally, and llama.cpp matches them even with parse_special=false (verified: --no-parse-special turns "<tool_call>" into 248058 and "</think>" into 248069, while "<|im_end|>" becomes 5 pieces).
  - So neither /tokenize nor encode_special_tokens is safe.
- Literal tag strings in user text, files, tool output and MCP descriptions become ordinary pieces.
- Assert: no content run produces an ID ≥ 248044.
- Template literals (the <tools> wrapper, the instruction block that mentions <tool_call>) count as structure and use the real IDs, exactly as HF renders them.
- Never add BOS (add_bos=false).
- Parity oracle: /tokenize with parse_special:false on plain-text corpora (code, Russian, CJK, emoji, CRLF).
- Budget Russian at about 2–3 characters per token.

4. PROMPT LAYOUT
Byte-identical to official template c3cf9e34 under Python-Jinja2 semantics. Copy the long literals from the template file at build time; don't retype them.
- System turn with tools:
  <|im_start|>system\n
  [EFFORT\n\n]
  # Tools\n\nYou have access to the following functions:\n\n<tools>
  {\nTOOL_JSON}*
  \n</tools>
  \n\nIf you choose to call a function ONLY reply in the following format with NO suffix:\n\n<tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n<parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\nmultiple lines\n</parameter>\n</function>\n</tool_call>\n\n<IMPORTANT>\nReminder:\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\n- Required parameters MUST be specified\n- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after\n- If there is no function call available, answer the question like normal with your current knowledge and do not tell the user about function calls\n</IMPORTANT>
  [\n\nSYSTEM.trim()]
  <|im_end|>\n
- System turn without tools:
  - <|im_start|>system\n[EFFORT\n\n]SYSTEM.trim()<|im_end|>\n
  - effort only: <|im_start|>system\nEFFORT<|im_end|>\n
  - neither: no system turn
- EFFORT, present only when thinking is on:
  - xhigh: "Reasoning effort is set to xhigh. Please think carefully through the task, validate key assumptions, consider plausible alternatives, and prioritize correctness, consistency, and clarity in the final answer."
  - low: "Reasoning effort is set to low. Keep your thinking brief and focused, moving directly to the conclusion without unnecessary elaboration."
  - medium: nothing. So medium and thinking-off render the same system turn.
- User turn: <|im_start|>user\nTEXT.trim()<|im_end|>\n
- Assistant history, canonical form (used only when not replaying raw IDs, see §12):
  <|im_start|>assistant\n<think>\nR.trim()\n</think>\n\nC.trim()CALLS<|im_end|>\n
  - CALLS: for each call i: (i==0 ? (C.trim() ? "\n\n" : "") : "\n") + "<tool_call>\n<function=NAME>\n" + Σ("<parameter=K>\nVALUE\n</parameter>\n") + "</function>\n</tool_call>"
  - VALUE: a string as-is; any other type as json.dumps(v, ensure_ascii=False) with ", " / ": " separators.
  - Keep the arguments in the order the model emitted them.
  - A non-thinking turn renders as <think>\n\n</think>\n\n + content.
- Tool results: consecutive results share one turn:
  <|im_start|>user{\n<tool_response>\nOUT.trim()\n</tool_response>}+<|im_end|>\n
  There are no names or ids, so results must follow call order.
- Generation prompt: <|im_start|>assistant\n<think>\n (thinking) or <|im_start|>assistant\n<think>\n\n</think>\n\n (off).
- Exactly one leading system turn. Date, cwd, git status, diagnostics, AGENTS.md and reminders go into user turns or into tool-result text.
- Vision mode only: <|vision_start|><|image_pad|><|vision_end|> placeholders inside user or tool content.

5. DECLARATIONS
- One line per tool: json.dumps({"type":"function","function":{"name","description","parameters"}}, ensure_ascii=False), keeping insertion order and the default separators.
- parameters is plain JSON Schema (type object, properties, required). Qwen reads the raw schema, so $ref and anyOf survive, but keep schemas flat and short.
- Names must not contain "<", ">" or newlines, because they are spliced into <function=…> and <parameter=…>. mcp__server__tool names are fine.
- Property order = the order you want the model to emit (Qwen mostly follows it).
- Freeze the tool set and descriptions per session; they sit at the top of the prompt.

6. PARSER
Two levels: a state machine over token IDs, then a byte-level XML reader inside call spans.
States:
- THINK (initial state when the prompt prefilled <think>):
  - until 248069 → GAP;
  - 248058 → implicit end of thinking (a known Qwen habit) → CALL, flag A1;
  - EOG → the turn ended inside its thought, flag A2;
  - a "</think>" generated as ordinary pieces stays reasoning; only ID 248069 counts, which fixes #27661/#28679.
- GAP: keep the separator tokens (canonical 271) → CONTENT.
- CONTENT: text is the preamble or the answer.
  - 248058 → CALL.
  - 248068 → if a call was already parsed, go to TRAIL (Qwen3.8's stray re-open, #27733); otherwise treat it as a second thinking block, merge it, flag A3.
  - EOG → END.
- CALL: buffer IDs until a 248059 whose preceding decoded bytes end with "</function>" plus optional whitespace.
  - Any other 248058, 248059, 248066–248069 or 248046 inside the span is literal value text.
  - EOG or the n_predict limit inside CALL → truncated call, never executed, flag A4.
- AFTER: [198]+248058 or a bare 248058 → next CALL; EOG → END as a tool step; anything else → TRAIL (drop it, flag A5).
- TRAIL: ignore everything until EOG.

XML reader for a span S (the bytes between 248058 and its closing 248059):
- R1 header: optional whitespace, then "<function=" NAME ">" (NAME = bytes up to ">" or a newline, trimmed), then one optional newline.
  - If S.trim() starts with "{", try Hermes JSON {name, arguments} instead, flag A6.
  - Resolve NAME: exact match, then case-insensitive, then an alias table; otherwise return an error result listing the tools.
- R2 footer: BODY ends at the last "</function>". A missing footer → use the rest of S, and flag it.
- R3 boundaries: a candidate opener is "<parameter=P>" at the start of BODY or right after "\n</parameter>\n" (lenient: after "</parameter>" plus whitespace).
  - P must be a declared parameter of that tool, or any [A-Za-z0-9_.-]+ if additionalProperties is allowed.
  - Take candidates left to right. Skip a candidate whose P is already assigned: it is value text.
- R4 values: remove exactly one newline after "<parameter=P>" and exactly one before the closing "</parameter>". Everything else stays byte-exact (CR, tabs, indentation, leading and trailing blank lines). The last value ends at the end of BODY minus its "\n</parameter>\n".
- R5 ambiguity: if validation fails and a value contains "\n</parameter>\n<parameter=Q>" for a declared Q, try the alternative splits (at most 2^4). Accept only a unique valid one. Otherwise return an error asking the model to split the edit or use another tool. Never guess silently.
- R6 types follow the schema, never the text:
  - string → raw text;
  - integer / number → parse the trimmed text;
  - boolean → true / false, case-insensitive;
  - null → only when the schema allows null and is not a string;
  - object / array → JSON parse;
  - anyOf / oneOf → try the non-string branches only when the text starts with "{", "[", a digit, true, false or null.
  - A failure goes back to the model as a validation error. Unlike SGLang, the string "null" in a string parameter never becomes null.
- R7: validate against the original JSON Schema. Missing required, unknown or duplicate parameters → error result.

Turn handling:
- Execute only after END on an EOG, never on the n_predict limit.
- Run read-only calls concurrently and mutating calls sequentially, in emission order. Return results in emission order.
- Dedupe identical (name, args) pairs within a turn; the duplicate gets an error text. Cap at 8 calls per turn.
- Announce-then-stop guard: END without a call while the plan still has open steps, and either the content ends with an intent ("Now let me…", a trailing ":") or the reasoning holds an unexecuted <tool_call>. Then append the user message "Continue." (at most twice) and log it.

7. CONSTRAINED DECODING (phase 2, behind an A/B flag)
Request fields: grammar_lazy:true, grammar_triggers:[{type:0 (TOKEN), token:248058, value:"<tool_call>"}], and a grammar compiled from the tool schemas:
  root ::= tc ( "\n" tc )*
  tc ::= <[248058]> "\n<function=" ( f-Read | f-Edit | f-Write | f-Bash | ... ) "</function>\n" <[248059]>
  f-Edit ::= "Edit>\n" ( a-file_path | a-old_string | a-new_string | a-replace_all )+
  a-old_string ::= "<parameter=old_string>\n" sval "\n</parameter>\n"
  a-replace_all ::= "<parameter=replace_all>\n" ( "true" | "false" ) "\n</parameter>\n"
  sval ::= any bytes not containing "\n</parameter>\n"   (lower it to a DFA; port llama.cpp's peg until() lowering)
Typed values come from json-schema-to-grammar.
- Parameters may come in any order; uniqueness and required are checked after parsing. Don't reuse llama.cpp's built-in Qwen grammar: it forbids optional-before-required and once dropped parameters (#28522).
- Once root completes, llama.cpp allows only EOG. That removes the trailing <think> (#27733) and text after calls.
- Measure the sampler cost on 30–100 KB Write contents and the interaction with MTP drafts before enabling by default.

8. REASONING
Effort:
- Effort is a session constant: its sentence is the first text in the prompt, so a change costs a full re-prefill.
- Default: medium. Evidence:
  - VulcanBench pass@1: 82.6% at medium and low, 73.9% at xhigh (the xhigh failures were time-outs);
  - xhigh used 8× the tokens for equal quality in another test;
  - xhigh thinking binges reached 80K tokens;
  - Artificial Analysis rates xhigh "very verbose".
- xhigh only as an opt-in "deep" session (hard debugging, a planning sub-agent). Don't offer low: it behaves like medium and can be slower end to end.

Thinking:
- On for the main loop.
- Turning it off for a single step is free only at medium (same system prefix). Use it only after an A/B test: its quality in agent loops is unmeasured, and the sampling differs.

History:
- Use preserve_thinking=true semantics (the official default): replay every past reasoning block verbatim. This keeps the prompt append-only and KV reuse perfect, and Qwen recommends it for agents.
- Never send an empty reasoning block for a turn that had reasoning: empty-think histories degraded Qwen3.6 agents.
- Reasoning is dropped only at compaction, as one batched re-prefill. Those turns render with no think block at all (the official non-preserve form), never as empty blocks.

Budget, enforced by the agent: llama-server's reasoning-budget sampler cannot see the prefilled <think> on raw /completion.
- Count THINK tokens. Hard cap 16K at medium (observed p95 7.8K, p99 9.2K, max 9.6K) and 40–48K at xhigh.
- At the cap: cancel the stream, append the Qwen early-stop text "\n\nConsidering the limited time by the user, I have to give the solution based on the thinking directly now.\n" plus [248069, 271], and resume. This stays append-only, with no re-prefill.
- The "stop thinking" button in the UI does the same.

n_predict per request: 32,768 (Qwen's agentic-eval setting).

9. SAMPLING
Send every field on every request: llama.cpp takes temp, top_k and top_p from the GGUF's general.sampling.* but keeps min_p at 0.05.
- Thinking:
  - temperature 1.0, top_p 0.95, top_k 20, min_p 0.0, typical_p 1.0, top_n_sigma -1
  - presence_penalty 0, frequency_penalty 0, repeat_penalty 1.0, dry_multiplier 0, xtc_probability 0
  - random seed per request; fixed seeds for evals
- Non-thinking, per the card: temperature 0.7, top_p 0.80, top_k 20, min_p 0, presence_penalty 1.5.
  - Use presence 1.5 only for prose replies; use presence 0 for steps that may emit code or tool arguments.
  - This deviates from the card and is untested. Rationale: presence penalties damage exact old_string copies and repeated identifiers.

10. STOP CONDITIONS
- EOG IDs {248046, 248044}. Also treat 248063/248064/248065, which llama.cpp marks as EOG, as abnormal ends.
- No stop strings. Never stop on </tool_call>: the model may emit parallel calls.
- Per request:
  - n_predict 32,768, a wall-clock cap, the thinking cap (§8);
  - degeneracy guard: the same 8-gram 4 times in THINK → cancel and resample once.
- Per turn:
  - final answer = END with no calls, after the announce-then-stop guard; tool step = END with ≥1 valid call;
  - caps: about 150 steps, 3 consecutive tool errors;
  - the same call and arguments 3 times → put a warning inside that tool result, never a system message.

11. TOOLS FOR QWEN
Mirror Claude Code, the harness Qwen used for SWE-bench Pro, DeepSWE, QwenSWEBench and NL2Repo:
- Bash{command, timeout?, description?}
- Read{file_path, offset?, limit?}: output with cat -n style line numbers
- Edit{file_path, old_string, new_string, replace_all?}: exact, unique-match replacement; one edit per call; several Edit calls in one turn apply in order
- Write{file_path, content}
- Glob{pattern, path?}
- Grep{pattern, path?, glob?, output_mode?, -i?, -n?, -C?, head_limit?}
- TodoWrite{todos:[{content, status, activeForm}]}: the array is JSON inside the parameter
- Task{description, prompt}: sub-agents

No diff or patch formats. Raw XML values carry code without any escaping, which is this model's main advantage as a transport.

Results:
- plain text, trimmed, inside <tool_response>;
- Bash: an exit-code line plus head and tail truncation with a note;
- errors as plain text; Claude Code's "<\tool_use_error>…<\/tool_use_error>" wording is in-distribution;
- never JSON-wrap plain output.

Known limit: a string value cannot contain "\n</parameter>\n". The parser handles it (R5); otherwise the model is asked to split the edit. Document this for files that contain Qwen tool syntax, including our own parser tests.

12. CACHE AND CONTEXT (hybrid Gated DeltaNet rules)
Invariant: request_k = request_{k-1} prompt IDs + every generated ID, including the final EOG, + the new suffix.
- Assert it in debug builds, log n_past/tokens_cached each step, and alert on "forcing full prompt re-processing".
- Why it matters:
  - llama-server reuses only an exact prefix;
  - any earlier divergence needs a checkpoint at or before it, otherwise the whole prompt is re-processed;
  - checkpoints are made only near prompt ends, never during generation;
  - partial recurrent rollback exists only inside speculative decoding.

Replay:
- The model's IDs go back verbatim.
- Exception: turns flagged A1, A3, A5 or A6, and repaired calls, are replaced by their canonical rendering (a one-turn re-prefill). Broken structure must not stay in the prefix.

Static prefix:
- Frozen per session: effort sentence, tools, instructions, system prompt. No date or cwd in it; the tool set is fixed per session.

Prefix pin:
- Once per session, process the static prefix alone (n_predict 1) and save that slot state with /slots/{id}?action=save to NVMe (checkpoints are persisted since #26004).
- Restore it for compaction epochs and for forking sub-agents from the same prefix.
- Don't rely on in-RAM checkpoints surviving: they are evicted oldest-first.

Compaction:
- An explicit epoch with a known cost, triggered at about 90K tokens.
- Order: truncate old tool outputs → summarize old steps → drop old reasoning.
- Result: static prefix → [user: task + structured summary + plan + touched files] → the last N steps verbatim, with their reasoning.

Working context ≤ 110–120K tokens:
- #27756: instant EOS beyond ~130K tokens on CUDA and CPU, closed as not planned;
- decode drops about 40% between 2K and 120K.

Server flags for the 24 GB RTX 5090 Laptop with Q4_0:
- base: -ngl 99 -fa on -c 131072 -ctk q8_0 -ctv q8_0 -ub 1024 -b 2048 -np 1
- --ctx-checkpoints 16: each checkpoint costs 149.6 MiB of host RAM
- --cache-ram 12288: never -1 (#29324)
- --slot-save-path: required for the prefix pin and session resume
- no context shift; no mmproj unless vision is needed
- no KV quantization below q8_0: #27109 (prefill collapse), #28455 (silent CPU attention)
- sub-agents: -np 2–3 with --kv-unified, each slot +150 MiB of state; turn --cache-idle-slots off if slots serve different users (#27148)

VRAM, estimated total about 20.5–21.5 GiB:
- weights on the GPU: 14.03 GiB (token_embd, 0.67 GiB, stays in RAM; MTP adds 0.25 GiB)
- KV q8_0: about 34 KiB/token, so 131K = 4.25 GiB; f16 is 64 KiB/token, so 96K = 6 GiB
- recurrent state: 0.15 GiB per slot (×(1+n_max) with MTP)
- compute buffers: 1–2 GiB

Never offload Gated DeltaNet layers to the CPU backend: it produces garbage on qwen35 (#30906, open).

13. SPECULATIVE DECODING
- Off by default for unattended loops.
- Candidate: --spec-type draft-mtp --spec-draft-n-max 2–3, using the GGUF's own nextn head (blk.64 is in the owner's Q4_0); alternatively -md with the 1.37 GB sidecar.
- Expected gain about +30–60% (desktop: 69.5 → 90.6 tok/s at n=3; community 5090 Laptop "Q4-MTP" about 48 tok/s).
- Enable per session only after a soak test on the pin: a 2-hour replay of a recorded agent session, with steady tok/s, acceptance ≥ 0.5, f_keep = 1.000 every step, and checkpoint size not growing by about 4 KiB per token.
- Why the soak test:
  - August builds decayed to about 0.5 tok/s;
  - #30782: 0% acceptance after an update;
  - #28049: previous answer lost (fixed);
  - #30502: checkpoint bloat;
  - #25618: greedy divergence.
- Next to evaluate: the DFlash2 sidecar (ggml-org dflash-Qwen3.8-27B-Q4_0, --spec-type draft-dflash --spec-draft-n-max 7).

14. GOLDEN TESTS
a) Renderer vs the official template under Python Jinja2 (trim_blocks, lstrip_blocks, tojson ensure_ascii=False). Byte-identical across: tools or none × xhigh/medium/low × thinking on/off × preserve on/off × parallel calls × empty preamble × multiple results × images.
b) Token parity with /tokenize on plain-text corpora. Content-safety fixtures: literal <tool_call>, </think>, <|im_end|> and <tool_response> in files must become ordinary pieces.
c) Parser corpus:
- canonical form
- call inside thinking
- trailing <think>
- text after a call
- Hermes JSON
- missing </function> or missing newlines
- inline "</parameter>" in a value
- an ambiguous "\n</parameter>\n<parameter=new_string>\n" inside old_string
- a literal </tool_call> inside a value
- typed values (int, bool, array, null)
- duplicate and unknown parameters
- truncated by the limit
- three parallel calls
- an emoji split across tokens
d) Round trip render(parse(x)) == x on canonical outputs; measure how often the model emits non-canonical whitespace (500 steps).
e) Prefix stability over 50-step sessions: tokens_cached == previous prompt + generated tokens.
f) Agent A/B: medium vs xhigh, thinking toggle, grammar on/off, MTP on/off, Q4_0 vs UD-Q4_K_XL.

## Risks and gotchas
- The default reasoning effort is xhigh: about 8× the tokens of medium for equal agentic quality, with binges up to 80K tokens, and Artificial Analysis rates it 'very verbose'. On a ~35 tok/s laptop that means minutes per step. Pin effort to medium explicitly; never rely on the template default.
- Prefix divergence on the hybrid model is expensive. llama-server restores the nearest checkpoint, which exists only at the ends of earlier prompts, or re-processes everything. One changed byte inside an earlier assistant turn re-prefills that whole turn, and possibly the whole prompt. The chat endpoint causes this at random through whitespace normalization (#30357).
- Content injection. <tool_call>, </tool_call>, <tool_response>, </tool_response>, <think> and </think> are USER_DEFINED tokens and are matched even with parse_special=false. A file or tool output containing them can fake call boundaries, close thinking, or end a tool response. On the chat path even <|im_end|>/<|im_start|> are injected, because the server tokenizes the whole prompt with parse_special=true.
- String-based parsers end reasoning at a literal '</think>' inside the thought (#27661, #28679; won't fix upstream). Only token-ID parsing (248069) gets this right.
- The XML format has no escaping. A value cannot safely contain '\n</parameter>\n'. Reference parsers truncate at the first '</parameter>' (SGLang) or '\n</parameter>\n' (llama.cpp). This bites exactly when the agent edits code or tests that contain Qwen tool syntax, including this project's own parser.
- Model behaviours to design for:
- tool calls inside the still-open thinking block;
- 'announce, then stop' turns with no call (Qwen3.6 #150, reported again for 3.8);
- a stray <think> after complete calls (#27733);
- text after a call, despite the template forbidding it;
- Hermes-JSON calls;
- duplicate calls.
- Official template exceptions (HTTP 500 on the chat path):
- reasoning_effort 'high' or any other unknown value;
- a system or developer message anywhere but first;
- no user query in the history;
- arguments passed as a JSON string (HF path).
Clients like Codex, Claude Code hooks and opencode plugins trigger these.
- Clients that drop reasoning poison the history. Because preserve_thinking now defaults to on, a client that does not send reasoning_content back makes the template render '<think>\n\n</think>\n\n' for every past turn. With Qwen3.6, such empty-think histories caused premature stops and ignored tools.
- Long-context reliability on llama.cpp: silent instant EOS once the prompt exceeds ~130K positions (#27756, closed as not planned). Decode also slows about 40% by 120K. Don't trust the advertised 262K/1M on this stack.
- MTP fragility on llama.cpp:
- decode degraded to ~0.5 tok/s after ~10 minutes of agent churn on August builds;
- 0% acceptance after updates (#30782);
- checkpoint size grows ~4 KiB/token (#30502);
- previous answer lost to tokens accepted past the EOG (#28049);
- greedy divergence on quantized targets (#25618);
- prefill slowdowns on some builds.
- Host RAM:
- every checkpoint is 149.6 MiB (32 by default = 4.7 GiB per slot);
- --cache-ram -1 is unbounded and every saved entry carries ~150-300 MiB (#29324);
- under concurrency the RAM prompt cache can resurrect an unrelated conversation in a new slot (#27148, open), which is a privacy risk for multi-user deployments.
- The server reasoning budget does nothing on raw /completion. Qwen prefills '<think>\n' in the prompt, and the budget sampler only sees a start tag through generation_prompt, which is set only by the chat path. Budgets must live in the agent.
- Silent sampling drift: llama.cpp takes temp, top_k and top_p from GGUF metadata but keeps min_p 0.05; the card wants 0. The card's non-thinking preset includes presence_penalty 1.5, which can corrupt exact code copies (old_string) and repeated identifiers.
- Changing reasoning effort, the tool set, tool descriptions or the system prompt changes tokens at the very start of the prompt. The cost is a full re-prefill: ~20-60 s at 30-60K tokens on the laptop.
- KV quantization below q8_0: q4_0/q4_1 KV collapsed prefill on qwen35 (#27109), and q4/q5 with flash attention silently ran attention on the CPU without GGML_CUDA_FA_ALL_QUANTS (#28455).
- The CPU backend produces repetition garbage for qwen35 (#30906, open). That matters for any partial CPU offload of Gated DeltaNet layers and for the owner's CPU-first engine roadmap: build a reference test for the recurrent path early.
- Streaming without PR #31244 loses token IDs. This tokenizer splits emoji, box-drawing and rare CJK characters into incomplete-UTF-8 tokens (🚀 = 3 tokens, └ = 2), so verbatim replay and argument bytes break silently.
- llama.cpp's own Qwen grammar has had bugs:
- WORD-trigger regex cost O(n²) with ~139 tools (#27615);
- dropped parameters for a tool with 48 optional parameters (#28522);
- an ordering restriction: optional parameters only after required ones.
Owning the grammar avoids all three.
- Quant files move: unsloth deleted and re-uploaded quants on 2026-08-19. Pin model files by sha256 (owner's file: ede16c7b…). Note that unsloth's 'Q4_0' MTP sidecar is actually Q6_K/Q4_K/Q3_K.
- Vision is a separate, less safe path. Loading mmproj (0.93 GB) disables cache reuse and forces string prompts with media markers, so token-array injection protection is lost. Keep vision optional.
- The 248K vocabulary costs:
- each logits row is ~1 MB in f32 (n_probs and full-vocab grammar fallbacks are expensive);
- Russian runs ~2-3 characters per token, so Russian conversations use up context faster than English.
- Benchmark caution: vendor numbers use a tuned Claude Code harness (Terminal-Bench 2.1 73.0 vs vals 58.4 on Terminus 2). The independent SWE-bench Verified 86% used about 2M tokens and 10 minutes per task with a 1M-context host. Expect far lower budgets and success on a 128K, 35 tok/s laptop.

## Questions for owner
- Default reasoning effort for the main loop: medium (recommended; there is no system sentence, so thinking can be toggled per step at no cache cost), or xhigh (Qwen's default, about 8× the tokens, minutes per step on the laptop)? Should xhigh be a per-session 'deep' switch?
- Tool surface for Qwen: mirror Claude Code's tool names and parameters exactly (Bash/Read/Edit/Write/Glob/Grep/TodoWrite/Task, which is closest to Qwen's evaluation harness), or keep our own canonical names and project them per model?
- May the agent turn thinking off for individual mechanical steps (free at medium, unmeasured quality), or should thinking stay on for every agent step?
- MTP: keep it off by default until a 2-hour soak test passes on the pinned build, or enable it by default for the +30-60% decode gain and accept the reported fragility?
- Context plan on 24 GB: 131K with q8_0 KV (recommended, about 21 GiB), or 96K with f16 KV? Is compaction at about 90K tokens, plus a hard working limit around 110-120K (llama.cpp's >130K instant-EOS reports), acceptable?
- Should grammar-constrained tool calls (our own GBNF, lazy on token 248058) be on by default after A/B testing, or should the tolerant parser alone be the default?
- Is vision (Qwen3.8's mmproj, 0.93 GB, screenshots in tool results) needed in v1? It costs VRAM and forces the less safe string prompt path.
- Model file: keep unsloth Q4_0 (16.06 GB, fastest, fits 131K), or also qualify UD-Q4_K_XL (17.56 GB, likely better quality, still fits 131K q8_0)?
- Should system prompts and tool descriptions stay in English (the training distribution) while user turns are in Russian, given that Russian costs ~2-3 characters per token?

## Unknowns
- How Qwen trained on literal tag strings inside tool outputs: as special IDs (what HF produces) or as pieces. The quality effect of our 'pieces only' content rule is unmeasured.
- How often Qwen3.8 emits non-canonical whitespace (cases A-D in #30357). This sets how often a client re-rendering canonically would miss the cache. Verbatim replay sidesteps it, but the rate is unknown.
- The current rates of tool calls inside thinking, announce-then-stop turns and trailing <think> for Qwen3.8 under our exact rendering. Community reports are anecdotal and mostly from modified templates.
- Real throughput and prefill on the owner's RTX 5090 Laptop with Q4_0 at the pin. All laptop numbers here are estimates scaled from a 3090, a desktop 5090 and community laptop runs.
- Whether the MTP decode degradation under long agent sessions (August builds) and the draft-KV checkpoint bloat (#30502; the pin still calls update_dft unconditionally) persist at 5e03a1ff4 + PR #31244.
- Whether the instant EOS beyond ~130K positions (#27756) reproduces at the pin with the unsloth Q4_0 file and q8_0 KV.
- Quality of non-thinking steps inside a thinking session, and the effect of mixed histories that contain both empty and full think blocks.
- Whether the Qwen3 early-stop budget text works as well for Qwen3.8, and which hard thinking caps preserve quality on real agent tasks.
- There are no official tau2 or BFCL scores. The only BFCL numbers are community category scores from a different engine and quantization (NInfer NVFP4).
- How lazy-grammar constrained decoding interacts with MTP drafts and with very long string arguments (30-100 KB Write contents) in llama.cpp, in both speed and correctness.
- Whether Qwen3.8 emits optional parameters before required ones or reorders parameters, which would break llama.cpp's built-in grammar but not ours.
- The effect of q8_0 KV quantization and of Q4_0 vs UD-Q4_K_XL weights on tool-call fidelity for this hybrid model.
- Whether in-RAM context checkpoints, and the slot save/restore path (#26004 vs the still-open #28194), reliably give exact-prefix reuse for qwen35 after restore at the pin.

## Files written
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dump_meta.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/tensors.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/render.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/html_tables.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/main.chat_template.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/mtp.chat_template.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/main.special_tokens.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/s1.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/s2.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/s3.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/official_chat_template.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/official_README.md
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/official_config.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/official_generation_config.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/official_tokenizer_config.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/qwen36_27b_chat_template.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/Qwen3.8-Flash-Next.README.md
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/Qwen3.8-2.4T-A95B.README.md
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/Qwen3.8-2.4T-A95B.chat_template.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/froggeric_README.md
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/dl/unsloth_README.md
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/src/sglang_qwen3_coder_detector.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/gh/issue_30357.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/gh/issue_27733.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/q38/gh/issue_27756.json
