# Review: correctness and robustness of crates/ognivo-models (llama.rs, engine.rs, doctor.rs, caps.rs, profile.rs) and crates/ognivo-cli

12 concrete problems: 5 major, 7 minor. I checked them against the pinned llama.cpp source (@12cc59ba4), the ik_llama.cpp checkout (@390b4445) and the M0 probe JSONs. For six of them I also ran the actual doctor binary (built into a separate target dir, so the repo is untouched) against a fake llama-server: /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/fake/fake_llama.py. Confirmed live: a wrong or missing API key and a `--url …/v1` both produce 'ik_llama.cpp · class C · family not recognised' with exit 0; a router endpoint reports n_ctx 0 / family not recognised; a mid-stream `{"error":…}` is swallowed without a warning; an ASCII-only reply on an old build gives a false class A; `--probe` hangs forever on a stuck /completion; the canonical gpt-oss template, as llama-server reports it, is flagged 'not in the vetted list'. The template mismatch comes from llama.cpp's lexer stripping one trailing newline plus the gpt-oss raise-check patch. That same normalisation reproduces M0's Gemma hash 845f1ee… → aa3185df… exactly, so the M0 note that the server uses its own Gemma template is a misreading. What holds up: no unwrap or overflow panics outside tests (only checked u32 conversions); the SSE parser handles CRLF, lines split across chunks, `:` pings and streams that close without [DONE] (/completion never sends [DONE]); stream-id counting on the pinned build is consistent (EOG id is streamed and counted in timings.predicted_n; ids still pending on a UTF-8 boundary arrive in the final payload and are counted before Done). Note: doctor.rs and profile.rs were being edited by another agent during the review (template checks were added); line numbers refer to the versions with md5 19c1e5ec… (doctor.rs) and 5ce077de… (profile.rs).

## [major] A failed /props is reported as 'ik_llama.cpp, class C'; auth and URL mistakes end with exit 0
`crates/ognivo-models/src/doctor.rs:130`

**Problem.** When GET /props fails, diagnose() swaps in Props::default(), and ServerKind::from_build_info(None) (caps.rs:18-20) turns the missing build_info into IkLlama, which classify() maps to class C. detect_profile (doctor.rs:198) throws away every /tokenize error via `if let Ok`, so the root cause is reported as 'model family not recognised'. Both notes are Warn, so has_errors() stays false and main.rs:66 exits 0. In upstream llama-server only /health and /v1/health skip the API-key check (llama.cpp@12cc59ba4 tools/server/server-http.cpp:252-259); /props, /tokenize and /completion return 401. ik works the same way (examples/server/server.cpp:667-673).

**Failure scenario.** Reproduced with the built binary against a fake server that copies upstream's middleware. (a) The server runs with --api-key and the user calls `ognivo doctor` without --api-key / OGNIVO_API_KEY. Output: 'server ik_llama.cpp · class C', '! model family not recognised: only the Generic path…', exit 0, for an upstream b11924 server. (b) `--url http://127.0.0.1:8080/v1` (the usual OpenAI base URL). Upstream answers /v1/health but has no /v1/props and no /v1/tokenize, so the verdict is the same, exit 0. Any non-llama HTTP service that answers 200 on /health ends the same way.

**Fix.** Keep a failed /props separate from 'no build_info': set kind to None (or ServerKind::Unknown) unless /props returned 200 with a JSON object, and infer ik only from a successful /props that lacks build_info. Treat 401/403 from any endpoint as Level::Error ('authentication failed: pass --api-key or OGNIVO_API_KEY') and stop there, which also makes the exit code non-zero. Make detect_profile return Result and report /tokenize HTTP errors instead of 'not recognised'. Strip a trailing /v1 from --url, or reject it with a hint.

## [major] Server errors in the middle of a stream, and streams that end without a final payload, pass as normal
`crates/ognivo-models/src/engine.rs:194`

