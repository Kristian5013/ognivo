# qwen38 findings

- [high] Identity and release: Qwen/Qwen3.8-27B is Apache-2.0, pipeline image-text-to-text, architecture Qwen3_5ForConditionalGeneration, BF16 weights in 18 safetensors shards (~51.7 GiB). Public on HF/ModelScope on 2026-08-14 (repo created 2026-08-05, last modified 2026-08-14). The official Qwen/Qwen3.8-27B-FP8 appeared 2026-08-13. It followed the API-only Qwen3.8-Max launch (blog 'Qwen3.8-Max: A New Bar for Coding and Cowork', 2026-08-02/03). About 7.5M downloads and 18.7K likes by 2026-10-29.
  evidence: https://huggingface.co/api/models/Qwen/Qwen3.8-27B (createdAt 2026-08-05T08:22:59Z, lastModified 2026-08-14T15:00:01Z, tags qwen3_5, license:apache-2.0); github.com/QwenLM/Qwen3.8 README news ('2026-08-14: Qwen3.8-27B is now available on Hugging Face Hub and ModelScope'); https://qwen.ai/blog?id=qwen3.8; https://recipes.vllm.ai/Qwen/Qwen3.8-27B (51.7 GiB BF16)
- [high] Family and variants: the dense 27B is the only Apache-2.0 member.
- Qwen/Qwen3.8-2.4T-A95B (+FP8): open 'Qwen3.8-Max'; 92 layers; 512 experts, 10 routed + 1 shared active; text-only; its template raises an error if thinking is disabled; custom 'qwen3.8-max' license with a separate-license condition above $50M revenue.
- Qwen/Qwen3.8-Flash-Next (+FP8, 2026-08-24): 125B total / 6B active, plus 51B n-gram embeddings and a 4B MTP; Gated DeltaNet + Qwen Sparse Attention + Gated Residual; described as a preview of the Qwen4 architecture; qwen-community-1.0 license; llama.cpp arch qwen4exp.
- The 27B and Flash-Next share a byte-identical chat_template.jinja (sha256 c3cf9e34...).
  evidence: HF API search author=Qwen 'Qwen3.8' (scratchpad/q38/dl/qwen38_list.json); READMEs and templates of Qwen3.8-2.4T-A95B and Qwen3.8-Flash-Next (scratchpad/q38/dl/); sqmagazine.co.uk 2026-08-14 (license clause); llama.cpp issues titled 'qwen4exp (Qwen3.8-Flash-Next)'
- [high] Architecture: 64 layers laid out as 16 × (3 × (Gated DeltaNet → FFN) → 1 × (Gated Attention → FFN)).
- Gated DeltaNet: 48 V heads and 16 QK heads, head_dim 128, conv kernel 4, SSM dtype fp32.
- Gated attention: 24 Q / 4 KV heads, head_dim 256, output gate on; RoPE on 64 dims (partial factor 0.25), interleaved mRoPE sections [11,11,10], theta 1e7.
- Hidden 5120; FFN 17408 with SiLU; vocab 248,320 (padded); untied embeddings.
- One MTP layer 'trained with multiple steps', without dedicated embeddings.
- Context 262,144 native; YaRN factor 4 reaches ~1M, but static YaRN hurts short texts.
- Vision: 27-layer ViT (hidden 1152, patch 16, spatial merge 2) for images and video.
SGLang states the serving-relevant architecture is identical to Qwen3.6-27B.
  evidence: official config.json and README 'Model Overview' (scratchpad/q38/dl/official_config.json, official_README.md); https://docs.sglang.io/cookbook/autoregressive/Qwen/Qwen3.8-27B
