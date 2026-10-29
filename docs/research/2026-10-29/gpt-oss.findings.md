# gpt-oss findings

- [high] Harmony special tokens (o200k_harmony): <|start|>=200006, <|end|>=200007, <|message|>=200008, <|channel|>=200005, <|constrain|>=200003, <|return|>=200002, <|call|>=200012. Other special tokens are <|startoftext|>=199998 (BOS, not used by Harmony), <|endoftext|>=199999 and <|endofprompt|>=200018; IDs 200014–201088 are reserved. The owner's GGUF has the same IDs and eos=200002. The vocabulary has 201088 tokens.
  evidence: https://cookbook.openai.com/articles/openai-harmony (Special Tokens table); /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/tokenizer_config.json; GGUF metadata parsed from .../gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf (tokenizer.ggml.eos_token_id=200002, bos 199998); harmony src/tiktoken_ext/public_encodings.rs special_tokens()
- [high] Message grammar: <|start|>{role}[ to={recipient}]<|channel|>{channel}[ <|constrain|>{type}]<|message|>{content}<|end|>. The recipient may sit in the role header or the channel header. Generation stops at <|call|> (tool call) or <|return|> (done). <|return|> is a decode-time stop token only and must be rewritten to <|end|> in stored history, while <|call|> stays. Tool results use the form <|start|>functions.NAME to=assistant<|channel|>commentary<|message|>{raw output}<|end|>.
  evidence: https://cookbook.openai.com/articles/openai-harmony (Message format, Implementation note, Receiving/Handling tool calls); harmony test-data/test_does_not_drop_if_ongoing_analysis.txt