**Problem.** Once the 200 headers are out, upstream (tools/server/server-context.cpp:5435-5439, format_error → `data: {"error":{…}}`) and ik (examples/server/server.cpp:1266-1268) report failures as an SSE data payload `{"error":{…}}` and then close the connection; /completion never sends [DONE]. token_events() finds no tokens, content or stop in that object and emits nothing. completion_stream treats EOF as a normal end (llama.rs:196) and also drops any half-received line left in buf. So RawEngine::complete yields neither an Err nor a Done. probe_stream_ids then turns 'no Done' into Missing (doctor.rs:305), or silently into NotProbed (doctor.rs:306), with no finding.

**Failure scenario.** Reproduced: a fake upstream b11924 streams 3 ids, then `data: {"error":{"code":500,"message":"Compute error."}}`. `ognivo doctor --probe` prints no warning at all and says 'known build with complete token-id streaming (run with --probe to verify)' although --probe was given; exit 0. If the decode fails during prefill, after upstream's begin marker has already sent 200 (server-context.cpp:4229), no ids arrive: the result is Missing, then class C and the message 'server streams no token ids (ik_llama.cpp)' for an upstream server. In the agent, a generation cut off by 'Compute error.' looks like a normal end of stream, which breaks the rule in architecture §6 that truncated calls are never executed.

**Fix.** In token_events, turn a payload that has an `error` object into Err(EngineError::Status{code, message}). In RawEngine::complete, track whether a `stop:true` payload arrived and, if not, yield Err(Protocol("stream ended without final payload")) at EOF; also return an error if buf still holds bytes at EOF. In probe_stream_ids, treat predicted == None as an error, never as Missing or NotProbed. When --probe was requested and failed, don't print '(run with --probe to verify)', and make the failure an Error so the exit code shows it. Build the class C message from the detected server kind instead of hard-coding '(ik_llama.cpp)'.

## [major] On ik_llama.cpp every stop reason becomes Stop::Other("unknown")
`crates/ognivo-models/src/engine.rs:209`

**Problem.** token_events reads only `stop_type`. ik_llama.cpp's final /completion payload has no stop_type field; it carries the booleans stopped_eos, stopped_word and stopped_limit instead (examples/server/server-context.cpp:2607-2626). The M0 probes confirm it: bench/m0/probes/ik-gpt-oss.json and ik-gemma4.json record "stop_type": null.

**Failure scenario.** On every class C server, every generation ends with Done{stop: Other("unknown")}. The Native path, which runs on ik in degraded mode, cannot tell EOG from an n_predict cut. It must then either refuse every tool call or execute calls that may be truncated, and the latter breaks architecture §6 ('never after truncation by n_predict').

**Fix.** When stop_type is missing, fall back to the booleans: stopped_eos → Eog, stopped_word → Word, stopped_limit → Limit (also treat `truncated: true` as Limit). Add a unit test with ik's final payload shape: content "", generated_text, stopped_*, timings.predicted_n.

## [major] No timeout on the /completion stream and 600 s per /tokenize: `doctor --probe` can hang forever
`crates/ognivo-models/src/llama.rs:166`

**Problem.** completion_stream sets no request timeout, and the client has only connect_timeout(5s) (llama.rs:68). Upstream sends the 200 headers only when a slot actually starts the task (begin marker, server-context.cpp:4229), so send() waits for as long as the slot is busy. After that, bytes.next() has no idle limit, and ik sends no SSE pings at all. /tokenize goes through post() with a 600 s timeout (llama.rs:106), and diagnose makes up to 28 /tokenize calls in a row (3 for detection, 2 per structural token, 1 for the probe). main.rs checks the URLs one after another, so one stuck server blocks the rest.

