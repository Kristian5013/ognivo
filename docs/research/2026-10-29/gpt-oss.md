# gpt-oss: gpt-oss-20b as an agentic coding model: Harmony format, CoT retention, the Rust harmony library, first-party harness conventions, failure modes, measured performance, and a gpt-oss profile spec for a llama.cpp-based coding agent

## Executive summary
gpt-oss-20b is a strong local coding agent only when the harness speaks Harmony natively. An independent native-Harmony harness (arXiv 2604.00362, 2026-04-01; MIT code at github.com/borislavmavrin/harmonyagent) reproduced OpenAI's SWE-bench Verified numbers: 60.4% on high (published 60.7) and 53.3% on medium (published 53.2). Generic harnesses score far lower: 26.0% for gpt-oss-120b on the bash-only SWE leaderboard, and 3.1–3.4% for gpt-oss-20b on Terminal-Bench 2.0.

What decides the result:
- Exact Harmony rendering: system/developer split, TypeScript-like tool namespaces, recipient/channel headers, and <|constrain|>json.
- Sending back the raw chain of thought (CoT) during tool loops. In llama.cpp #27720, stripping it caused malformed headers in 7 of 120 turns versus 0 of 120 when it was sent back; other reports show endless tool loops.
- Exposing the model's trained-in tools: a container.exec-style shell with an argv array, repo_browser print_tree/search/open_file, and apply_patch in the V4A diff format.
- Tolerant parsing with retries, because the recommended sampling is temperature 1.0 with top_p 1.0.

Harmony format basics: 7 control tokens (start 200006, end 200007, message 200008, channel 200005, constrain 200003, call 200012, return 200002) and channels analysis (CoT), commentary (tool calls and user-visible preambles) and final. Generation stops at <|call|> or <|return|>, so there is exactly one tool call per sampling. <|return|> must be rewritten to <|end|> in history.

OpenAI's CoT rule: keep analysis messages from the most recent final onward while a tool loop is in progress, and drop them once a later final exists; tool calls stay.

llama.cpp at commit 5e03a1ff4 works through /v1/chat/completions, but that path is lossy:
- The official template JSON-quotes tool outputs, omits the <|constrain|> token, breaks multi-line and nested schemas, renders a non-object parameter schema as a no-argument function, and relabels preambles as CoT.
- The Unsloth template embedded in the owner's GGUF relabels earlier final answers as analysis when reasoning is sent back.
- The server parser hard-fails a whole turn on a malformed header.
- The prompt is tokenized with special-token parsing, so control-token strings inside file contents can likely inject message boundaries.

llama-server already offers a token-level path: /completion accepts token-ID prompts and streams token IDs, with cache_prompt, id_slot, lazy grammar and reasoning-budget fields. A Rust agent can therefore render prompts itself and parse token IDs.

openai-harmony (Rust crate 0.0.8, Apache-2.0) has had no code release since 2025-11-05. It downloads its vocabulary at load time, renders recipient-first headers (the model emits channel-first, which breaks KV-cache reuse) and has a strict parser. vLLM's Rust path moved to the maintained oss-harmony 0.0.11 fork, which embeds the vocabulary, parses both header orders and emits a real <|constrain|> token.

Recommended setup:
- Render with a pinned oss-harmony and parse with our own tolerant token-level state machine.
- Replay the model's own assistant tokens verbatim for perfect KV-cache reuse.
- Sampling: temperature 1.0, top_p 1.0, top_k 0, min_p 0, no penalties.
- Reasoning effort: high for the main loop (SWE-bench Verified 37.4 / 53.2 / 60.7 for low / medium / high), fixed per session, because effort lives in the system message and changing it forces a full re-prefill.
- Edits through apply_patch, reusing Codex's Apache-2.0 Rust apply-patch crate.

## Recommendation
GPT-OSS PROFILE v1 (gpt-oss-20b on llama.cpp; also valid for 120b)

A. Transport
- Primary path: llama-server POST /completion (the non-OpenAI endpoint). The prompt is a token-ID array rendered by the agent.
  - Request: stream=true (each chunk carries tokens:[id]), cache_prompt=true, id_slot pinned per session, return_progress=true.
  - n_predict cap per step: about 16384 on high, 8192 on medium, 2048 on low, and never more than n_ctx minus the prompt.
  - Parse token IDs only. The text deltas are for display.
