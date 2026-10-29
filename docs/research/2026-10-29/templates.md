# templates: Vetted chat templates for the three reference models of a llama.cpp-only coding agent: gpt-oss-20b, Gemma 4 26B A4B (gemma-4-26B-A4B-it) and Qwen3.8-27B

## Executive summary
I fetched every template, extracted the ones inside the GGUFs, diffed them and rendered 8 fixed conversations through 9 template variants. Each render went through three engines: Python Jinja2 3.1.6 set up like transformers; Jinja2 fed messages pre-processed exactly as llama.cpp does; and llama.cpp's own pipeline. For the last one I compiled a small harness against the owner's build (5e03a1ff4): `_lcpp/lcpp_render.cpp` calls the same `common_chat_templates_apply` code that llama-server uses, with no model and no GPU.

Key results:

1. All three owner GGUFs are Unsloth uploads, byte-identical (LFS sha256) to the current Unsloth main. Each carries an Unsloth-modified template, and llama-server uses that template whenever `--chat-template-file` is not given.

2. Canonical upstream templates:
   - gpt-oss: openai/gpt-oss-20b, sha256 a4c9919c…, last changed 2025-08-08. Identical to gpt-oss-120b, to llama.cpp's bundled fixture and to the local bf16 copy.
   - Gemma: google/gemma-4-26B-A4B-it, sha256 ae53464b…, last changed 2026-07-15. The local NVFP4 copy is the 2026-04-28 version; llama.cpp's bundled 31B template is the 2026-04-10 version.
   - Qwen: Qwen/Qwen3.8-27B, sha256 c3cf9e34…, last changed 2026-08-13 10:16 UTC. The copy inside tokenizer_config.json is identical. A different template uploaded 2 hours earlier on release day changed semantics.

3. llama.cpp's Jinja engine matched Jinja2 byte-for-byte for gpt-oss and Qwen. For Gemma it differs in two ways: key sorting is case-sensitive, and floats are formatted with 6 decimals (2.5e-7 becomes 0.0). For gpt-oss, any tool with an array parameter that has no `items` crashes llama.cpp with an HTTP 500 (both templates). The Gemma and gpt-oss templates render in quadratic time: 6.5 s and 3.7 s per request at 1002 messages, against 0.22 s for Qwen.

Pins:
- **Generic path (`--chat-template-file`):** the canonical upstream template for all three models.
- **Golden oracle for the agent's own renderer:**
  - gpt-oss: the openai-harmony 0.0.8 reference renderer, not the HF template. The HF template departs from it in six ways: tool outputs JSON-quoted, no `<|constrain|>`, multi-line description comments, integer types, trailing commas after defaults, object parameters.
  - Gemma and Qwen: the canonical template rendered with Jinja2.

Main bugs to work around:
- Unsloth gpt-oss: earlier final answers are relabelled as analysis whenever non-empty reasoning_content is sent back.
- Gemma: a preamble on a tool-call message is moved after the tool responses, and with thinking on the prompt ends with a broken generation prompt.
- Gemma: union types render as `['INTEGER', 'NULL']`.
- Qwen: `reasoning_effort="high"` raises an error, a second system message raises, and a conversation without a user message raises.

Template defaults differ from llama-server defaults. Gemma's template defaults to enable_thinking=false and preserve_thinking=false; llama-server defaults to true for both. So set these kwargs explicitly.

## Recommendation
gpt-oss-20b
(a) Generic path:
- Pass `--chat-template-file templates/gpt-oss-20b/canonical.jinja`.
- Source: openai/gpt-oss-20b@6cee5e81 (file commit cbf31f62, 2025-08-08), sha256 a4c9919cbbd4acdd51ccffe22da049264b1b73e59055fa58811a99efbd7c8146. Same file as llama.cpp's bundled openai-gpt-oss-120b.jinja and the local bf16 copy.
- Pass it explicitly. Otherwise llama-server uses the Unsloth template in the GGUF (70da0d23…), which relabels earlier final answers as analysis.
(b) Golden oracle:
- Use the openai-harmony 0.0.8 reference renderer (git abd677f7) plus the agent's own CoT rule: keep analysis only since the latest final. Harmony's auto_drop_analysis does not implement that rule.
- First golden: gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.txt (sha256 aa65385fbc8e8970f5faa3dcbf54327b525c610e8b12ff0745d91bffd4ca4621, plus .tokens.json).
- Use canonical.jinja only as a secondary oracle: system/developer header, flat tool namespace, CoT-drop parity with the Generic path.
Kwargs:
- Set reasoning_effort=low|medium|high explicitly (`--reasoning-effort`).
- Leave model_identity, builtin_tools and developer_instructions unset.
- In the own renderer, fix 'Current date' per session; on the Generic path the date changes at midnight and invalidates the whole cached prefix.
Workarounds:
- Add items:{} to arrays without items.
- One tool call per message.
- Don't keep preamble content on tool-call messages (llama.cpp drops it, or relabels it as analysis).
- Make descriptions single-line or prefix every line with '// '.
- Accept JSON-quoted tool outputs on the Generic path. The minimal fix, if M0 shows harm, is Unsloth's `message.content is string` hunk alone.