- [high] Owner's GGUF: unsloth Q4_0, 16,056,478,688 bytes, lfs sha256 ede16c7b36e578ca...
- Metadata: architecture qwen35, block_count 65 (64 + nextn), nextn_predict_layers 1, full_attention_interval 4, ssm inner_size 6144 / state 128 / groups 16 / dt_rank 48, context_length 262144.
- Tensors (866): 352 Q4_0, 48 Q5_K ssm_out, 8 Q4_1, output Q6_K, MTP eh_proj Q8_0; imatrix from unsloth calibration.
- It already contains the MTP head (blk.64 nextn.* tensors, 0.25 GiB).
- Sampling metadata: top_k 20, top_p 0.95, temp 1.0.
- The MTP sidecar mtp-Qwen3.8-27B-Q4_0.gguf (1.37 GB, 18 tensors) holds only blk.64 plus its own Q3_K token_embd/output; despite the name its tensors are Q6_K/Q4_K/Q3_K.
- Snapshot 4ca72078 also offers mmproj BF16/F16 (0.93 GB) and 27 quants.
- A 65-block GGUF needs a llama.cpp build that knows the qwen35 nextn layer; older builds fail with missing blk.64.ssm_conv1d.
  evidence: GGUF headers parsed with scratchpad/q38/dump_meta.py and scratchpad/q38/tensors.py; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/.cache/huggingface/download/*.metadata and trees/4ca720788d1e01f1bff70c033e0d0028fd02e502.json; Reddit report of the missing tensor via web search
- [high] Memory math.
- Full-attention KV: 16 layers × 4 KV heads × 256 × (K+V) = 64 KiB/token at f16 (~34 KiB at q8_0). At 131,072 tokens that is 8 GiB f16 or 4.25 GiB q8_0.
- Recurrent state per sequence: 48 layers × (48×128×128 SSM + 3×10240 conv) in fp32 = 149.6 MiB, independent of context length. llama-server logs 'size = 149.626 MiB' per checkpoint; SGLang gives 153.9 MB fp32 / 78.4 MB bf16.
- GPU-resident weights of the Q4_0 file: 14.03 GiB. llama.cpp always keeps token_embd (0.67 GiB) on the CPU; the MTP layer adds 0.25 GiB.
  evidence: computed from config.json and the tensor table; llama.cpp #30502 logs; SGLang cookbook 'Mamba ratio' section; src/llama-model.cpp:1670-1672 ('there is very little benefit to offloading the input layer, so always keep it on the CPU')
- [high] Special tokens.
- CONTROL: <|endoftext|> 248044, <|im_start|> 248045, <|im_end|> 248046, object_ref/box/quad 248047-248052, vision 248053-248057, FIM/repo/file_sep 248060-248065, audio/tts 248070-248076. The FIM tokens are special=false in HF but CONTROL in the GGUF.
- USER_DEFINED (special=false in HF): <tool_call> 248058, </tool_call> 248059, <tool_response> 248066, </tool_response> 248067, <think> 248068, </think> 248069.
- 243 unused [PAD] tokens at 248077-248319.
- EOS is 248046; generation_config eos_token_id is [248046, 248044].
- HF pad is <|endoftext|>; the GGUF pad is 248055.
- add_bos_token false, bos_token null; llama.cpp adds no BOS for this BPE vocab.
- llama.cpp's EOG set is {248046, 248044, 248063, 248064, 248065}.
  evidence: GGUF token_type table (scratchpad/q38/main.special_tokens.json); official tokenizer_config.json and generation_config.json (scratchpad/q38/dl/); src/llama-vocab.cpp:3010-3060 (EOG list incl. fim_pad/rep/sep) and add_bos default false for BPE
- [high] Token-level shape of the format, measured on the owner's GGUF with llama-tokenize.
- XML tags are plain multi-token text, and tag/name boundaries are not token boundaries: '<function=read>' = [27 '<', 1628 'function', 86779 '=read', 29 '>'], but '<function=edit>' = [27, 1628, 28 '=', 3468 'edit', 29]. '<parameter=path>' = [27, 15704, 79114 '=path', 29]; '</parameter>' = [510, 15704, 29].
- Newline pieces: 198 = one LF, 271 = two LF, 1358 = three LF.
- Thinking generation prompt: [248045, 74455, 198, 248068, 198]. Non-thinking: [248045, 74455, 198, 248068, 271, 248069, 271]. Canonical end of reasoning: [198, 248069, 271]. Parallel-call separator: [248059, 198, 248058].
- Emoji and box-drawing split into incomplete-UTF-8 tokens: the rocket emoji is 3 tokens, '└' is 2, '⚠' is 3.
- Russian costs about 2-3 characters per token ('Привет' = 3 tokens).
  evidence: /home/kristian/Рабочий стол/llama.cpp/build/bin/llama-tokenize (vocab-only, CUDA hidden) on the Q4_0 GGUF with fixtures scratchpad/q38/s1.txt-s6.txt
- [high] Special-token injection cannot be prevented with llama.cpp's tokenizer.
- With parse_special=false llama.cpp skips only CONTROL/UNKNOWN tokens, so all six USER_DEFINED tags are still matched: '<tool_call>' becomes 248058 and '</think>' becomes 248069, while '<|im_end|>' splits into 5 pieces.
- HF tokenizers also match non-special added tokens unconditionally.
- llama-server's chat path tokenizes the whole rendered prompt with parse_special=true, so CONTROL tokens in file contents are injected too.
- Safe content encoding needs an encoder with the added-token pass disabled.
  evidence: src/llama-vocab.cpp:3400-3411; llama-tokenize --no-parse-special on scratchpad/q38/s2.txt; llama.cpp feature request #26273 (escape special tokens in user/tool input; closed stale)
- [high] Exact prompt format of the official template (sha256 c3cf9e34...).
- System turn: optional effort sentence plus a blank line; then '# Tools\n\nYou have access to the following functions:\n\n<tools>'; then one JSON object per line (tojson, ensure_ascii false, ', ' and ': ' separators); then '\n</tools>'; then fixed XML instructions and an <IMPORTANT> reminder; then a blank line and the trimmed user system prompt.
- User, system and tool content are trimmed.
- Consecutive tool results become one user turn: '<|im_start|>user' + '\n<tool_response>\n' + output + '\n</tool_response>' per result + '<|im_end|>\n'. No names, no ids.
- Assistant history: '<think>\n' + reasoning.trim() + '\n</think>\n\n' + content.trim() + calls. Two newlines precede the first call only if content is non-empty; later calls are preceded by one newline. String args are raw; other types use tojson.
- Generation prompt: '<think>\n', or '<think>\n\n</think>\n\n' when thinking is off.
- A system message anywhere but first raises 'System message must be at the beginning.'
  evidence: scratchpad/q38/dl/official_chat_template.jinja; renders with Python Jinja2 via scratchpad/q38/render.py (default/medium/nothink/nopreserve/notools)
- [high] Reasoning controls.
- reasoning_effort takes xhigh (default), medium or low.
- xhigh and low prepend a fixed sentence as the very first text of the system turn; medium adds nothing.
- Any other value, including 'high', raises in the official template; Unsloth's GGUF template maps high to xhigh.
- With enable_thinking=false the sentence is dropped, so medium and thinking-off render identical system turns.
- preserve_thinking now defaults to on: when undefined, every past reasoning block is kept. With false, assistant turns at or before the last real user query lose the think block entirely (not an empty one).
- The card calls preserved thinking 'especially beneficial for agent scenarios' and says it 'also improves KV cache utilization'.
  evidence: official template effort and preserve blocks; README sections 'API Usage' and 'Disable Preserved Thinking'; render.py outputs for medium vs nothink vs nopreserve
- [high] Template changes from 3.6 to 3.8:
- added the reasoning_effort block;
- flipped the preserve_thinking default from off to on;
- removed the extraction of '</think>' from inside content, so reasoning must arrive as reasoning_content; otherwise content that contains <think> gets wrapped twice;
- added a guard tool_call.arguments != ''.
The 2.4T template differs: text-only, and it rejects disabling thinking.
  evidence: diff of scratchpad/q38/dl/qwen36_27b_chat_template.jinja vs official_chat_template.jinja; scratchpad/q38/dl/Qwen3.8-2.4T-A95B.chat_template.jinja; froggeric Qwen-Fixed-Chat-Templates README ('duplicate blank <think></think>')
- [high] Official sampling and output length.
- Thinking mode: temperature 1.0, top_p 0.95, top_k 20, min_p 0, presence 0, repetition 1.0.
- Non-thinking mode: 0.7, 0.80, 20, 0, presence 1.5. Presence 0-2 can reduce endless repetition; higher values cause language mixing.
- Output length: reasoning up to 262,144 tokens and the final answer up to 131,072, for frameworks that support 1M context.
- Qwen's own agentic evals used the Claude Code harness, temp 1.0, top_p 0.95 and 256K context. QwenSWEBench: max_tokens 32,768, avg@3, 8-hour timeout.
- llama.cpp applies the GGUF's general.sampling.* values as defaults (top_k 20, top_p 0.95, temp 1.0) but keeps its own min_p 0.05.
  evidence: official README 'Best Practices' and benchmark footnotes; generation_config.json; llama.cpp common/common.cpp:1124-1146 and common/common.h:235
- [high] llama.cpp routes the Qwen3.8 template to common_chat_params_init_qwen3_coder (PEG native format), because it contains <tool_call>, <function= and <parameter=. Grammar of that parser:
- reasoning: optional '<think>', then text until '</think>' or '<tool_call>';
- content: until '<tool_call>';
- string args: until '\n</parameter>\n'; non-string args are validated against the JSON schema;
- required args in any order, optional args only after them;
- each call ends with '</function>\n' then '</tool_call>';
- lazy grammar with WORD triggers;
- thinking_end_tags {'\n</think>', '</think>', '<tool_call>'} feed the server reasoning budget.
vLLM uses --tool-call-parser qwen3_coder (newer releases: qwen3_xml) plus --reasoning-parser qwen3; MTP via speculative-config method mtp with 3 tokens.
  evidence: llama.cpp common/chat.cpp:1238-1245; common/parsers/qwen3-coder.cpp; https://recipes.vllm.ai/Qwen/Qwen3.8-27B; froggeric README (qwen3_xml)
- [high] Reference parsers end a value at the first plausible tag.
- SGLang ends a value at the first '</parameter>', '<parameter=' or '</function>', strips one leading and one trailing newline, turns 'null' into None for any type (strings included), and converts other values by schema.
- llama.cpp ends a value at the first '\n</parameter>\n'.
A file whose content contains such a sequence is silently truncated by both. The format has no escaping.
  evidence: scratchpad/q38/src/sglang_qwen3_coder_detector.py (_convert_param_value, parse_streaming_increment, tool_call_parameter_regex); qwen3-coder.cpp arg_string rule
- [high] llama.cpp chat-path defects that hit this model.
Open at the pin:
- #30357: reasoning/content whitespace is not round-trippable, so the prefix misses and the hybrid model re-prefills fully. Fix PR #30557 is still open; a maintainer warns that stripping whitespace before <tool_call> degrades tool calling.
- #27733: a stray '<think>' after complete tool calls fails the final PEG parse and the server cancels the whole turn. PR #28914 closed unmerged.
- #29947 (#27367 likewise): a non-leading system message causes a Jinja exception and HTTP 500 (opencode plugins, Claude Code hooks).
- #31342: tools plus response_format returns 400.
Closed:
- #27661 / #28679 (not planned): a literal '</think>' inside reasoning ends the reasoning, because parsing works on strings rather than token 248069.
- #27615: with 139 Claude Code tools, the lazy-grammar WORD-trigger regex search is O(n²) and cut decode from 55 to 13 tok/s.
- #28522: the grammar kept only 1 of 48 optional parameters.
  evidence: GitHub issues fetched via api.github.com and exa (scratchpad/q38/gh/issue_30357.json, issue_27733.json, etc.); PR #30557 discussion (pwilkin comment)
- [high] Hybrid caching in llama-server at the pin.
- qwen35 is hybrid and supports bounded recurrent-state (rs) rollback, but n_rs_seq is non-zero only with draft-mtp/eagle3/dflash/dspark, where it equals the draft n_max.
- Prompt reuse is prefix-only. After a divergence the server restores the newest checkpoint whose pos_max is at or before the divergence point, or logs 'forcing full prompt re-processing'.
- Checkpoints are created only during prompt processing: 4 + n_ubatch and 4 tokens before the prompt end, plus user-message starts on the chat path. None are created during generation.
- Defaults: --ctx-checkpoints 32, --checkpoint-min-step 8192, --cache-ram 8192 MiB, --cache-idle-slots on.
- Raw /completion requests carry no message spans.
- On raw /completion the server reasoning budget never sees a prefilled <think>: generation_prompt is set only by the chat session.
  evidence: src/llama-arch.cpp:1214-1270; src/llama-context.cpp:192; common/common.h:401 and 636-642; tools/server/server-context.cpp:3700-3905 and 3983-4120; common/sampling.cpp:284-327; common/chat.cpp:1741
- [medium] RAM and correctness costs of the prompt and checkpoint machinery:
- Without MTP a checkpoint is 149.6 MiB.
- With draft-mtp the draft KV is added, about 4 KiB/token, or ~1.2 GiB per checkpoint at 262K (#30502, closed; the pin's create_checkpoint still calls update_dft unconditionally).
- --cache-ram -1 is unbounded, and every saved entry carries the ~150-300 MiB state (#29324, open).
- Under concurrent load the RAM prompt cache can restore an unrelated conversation into a fresh slot (#27148, open).
- Speculative decoding kept tokens past the EOG and so lost the previous answer on hybrids (#28049, closed).
- An opt-in int16 checkpoint codec would halve RAM (#27211, stale).
  evidence: GitHub issues #30502, #29324, #27148, #28049, #27211; tools/server/server-context.cpp:2598-2600
- [medium] MTP in llama.cpp.
- --spec-type draft-mtp uses the main GGUF's nextn head ('an MTP context runs on the weights of the main model') or a -md sidecar. --spec-draft-n-max defaults to 3. The draft context is sized like the target, and its KV is f16 by default.
- Measured: desktop RTX 5090 Q4_K_M went from 69.5 to 90.6 tok/s at n=3 (+30%); n=4 was worse. A community 'Q4-MTP' run reaches about 48 tok/s on an RTX 5090 Laptop. Acceptance was 0.52 in one heavy agent log versus 0.75-0.90 on vLLM.
- Failure reports: decode fell to ~0.5 tok/s after ~10 min of agent traffic on August builds b10362/b10472; 0% acceptance with the sidecar after an update (#30782); greedy output diverges on quantized targets (#25618); -fit did not reserve MTP VRAM (#28172).
- ggml-org also ships DFlash2 drafters (dflash-Qwen3.8-27B-Q4_0/Q8_0/BF16).
  evidence: common/common.cpp:1245-1275; common/speculative.cpp:2645-2680; docs/speculative.md; medium.com (Sepici, 2026-08-23); llm-bench.io/hardware/rtx-5090-laptop; erfi.dev/reference/qwen38-agentic-tuning; issues #27733 (draft acceptance log), #30782, #25618, #28172; HF API ggml-org/Qwen3.8-27B-GGUF
- [medium] Long-context reliability in llama.cpp is doubtful.
- Qwen3.8-27B silently emits EOS as the first token once the prompt exceeds ~130K positions, on both CUDA and CPU. Qwen3.5-35B-A3B is fine at 243K. The issue was closed as stale/not planned (#27756).
- The CPU backend produces repetition garbage for qwen35 while Vulkan is correct (#30906, open).
- Decode on an RTX 3090 falls from 36.4 tok/s at 2K to 20.9 at 120K.
  evidence: llama.cpp #27756 (closed not_planned 2026-10-23), #30906; bittide.aicompass.dev RTX 3090 + OpenCode report
- [high] Official benchmarks (Qwen; coding rows run with the Claude Code harness):
- Terminal-Bench 2.1 (Terminus) 73.0, vs Qwen3.6-27B 63.4 and Opus 4.6 Max 78.2
- SWE-bench Pro 61.7
- NL2Repo 42.3
- DeepSWE 1.1 42.2
- QwenSWEBench 79.0
- CoWorkBench 70.7
- IFBench 79.5
- GPQA-D 89.2
- HLE 30.8
- LiveCodeBench v6 90.3
- OSWorld-Verified 84.3
- SWE-MM 38.6
Qwen publishes no official SWE-bench Verified, tau2 or BFCL scores.
  evidence: official README tables (scratchpad/q38/dl/official_README.md, extracted with scratchpad/q38/html_tables.py)
- [medium] Independent results.
- vals.ai SWE-bench Verified: 86.0% ±1.55 (mini-swe-agent, bash-only; 2026-08-20 sweep; rank 15 of 88; about 1.99M tokens and 10 minutes per task). For comparison: Qwen3.6-27B 70.0, gpt-oss-120b 33.6.
- vals Terminal-Bench 2.1 (Terminus 2): 58.4%, versus the vendor's 73.0.
- Artificial Analysis Intelligence Index v5: 20 at xhigh, rated 'very verbose' (130M output tokens vs a 84M median), 36.6 tok/s on the hosted API.
- Community BFCL non-live categories (NInfer NVFP4): 68-76.5%; HumanEval 0.598 greedy.
  evidence: https://www.vals.ai/benchmarks/swebench; chatoss.ai top-40 table; https://artificialanalysis.ai/models/qwen3-8-27b; erfi.dev
- [medium] Reasoning-effort evidence:
- VulcanBench (23 real merged PRs, DashScope): pass@1 low 82.6%, medium 82.6%, xhigh 73.9%. xhigh's median time was 17.1 min vs 6.8-7.1, and all its failures were time-outs.
- HF discussion #194: low and medium used similar tokens (4,984 vs 4,792); xhigh used 36,188 (8×) for +0.6 on the median score.
- engineerprompt: mean thinking tokens 22.1K / 27.8K / 42.7K for low / medium / xhigh. The settings bind weakly and inversions happen.
- erfi: medium was quality-neutral against xhigh on a task suite. One xhigh run took 1308 s on a task another run finished in 19.7 s. At medium, generation length was p95 7,819, p99 9,201, max 9,564 tokens.
- Kodesage (codebase Q&A): medium 8.18, xhigh 8.61, low 7.79; low was also slower than medium.
- The card warns that lower effort does not always reduce total agentic time.
  evidence: https://vulcanbench.com/benchmarks/17-qwen38-27b-effort.html; https://huggingface.co/Qwen/Qwen3.8-27B/discussions/194; https://engineerprompt.ai/writing/qwen-reasoning-budget/data/full-report.md; erfi.dev; https://kodesage.ai/blog/qwen-3-8-27b-llm-model-review; official README tip
- [medium] Community agent failure modes:
- Tool calls emitted inside the still-open thinking block.
- 'Announce then stop' turns: text like 'Now let me write the full Rust implementation.' and then end_turn with no call. This was a Qwen3.6 issue; Qwen maintainers attribute it to calls emitted before '</think>'. It is reported to persist in 3.8 (HF #68: 'tool call frequently fails in long agentic tasks').
- Histories with empty thinking blocks (clients that don't send reasoning back) degraded 3.6 agents; preserving the reasoning fixed it.
- With modified templates: Hermes-JSON drift inside <tool_call> and duplicate tool calls.
- Overthinking complaints (HF #136).
- In one RTX 3090 + OpenCode run, 1 of 4 tasks spent its whole 8K output budget on reasoning and produced no patch.
  evidence: https://github.com/QwenLM/Qwen3.8/issues/150; huggingface.co/Qwen/Qwen3.8-27B/discussions/68 and /136; froggeric discussions 23/25 and README; allanchan339.github.io 2026-05-02 post; bittide.aicompass.dev report
- [medium] Measured speed data points (llama.cpp):
- RTX 3090 (936 GB/s), Q4_K_M, q8_0 KV, 131K context: decode 36.4 / 33.5 / 25.9 / 20.9 tok/s at 2K / 16K / 65K / 120K; fresh prefill ~750-960 tok/s; cached-continuation TTFT ~0.5 s; peak VRAM 22.2 GiB including the desktop.
- llm-speed (build 11803): 3090 38.6, 4090 43.4, desktop 5090 75.2 tok/s.
- Desktop RTX 5090: pp1024 about 2,300 tok/s.
- RTX 5090 Laptop: Qwen3.6-27B GGUF 33-36 tok/s (April); Qwen3.8 'Q4-MTP' about 48 tok/s; custom NVFP4 + speculative builds 74-78 tok/s.
  evidence: bittide.aicompass.dev (3090 OpenCode); https://llm-speed.com/m/qwen3-8-27b; llama.cpp #28196; https://airelien.dev/en/posts/qwen36-27b-blackwell-mobile/; llm-bench.io; safaia.net/lifeidle repo
- [low] ESTIMATE, not measured, for the owner's RTX 5090 Laptop (24 GB, 896 GB/s), Q4_0 at the pin, Linux, one slot:
- decode: ~36-42 tok/s at ≤16K, ~28-32 at 64K, ~21-25 at 120K;
- with MTP n=2-3: ~45-60 tok/s, if MTP behaves;
- prefill: ~1,100-1,600 tok/s at ≤16K, ~700-1,000 at 64K, ~500-700 at 120K.
Practical effect: a medium-effort step with ~3K reasoning tokens takes ~80-100 s of decode; a 30K-token xhigh binge takes 13+ minutes; a cold 30K-token start takes ~20-30 s.
  evidence: Scaled from the 3090 (similar bandwidth) and the desktop/laptop data points above; bandwidth ceiling ≈ 896 GB/s ÷ 14.0 GiB resident ≈ 60 tok/s
- [medium] VRAM fit (estimate).
- Budget: Q4_0 weights 14.03 GiB on GPU + q8_0 KV 4.25 GiB at 131,072 + 0.15 GiB recurrent state + 1-2 GiB compute ≈ 20.5-21.5 GiB. That fits 24 GB with ~2 GiB headroom.
- f16 KV fits about 96K.
- MTP adds ~0.25 GiB of weights, ~0.45 GiB of rollback snapshots and ~0.27-0.5 GiB of draft KV.
- A community 3090 run (Q4_K_M, q8_0, 131K) peaked at 22.2 GiB including the desktop.
- Avoid q4/q5 KV: prefill collapses on qwen35 (#27109), and without GGML_CUDA_FA_ALL_QUANTS attention silently falls back to the CPU (#28455).
  evidence: computed from tensor table and config; bittide report; llama.cpp #27109, #28455
- [medium] Harness alignment: Qwen ran SWE-bench Pro, NL2Repo, DeepSWE, QwenSWEBench, Vision2Web and SWE-MM through the Claude Code harness. The card also claims 'broader support for popular harnesses', and Unsloth added developer-role support for Codex. Claude-Code-shaped tools (Bash, Read, Edit with old_string/new_string, Write, Glob, Grep, TodoWrite) are therefore the most in-distribution tool surface.
  evidence: official README highlights and benchmark footnotes; unsloth/Qwen3.8-27B-GGUF README (scratchpad/q38/dl/unsloth_README.md)
- [medium] Quant provenance. On 2026-08-19 unsloth deleted the plain K-quants and re-uploaded dynamic quants, which broke configs that named the old files. ggml-org/Qwen3.8-27B-GGUF ships Q4_K_M, Q8_0 and BF16 plus mtp- and dflash- sidecars (DFlash source: incoai/Qwen3.8-27B-DFlash2). The owner's file comes from unsloth snapshot 4ca72078, last modified 2026-08-20.
  evidence: erfi.dev; HF API for ggml-org/Qwen3.8-27B-GGUF and unsloth/Qwen3.8-27B-GGUF; ggml-org README; local HF cache metadata
- [medium] Thinking-budget mechanics. Qwen's documented early-stop text (Qwen3 docs) is '\n\nConsidering the limited time by the user, I have to give the solution based on the thinking directly now.\n</think>\n\n'. The Qwen3 report says the ability 'is not explicitly trained but emerges'. Community measurements argue that caps below about the p99 reasoning length cut reasoning that was converging.
  evidence: github.com/QwenLM/Qwen3 docs/source/getting_started/quickstart.md 'Thinking Budget'; Qwen3 Technical Report §4.3; erfi.dev