**Failure scenario.** Reproduced: against a fake server that accepts /completion and never answers, `ognivo doctor --probe` never returns (killed by `timeout 20`, exit 124). Real triggers: a -np 1 server busy with another session's long generation, a runaway generation with n_predict -1, or a stalled LAN host (every metadata call then waits 600 s, hours in total). On a server started with --sleep-idle-seconds, /tokenize also blocks in wait_until_no_sleep (server-context.cpp:5166-5172) and quietly reloads the model into VRAM; /props reports is_sleeping, but the doctor ignores it.

**Fix.** Use a timeout per request type: about 10–15 s for /health, /props, /tokenize and /detokenize; a total `.timeout(~120s)` for the doctor's probe; and ClientBuilder::read_timeout(~90s) for agent streams, above upstream's default 30 s SSE ping interval. In the CLI, wrap each diagnose() in tokio::time::timeout. Before probing, check /slots (on ik, /health slots_processing) and report 'server busy' instead of queueing. Read /props.is_sleeping and warn that the check will wake the model.

## [major] The template check compares raw file hashes, but llama-server reports a normalised template, so canonical templates are never recognised
`crates/ognivo-models/src/doctor.rs:374`

**Problem.** check_template looks up sha256(/props.chat_template) in `known`, whose canonical entries are hashes of the committed files. llama-server and ik both change the template before /props reports it. The jinja lexer normalises CR/CRLF and drops one trailing \n (llama.cpp common/jinja/lexer.cpp:44-58), and common_chat_template keeps the lexer's output (chat.cpp:1344). common_chat_templates_init also rewrites the gpt-oss raise-check line to `{%- if false %}` (chat.cpp:839-848; ik common/chat.cpp:625). Applying exactly these steps to the committed files reproduces the M0 measurement: Gemma's GGUF template 845f1ee… becomes aa3185df…. So the M0 note that the server uses its own Gemma 4 template is wrong: the server only strips the trailing newline.

**Failure scenario.** The owner follows the doctor's own advice (`--chat-template-file …/canonical.jinja`). /props then reports b2215de6da8b… for gpt-oss (patched) and 6a1015c47ccf… for Gemma (trailing newline removed). Neither hash is in `known`, so the doctor says 'chat template not in the vetted list' for 2 of the 3 reference models; only Qwen, whose file has no trailing newline and needs no patch, is recognised as canonical. Reproduced with the built binary against a fake /props serving the transformed gpt-oss canonical. Separately, the advice and recommended_flags use a repo-relative path (profiles/templates/…). llama-server resolves it against its own working directory, and the templates are not embedded in the binary, so the copied command fails anywhere except the repo root.

**Fix.** Compare hashes of the server-normalised form. Add a helper that repeats llama.cpp's transforms (CR/CRLF→LF, drop one trailing \n, the gpt-oss raise-check substitution), and make canonical_templates_match_their_pins assert that the hash of normalize(canonical.jinja) is listed as canonical. Concretely, add b2215de6da8ba369957eece8c5aa18f4af94f6c4d311d85e691373a421d80e89 (gpt-oss) and 6a1015c47ccfcfa67c3b772385bccee357a4d37c3cda37bd202e9047f391ab82 (Gemma) with verdict canonical. Print an absolute path to the template, or embed the templates and write them to ~/.ognivo/templates.

## [minor] Within a family, the first profile in file order wins on a match anywhere in the full path, and the fallback is silent
`crates/ognivo-models/src/doctor.rs:209`

**Problem.** detect_profile keeps every profile whose probe id matches. It then takes the first one, in BUILTIN order, that has any hint contained anywhere in the lowercased model_path, directories included. If no hint matches, it silently takes matches.first(). Hints such as 'gpt-oss', 'gemma-4' and 'qwen3.5' cover a whole family. Everything profile-specific (working_max, recommended_flags, the --chat-template-file advice, the list of known templates) is then shown as if it had been checked for that model.