- Server flags: -c 131072 per active slot (or -np N with --kv-unified-per-slot), -fa on, -ngl 99, -ub 2048 -b 2048, keep --context-shift off, set --slot-save-path to persist session KV on NVMe.
  - Prefer --swa-full when VRAM allows (about +3 GiB per 131k slot). Otherwise rely on --ctx-checkpoints and alert on 'forcing full prompt re-processing' in the logs.
  - Never rely on the GGUF-embedded template; the owner's Unsloth template has the analysis-relabel bug.
- Diagnostics-only fallback: /v1/chat/completions with --chat-template-file set to the official template. Send reasoning_content back on tool-call turns only, and use reasoning_control=true for 'stop thinking'.

B. Tokenizer and renderer
- o200k_harmony through oss-harmony pinned to a git rev (lib openai_harmony, embedded vocabulary).
- Alternative: openai-harmony 0.0.8 with a vendored vocabulary via TIKTOKEN_ENCODINGS_BASE.
- Either way, write golden tests against harmony test-data plus our own fixtures.
- Encode all message content with ordinary (non-special) encoding, so a literal '<|end|>' in a file cannot inject structure. Never add BOS.
- Token IDs: start 200006, end 200007, message 200008, channel 200005, constrain 200003, call 200012, return 200002.

C. Prompt layout, exact
- system (lines joined by newline):
  You are ChatGPT, a large language model trained by OpenAI.
  Knowledge cutoff: 2024-06
  Current date: <session start date, YYYY-MM-DD, frozen for the session>
  (blank line)
  Reasoning: <low|medium|high>
  (blank line)
  [optional '# Tools' block with built-in-style namespaces, see D]
  # Valid channels: analysis, commentary, final. Channel must be included for every message.
  [only if functions exist: Calls to these tools must go to the commentary channel: 'functions'.]
