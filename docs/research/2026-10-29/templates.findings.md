# templates findings

- [high] Canonical gpt-oss-20b template: openai/gpt-oss-20b chat_template.jinja at main 6cee5e81ee83917806bbde320786a8fb61efebee; file last changed in cbf31f62664d (2025-08-08T22:46:37Z, 'Update chat_template.jinja (#94)'); 16738 B; git blob dc7bb119; sha256 a4c9919cbbd4acdd51ccffe22da049264b1b73e59055fa58811a99efbd7c8146. tokenizer_config.json (sha256 9279e942…) has no chat_template; chat_template.json was removed in ef533c92 (#59). The same blob is in openai/gpt-oss-120b, in llama.cpp models/templates/openai-gpt-oss-120b.jinja and in the owner's gpt-oss-20b-bf16/chat_template.jinja (HF metadata: downloaded at 6cee5e81).
  evidence: HF API tree?expand=true and commits (saved in templates/_api/); files downloaded via resolve/<sha>/; hashes from templates/hashes.py; history in templates/gpt-oss-20b/upstream/history (f81fef1d/5a5e34eb 3aff18ec…, ec854da5 f8d92557…, 10e9d713 #82 b474759b…, cbf31f62 #94 a4c9919c…); #94 only moved the '\n\n' after the developer instructions (diffs/history_08-07_vs_08-08.diff)
- [high] Canonical Gemma 4 template: google/gemma-4-26B-A4B-it chat_template.jinja at main 4d7ae4984b7db7de8f8457170b3f1a419ee76d52; file last changed in 35b4173cf621 (2026-07-15T16:35:17Z, #47 'null handling, reasoning preservation, turn-tag balance, input validation'; header 'Published: 2026-07-09'); 18683 B; sha256 ae53464bf3be25802b3a5b37def7fd89667067d7577049b3b2d74c4d8de4c6d4. tokenizer_config.json (sha256 9f4fec4b…, 2026-07-20 #50) has no chat_template but carries Google's response_template parse rules: thinking opens with '<|channel>thought\n' and closes with '<channel|>'; tool_calls match open_pattern '<\|tool_call>call:(?P<name>\w+)'; content closes at '<turn|>', '<|tool_response>' or '<eos>'. Template history: 04-02 release 2dfbfc7d → 04-10 #21 85a08664 (= llama.cpp google-gemma-4-31B-it.jinja) → 04-28 #36 94899c0f (= owner's NVFP4 copy; nvidia/Gemma-4-26B-A4B-NVFP4@a19cfe00 is still on it) → 05-18 #38 36e3a42e → 07-15 #47 ae53464b.
  evidence: templates/gemma-4-26B-A4B-it/upstream/history/*, diffs/history_*.diff, PROVENANCE.json; _api/tree_nvidia_Gemma-4-26B-A4B-NVFP4.json (blob f62ca843, 2026-05-07)
- [high] Canonical Qwen3.8-27B template: the official repo is Qwen/Qwen3.8-27B (siblings: -FP8, Qwen3.8-Flash-Next, 2.4T-A95B). chat_template.jinja at main 1d4bf0f2ff6012fd82039f2fa52739d0dd7c60c0; file last changed in 412f8b6bd7f5 (2026-08-13T10:16:05Z); 8952 B; sha256 c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041. tokenizer_config.json (sha256 b11349aa…, 452438ec 10:15:48Z) embeds an identical template. The initial upload 72a217af (08:23Z, 49d20d09…, 9421 B) had different semantics: preserve_thinking defaulted to false, reasoning_effort accepted low/high/xhigh (unset or 'high' injected nothing, 'medium' raised), it parsed <think> out of content, and it rendered audio placeholders. Any template copied before 10:16Z on 2026-08-13 is wrong.
  evidence: templates/Qwen3.8-27B/upstream/history/*, diffs/history_initial_vs_canonical.diff; HF search API author=Qwen
- [high] All three owner GGUFs are Unsloth quantisations and byte-identical to the current main of their Unsloth repos on 2026-10-29. Their templates are Unsloth-modified, and llama-server uses them by default when --chat-template-file is not passed. (1) gpt-oss-20b-F16.gguf: unsloth/gpt-oss-20b-GGUF@d449b42d, file unchanged since a1b6de7b (2025-08-08); template 17221 chars, sha256 70da0d2348e40aaf8dad05f04a316835fd10547bd7e3392ce337e4c79ba91c01. (2) gemma-4-26B-A4B-it-UD-Q4_K_M.gguf: unsloth/gemma-4-26B-A4B-it-GGUF@c099eb48 (2026-07-17 'Added Gemma official chat template update'); sha256 845f1ee48e39fc942fe190da9df6a1c5db229e17a96ea08966ad1c9274e73d1b. (3) Qwen3.8-27B-Q4_0.gguf: unsloth/Qwen3.8-27B-GGUF@4ca72078, file from 8fe26993 (2026-08-14 'Add Unsloth style chat template'); sha256 12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce.
  evidence: templates/extract_gguf.py (gguf_meta.meta, read-only) → <model>/gguf-embedded.jinja + gguf-meta.json; HF download metadata in the disk .cache/huggingface/download/*.metadata (commit + LFS sha256) matched against _api/tree_unsloth_*.json; HF ?expand[]=gguf reports the same template sha256
- [high] Engine parity, gpt-oss and Qwen: llama.cpp's Jinja engine produced the same bytes as Jinja2 run on llama.cpp-equivalent preprocessed messages in every scenario. This includes tojson of unicode/emoji tool outputs, Qwen's 'tool|tojson' declarations and floats/null inside tojson.

Engine parity, Gemma: two differences.
- dictsort is case-sensitive in llama.cpp and case-insensitive in Jinja2. Keys 'cmd'/'Cwd' come out as 'cmd,Cwd' in Jinja2 and 'Cwd,cmd' in llama.cpp, both in tool declarations and in replayed call arguments.
- '{{ number }}' formatting in llama.cpp loses precision: 2.5e-07→0.0, 3.14159265358979→3.141593, 1e20→100000000000000000000.0. Jinja2 prints the Python repr.

llama.cpp caps detected (5e03a1ff4):
- gpt-oss: object_arguments on, parallel off, preserve_reasoning off, reasoning_effort on (format peg-native).
- Gemma: object_arguments and parallel on, no preserve/effort caps (format peg-gemma4).
- Qwen: all on (format peg-native via the qwen3-coder handler).
  evidence: templates/_lcpp/lcpp_render.cpp (links libllama-common from the owner's build; CUDA_VISIBLE_DEVICES=''); render_all.py → <model>/rendered/_summary.json (ENG ok/DIFF); gemma-4-26B-A4B-it/diffs/render__seq.think-preserve.canonical__py-lcpp_vs_lcpp.diff; gemma-4-26B-A4B-it/diffs/engine__number_formatting_jinja2_vs_llamacpp.txt
- [high] The HF gpt-oss template departs from the openai-harmony 0.0.8 reference renderer in 6 visible ways on the same conversation:
1. Tool outputs are JSON-quoted via message.content|tojson (line 322).
2. The call header is 'commentary json' instead of 'commentary <|constrain|>json'.
3. Only the first line of a multi-line description gets '//'.
4. ['integer','null'] renders as 'integer | null'; the reference renders 'number | null'.
5. Defaults render as '// default: false,' with a trailing comma.
6. An object parameter without properties renders as 'object'; the reference itself renders an odd 'env?:     // Extra environment\n{\n    },'.

Also: only message.tool_calls[0] is rendered (line 285), and 'Current date' comes from strftime_now on every request. Neither renderer reproduces the model's own channel-first header ('<|channel|>commentary to=functions.X <|constrain|>json'), so re-rendering a generated call always changes its tokens.
  evidence: templates/harmony_ref.py (uv run --with openai-harmony==0.0.8; vocab rebuilt from the local tokenizer.json, sha256 446a9538… = tiktoken o200k_base) → gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.txt (sha256 aa65385f…); diffs/render__seq__harmony-reference_vs_canonical.diff; HF discussions #215 (missing constrain) and #218 (ordering/double encoding), both open
- [high] The Unsloth gpt-oss template (gguf-embedded.jinja) differs from canonical as follows.

Changes:
- String tool outputs rendered raw (matches harmony).
- String arguments passed through unchanged.
- The '<|channel|>' content check removed.
- New developer_instructions kwarg: replaces messages[0] but still loops over it, so a system message is silently dropped.
- 'builtin_tools is defined' is used, so builtin_tools=[] emits an empty '# Tools' header.
- No trailing '\n\n' after instructions when there are no tools (matches harmony; canonical emits it).
- The last assistant message without a generation prompt ends with <|end|> instead of <|return|>.

Bug at lines 317-320: '{%- elif "thinking" in message %} … "<|start|>assistant<|channel|>analysis<|message|>" + message.content'. Any earlier final answer that carries a thinking key is rendered as analysis and its final message disappears. Through llama.cpp this happens whenever a client sends back non-empty reasoning_content on a final answer: the gpt-oss handler copies it to 'thinking'. Reproduced: seq and final_reasoning both show '<|channel|>analysis<|message|>4'. Correction to the earlier research: an empty reasoning_content does NOT trigger it in llama.cpp, because to_json_oaicompat drops empty reasoning. It would trigger in HF transformers with thinking=''.
  evidence: gpt-oss-20b/diffs/canonical_vs_gguf-embedded.diff; rendered/final_reasoning.effort-high.gguf.lcpp.txt; _lcpp/req_gptoss_edge.json run; gptoss_extra.py output; llama.cpp common/parsers/gpt-oss.cpp:7-17, common/chat.cpp to_json_oaicompat (reasoning only if non-empty)
- [high] What llama.cpp 5e03a1ff4 does to gpt-oss requests before rendering:
- Rewrites the canonical content check to '{%- if false %}' at load time.
- Converts null content to ''.
- Parses string arguments into objects (caps.object_arguments=true), so the tojson double-encoding seen with HF does not happen.
- Copies reasoning_content to thinking and ERASES content on tool-call messages, so a preamble is silently lost.
- Without reasoning, a preamble on a tool-call message is relabelled as analysis.
- Keeps the developer role (only for templates containing '<|channel|>').
- The --reasoning-preserve flag has no effect.
- A request with reasoning_effort 'none' only sets enable_thinking=false and removes the kwarg, so the template falls back to 'Reasoning: medium'.

With HF semantics (oracle side), canonical raises TypeError on content:null (HF PR #272 open), raises on content+thinking, and double-encodes string arguments.
  evidence: common/chat.cpp:762-768 (patch), 1000-1040 (workarounds), 1336-1358, tools/server/server-common.cpp:1419-1427; rendered/seq_null_content.*.canonical.py-hf.ERROR.txt, preamble_last.*.canonical.py-hf.ERROR.txt vs .lcpp.txt, seq_str_args.*.canonical.py-hf.txt
- [high] Under llama.cpp's Jinja, both gpt-oss templates fail on an array parameter without 'items' with 'Function is not a bool value': param_spec['items'] resolves to the dict method. Every request carrying such a tool, which is common in MCP schemas, gets an HTTP 500. Jinja2 renders 'any[]'. Adding items:{} gives 'any[]' and items:{type:string} gives 'string[]' in llama.cpp. Gemma renders anyOf as an empty type ('type:<|"|><|"|>'); Qwen dumps the raw JSON.
  evidence: gpt-oss-20b/diffs/engine__array_without_items.txt; _lcpp/req_array_noitems.json, probe_array.py; llama.cpp issue #30781 (from earlier research)
- [high] Gemma July (canonical) semantics:
- enable_thinking|default(false) and preserve_thinking|default(false).
- A system turn is opened when thinking is on, tools exist or the first message is system/developer. It contains '<|think|>\n', the trimmed system text and the <|tool> declarations.
- Thoughts are rendered as '<|channel>thought\n'+text+'\n<channel|>' only when the message comes after the last user message, or when preserve_thinking is set and the message has tool_calls. Final-answer thoughts from earlier turns are therefore always dropped.
- Consecutive assistant and tool messages form ONE model turn.
- Tool responses are found by forward scan and matched to calls by tool_call_id. String content becomes response:NAME{value:<|"|>…<|"|>}; JSON-looking strings are not parsed.
- Generation prompt after a user turn: '<|turn>model\n' (plus '<|channel>thought\n<channel|>' when thinking is off).
- After a tool response with thinking on, the July template prefills '<|channel>thought\n'. With thinking off it adds nothing, so there is no empty-thought guard (HF PR #67 open).
- With no kwargs the HF defaults are thinking off and preserve off. llama-server's defaults (--reasoning auto plus default --reasoning-preserve) are thinking on and preserve on, so the 'hf-defaults' renders differ from the server.
  evidence: canonical.jinja:186-187, 190-216, 239-243, 269-321, 363-374, 381-389; rendered/seq.{think-preserve,think-nopreserve,nothink,hf-defaults}.canonical.*; _summary.json lcpp_generation_prompt; Google prompt-formatting-gemma4 (updated 2026-06-03, saved in _web/) says to strip thoughts between standard turns and keep them between calls within one turn
- [high] Gemma July has a preamble bug. When an assistant message has both content and tool_calls, the content is rendered AFTER the tool responses (the reverse of generation order). If it is the last such message, '<turn|>\n' is emitted. With thinking on, the generation prompt then becomes '<turn|>\n<|channel>thought\n' with no '<|turn>model\n'. llama.cpp only patches prompts ending in '<turn|>\n', which is the thinking-off case. Reproduced in llama.cpp's pipeline. The Unsloth variant behaves identically.
  evidence: rendered/preamble_last.think-preserve.canonical.lcpp.txt (tail) vs preamble_last.nothink.canonical.lcpp.txt; common/parsers/gemma4.cpp:158-164; HF PR #40 (open, OpenCode report)
- [high] Gemma argument and schema handling:
- Canonical raises on string arguments ('must be a JSON object').
- Unsloth renders the JSON string's inner text instead, giving JSON syntax inside the DSL: call:read_file{"path": "src/lib.rs", …}.
- The 04-28 template gives '{{…}}'.
- None renders as 'null' (July); older templates give 'None' in Jinja2 and '' in llama.cpp ('end_line:,').
- JSON-Schema type lists render as garbage 'type:<|"|>['INTEGER', 'NULL']<|"|>' in every version.
- anyOf renders an empty type; $ref, default, format and min/max are dropped.
- llama.cpp's bundled 04-10 template silently drops top-level parameters named description/type/properties/required/nullable (fixed in 04-28 #36).
- The 04-28 copy (owner's NVFP4) ignores 'image_url' parts.

Under llama.cpp the canonical and Unsloth templates rendered byte-identically in all 28 scenario/kwarg combinations, because llama.cpp parses arguments into objects first.
  evidence: rendered/seq_str_args.*.py-hf.*; probe_misc.py output (create_task with description/type params); rendered/multimodal.*.disk-nvfp4-2026-04-28.py-hf.txt; diffs/history_04-10_vs_04-28.diff; summary check canonical==gguf (28/28)
- [medium] Multimodal placeholders:
- Gemma July emits '<|image|>' (or <|audio|>, <|video|>) inline in user content and AFTER '<tool_response|>' for tool-result images. HF PR #55 (open) wants them inside the block.
- Qwen emits '<|vision_start|><|image_pad|><|vision_end|>' inside <tool_response>; add_vision_id prefixes 'Picture N: '.
- gpt-oss raises TypeError on list content.
- llama.cpp ignores these placeholders. The server turns media parts into '<__media__>' markers and concatenates them into the content text before templating, so a Gemma tool-result image ends up inside the value:<|"|>…<|"|> string.
  evidence: rendered/multimodal.*.py-hf.txt; common/chat.cpp to_json_oaicompat (media concatenation), tools/mtmd/mtmd.cpp:485, tools/server/server-common.cpp:1209-1240 (code reading; mtmd path not executed)
- [high] Qwen3.8 canonical semantics:
- When enable_thinking is undefined or true, reasoning_effort|default('xhigh') must be one of xhigh/medium/low, or the template raises. xhigh injects 'Reasoning effort is set to xhigh…' into the system prompt, low injects a short line, medium injects nothing. 'high' raises: Claude Code's default produces HTTP 500 (HF PR #147, open). Through llama.cpp, '--reasoning-effort high' or a request reasoning_effort='high' fails the same way.
- preserve_thinking is TRUE when undefined (model card: 'enabled by default'). With false, earlier turns lose their <think> block entirely.
- Generation prompt: '<|im_start|>assistant\n<think>\n'; enable_thinking=false gives '<think>\n\n</think>\n\n'.
- Content, tool outputs and reasoning are all |trim'ed.
- Parallel calls render fully. Consecutive tool results are grouped into one user turn.
- String parameter values are raw (no escaping); non-strings go through tojson (true/null/1.0).
- Arguments must be a mapping (Jinja2 'Can only get item pairs from a mapping'); llama.cpp parses them.
- Raises on: developer role (in Jinja2), any second system message, and no non-tool user message.
  evidence: canonical.jinja:45-56, 99-106, 116-120, 135-141, 147-158; rendered/seq.*.canonical.*, *.think-high.canonical.*.ERROR.txt (Jinja2 and llama.cpp both raise); probe_misc.py output; _web/qwen38_readme.md lines 252-300, 469-493
- [high] The Unsloth Qwen template ('Unsloth fixes - developer role, merged system messages, tool calling'):
- Merges consecutive leading system/developer messages with '\n'.
- Maps 'high' to 'xhigh'.
- Drops the 'No user query' raise.
- Validates tool name and arguments explicitly; a JSON-string argument still raises.

Through llama.cpp (which maps developer to system itself) it rendered identically to canonical in every tested case except reasoning_effort='high'. A mid-conversation system message still raises in both.
  evidence: Qwen3.8-27B/diffs/canonical_vs_gguf-embedded.diff; _summary.json (canonical==gguf except think-high, 7 cases); probe_misc.py; _web/readme_unsloth_Qwen3.8-27B-GGUF.md lines 28-29
- [high] Template rendering cost in llama.cpp grows quadratically for Gemma July and gpt-oss because of forward scans per message. Whole-harness time per request (init + caps + apply, two renders) for 102/402/1002 messages: Gemma 0.17/1.15/6.53 s, gpt-oss 0.11/0.66/3.69 s, Qwen 0.08/0.13/0.22 s. On the Generic path this CPU cost is paid on every request of a long agent session.
  evidence: _lcpp/render_timing.txt (copied to each <model>/diffs/engine__render_time_long_history.txt), req_long{50,200,500}.json; HF PRs #63/#66 on google/gemma-4-26B-A4B-it (open, 'quadratic')
- [medium] Round-trip and KV-reuse hazards when history is re-rendered instead of replayed verbatim:
- Gemma adds '\n' before <channel|>, and llama.cpp's PEG mapper does not trim reasoning_content. A generated '…\n<channel|>' may come back as '…\n\n<channel|>'.
- gpt-oss canonical re-dumps parsed arguments with ', ' and ': ' separators. A compact '{"path":"README.md"}' became '{"path": "README.md"}'.
- gpt-oss headers are always re-rendered recipient-first.
- Qwen trims content and tool outputs.

For the agent's own renderer this argues for verbatim token replay of generated spans. For the oracle it argues for Python-compatible serialisation: json.dumps with default separators and ensure_ascii=False, Python float repr, and Jinja2's case-insensitive dictsort.
  evidence: common/chat-peg-parser.cpp:300-321 (no trim, whitespace-only reasoning dropped); _lcpp/req_gptoss_edge.json output; Gemma canonical.jinja:242; Qwen canonical.jinja:103,115-117
- [high] Kwarg surface (external inputs read by each template):
- gpt-oss canonical: messages, tools, add_generation_prompt, reasoning_effort (default medium), model_identity, builtin_tools; globals strftime_now and raise_exception. Unsloth adds developer_instructions.
- Gemma: messages, tools, add_generation_prompt, bos_token, enable_thinking, preserve_thinking.
- Qwen: messages, tools, add_generation_prompt, enable_thinking, preserve_thinking, reasoning_effort, add_vision_id.

llama-server always passes enable_thinking, plus datetime/date_string and preserve_reasoning (default 'true'), which becomes preserve_thinking/clear_thinking/truncate_history_thinking/drop_thinking. It passes reasoning_effort (+reasoning_strength) only when set.
  evidence: templates/kwargs_inventory.py → _api/kwargs_inventory.json; common/chat.cpp:880-925, 1060-1072, 1360-1363; common/jinja/caps.cpp:22-33; common/arg.cpp:964-967, 3708-3768
- [high] Open upstream items that would change the pins if merged, all checked on 2026-10-29:
- Qwen #147 ('high' alias), #203 (template update), #216 (Claude Code template).
- Gemma #40 (generation prompt after tools), #48 (prior-turn reasoning re-injection → loops), #54 (keep tool-call reasoning across later user turns; claims ~27% of the cached prefix is invalidated), #55 (placeholders inside the block), #63/#66 (quadratic), #67 (empty thought after tool response).
- gpt-oss #272 (content:null TypeError), #274 ('commentary' without tools), #218 (ordering).
None is merged; the canonical files are still the versions listed above.
  evidence: HF discussions API (templates/_api/disc_*.json, d_*.json)