**Failure scenario.** A Qwen3.5-35B-A3B GGUF has the same vocabulary (<tool_call> = 248058), so the hint 'qwen3.5' gives it the qwen3.8-27b profile. Its template is reported as not vetted, and the doctor tells the user to load profiles/templates/Qwen3.8-27B/canonical.jinja into a Qwen3.5 server. A gpt-oss-120b server gets gpt-oss-20b through the hint 'gpt-oss', together with `-np 2 --kv-unified --swa-full`. Even after a gpt-oss-120b profile is added behind gpt-oss-20b, the 20b profile still wins for 120b paths because its broad 'gpt-oss' hint is checked first. A directory named after another model also counts as a match.

**Fix.** Match hints against the GGUF file name (basename of model_path) and /props model_alias, and pick the candidate with the longest matching hint across all candidates, not the first in file order. If no specific hint matches, keep the family profile but add a Warn ('profile chosen by tokenizer family only') and leave out model-specific flags and template advice. Add a test with two profiles of the same family.

## [minor] A stream probe that never hits a split UTF-8 character still counts as 'Complete' and overrides the class from the build
`crates/ognivo-models/src/doctor.rs:298`

**Problem.** probe_stream_ids reports Complete whenever received == predicted. It never checks that the generated text actually contained tokens ending in incomplete UTF-8, which is the only case where an unfixed build loses ids. classify() gives the probe result priority over the build (caps.rs:59-61). A count where more ids arrive than were predicted is also labelled Lossy.

**Failure scenario.** Reproduced: a fake old build b11681-5e03a1ff4 answers in plain ASCII, and the doctor prints 'class A' and '✓ every sampled token id is streamed: verbatim token replay works'. Possible real triggers on an arbitrary server: a server-wide --grammar or --json-schema, which a request cannot clear because an empty "grammar" is ignored (server-schema.cpp:291-293); server --reverse-prompt stop strings, which apply again when the request's stop list is empty (server-schema.cpp:466-469); or a same-family model that paraphrases instead of repeating the emoji line. The Native path would then rely on verbatim replay on a server that loses ids.

**Fix.** Collect the streamed text and require that it contains the probe's 4-byte characters (for example '🚀', or any char ≥ U+10000) before the stop. Otherwise return a new StreamIds::Inconclusive that classify() treats like NotProbed. Treat received > predicted as a protocol anomaly rather than Lossy.

## [minor] 'Neutralise every other sampler' misses server defaults that change or break generation
`crates/ognivo-models/src/engine.rs:163`

**Problem.** Upstream starts every request from the server's command-line sampling settings (`params.sampling = params_base.sampling`, server-schema.cpp:488-500). completion_body resets typical_p, the penalties, DRY and XTC, but not mirostat, top_n_sigma, dynatemp_range, adaptive_target or the sampler order. It sends logit_bias only when something is banned, so the EOG −inf biases that --ignore-eos puts into the default logit_bias (common/common.cpp:1366-1370) stay active. The doctor compares only temperature, top_k, top_p and min_p, although /props.default_generation_settings.params also exposes mirostat, ignore_eos, top_n_sigma, adaptive_target and samplers (server-task.cpp:29-80).

**Failure scenario.** On a LAN server started with --ignore-eos, ognivo requests with no banned ids never stop at EOG and always run to n_predict, so no call ever ends with EOG. With --mirostat 2, mirostat replaces the profile's top-k/top-p/temperature sampling. With a custom --samplers list that omits min_p or top_p, the profile's values are silently ignored. The doctor reports none of this.

**Fix.** Always send logit_bias; an empty array clears inherited biases because the handler calls clear() (server-schema.cpp:400-405). Also send ignore_eos:false, mirostat:0, top_n_sigma:-1, dynatemp_range:0, adaptive_target:-1 and an explicit samplers list. In the doctor, add a Warn when /props shows non-neutral values for these fields. Document that the server's default stop strings and grammar cannot be overridden per request.

## [minor] ik_llama.cpp sampling defaults are read from a path that does not exist on ik
`crates/ognivo-models/src/llama.rs:43`