- developer: '# Instructions' + blank line + agent rules, then blank line + '# Tools' + blank line + '## functions' + blank line + 'namespace functions {' ... '} // namespace functions'.
  - Omit sections that are empty, with no trailing blank lines.
  - Use '# Response Formats' only in separate tool-less structured calls. Never combine tools with response_format (llama.cpp #30377).
- user: raw text. All volatile context (cwd, git status, diagnostics, AGENTS.md, @-mentions) goes into user messages. System and developer messages are immutable for the session.
- The prompt ends with '<|start|>assistant'.

D. Tool declarations
- Harmony TypeScript syntax:
  - every description line prefixed with '//';
  - 'type NAME = (_: {' then '// param description' and 'param?: TYPE, // default: X' lines, then '}) => any;';
  - 'type NAME = () => any;' when there are no parameters;
  - a blank line after each tool.
- The JSON-Schema to TypeScript converter must inline $ref/$defs, turn oneOf/anyOf into unions, enums into literal unions, nullable into '| null', and arrays without items into any[].
- Tool names match [A-Za-z0-9_] only; MCP tools become mcp__SERVER__TOOL, with no dots.
- Core tools follow the model's priors:
  - shell {command: string[] (argv, e.g. ['bash','-lc','...']), workdir?: string, timeout_ms?: number}
  - apply_patch {patch: string}
  - print_tree {path, depth}
  - search {path, query, max_results?}
  - open_file {path, line_start?, line_end?}
  - update_plan {explanation?, plan:[{step, status: pending|in_progress|completed}]}
- Placement (A/B test before freezing):
  - Default A: core tools as system-message namespaces container{exec} and repo_browser{print_tree, search, open_file, apply_patch}. This is the layout that reproduced 60.4% on SWE-bench Verified. Auxiliary and MCP tools go in developer 'functions'.
  - B: every tool in functions.
- Always-on alias router:
  - container.exec, functions.shell, exec and bash route to shell;
  - repo_browser.print_tree, list_files and list_dir route to print_tree;
  - repo_browser.search and find route to search;
  - repo_browser.open_file and read_file route to open_file;
  - apply_patch with any namespace routes to apply_patch;
  - python and browser.* get a 'not available' tool error;
  - argument aliases accepted: cmd/commands, input/patch, file_path, start_line/end_line.
- The tool set is frozen per session; any change invalidates KV from the developer message onward.

E. Streaming parser (token-level state machine)
- Start in the Header state with role=assistant.
  - The header holds the role, ' to=RECIPIENT' (before or after the channel), <|channel|> plus the channel name, and an optional ' <|constrain|>TYPE' or plain ' json'/' code'. It ends at <|message|>.
  - Content ends at <|end|> (continue), <|call|> (tool call, stop) or <|return|> (final, stop).
- Classify each message:
  - analysis with no recipient: CoT, shown in a collapsible pane;
  - commentary with no recipient: preamble, shown to the user;
  - any recipient on commentary or analysis: tool call;
  - final: the answer.
- Tolerance rules:
  - accept a missing <|start|>;
  - resync after junk headers (unknown channel, '??', a doubled <|channel|>) and treat the junk as analysis;
  - normalize the recipient: strip 'functions.', a glued <|constrain|>, trailing json/code; resolve bare names;
  - arguments: take the first JSON value, unwrap a single-element array, unwrap a JSON-string-encoded object, strip code fences;
  - for patch tools, accept raw text.
- Unknown tool or bad arguments: append a tool result authored by the same recipient, containing {error, available_tools}, then resample. Allow at most 3 consecutive and 10 per turn.
- Never consume tokens after <|call|> or <|return|>.

F. History and CoT
- Store each structured message together with the exact token IDs the model generated.
- Replay assistant tokens verbatim: rewrite the final <|return|> to <|end|> and keep <|call|>. Re-render canonically only when the header was malformed.
- Tool result format: '<|start|>RECIPIENT to=assistant<|channel|>CHANNEL_OF_CALL<|message|>' + raw text + '<|end|>'.
  - Raw text only, never JSON-quoted.
  - Include the exit code.
  - Cap at about 10k characters (head plus tail, with a truncated-lines note); this also avoids the harmony stack overflow on huge repeated-character inputs.
- CoT policy (OpenAI): keep every analysis message that comes after the most recent final. When a new turn starts after a final, drop recipient-less analysis messages located before that final; keep tool calls, tool results and preambles. Never drop CoT inside an unfinished turn.
- Compaction: tool call and result pairs are atomic. Order of measures:
  1. truncate old tool outputs;
  2. summarize older steps into a progress note;
  3. as a last resort, drop in-turn analysis older than the last N steps.

G. Sampling
- temperature 1.0, top_p 1.0, top_k 0, min_p 0, repeat_penalty 1.0, presence 0, frequency 0, dry_multiplier 0, XTC off.
- Random seed per attempt; never greedy.
- Phase 2: a lazy grammar (GBNF, or llguidance built with LLAMA_LLGUIDANCE) for tool-argument JSON, triggered at <|message|> after a ' to=' header.

H. Reasoning effort
- high for the main coding loop (SWE-bench Verified 60.7 vs 53.2 on medium vs 37.4 on low).
- medium for exploration sub-agents.
- low for utility calls (titles, commit messages, summaries), run in separate sessions or slots.
- Effort is part of the system message, so a mid-session change costs a full re-prefill. Escalate only by restarting a failed turn at higher effort.

I. Edit format
- apply_patch V4A envelope:
  *** Begin Patch
  *** Add File: P (followed by + lines)
  *** Delete File: P
  *** Update File: P [*** Move to: Q], with hunks '@@ [context]' made of ' ', '-' and '+' lines [*** End of File]
  *** End Patch
- Paths are relative.
- Apply with the vendored codex-apply-patch (fuzzy matching). Also intercept apply_patch invoked through a shell heredoc.
- On failure, return the failing hunk and the nearest excerpt of the actual file.

J. Stop conditions
- Per generation:
  - EOG <|call|> or <|return|>;
  - abort client-side when a final message closes with <|end|>;
  - n_predict reached;
  - analysis budget exceeded: abort, append '<|end|><|start|>assistant' and resume (the UI 'stop thinking' button does the same);
  - degeneracy detector (32 or more '?' or a repeated 8-gram 4 times): abort and resample.
- Per turn:
  - final: yield to the user;
  - tool call: run it and continue;
  - caps on steps (about 150), wall-clock time and consecutive errors;
  - the same call with the same arguments 3 times: inject a warning tool result;
  - context at 85% or more: compact.

K. Conformance gate
- Golden renders compared with harmony test-data.
- Parser fixtures built from real failures: #27720 junk headers, container.exec, text-form repo_browser calls, array-wrapped arguments, missing namespace, analysis-channel calls.
- Multi-turn CoT retention tests.
- OpenAI compatibility-test adapted: 0 invalid requests and at least 90% on pass@5 and pass^5.
- Regression subsets of SWE-bench Verified (about 50 tasks) and Terminal-Bench at medium and high.

## Architecture implications
- The per-model profile layer has to own the token-level renderer, the streaming parser, history serialization (verbatim replay of assistant tokens), the CoT retention policy, how tools are presented (names, schemas, placement, aliases), sampling defaults, reasoning effort and stop rules. A generic 'messages + tools JSON' abstraction cannot express gpt-oss correctly.
- Define the backend interface at token level: complete(prompt_tokens, sampling, optional grammar/triggers, slot) returns a stream of token IDs, plus tokenize/count and slot KV save/restore. llama-server /completion is the first implementation, and the owner's own Rust engine can implement the same interface later.
- The conversation store must keep both the structured messages and the raw token spans of each assistant message, so that stored transcripts reproduce the exact prompt bytes and debugging is deterministic.
- Prefix-stability discipline is needed for KV-cache reuse: system and developer messages are immutable per session, the date is frozen at session start, the tool set is frozen, volatile context is appended at the tail, sessions are pinned to slots, and slot KV is persisted to NVMe for instant resume. The CoT drop at each new user turn is the only planned break in the prefix.
- The tool layer needs canonical internal operations behind a per-profile facade (model-specific names, schemas, namespace placement) plus an alias router for prior-driven recipients, and an MCP schema normalizer to the Harmony TypeScript form (inline $ref, handle arrays without items, sanitize names).
- The executor is sequential, because gpt-oss makes at most one tool call per sampling. Throughput comes from batch-capable tools (multi-path read and search) and from separate sessions for sub-agents.
- The UI needs separate streaming lanes for CoT (analysis), preambles (commentary), tool calls (with arguments streaming in), tool output and the final answer, plus a 'stop thinking' control (abort and resume, or reasoning_end) and a context and KV-cache meter.
- An in-process o200k_harmony tokenizer is required for rendering, counting and caps; all content is encoded with ordinary encoding to prevent special-token injection.
- A built-in conformance and eval harness (golden renders, parser fuzzing from real failure corpora, multi-turn CoT tests, SWE and Terminal-Bench subsets) acts as the release gate for every model profile, because the harness changes measured scores by tens of points.

## Risks and gotchas
- Using the llama-server chat path with the owner's Unsloth GGUF: when reasoning_content is sent back on final turns, previous final answers are re-rendered as analysis, so the model loses its own answers.
- The official template quietly degrades prompts: tool outputs are JSON-quoted, the <|constrain|> token is missing, multi-line and nested declarations are malformed, an apply_patch declared with a string parameter is shown as a no-argument function, preambles are relabeled as CoT, only the first tool call is rendered, and the date changes daily, invalidating the KV cache at midnight.
- Dropping CoT inside a tool loop produces malformed headers and runs of '?' (about 6% of turns), plus repeated identical tool calls. Retention must be per turn, not per request.
- The llama-server parser fails the whole turn on malformed headers; with an agent-side tolerant parser the turn can be recovered instead.
- Likely control-token injection on the chat path: file contents or tool outputs containing '<|start|>assistant<|channel|>final<|message|>' are tokenized with parse_special=true. This matters for any repository containing chat templates or Harmony docs.
- Prior-driven recipients (container.exec, repo_browser.*, python, bare names, apply_patch without a declaration) are silently turned into empty content by llama.cpp (#21213), so the agent sees an empty reply and no error.
- Argument JSON pathologies: single-element array wrapping (about 40% in one apply_patch-style setup), trailing garbage after the JSON, tool calls written as plain text, and double encoding.
- Tools combined with response_format leads to endless reasoning (llama.cpp #30377, open). Never combine them; use two passes instead.
- High effort inflates CoT and can overflow 128k inside a single task (seen in the SWE-bench reproduction), so compaction is mandatory.
- Sliding-window attention: when the prompt diverges mid-prefix (the CoT drop), llama-server may force a full re-prefill unless --swa-full is set or a usable checkpoint exists.
- Changing reasoning effort, the date, the developer instructions or the tool set invalidates the entire KV cache for the session.
- llama-server defaults (temp 0.8, top_k 40, top_p 0.95, min_p 0.05) apply silently unless overridden per request. Repetition penalties corrupt code and JSON.
- openai-harmony 0.0.8 downloads its vocabulary over the network at load (fails offline), its parser bails on deviations, re-rendering role-first breaks KV prefixes, and content_type must be exactly '<|constrain|>json'. The oss-harmony fork has a single maintainer, 1 star and is git-only.
- MCP schemas can break llama.cpp's chat path: array parameters without items return HTTP 500 with the gpt-oss template (#30781, open). Large tool lists and odd schemas previously broke GBNF.
- Local llama.cpp checkout 5e03a1ff4 predates the 2026-10-20 fix for GPT-4o pre-tokenizer combining marks (affects server-side tokenization of some scripts).
- OpenAI advises against showing raw CoT to end users; it is not safety-trained and may echo developer instructions. This matters if sessions are shared.
- Third-party backends differ: Ollama defaults to a 4k context and medium effort, LM Studio had a CUDA bug producing '?' output, and Codex now requires the Responses API and sends custom tool types that llama-server rejected.

## Questions for owner
- Primary gpt-oss transport: (a) the agent renders Harmony itself and talks to raw llama-server /completion with token IDs (recommended; needs an in-process o200k tokenizer and a token-level history store), or (b) llama-server /v1/chat/completions with Jinja templates (simpler, but lossy and with server-side parse failures)? The choice shapes the backend abstraction for every model.
- Tool surface policy: may each model get its own tool names, schemas and placement (gpt-oss gets container.exec, repo_browser.* and apply_patch{patch} in system-message namespaces, which matches the validated reproduction), or must all models share one uniform tool set with alias routing only?
- Default reasoning effort for the main coding loop: high is recommended (+7.5 points on SWE-bench Verified over medium, at the cost of many more CoT tokens and more context pressure). Should users be able to switch effort mid-session, which costs a full KV re-prefill each time?
- Raw CoT in the UI: show the analysis channel to the user (collapsed), or show only preambles and final answers, as OpenAI recommends for end users? This matters if sessions or results are shared with other people.
- Does the agent own and launch llama-server (pinned version, enforced --swa-full, -np, context size and --slot-save-path), or must it work against any user-started server whose flags it cannot control?
- Concurrency target per GPU: how many simultaneous sessions or sub-agents are expected? One 131k gpt-oss slot needs about 3 GiB of f16 KV, or about 6 GiB with --swa-full, next to the roughly 13 GB model on the 24 GB RTX 5090 Laptop.
- Should gpt-oss's built-in python tool (a stateful Jupyter sandbox) and browser tool (needs a search and fetch backend) be offered, or always declared unavailable?

## Unknowns
- Not executed: whether llama-server includes the stop token's ID (<|call|> or <|return|>) in the last stream chunk before the final response. The code path suggests it does.
- Whether the system-message versus developer-message placement effect (arXiv 2604.00362, measured on vLLM) also holds on llama.cpp, and whether it applies to tools in the functions namespace or only to the prior namespaces container and repo_browser.
- Whether the apply_patch argument key ({patch} vs Codex's {input} vs a raw string parameter) measurably changes patch validity.
- Effect of top_k 0 vs 100 and min_p 0 vs 0.05 on tool-call validity and on sampler speed in llama.cpp (201k vocabulary). Not measured.
- gpt-oss-20b prefill and decode throughput on the owner's RTX 5090 Laptop (MXFP4 or unsloth F16 GGUF, llama.cpp 5e03a1ff4), and the real cost of re-prefill after a CoT drop. Not measured.
- Not executed: whether the llama-server chat path really tokenizes control-token strings inside message content as control tokens. Strong code evidence; to be confirmed with /apply-template plus /tokenize.
- No official guidance on whether tool calls and results emitted on the analysis channel (built-in-style namespaces) should be kept or dropped across user turns.
- Reasoning effort used in the Terminal-Bench 2.0 leaderboard runs for gpt-oss-20b; also, there is no official BFCL v4 overall score for gpt-oss-20b.
- Whether llama-server context checkpoints reliably sit at the CoT-drop divergence point without --swa-full.
- Long-term maintenance of the oss-harmony fork (single maintainer, not on crates.io), and whether upstream openai/harmony will ship 0.0.9 or later.
- How OpenAI's internal training harness formatted tool outputs (plain text vs structured) and whether update_plan or preamble behaviours were trained for gpt-oss specifically.