- [high] Roles and instruction hierarchy: system > developer > user > assistant > tool. The system message carries the identity line (keep 'You are ChatGPT, a large language model trained by OpenAI.'; set the agent's identity in developer instructions instead), 'Knowledge cutoff: 2024-06', 'Current date: YYYY-MM-DD', 'Reasoning: low|medium|high', built-in tools under '# Tools' (browser, python), '# Valid channels: analysis, commentary, final. Channel must be included for every message.' and, when functions exist, 'Calls to these tools must go to the commentary channel: 'functions'.'. The developer message holds '# Instructions', then '# Tools' with namespace functions, then an optional '# Response Formats'.
  evidence: https://cookbook.openai.com/articles/openai-harmony (System/Developer message format); local chat_template.jinja:196-255 (/run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/chat_template.jinja); harmony src/chat.rs SystemContent::default (identity, effort Medium, cutoff 2024-06)
- [high] Channels: analysis holds the raw CoT (not trained to final-answer safety standards; OpenAI says not to show it to end users). Commentary holds function calls and optional user-visible preambles, for example an action plan before several calls. Final holds the answer. Built-in tool calls (browser.search/open/find, python) normally go to the analysis channel and sometimes to commentary.
  evidence: https://cookbook.openai.com/articles/openai-harmony (Channels, Preambles, Built-in tools); https://cookbook.openai.com/articles/gpt-oss/handle-raw-cot
- [high] Tool declarations use a TypeScript-like 'namespace functions {' block. Each description line becomes a '//' comment, functions are written 'type name = (_: { ...fields... }) => any;' or 'type name = () => any;' when there are no arguments, the return type is always any, a blank line follows each function, and the block ends with '} // namespace functions'. The reference renderer comments every line of multi-line descriptions and handles oneOf, nullable, enum, defaults and arrays (Array<any> when items is missing).
  evidence: https://cookbook.openai.com/articles/openai-harmony (Defining available tools); harmony src/encoding.rs:400-790 (json_schema_to_typescript, template_tools_section); harmony test-data/test_render_functions_with_parameters.txt
- [high] The official HF chat template deviates from the reference renderer in eight ways. llama.cpp's bundled models/templates/openai-gpt-oss-120b.jinja is byte-identical to it. Verified by rendering locally with Jinja2 3.1.6:
(a) tool outputs are JSON-quoted through message.content|tojson, producing a quoted string with escaped newlines instead of raw text;
(b) tool calls are rendered '<|channel|>commentary json<|message|>' without the <|constrain|> token;
(c) only the first line of a multi-line description gets '//';
(d) a non-object parameter schema (for example apply_patch with type string) is rendered as '() => any';
(e) nested objects are rendered with stray newlines and indentation;
(f) a preamble passed as content on a tool-call message is rendered as analysis;
(g) only tool_calls[0] is rendered;
(h) Current date comes from strftime_now on every request.
  evidence: local chat_template.jinja:113-147, 285-299, 317-322, 202; diff against /home/kristian/Рабочий стол/llama.cpp/models/templates/openai-gpt-oss-120b.jinja is empty; render scripts /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/tools/render_tmpl.py and render_tmpl2.py; https://github.com/openai/harmony/issues/91 (2026-01-15)
- [high] The Unsloth template embedded in the owner's gpt-oss-20b-F16.gguf has a bug. Any non-last assistant message without tool calls that has a 'thinking' key is rendered as '<|channel|>analysis<|message|>' + message.content, so the previous final answer becomes CoT and the final message disappears. llama.cpp copies every string reasoning_content into 'thinking', so this triggers whenever a client sends reasoning back on final turns, even an empty string. Unsloth's other changes: raw (non-tojson) tool outputs when the content is a string, string args passed through unchanged, and support for a developer_instructions kwarg.
  evidence: extracted template /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/gguf_template.jinja:317-320 (from gpt-oss-20b-F16.gguf); rendered demo with render_tmpl.py 'after_final'; /home/kristian/Рабочий стол/llama.cpp/common/parsers/gpt-oss.cpp:7-17
- [high] OpenAI's CoT retention rule:
(1) CoT is emitted on the analysis channel;
(2) after a message to final, all earlier analysis messages are dropped in later sampling, while function calls on commentary may remain;
(3) if the last assistant message was a tool call, analysis messages back to the previous final are kept until a new final is issued.
For Chat Completions, OpenAI asks for a 'reasoning' field (OpenRouter convention); for the Responses API, reasoning items carry reasoning_text content.
  evidence: https://cookbook.openai.com/articles/gpt-oss/handle-raw-cot; https://cookbook.openai.com/articles/openai-harmony (Handling reasoning output in subsequent sampling); https://cookbook.openai.com/articles/gpt-oss/verifying-implementations
- [high] Dropping CoT inside a tool loop has measured consequences:
- llama.cpp #27720 (closed 2026-08-26): gpt-oss-20b MXFP4 at temperature 1.0 produced malformed Harmony headers and runs of '?' in 7 of 120 turns when reasoning_content was stripped from tool-call turns, and 0 of 120 when it was sent back.
- llama.cpp #20302 (2026-03): infinite agentic loops in the WebUI with MCP and with Claude Code, because every reasoning block restarted with 'The user asks...'.
- Codex #2461 (2025-09-02): 'otherwise the model forgets why the tools were called'.
- Community Codex guide: quality degrades significantly after 5+ tool turns without CoT passback.
  evidence: https://github.com/ggml-org/llama.cpp/issues/27720 (comments 2026-08-25/26); https://github.com/ggml-org/llama.cpp/issues/20302; Codex commit b127a3643 (PR #2461, 2025-09-02); https://github.com/ivanopcode/gpt-oss-local-codex-guide
- [high] The harmony library's auto_drop_analysis (RenderConversationConfig, default true) drops analysis only before the FIRST final message, and only when the last assistant message is final. It deliberately keeps analysis between later finals (test_keep_analysis_between_final_messages), so it does not implement the multi-turn rule; an agent must implement the 'keep only since the most recent final' rule itself. The HF template behaves equivalently to that rule: a tool-call message's analysis is dropped if any later non-tool-call assistant message exists, and a final message's own thinking is never rendered.
  evidence: harmony src/encoding.rs:164-224 and 1592-1604; tests/test_harmony.py test_keep_analysis_between_final_messages; local chat_template.jinja:273-316
- [high] The openai-harmony Rust crate is version 0.0.8 on crates.io (Apache-2.0, published 2025-11-05, ~2.38M downloads). The repository has 4.5k stars and 73 open issues; the only commits since are CI pins (last push 2026-04-08). API:
- load_harmony_encoding(HarmonyEncodingName::HarmonyGptOss);
- render_conversation_for_completion(&convo, Role::Assistant, Option<&RenderConversationConfig>), plus render_conversation, render_conversation_for_training and render(&Message);
- parse_messages_from_completion_tokens(tokens, Some(Role::Assistant)) and parse_messages_from_completion_tokens_with_options with ParseOptions{strict};
- stop_tokens() = {return, call, end} and stop_tokens_for_assistant_actions() = {return, call};
- StreamableParser::new(enc, Some(Role::Assistant)) with process(u32), process_eos(), current_channel(), current_recipient(), current_content_type(), last_content_delta(), messages(), into_messages() and state_json();
- tokenizer() returning CoreBPE (encode_ordinary, encode_with_special_tokens, decode_utf8).
Message content is encoded with encode_ordinary, so it is safe from special-token injection.
  evidence: https://crates.io/crates/openai-harmony; https://docs.rs/openai-harmony/latest/openai_harmony/; gh api repos/openai/harmony (2026-10-29); source clone at /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/src-harmony/harmony (src/registry.rs, src/encoding.rs:132-160, 374-396, 1041-1110)
- [high] Caveats for openai-harmony 0.0.8:
- It downloads o200k_base.tiktoken at load time from openaipublic.blob.core.windows.net (override with TIKTOKEN_ENCODINGS_BASE; cache dir TIKTOKEN_RS_CACHE_DIR; sha256 checked) and pulls in reqwest, image and clap.
- It renders tool calls recipient-first ('assistant to=functions.x<|channel|>commentary'), while the model emits channel-first ('<|channel|>commentary to=functions.x <|constrain|>json'; harmony #27), so re-rendering changes tokens and breaks KV-cache reuse.
- <|constrain|> becomes a special token only if content_type starts exactly with '<|constrain|>'.
- The strict parser bails with errors such as 'Unexpected token X while expecting start token 200006' (#38/#48/#60, vLLM #22519).
- About 1.3M repeated characters overflow the stack (#93).
  evidence: harmony src/tiktoken_ext/public_encodings.rs:56-140, 417-485; src/encoding.rs:842-873; https://github.com/openai/harmony/issues/27, /53, /91, /93, /101; https://github.com/vllm-project/vllm/issues/22519
- [medium] The oss-harmony fork (github.com/oss-harmony/harmony; v0.0.11 released 2026-05-14; commits as recent as 2026-10-29) keeps the Rust lib name openai_harmony under package name oss-harmony. It is a git dependency only, not on crates.io. Changes:
- embeds the tiktoken vocabulary (zstd) and drops reqwest, image, clap and regex;
- parses both recipient-first and channel-first headers, including <|constrain|> next to to=;
- renders channel-first headers with a real <|constrain|> token;
- strips duplicated channel markers.
vLLM's Rust path pins it (#46799), and a switch of vLLM's Python dependency was requested (#54615).
  evidence: gh api repos/oss-harmony/harmony (+commits, Cargo.toml, README); https://github.com/vllm-project/vllm/issues/54615 (2026-08-31)
- [high] llama-server supports a token-level client path:
- POST /completion accepts the prompt as a token-ID array or a mixed array of IDs and strings; no BOS is added for arrays.
- Each SSE chunk carries 'tokens':[id] plus 'content'.
- Request fields: return_tokens, cache_prompt, id_slot, n_predict, samplers, logit_bias, grammar, grammar_lazy, grammar_triggers, preserved_tokens, reasoning_budget_tokens/_start_tag/_end_tags/_message, return_progress, timings_per_token.
- Endpoints: /tokenize (parse_special, with_pieces), /detokenize, /apply-template, and /slots/{id}?action=save|restore (with --slot-save-path).
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:494-663, 680-760, 1151-1200, 226; tools/server/server-task.cpp:1041-1051; tools/server/server-schema.cpp (field list)
- [high] llama.cpp special-cases the gpt-oss vocabulary. <|channel|>, <|message|>, <|start|> and <|constrain|> are forced to USER_DEFINED, so they always appear as text in content. <|end|> is removed from the end-of-generation (EOG) list when <|return|> and <|call|> exist, so generation runs across message boundaries and stops only at <|call|> or <|return|>.
  evidence: /home/kristian/Рабочий стол/llama.cpp/src/llama-vocab.cpp:3073-3083 and 3101-3143
- [medium] In /v1/chat/completions, llama-server flattens the rendered prompt to one string and tokenizes it with parse_special=true. Literal control strings such as '<|end|>' or '<|start|>assistant<|channel|>final<|message|>' inside user messages, tool outputs or file contents are therefore very likely tokenized as real control tokens, which allows format injection. The Jinja engine marks user-input strings (is_input), but that marking appears not to be used on this tokenization path.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1437; tools/server/server-context.cpp:4926; common/jinja/string.h:11-20 (code reading, not executed)
- [high] The llama.cpp gpt-oss output parser at 5e03a1ff4:
- accepts a tool call with the recipient in either the role or the channel header, on commentary or analysis, with an optional ' <|constrain|>TYPE';
- tolerates exactly one stray-header shape ('<|channel|>commentary[ to=assistant]' prefix);
- allows one tool call per response;
- returns unsolicited or built-in calls (container.exec, python) as empty content (#21213);
- uses a lazy grammar triggered on ' to=functions';
- maps a preamble to content.
Any other malformed header makes the whole stream fail with 'does not match the expected peg-native format', discarding a valid tool call.
  evidence: /home/kristian/Рабочий стол/llama.cpp/common/parsers/gpt-oss.cpp:68-149; tests/test-chat.cpp:6708-6760; https://github.com/ggml-org/llama.cpp/issues/27720; llama.cpp commit for #21213 (2026-03-31)
- [high] Chat-path bugs relevant to the local checkout:
- #30781 (open, 2026-10-18): an array parameter without 'items' returns HTTP 500 'Function is not a bool value' with the gpt-oss template; common in MCP schemas.
- #30377 (open, 2026-10-12): tools combined with response_format makes the model loop in analysis until the token budget runs out.
- Fixed earlier: #19520/#19701/#19703/#20500 (double escaping, 'Cannot pass both content and thinking') and #25967 (duplicate GBNF rules with large tool lists, fixed 2026-09-05).
- #30787 (GPT-4o pre-tokenizer splitting combining marks) was fixed 2026-10-20, after the local checkout 5e03a1ff4 of 2026-10-16.
- The gpt-oss handler deletes 'content' when reasoning_content and tool_calls are both present, so preambles are lost from history.
  evidence: https://github.com/ggml-org/llama.cpp/issues/30781, /30377, /19520, /20500, /25967, /30787; common/parsers/gpt-oss.cpp:10-14
- [high] Reasoning controls in llama-server:
- the OpenAI 'reasoning_effort' field is passed to the template as a kwarg ('none' disables thinking, but gpt-oss has no no-reasoning mode);
- --reasoning-effort and --chat-template-kwargs '{"reasoning_effort":"high"}';
- --reasoning-budget N with --reasoning-budget-message, which forces the end tag <|end|> for gpt-oss (thinking start tag '<|channel|>analysis<|message|>');
- 'reasoning_control': true combined with POST /v1/chat/completions/control {id, action: 'reasoning_end'} for a live 'stop thinking'.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:236-237, 1349-1355, 1472-1482; tools/server/server-common.cpp:1419-1452; common/arg.cpp:3728-3739; common/parsers/gpt-oss.cpp:43-44
- [medium] Architecture: 21B total / 3.6B active parameters, 32 experts with top-4 routing, 24 layers alternating sliding-window (128) and full attention, 8 KV heads of dim 64, context 131072 via YaRN (factor 32 from 4096), MXFP4 MoE weights, Apache-2.0.
KV-cache estimate (calculated, not measured): about 24 KiB per token f16 for the 12 full-attention layers, so about 3 GiB per 131k-token slot (about 6 GiB with --swa-full).
When a new prompt diverges in the middle (for example after dropping the previous turn's CoT), llama-server must restore a context checkpoint (default 32 per slot) or rely on --swa-full; otherwise it logs 'forcing full prompt re-processing'.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/config.json; GGUF metadata (head_count_kv=8, key_length=64, block_count=24, sliding_window=128); llama.cpp tools/server/server-context.cpp:3844-3890; README.md:55, 166
- [medium] The model has trained-in tool habits (reverse-engineered). With no tools defined, gpt-oss calls container.exec (4.8%), repo_browser.print_tree (3.4%) and repo_browser.apply_patch (1.2%). Canonical schemas: container.exec {command: string[]}, print_tree {path, depth}, search {path, query, max_results?}, open_file {path, line_start?, line_end?}, apply_patch {patch}. Aliases are list_files/list_dir, find and read_file. Declaring these tools in the system message raised call rates (print_tree from 29% to 99%); declarations in the developer message were reported as less reliable. Other reports show the same priors: Continue #10711 ('repo_browser.open_file({...})' emitted as text), Agents SDK #686 (repo_browser.create_file), Codex discussion #4686 (print_tree), smolagents #1860 and llama.cpp #21213 (container.exec).
  evidence: https://arxiv.org/abs/2604.00362 (2026-04-01) Tables 2-5; https://github.com/borislavmavrin/harmonyagent src/harmonyagent/agents/tools/{container_exec.py, repo_browser.py}; https://github.com/continuedev/continue/issues/10711; https://github.com/huggingface/smolagents/issues/1860
- [medium] The native-Harmony harness (harmonyagent) reproduced OpenAI's published scores:
- SWE-bench Verified: 60.4% high (published 60.7) and 53.3% medium (published 53.2);
- AIME25 with tools: 91.7% (published 90.4).
Setup: vLLM 0.14.1 raw /v1/completions; temperature 1.0, top_p 1.0, top_k 0, min_p 0; stop tokens <|return|> and <|call|>; max_new_tokens 65536; context 131070; auto_drop_analysis=False in a single-turn task; up to 10 retries on malformed output. Tool outputs were plain text with return code and 10k-character truncation. High effort sometimes overflowed the 128k context. On the official SWE-bench leaderboard, gpt-oss-120b logs show 860 calls to an undefined apply_patch tool.
  evidence: https://arxiv.org/html/2604.00362; harmonyagent config/swebench_harmony.yaml, swebench_harmony_high.yaml, models/vllm_raw.py:35-58, agents/harmony_agent.py:120-160, 214-222
- [high] Model card (2025-08-05), gpt-oss-20b at low / medium / high effort:
- SWE-bench Verified (n=477, with a Codex-CLI-like exec tool): 37.4 / 53.2 / 60.7
- tau-bench Retail: 35.0 / 47.3 / 54.8
- tau-bench Airline: 32.0 / 42.6 / 38.0
- Aider Polyglot: 16.6 / 26.6 / 34.2
- Codeforces Elo with tools: 1251 / 2064 / 2516
The 20b model uses over 20k CoT tokens per AIME problem on average at high effort.
  evidence: https://arxiv.org/abs/2508.10925 (Table 3, sections 2.5.3 and 2.6)
- [medium] Generic or non-native harness results:
- SWE-bench Verified bash-only (mini-SWE-agent v1.7, actions parsed from text, temperature 0): gpt-oss-120b 26.0% (2025-08-07); 20b is not listed. Users report about 15% for 120b with 157 empty patches (mini-swe-agent #714).
- Terminal-Bench 2.0 leaderboard, gpt-oss-20b: 3.1% with Terminus 2 and 3.4% with mini-SWE-agent (effort not stated).
- AfterQuery's SFT+RL fine-tune of 20b reached 17.0% with Terminus 2 (temperature 1.0, 32K context, 2026-03-31).
  evidence: https://www.benchleader.com/benchmarks/swe_bench_verified (swebench.com data); https://github.com/SWE-agent/mini-swe-agent/issues/714; https://aimap.page/en/benchmarks/model/gpt-oss-20b/ (Epoch AI data); https://www.afterquery.com/blog/how-we-improved-terminal-bench-2-with-tinker-and-harbor
- [medium] BFCL: gpt-oss does not appear in the official BFCL v4 leaderboard snapshots up to 2025-12-16. vLLM CI's BFCL live_simple score for gpt-oss-20b is 81.4% via Chat Completions and 82.2% via the Responses API (2026-03-09).
  evidence: https://raw.githubusercontent.com/HuanzhiMao/BFCL-Result/main/2025-12-16/score/data_overall.csv (no gpt-oss rows); https://github.com/vllm-project/vllm/pull/36560
- [high] Codex history with gpt-oss:
- --oss was added 2025-08-05 (#1848; Ollama gpt-oss:20b by default).
- By September 2025, gpt-oss had its own Codex model family (ApplyPatchToolType::Function). apply_patch was a JSON tool {input: string} whose long V4A description said 'Should only be used with gpt-oss models'. The shell tool was {command: string[], workdir?, timeout_ms?}, update_plan existed, and reasoning was passed back over Chat Completions (#2461). #2811 notes gpt-oss 'will often try to call the apply_patch tool directly instead of via the shell'.
- Codex HEAD (2026-10-29) has no gpt-oss-specific metadata. The fallback profile uses exec_command plus write_stdin, apply_patch intercepted from shell commands, 10,000-byte output truncation, and a base prompt that asks for preambles and update_plan. Only the Responses wire API remains (chat was removed); the OSS providers are Ollama on 11434 and LM Studio on 1234.
  evidence: Codex commits 928535084 (2025-08-05), b127a3643 (2025-09-02), de6559f2a (2025-09-09) incl. codex-rs/core/src/model_family.rs, tool_apply_patch.rs, openai_tools.rs at that rev; HEAD clone: codex-rs/models-manager/src/model_info.rs:98-150, models-manager/prompt.md:31-60,132, model-provider-info/src/lib.rs:108,767-771, core/src/tools/handlers/apply_patch.rs:423-500
- [high] The gpt-oss reference repository provides:
- a browser tool (search/open/find with citation format);
- a stateless docker python tool (the model was trained with a stateful Jupyter tool and has /mnt/data and 120 s timeout wording);
- apply_patch (apply_patch.md specifies the V4A envelope). chat.py declares functions.apply_patch with parameters {type: string} and accepts either raw patch text or a JSON object whose single value is taken.
The Responses API server reconstructs Harmony from Responses items: reasoning is included only after the last assistant message and only when the last input is a function_call_output; tool outputs are raw on the call's channel; the Ollama backend uses /api/generate in raw mode. Recommended sampling: temperature=1.0, top_p=1.0.
  evidence: /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/src-gptoss/gpt-oss: README.md:376-576, gpt_oss/tools/apply_patch.md, gpt_oss/chat.py:105-125,193-225, gpt_oss/responses_api/api_server.py:1225-1320, responses_api/inference/ollama.py:64-95 (HEAD 2026-10-12)
- [high] No parallel tool calls: generation stops at <|call|>, so each sampling produces at most one call. The HF template assumes a single call per message, and Ollama confirms gpt-oss has no parallel calling. Batch work must be expressed through tools that take several inputs.
  evidence: local chat_template.jinja:283-285; https://github.com/ollama/ollama/issues/12159; llama.cpp common/parsers/gpt-oss.cpp:128-132
- [high] OpenAI's verification guidance: run compatibility-test from github.com/openai/gpt-oss (Agents SDK; 30 single-turn cases over 6 tools; use -k 5). Pass criteria are 0 invalid requests and more than 90% on both pass@k and pass^k. Then run gpt_oss.evals (AIME 16 attempts, GPQA 8, HealthBench 1) through the chat_completions or responses sampler, or check Artificial Analysis provider evals. The suite is a smoke test only: it has no multi-turn or CoT-retention coverage.
  evidence: https://cookbook.openai.com/articles/gpt-oss/verifying-implementations; gpt-oss/compatibility-test/README.md, cases.jsonl, analysis.ts
- [high] Prompt-defined XML tool protocols (classic Cline and Roo) fail with gpt-oss-20b: 'gpt-oss-20b is adamant about producing a native tool call when told it has tools'. 120b works most of the time. The workaround was a grammar that forces the final channel. Native Harmony tool calls are therefore mandatory.
  evidence: https://github.com/ggml-org/llama.cpp/discussions/15396 (aldehir, 2025-08-19); https://alde.dev/blog/gpt-oss-20b-with-cline-and-roo-code/; https://github.com/cline/cline/issues/5393, /5398
- [medium] Argument and recipient failure modes seen in the wild:
- Ollama #17638 (2026-08-09): with an apply_patch-style single-string tool, a previous tool result of about 1.6 KB and a long tool description, about 2 in 5 requests produced array-wrapped args [{"input": ...}], and Ollama returned HTTP 500.
- Ollama #12884: trailing characters after valid JSON.
- Ollama #11991: recipient without the 'functions.' prefix (Cline tool names).
- Continue #10711: tool call written as plain text 'repo_browser.open_file({...})'.
- vLLM #22519: tokens after <|call|> fed to the parser.
  evidence: https://github.com/ollama/ollama/issues/17638, /12884, /11991; https://github.com/continuedev/continue/issues/10711; https://github.com/vllm-project/vllm/issues/22519
- [medium] Reasoning loops and degenerate output:
- 20b at high effort with about 8 tools sometimes loops (Ollama #12606, open).
- 'Thinking forever' with Cline's XML tools (cline #5398).
- tools combined with response_format loops to max tokens (llama.cpp #30377).
- runs of '?' in output: an LM Studio CUDA bug (#1841) and missing CoT (llama.cpp #27720).
  evidence: https://github.com/ollama/ollama/issues/12606; https://github.com/cline/cline/issues/5398; https://github.com/ggml-org/llama.cpp/issues/30377; https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1841
- [high] Sampling guidance: OpenAI recommends temperature 1.0 and top_p 1.0 (no truncation). The llama.cpp guide says not to use repetition penalties. llama.cpp defaults (temp 0.8, top_k 40, top_p 0.95, min_p 0.05) must be overridden per request. Unsloth's docs list top_k 0 (or 100 to experiment) and a minimum context of 16,384.
  evidence: gpt-oss README.md:574-576; https://github.com/ggml-org/llama.cpp/discussions/15396 (FAQ); /home/kristian/Рабочий стол/llama.cpp/common/common.h:233-243; https://unsloth.ai/docs/models/gpt-oss-how-to-run-and-fine-tune (2026-08-13)
- [low] Community experience in 2025–2026:
- OpenCode with gpt-oss-20b on medium stalls on failed fetches; on high it produced a working Go CLI with passing tests (2026-03-21).
- Codex with llama-server through a Responses bridge, with CoT passed back: a 36-tool-call chain finished without loops; measured reasoning per call was about 10–30 tokens on low, 50–200 on medium and 200–1500 on high.
- Ollama defaults (4096 context, medium effort) break OpenCode tool use.
- OpenCode needs the 'interleaved' setting to send reasoning back.
- Claude Code pointed at llama-server's /v1/messages loops (#20302).
  evidence: https://dev.to/rosgluk/best-llms-for-opencode-tested-locally-499l; https://github.com/ivanopcode/gpt-oss-local-codex-guide; https://www.nijho.lt/post/ollama-opencode/; https://github.com/ggml-org/llama.cpp/issues/17702 (aldehir 2026-01-07); https://github.com/ggml-org/llama.cpp/issues/20302
- [high] Codex's apply-patch implementation (crate codex-apply-patch under codex-rs/apply-patch, Apache-2.0) is reusable from Rust. It includes a parser, a streaming parser, fuzzy context matching in seek_sequence (exact, then trim_end, then trim, then normalized punctuation) and detection of apply_patch invoked through a shell or heredoc.
  evidence: /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/src-codex/codex/codex-rs/apply-patch/src/{parser.rs, streaming_parser.rs, seek_sequence.rs, invocation.rs}; codex LICENSE (Apache-2.0)
- [medium] Codex sends custom/freeform tool types (apply_patch with a grammar, web_search) to llama-server's /v1/responses, which rejected them in February 2026 with ''type' of tool must be 'function''. Community bridges skip non-function tools. Codex-compatible endpoints must therefore tolerate non-function tool types.
  evidence: https://github.com/ggml-org/llama.cpp/issues/19343; https://github.com/ivanopcode/gpt-oss-local-codex-guide (Known Limitations)