**Problem.** Props::from_json reads default_generation_settings.params.*. ik's /props has no params object: temperature, top_k, top_p, min_p, mirostat and the rest sit directly in default_generation_settings (examples/server/server-context.cpp:2417-2427, via get_formatted_generation).

**Failure scenario.** On ik, default_sampling is all None, so the doctor's sampling comparison never runs and says nothing. scripts/probe/server_caps.py:235 reads the same wrong path, which is why the M0 table shows '—' for ik sampling and bench/m0/probes/ik-*.json store nulls.

**Fix.** When settings["params"] is not an object, read settings[key] directly. Add a test with ik's flat layout (n_ctx is already read from the right place on both servers).

## [minor] Class A depends on an exact 9-character commit prefix: short hashes and future upstream builds are missed
`crates/ognivo-models/src/caps.rs:67`

**Problem.** The commit in build_info comes from `git rev-parse --short HEAD` (llama.cpp cmake/build-info.cmake:21), so its length depends on the clone; a shallow clone gives 7 characters, and c.starts_with("12cc59ba4") fails for "12cc59b". Separately, every other upstream build is classified B without a probe, and the doctor states as fact that its ids 'are lossy' and tells the user to rebuild with PR #31244. Once that PR is merged upstream, a list of commits cannot express 'build ≥ N'.

**Failure scenario.** Someone builds the pinned PR head from a shallow clone and gets build_info 'b1-12cc59b'. The doctor reports class B and says to build llama.cpp with PR #31244, although this is exactly that build. After the fix lands upstream, every newer build keeps getting the 'lossy' warning until someone adds its hash by hand.

**Fix.** Count it as a match when either string is a prefix of the other and the shorter one has at least 7 hex characters; store the full 40-character SHA in the list. Once the PR is merged, add a min_fixed_build threshold. When the server was not probed, phrase class B as 'expected lossy (not verified); run --probe'.

## [minor] llama-server router mode is reported as 'model family not recognised' with n_ctx 0
`crates/ognivo-models/src/doctor.rs:148`

**Problem.** In router mode, upstream /props without ?model= returns {role:"router", model_path:"none", default_generation_settings:{params:{}, n_ctx:0}, build_info} (tools/server/server-models.cpp:2164-2190). Every /tokenize without a "model" field fails with 400 'model name is missing from the request' (router_validate_model, :2092-2097), and detect_profile swallows that error. Architecture §4 lists the router as a supported endpoint type.

**Failure scenario.** Reproduced against a fake router: 'llama.cpp b11924 · class A · n_ctx 0 · slots ?', 'model unknown (generic) · none', '! model family not recognised…', exit 0, while the router is in fact serving gpt-oss.

**Fix.** Detect role == "router". Then either check each model listed by GET /models (add ?model= to GETs and "model" to POST bodies), or report 'router endpoint: choose a model'. Do not print n_ctx 0 as if it were a real value.

## [minor] Both servers reject a Trigger::Word that is a single token, because preserved_tokens is never sent
`crates/ognivo-models/src/engine.rs:186`

**Problem.** If a WORD grammar trigger tokenizes to exactly one token, upstream (common/sampling.cpp:1068-1075) and ik (examples/server/server-context.cpp:1550) throw 'Grammar trigger word should be marked as preserved token' unless that token is listed in preserved_tokens. completion_body never sends preserved_tokens.

**Failure scenario.** A Qwen codec that follows architecture §5.4 ('Qwen — <tool_call>') uses Trigger::Word("<tool_call>"), which is a single token (248058). Every constrained request then fails before any generation. The M0 lazy-grammar check passed only because 'ANSWER:' spans several tokens, and the existing unit test covers only Trigger::Token.

**Fix.** For a Word trigger, also send "preserved_tokens": [word], which is harmless when the word spans several tokens. Alternatively, resolve single-token words to Trigger::Token via /tokenize. Add a request-body test for a Word trigger.