gemma-4-26B-A4B-it
(a) Generic path:
- Pass `--chat-template-file templates/gemma-4-26B-A4B-it/canonical.jinja`.
- Source: google/gemma-4-26B-A4B-it@4d7ae498 (file commit 35b4173c, 2026-07-15, #47), sha256 ae53464bf3be25802b3a5b37def7fd89667067d7577049b3b2d74c4d8de4c6d4.
- The Unsloth GGUF template (845f1ee4…) rendered byte-identically under llama.cpp, so it is an acceptable fallback; pin canonical anyway.
- Never use the NVFP4 disk copy (04-28) or llama.cpp's bundled google-gemma-4-31B-it.jinja (04-10).
(b) Golden oracle:
- canonical.jinja under Jinja2 3.1.6 with the transformers environment. The Rust renderer must match it exactly: case-insensitive dictsort, Python float repr, tojson with ensure_ascii=False and ', '/': ' separators, Jinja2 trim.
- Parse oracle: tokenizer_config.json response_template @4d7ae498 (sha256 9f4fec4b1dc6ecddf8f4a92e9caea5971c0e67d81309f3f9066a2bee8c362633).
- Goldens: rendered/seq.think-nopreserve.canonical.py-hf.txt (047d202c…) and rendered/seq.think-preserve.canonical.py-hf.txt (a97e261c…).
Kwargs:
- enable_thinking=true, set explicitly.
- preserve_thinking, set explicitly. false is Google's documented behaviour and the template default. llama-server defaults it to true, so pass `--no-reasoning-preserve`, unless true is chosen on purpose for append-only, cache-friendly history and validated on M0.
- bos_token=<bos>.
Workarounds:
- Never send content together with tool_calls.
- Normalise tool schemas: [X,'null'] → X + nullable; no anyOf/$ref; parameter names that don't collide by case; tool names matching \w+.
- Avoid high-precision or tiny floats in arguments on the Generic path.
- Budget templating CPU for long sessions.

Qwen3.8-27B
(a) Generic path:
- Pass `--chat-template-file templates/Qwen3.8-27B/canonical.jinja`.
- Source: Qwen/Qwen3.8-27B@1d4bf0f2 (file commit 412f8b6b, 2026-08-13), sha256 c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041; identical to the tokenizer_config.json chat_template.
- Use Unsloth's GGUF template (12827f24…) only if third-party clients must send reasoning_effort='high'. Under llama.cpp that is its only practical difference.
(b) Golden oracle:
- canonical.jinja under Jinja2.
- Goldens: rendered/seq.think-medium-preserve.canonical.py-hf.txt (8b8cc52bac7b977259766b9a6979703a0a572db6c4621e8e2715f3576a3e7cdb) and rendered/seq.hf-defaults.canonical.py-hf.txt (21d78878…, the xhigh variant).
Kwargs:
- enable_thinking=true and preserve_thinking=true, both explicit.
- reasoning_effort explicitly one of xhigh|medium|low (`--reasoning-effort`, never 'high').
- Leave add_vision_id unset. Keep the default `--reasoning-preserve`.
Workarounds:
- Map client 'high' to 'xhigh'.
- Exactly one leading system message; no mid-conversation system or developer messages (put reminders in user turns).
- At least one real user message, including for subagents.
- Arguments as objects.
- Never put '\n</parameter>\n' inside string values.
- Expect trimmed tool outputs.

All models:
- Commit the templates, PROVENANCE.json and the sha256 checks into the repo.
- Add `_lcpp/lcpp_render.cpp` plus the `_scenarios` set as a regression test against llama.cpp.
- Rerun it after every llama.cpp rebase, including the PR #31244 build, and whenever an upstream template changes.

## Risks and gotchas
- llama-server silently uses the GGUF-embedded Unsloth templates unless --chat-template-file is passed. For gpt-oss that means a template that turns earlier final answers into analysis when reasoning_content is sent back (e.g. OpenCode with reasoning passback).
- The llama-server defaults (--reasoning auto, --reasoning-preserve on) give Gemma enable_thinking=true and preserve_thinking=true, while the template defaults are false/false. A baseline run with server defaults is therefore not the HF/Google format unless the kwargs are pinned.
- Qwen canonical: reasoning_effort='high' (Claude Code's default; a common OpenAI-client value) is an HTTP 500 for every request. The same happens with a second system message, a developer role outside llama.cpp, or a request without a user query.
- gpt-oss on llama.cpp: an array parameter without items in any tool returns HTTP 500 ('Function is not a bool value') for the whole request.
- Gemma on llama.cpp: case-sensitive dictsort reorders mixed-case keys, and floats lose precision in replayed arguments (2.5e-07 → 0.0). Both make the Generic prompt differ from the HF/oracle prompt.
- Gemma: a preamble on a tool-call message is moved after the tool responses. With thinking on, the prompt ends with '<turn|>\n<|channel>thought\n' and no model turn header.
- Gemma: JSON-Schema type lists render as ['INTEGER', 'NULL'] and anyOf renders an empty type. MCP schemas need normalising before they reach the template.
- Re-rendering history instead of replaying generated tokens changes tokens in several places:
- gpt-oss: header order, <|constrain|>, argument whitespace.
- Gemma: '\n' before <channel|>.
- Qwen: trimming.
The result is lost KV reuse and slightly out-of-distribution prompts.
- Quadratic templating: about 6.5 s (Gemma) and 3.7 s (gpt-oss) of server CPU per request at roughly 1000 messages.
- gpt-oss 'Current date' comes from strftime_now or the server's local time. Crossing midnight changes the first tokens of the prompt and invalidates the entire cached prefix.
- Rust serde_json defaults (compact separators, a different float formatting) will NOT match Jinja2 tojson or Python repr. Golden tests will fail unless the renderer emulates Python serialisation.
- Several upstream PRs are open (Gemma #40/#54/#55/#63/#66/#67, Qwen #147/#203, gpt-oss #272). Pin by sha256, not by 'main'.

## Questions for owner
- Gemma preserve_thinking: default to false (Google-documented, template default, more re-prefill at each user turn) or true (llama-server default, append-only history, more cache reuse, possible loops per HF #48)? Or A/B both on M0?
- gpt-oss Generic path: accept the canonical template's JSON-quoted tool outputs as the stock baseline, or allow a one-hunk local patch (raw string tool outputs, as in Unsloth's template without its bug) with its own sha256?
- Qwen reasoning_effort default for the CPU budget: xhigh (model-card default, which injects an instruction line) or medium (no injection)?
- Should the ToolProjector also normalise schemas on the Generic path? That means items:{} for bare arrays, nullable instead of type lists, lowercase non-colliding parameter names, and \w+ tool names. It keeps llama-server from erroring or diverging, but it changes what third-party baselines see.
- OK to commit the templates, PROVENANCE.json and the llama.cpp harness (_lcpp/lcpp_render.cpp, linked against llama.cpp's libllama-common) into the repo as a CI regression test?

## Unknowns
- Whether gpt-oss-20b emits compact or spaced JSON arguments. Re-rendered arguments use ', '/': ', so this decides whether re-rendering breaks KV reuse. Needs real generations on M0.
- Whether Gemma's generated thoughts end with '\n' before <channel|>. llama.cpp does not trim reasoning, so re-rendering may double the newline. Needs real generations.
- Quality impact of the canonical gpt-oss template's JSON-quoted tool outputs versus raw outputs (harmony/Unsloth), and of Gemma preserve_thinking true versus false. These need M0 A/B runs.
- How image parts in tool results are placed on llama.cpp's mtmd path for Gemma and Qwen. This is code reading only; the mtmd media loading path was not executed.
- Whether the PR #31244 build changes anything on the chat-template path. Code reading says no (server streaming only), but the harness should be rerun on that build.
- Exact rules for the harmony reference's rendering of object parameters without properties (it emitted an odd 'env?:     // Extra environment\n{\n    },'), and whether OpenAI's training used the same output.

## Files written
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/INDEX.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/canonical.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/gguf-embedded.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/PROVENANCE.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/gguf-meta.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/upstream/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/diffs/canonical_vs_gguf-embedded.diff
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/diffs/render__seq__harmony-reference_vs_canonical.diff
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/diffs/render__seq__harmony-reference_vs_gguf-lcpp.diff
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/diffs/engine__array_without_items.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gpt-oss-20b/rendered/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/canonical.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/gguf-embedded.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/PROVENANCE.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/upstream/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/local-copies/llamacpp-5e03a1ff4-google-gemma-4-31B-it-interleaved.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/diffs/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/diffs/engine__number_formatting_jinja2_vs_llamacpp.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/rendered/seq.think-nopreserve.canonical.py-hf.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/gemma-4-26B-A4B-it/rendered/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/canonical.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/gguf-embedded.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/PROVENANCE.json
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/upstream/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/local-copies/llamacpp-5e03a1ff4-Qwen3.5-4B.jinja
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/diffs/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/rendered/seq.think-medium-preserve.canonical.py-hf.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/Qwen3.8-27B/rendered/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_lcpp/lcpp_render.cpp
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_lcpp/render_timing.txt
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_scenarios/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/render_all.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/make_scenarios.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/extract_gguf.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/harmony_ref.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/vocab_from_tokenizer_json.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/write_provenance.py
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_api/
- /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_web/
