# llamacpp findings

- [high] Локальный чекаут 5e03a1ff4 (16.10.2026) — линия llama.cpp v0.7.0 (в CMake 0.7.0; релиз v0.7.0 вышел 15.10.2026: смена порта на 9931, chat-слой переписан на common_chat_session, вывод шаблона стал массивом частей). Темп изменений высокий: b11484 (07.10) → b11946 (28.10), ≈460 сборок за 3 недели. После 16.10 появились /v1/realtime (b11946), фикс Jinja select/selectattr (b11869) и фикс output_index в Responses (b11768).
  evidence: /home/kristian/Рабочий стол/llama.cpp (git log -1: 5e03a1ff4, 2026-10-16); /home/kristian/Рабочий стол/llama.cpp/CMakeLists.txt:5-8; https://starstruk.ai/repository/ggml-org/llama.cpp/release/v0.7.0; https://github.com/ggml-org/llama.cpp/releases/tag/b11946 (2026-10-28); https://freedom.tech/project/llama-cpp/
- [high] Возможности /v1/chat/completions: tools; tool_choice; parallel_tool_calls (по умолчанию = caps шаблона supports_parallel_tool_calls, с #22217 от 22.04.2026); стриминг дельт tool_calls (сначала id+name, затем фрагменты arguments; finish_reason=tool_calls|length|stop); response_format json_object/json_schema и llama-поля json_schema/grammar (grammar с tools несовместим); reasoning_format на запрос (none/deepseek/deepseek-legacy/auto); reasoning_content в ответе и во входящих сообщениях; reasoning_effort (none → enable_thinking=false); chat_template_kwargs (сливаются с --chat-template-kwargs); reasoning_budget_tokens/thinking_budget_tokens, reasoning_budget_message, reasoning_control; continue_final_message/echo/add_generation_prompt. Остальные поля /completion (samplers, dry, id_slot, cache_prompt, return_progress, timings_per_token) пробрасываются как есть.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:1339-1357,1416-1486; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1270-1484; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-task.cpp:159-200,407-420; https://github.com/ggml-org/llama.cpp/pull/22217
- [high] tool_choice читается как строка, допустимы только auto|none|required. Объектная форма OpenAI {type:function,function:{name}} молча (только WARN в логе) становится auto. В Anthropic-конверсии {type:tool,name} превращается в required и теряет имя. Фикс — PR #30800 (draft от 19.10.2026, issue #30369). Принудить конкретный инструмент сейчас можно только так: передать в tools один этот инструмент и tool_choice=required.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1282,1365; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.h:43-55; /home/kristian/Рабочий стол/llama.cpp/common/chat.cpp:314-325; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-chat.cpp:567-578; https://github.com/ggml-org/llama.cpp/pull/30800 (2026-10-19)
- [high] Ограничения на запрос: logprobs + tools + stream даёт 400. Продолжать (prefill) ассистентское сообщение с tool_calls нельзя. --prefill-assistant включён по умолчанию: последнее assistant-сообщение трактуется как префилл. После #30210 (09.10.2026) префилл возвращается в ответе только с echo:true. В обработчиках gpt-oss и Gemma 4 ветка response_format стоит раньше tools: если передать их вместе, tools игнорируются.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1371-1390,1458-1461; /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:242; /home/kristian/Рабочий стол/llama.cpp/common/parsers/gpt-oss.cpp:94-101; /home/kristian/Рабочий стол/llama.cpp/common/parsers/gemma4.cpp:213-218; https://github.com/ggml-org/llama.cpp/pull/30210 (комментарий 2026-10-11)
- [high] «Сырой» /completion (и /completions, /v1/completions). prompt — строка, массив ID токенов, смесь или объект с multimodal_data; BOS добавляется только для строкового промпта. Поля: cache_prompt (по умолчанию true), n_cache_reuse, n_probs, post_sampling_probs, return_tokens, samplers (порядок), logit_bias, grammar/json_schema, grammar_lazy, grammar_triggers ({type: 0 TOKEN|1 WORD|2 PATTERN|3 PATTERN_FULL, value, token}), preserved_tokens, reasoning_budget_tokens + reasoning_budget_start_tag/reasoning_budget_end_tags/reasoning_budget_message, reasoning_control, id_slot, n_cmpl, return_progress, t_max_predict_ms, response_fields. Пользовательская grammar помечается как USER и не «префиллится» generation_prompt’ом.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:486-606; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-schema.cpp:255-380; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.h:70-93; /home/kristian/Рабочий стол/llama.cpp/common/common.h:147-151,216-224
- [high] Служебные эндпоинты:
- /apply-template принимает то же тело, что chat, и возвращает только {prompt} (без грамматики и парсера);
- /tokenize: add_special по умолчанию false, parse_special по умолчанию true, with_pieces;
- /detokenize;
- GET /props: build_info, model_path, chat_template, chat_template_caps, total_slots, default_generation_settings.n_ctx, is_sleeping;
- /slots включён по умолчанию; save/restore/erase требуют --slot-save-path; с #26004 (08.10.2026) в файл попадают и чекпоинты, с #30574 — состояние драфта;
- /metrics (флаг --metrics, есть счётчики принятия спекуляции);
- /health отвечает 503, пока модель грузится;
- подсчёт токенов: /v1/chat/completions/input_tokens, /v1/messages/count_tokens.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:473-484,680-741,833-1200,1594-1620; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:5703-5717; git log 033df86b6 (2026-10-08), ea736c1d5 (2026-10-15)
- [high] /v1/responses и /v1/messages — односторонние конверсии в chat-completions. Responses помечает усечённые ответы и обрезанные tool calls как completed (#30773, сборка b11753, 20.10.2026), тогда как chat отдаёт finish_reason=length. Anthropic-путь отдельно нормализует billing-заголовок Claude Code.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:1528; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-chat.cpp:6,312-348; https://dev.to/homelabpm/llama-servers-responses-api-marks-truncated-replies-and-tool-calls-as-completed-153n (2026-10-20)
- [high] Режим роутера: llama-server запускается без -m. Флаги: --models-dir, --models-preset (INI, ключи = CLI-аргументы плюс load-on-startup, stop-timeout, dedup-cache-models), --models-max (по умолчанию 4), --models-autoload. Эндпоинты: GET /models (?reload=1), POST/DELETE /models, /models/load, /models/unload, /models/sse. Маршрутизация по полю model. Падение дочернего процесса фиксируется как unloaded с exit_code; следующий запрос с autoload поднимает модель заново.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:1841-2274; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-models.cpp:1248-1267,1540-1575
- [high] Возможности для супервизора:
- --port по умолчанию 9931 (с v0.7.0, #30159; раньше 8080);
- --host принимает путь *.sock (UNIX-сокет);
- --api-key, --sleep-idle-seconds, --log-jsonl, --log-prompts-dir;
- env LLAMA_SERVER_SLOTS_DEBUG печатает токен-дифф в месте промаха кэша;
- возобновляемый SSE: заголовок X-Conversation-Id и GET /v1/stream?conv_id&from; на один conv — одна живая сессия.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:108,193-194,213,245-246; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-http.cpp:500-505; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:1468-1472,3803-3843; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-stream.h:12-59; /home/kristian/Рабочий стол/llama.cpp/tools/server/server.cpp:315-325
- [high] Точные флаги в чекауте (умолчание в скобках):
- шаблоны и разбор: --jinja/--no-jinja (вкл), --chat-template, --chat-template-file, --chat-template-kwargs, --skip-chat-parsing, --prefill-assistant (вкл);
- reasoning: --reasoning-format none|deepseek|deepseek-legacy (auto), -rea/--reasoning on|off|auto (auto), --reasoning-effort, --reasoning-budget N (-1), --reasoning-budget-message, --reasoning-preserve/--no-reasoning-preserve (вкл);
- кэш: --cache-prompt (вкл), --cache-reuse N (0), --swa-full, -ctxcp/--ctx-checkpoints/--swa-checkpoints N (32), -cms/--checkpoint-min-step (8192), -cram/--cache-ram MiB (8192), --cache-idle-slots, --context-shift (выкл);
- слоты: -kvu/--kv-unified (вкл только при -np auto), --kv-unified-per-slot N, -np/--parallel (-1 = auto → 4 слота + unified), -sps/--slot-prompt-similarity (0.10);
- железо: -fa on|off|auto (auto), -ctk/-ctv f32|f16|bf16|q8_0|q4_0|q4_1|iq4_nl|q5_0|q5_1 (f16), -cmoe/--cpu-moe, -ncmoe/--n-cpu-moe N, --moe-cache-mib (0), -ngl N|auto|all (auto), -fit on|off (on, порог 1024 MiB, минимальный ctx 4096), -c (0 = из модели), -b 2048, -ub 512;
- спекуляция: --spec-type none|draft-simple|draft-eagle3|draft-mtp|draft-dflash|draft-dspark|ngram-simple|ngram-map-k|ngram-map-k4v|ngram-mod|ngram-cache, -md/--spec-draft-model, -hfd, --spec-draft-n-max (3), --spec-draft-n-min, --spec-draft-p-min, --spec-draft-sampling greedy|probabilistic, -ctkd/-ctvd, --spec-default (= ngram-mod).
Отдельного флага --mtp нет: MTP включается через --spec-type draft-mtp и -md.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:50-91,115-116,165-178,217-243,247-303; /home/kristian/Рабочий стол/llama.cpp/tools/server/server.cpp:168-173; /home/kristian/Рабочий стол/llama.cpp/common/arg.cpp:4769-4776
- [high] Встроенные в сервер инструменты (--tools read_file,grep_search,exec_shell_command,write_file,edit_file,..., --tools-runtime docker|podman|ssh) и MCP (--mcp-servers-config/--mcp-servers-json, только stdio, инструменты именуются server_tool), а также --agent помечены как experimental. REST /tools — внутренний API WebUI («Please do NOT use this endpoint in a downstream application»). Для агента это не основа: MCP-клиент и терминал должны жить на стороне агента.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:205-209,350-390,1833-1839
- [high] Архитектура разбора сейчас:
- enum common_chat_format = CONTENT_ONLY, PEG_SIMPLE, PEG_NATIVE, PEG_GEMMA4, PEG_MINIMAX_M3; старых форматов (Generic, GPT-OSS и т.п.) больше нет;
- выделенный обработчик выбирается по подстрокам шаблона, порядок важен. Всего около 20: Ministral3, LLM-jp Harmony, GPT-OSS, Muse Spark/Glimmer, Functionary v3.2, Kimi K2/K3, K2 Horizon, Ling3, Cohere2MoE, LFM2/2.5, GigaChat3, MiniMax-M3, DeepSeek V3.2/V4, Gemma4 (+legacy), MiniCPM5, TranslateGemma, Qwen3-Coder;
- остальные шаблоны идут через дифференциальный автопарсер: рендерит варианты шаблона и диффит их. Введён #18675 (06.03.2026), стриминг #20177. Анализ выполняется один раз на шаблон;
- если анализ не удался, запрос падает с ошибкой «Unable to generate parser».
  evidence: /home/kristian/Рабочий стол/llama.cpp/common/chat.h:235-245; /home/kristian/Рабочий стол/llama.cpp/common/chat.cpp:1090-1257,1376-1395; /home/kristian/Рабочий стол/llama.cpp/common/parsers/sources.cmake; /home/kristian/Рабочий стол/llama.cpp/docs/autoparser.md; https://github.com/ggml-org/llama.cpp/pull/18675
- [high] Обработчик gpt-oss (common/parsers/gpt-oss.cpp):
- включается по подстроке <|channel|>, формат peg-native;
- reasoning_content копируется в поле thinking шаблона; если есть tool_calls, content удаляется (значит, commentary-преамбула в истории теряется);
- вызов распознаётся и с recipient в заголовке роли, и в заголовке канала;
- ровно один tool call за генерацию, параллельность убрана в #20393 (18.03.2026), потому что модель обучена на одиночные вызовы;
- аргументы ограничены JSON-схемой через ленивую грамматику с 4 regex-триггерами;
- терпимость к «stray commentary» (#21286) и непрошеным вызовам builtin-инструментов (#21213);
- замена <|return|> на <|end|> в истории;
- reasoning-теги для бюджета: <|channel|>analysis<|message|> … <|end|>.
  evidence: /home/kristian/Рабочий стол/llama.cpp/common/parsers/gpt-oss.cpp:7-17,24-28,40-49,68-149; /home/kristian/Рабочий стол/llama.cpp/common/chat.cpp:1108-1111; https://github.com/ggml-org/llama.cpp/pull/20393
- [high] Обработчик Gemma 4 (common/parsers/gemma4.cpp):
- включается по подстроке '<|tool_call>call:'; при отсутствии маркера «OpenAI Chat Completions:» выдаёт WARN и применяет legacy-обходы;
- формат PEG_GEMMA4: отдельный mapper переводит DSL Gemma (ключи без кавычек, строки в <|"|>) в JSON;
- схема аргументов НЕ применяется: TODO в строке 260; в PR #21418 это явный non-goal; issue #21680 показывает порчу вложенных объектов;
- параллельные вызовы — повтор с max=-1, если parallel_tool_calls; по caps шаблона это включено по умолчанию;
- ленивый триггер WORD <|tool_call>;
- теги размышления <|channel>thought / <channel|>;
- при response_format вывод обязан быть в блоке ```json;
- исправлен generation prompt для случая «контент + tool_call»;
- учтены артефакты 26B-A4B (#21760): повторные <|channel> и висячий <channel|>.
  evidence: /home/kristian/Рабочий стол/llama.cpp/common/parsers/gemma4.cpp:151-305; /home/kristian/Рабочий стол/llama.cpp/common/chat.cpp:1212-1221; /home/kristian/Рабочий стол/llama.cpp/common/chat-peg-parser.cpp:963-1090; https://github.com/ggml-org/llama.cpp/pull/21418 (2026-04-04); https://github.com/ggml-org/llama.cpp/pull/21760
- [high] Стриминг и ошибки разбора. На каждый токен весь накопленный вывод заново разбирается в lenient-режиме (частичный разбор), диффы превращаются в OAI-дельты. Если финальный строгий разбор не совпал с форматом, бросается исключение «The model produced output that does not match the expected <format> format». В стриме это SSE-событие error и обрыв, без стрима — HTTP 500; уже сгенерированный ответ теряется. Мейнтейнеры отклоняют универсальные lenient-fallback (#25321, #25100).
  evidence: /home/kristian/Рабочий стол/llama.cpp/common/chat.cpp:1611-1662,1740-1758; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:5134-5139; https://github.com/ggml-org/llama.cpp/issues/25321; https://github.com/ggml-org/llama.cpp/issues/27733 (open)
- [high] Известные проблемы gpt-oss-20b в llama.cpp:
- #27720 (авг. 2026): ≈4% ходов при temp 1.0 давали испорченный заголовок канала и 500. Причина — клиент не возвращал reasoning_content: с возвратом 0/120 сбоев, без него 7/120;
- #19520 (фев. 2026): официальный HF-шаблон дважды экранирует аргументы и ответы инструментов;
- #15789: 20B галлюцинирует вызовы, если инструменты описаны в промпте, а не в поле tools;
- #25321: просьба «без каналов» ломает формат, мейнтейнеры считают такой случай невалидным.
  evidence: https://github.com/ggml-org/llama.cpp/issues/27720 (2026-08-25..26); https://github.com/ggml-org/llama.cpp/issues/19520 (2026-02-11); https://github.com/ggml-org/llama.cpp/issues/15789; https://github.com/ggml-org/llama.cpp/issues/25321 (2026-07-05)
- [medium] Известные проблемы Gemma 4 (26B-A4B) в llama.cpp:
- бесконечный повтор tool call — #21375, исправлен в #21661;
- сложные вложенные аргументы портятся — #21680; мейнтейнер советует принимать такие аргументы JSON-строкой;
- tool call уходит в content, если не возвращать reasoning_content — #22786;
- 500 «peg-gemma4» — #25072, оказалось проблемой шаблона;
- нестабильный стриминг многострочных аргументов — #29655 (29.09.2026); парсер переписывается в #29307 (draft: token-aware parsing);
- 26B иногда «отыгрывает назад» уже начатый вызов — по словам мейнтейнера, при стриминге tool calls это неразрешимо;
- сторонние отчёты: петли из-за повторно вставленных мыслей (HF #119) и незакрытый thought после tool response при enable_thinking=false (HF #147).
  evidence: https://github.com/ggml-org/llama.cpp/issues/21375; https://github.com/ggml-org/llama.cpp/issues/21680 (2026-04-09); https://github.com/ggml-org/llama.cpp/issues/22786; https://github.com/ggml-org/llama.cpp/issues/25072; https://github.com/ggml-org/llama.cpp/issues/29655; https://github.com/ggml-org/llama.cpp/pull/29307; https://huggingface.co/google/gemma-4-31B-it/discussions/119; https://huggingface.co/google/gemma-4-31B-it/discussions/147
- [high] Инъекция спец-токенов. В chat-пути промпт склеивается в одну строку и токенизируется с parse_special=true. Поэтому текст вида <|end|>, <turn|>, <|tool_call>, <|im_end|> внутри user- или tool-контента (например, при чтении исходников с шаблонами) становится управляющим токеном. Разметка частей is_input уже есть (#30389, 14.10.2026), но к серверу не подключена: #28249 открыт, пункт «Wire it up to server» не отмечен.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1437; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:4921-4926; /home/kristian/Рабочий стол/llama.cpp/common/chat.h:276-296; https://github.com/ggml-org/llama.cpp/issues/29546 (2026-09-27); https://github.com/ggml-org/llama.cpp/pull/28249 (open)
- [medium] Дрейф шаблонов Gemma 4. В google/gemma-4-26B-A4B-it 15.07.2026 влит канонический шаблон «Published 2026-07-09»: preserve_thinking, восстановление model turn и thought cue после tool response, исключение для строковых arguments. Локальная копия владельца (NVIDIA NVFP4, 30.04.2026) старее: старый гейт мыслей «после последнего user И есть tool_calls», без канонического заголовка. Тестовый шаблон в llama.cpp тоже старый. llama.cpp по умолчанию (--reasoning-preserve) выставляет preserve_thinking=true, clear_thinking=false для шаблонов, которые это поддерживают, — это расходится с дефолтом Google.
  evidence: https://huggingface.co/google/gemma-4-26B-A4B-it/commits/main (Jul 15, #47); https://huggingface.co/google/gemma-4-26B-A4B-it/raw/main/chat_template.jinja (прочитано 2026-10-29); /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/chat_template.jinja:238-241,347-354; /home/kristian/Рабочий стол/llama.cpp/common/jinja/caps.cpp:22-27; /home/kristian/Рабочий стол/llama.cpp/common/arg.cpp:964-966
- [high] Шаблоны gpt-oss.
- Официальный HF (совпадает с llama.cpp models/templates/openai-gpt-oss-120b.jinja): дата «Current date» через strftime_now; reasoning по умолчанию medium; не больше одного tool call в сообщении (берётся tool_calls[0]); analysis отбрасывается, как только позже есть final; аргументы и ответы рендерятся через tojson.
- В GGUF владельца (unsloth F16) вшит изменённый «Unsloth fixes» шаблон: строковые args и результаты идут как есть, поддержан developer_instructions, и есть ветка elif "thinking" in message, которая рендерит message.content прошлых не-tool ответов в канал analysis.
- strftime_now в llama.cpp берёт время запроса, переопределить его в запросе нельзя.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/chat_template.jinja:197-206,274-316,321-322; /tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/gptoss_gguf_tmpl.jinja (извлечён из /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf) строки 1,235-238,300-320,331-335; /home/kristian/Рабочий стол/llama.cpp/common/jinja/value.cpp:429-440
- [high] Официальный крейт OpenAI openai-harmony (Rust, docs.rs 0.0.8) даёт:
- render_conversation_for_completion → ID токенов;
- parse_messages_from_completion_tokens и StreamableParser (потоковый, по токенам; ParseOptions{strict:false} для восстановления после ошибок);
- stop_tokens_for_assistant_actions;
- auto_drop_analysis.
То есть канонический рендер и разбор Harmony для Rust-агента без Jinja. Нюанс: vocab o200k_base.tiktoken скачивается в рантайме, если не задан TIKTOKEN_ENCODINGS_BASE. В примере OpenAI вывод инструмента идёт как сырой текст, без JSON-экранирования.
  evidence: https://github.com/openai/harmony/blob/main/docs/rust.md; https://docs.rs/openai-harmony/latest/openai_harmony/struct.StreamableParser.html; https://developers.openai.com/cookbook/articles/openai-harmony; https://github.com/openai/harmony/issues/95
- [high] iSWA-геометрия и KV.
- gpt-oss-20b: 24 слоя, sliding и full чередуются, окно 128, 8 KV-голов × 64, максимум 131072 (YaRN ×32 от 4096). Full-attn KV = 24 КиБ/токен в f16, 3 ГиБ на 128K.
- Gemma 4 26B-A4B: 30 слоёв, full на 5/11/17/23/29 (2 KV-головы × 512), 25 SWA-слоёв (8 × 256), окно 1024, максимум 262144. Full-attn KV = 20 КиБ/токен, SWA = 200 КиБ/токен; V хранится отдельно от K.
- Размер SWA-кэша в ячейках = pad256(min(ctx, n_swa·(unified ? n_seq : 1) + n_ubatch)); при --swa-full он равен ctx.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/config.json; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/config.json (text_config); /home/kristian/Рабочий стол/llama.cpp/src/llama-kv-cache-iswa.cpp:68-80; /home/kristian/Рабочий стол/llama.cpp/src/models/gemma4.cpp:239-262
- [high] Как переиспользуется префикс между запросами. Ищется LCP токенов со слотом; всё после точки расхождения удаляется (seq_rm). Если SWA-память не покрывает точку расхождения (pos_min ≥ pos − n_swa + 1, после фикса #30580 от 15.10.2026), восстанавливается ближайший более ранний чекпоинт и пересчёт идёт от него. Чекпоинта нет — полный пересчёт («forcing full prompt re-processing»).
Где создаются чекпоинты:
- на начале последнего user-сообщения;
- на других user-сообщениях, если с прошлого чекпоинта прошло больше 8192 токенов;
- за 4+n_ubatch и за 4 токена до конца каждого промпта (#20288).
Максимум 32 на слот; при заполнении вытесняются слишком близкие; хранятся в RAM.
История: появились в #15293 (14.08.2025); #24176 (23.06.2026) — чекпоинты на user-сообщениях; #23981 (02.06.2026) — хранить только немаскированные ячейки.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:2544-2607,3700-3905,3983-4180; /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:166-170; https://github.com/ggml-org/llama.cpp/pull/15293; git log a7b3dee7a, 73618f27a, 236531595, 61ad80bf9
- [medium] Размер чекпоинта: у Gemma 4 26B-A4B в f16 измерено 200,012 МиБ, что совпадает с расчётом 1024 × 200 КиБ; до #23981 (апрель) было 282–398 МиБ. У gpt-oss-20b по расчёту ≈3 МиБ (128 × 24 КиБ). Значит, 32 чекпоинта Gemma 4 занимают до ≈6,4 ГиБ RAM на слот.
  evidence: https://github.com/ggml-org/llama.cpp/discussions/24264 (лог 2026-06-07); https://github.com/ggml-org/llama.cpp/issues/21769 (лог 2026-04-11); расчёт по config.json
- [high] --cache-reuse (переиспользование чанков со сдвигом KV) требует llama_memory_can_shift. Для iSWA это true только при равных размерах base- и SWA-кэша, т.е. только с --swa-full; при мультимодальности (mmproj) отключено всегда. В обсуждениях подчёркивают, что название вводит в заблуждение: это не префиксный кэш, а приближённый трюк со сдвигом позиций. Префиксный кэш — это cache_prompt, он включён по умолчанию.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:3718-3727; /home/kristian/Рабочий стол/llama.cpp/src/llama-kv-cache-iswa.cpp:251-255; https://github.com/ggml-org/llama.cpp/issues/21468; https://github.com/ggml-org/llama.cpp/discussions/22354 (2026-04-25)
- [medium] Цена --swa-full. У gpt-oss +24 КиБ/токен: +3 ГиБ на 128K на слот, приемлемо при -np 1. У Gemma 4 +200 КиБ/токен: +6,25 ГиБ на 32K и +25 ГиБ на 128K, на 24 ГБ невозможно. С --swa-full сервер переиспользует любой LCP без чекпоинтов (логика исправлена в #22288 от 24.04.2026).
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:55; /home/kristian/Рабочий стол/llama.cpp/src/llama-kv-cache-iswa.cpp:75-80; git log ffdd983fb; https://github.com/ggml-org/llama.cpp/pull/21749; расчёт
- [high] RAM-кэш промптов (--cache-ram, по умолчанию 8192 МиБ; --cache-idle-slots). Состояния простаивающих слотов вместе с чекпоинтами сохраняются в RAM и восстанавливаются по лучшему LCP (условие f_keep ≥ 0,25). Так можно чередовать несколько диалогов на одном слоте без полного пересчёта. В этой зоне недавно чинили серьёзное:
- #30295 (10.10.2026): закреплённый занятой слот мог продолжить генерацию на чужом контексте;
- #30580 (15.10.2026): восстановленные SWA-состояния всегда отбрасывались;
- #30498, #30567 (15.10.2026).
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-task.cpp:1714-1870; /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:168-170; git log 23b0202a1, 61ad80bf9, 2639a3b4e, b0d239164
- [medium] Границы сообщений (message_spans), по которым ставятся чекпоинты на user-сообщениях, вычисляются только из chat-сессии (apply_chat_session). У «сырого» /completion такие чекпоинты не создаются, остаются только чекпоинты у конца промпта. Для агентного цикла их обычно хватает: конец промпта предыдущего запроса как раз совпадает с типичной точкой расхождения.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-task.h:266-275; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:4050-4124,4150-4174
- [medium] Поведение кэша в агентном цикле (вывод из шаблонов и логики сервера).
Внутри хода: перерендер последнего ассистентского сообщения слегка расходится со сгенерированными токенами. У gpt-oss recipient переезжает в заголовок роли, меняются пробелы tojson; у Gemma 4 ключи сортируются через dictsort, добавляется \n перед <channel|>, content переносится после tool responses. Поэтому заново считается только хвост — дёшево, а при длинном хвосте выручает чекпоинт у конца промпта.
На новом user-ходе:
- gpt-oss выбрасывает analysis всех tool-call сообщений прошлого хода → пересчёт всего прошлого хода;
- Gemma 4 со старым шаблоном теряет мысли tool-call сообщений, с каноническим + preserve — только у финального ответа.
Компакция, изменение tools или system, смена даты в системном сообщении gpt-oss пересчитывают всё после точки изменения.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/chat_template.jinja:202,274-316; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/chat_template.jinja:238-257,262-343; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:3784-3905,4104-4124
- [high] Сэмплинг по умолчанию — это дефолты llama.cpp: temp 0.8, top_k 40, top_p 0.95, min_p 0.05. Исключение — если в GGUF есть ключи general.sampling.*; в GGUF gpt-oss владельца их нет, mainline-конвертер их тоже обычно не пишет. Официальный пресет llama.cpp --gpt-oss-20b-default: temp 1.0, top_p 1.0, top_k 0, min_p 0.01, -ub 2048, -b 32768, -np 2, ctx 2×131072, jinja. generation_config Gemma 4: temp 1.0, top_k 64, top_p 0.95.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:127-130; /home/kristian/Рабочий стол/llama.cpp/common/common.cpp:1124-1146; /home/kristian/Рабочий стол/llama.cpp/common/arg.cpp:4710-4724; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/generation_config.json; метаданные /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf (прочитаны gguf-py)
- [medium] Режимы мышления.
- При -rea auto enable_thinking=true, если шаблон его поддерживает. Значит, у Gemma 4 мышление включено по умолчанию (в системный turn вставляется <|think|>). Выключение — chat_template_kwargs.enable_thinking=false или reasoning_effort:none.
- Для gpt-oss reasoning_effort:none лишь снимает kwarg, а шаблон не знает enable_thinking и по умолчанию берёт medium. Скорее всего, рассуждение не отключается (вывод из кода).
- Бюджет мышления работает для обеих моделей: теги берутся из обработчиков, per-request reasoning_budget_tokens работает с #23116 (13.07.2026). Досрочный конец — POST /v1/chat/completions/control {id, action:reasoning_end}, если в запросе было reasoning_control:true.
  evidence: /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:1601-1614,5580-5615; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-common.cpp:1394-1455; /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:1355,1472-1486; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/chat_template.jinja:203-206
- [medium] Спекулятивное декодирование.
Gemma 4:
- MTP есть с #23398 (07.06.2026) через «assistant»-драфтеры. В ggml-org/gemma-4-26B-A4B-it-GGUF лежат mtp-…-Q4_0 (252 МБ) / Q8_0 (462 МБ) и dflash-драфтер (472 МБ); -hf сам находит mtp-сайдкар;
- для 26B MoE результаты смешанные: автор PR не увидел ускорения на своей системе; unsloth на B200 получил 159 → 257 ток/с при принятии 0,72; на агентной нагрузке с длинным контекстом (#24264) 94 → 71 ток/с; Google предупреждает, что MoE-драфтер может не ускорять при batch 1.
gpt-oss:
- EAGLE3-v3 (#25794, 28.07.2026), драфтер RedHatAI/gpt-oss-20b-speculator.eagle3.
Общее: --spec-default включает draftless ngram-mod, полезный при переписывании кода.
  evidence: https://github.com/ggml-org/llama.cpp/pull/23398; https://huggingface.co/ggml-org/gemma-4-26B-A4B-it-GGUF/tree/main; https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/blob/main/MTP/README.md; https://github.com/ggml-org/llama.cpp/discussions/24264; https://gemma.ai/get-started/capabilities/mtp/; /home/kristian/Рабочий стол/llama.cpp/docs/speculative.md:16-51,181-200
- [high] FFI-путь. В публичном include/llama.h (1746 строк; 67 коммитов в 2026; semver 0.x, введён в авг. 2026) есть только модель, контекст, batch, memory_seq_*, state_seq_*_ext (с флагом PARTIAL_ONLY), семплеры, словарь и legacy llama_chat_apply_template (Gemma 4 с <|turn> он не знает). Jinja, все чат-парсеры, автопарсер, reasoning budget, спекуляция (MTP через приватный staging-API src/llama-ext.h) и политика чекпоинтов и RAM-кэша — это внутренний C++ в common/ и tools/server. Его API меняется: #30210 от 09.10 и #30389 от 14.10.2026. Крейт llama-cpp-2 (0.1.160 от 15.10.2026, на llama.cpp v0.6.0; README: «не safe», «semver не соблюдает») в 0.1.147 (PR #1037, 12.06.2026) вырезал OAI-обёртку apply_chat_template_oaicompat и разбор tool calls, потому что upstream перенёс функции в server. Внутрипроцессный GGML_ABORT (например, при ошибке восстановления чекпоинта) убьёт хост.
  evidence: /home/kristian/Рабочий стол/llama.cpp/include/llama.h (git log --since=2026-01-01 -- include/llama.h: 67); /home/kristian/Рабочий стол/llama.cpp/common/speculative.cpp:13; /home/kristian/Рабочий стол/llama.cpp/src/llama-chat.cpp:44,155; /home/kristian/Рабочий стол/llama.cpp/tools/server/server-context.cpp:3867; https://github.com/utilityai/llama-cpp-rs/pull/1037; https://crates.io/crates/llama-cpp-2; https://github.com/utilityai/llama-cpp-rs/releases
- [medium] Скорость на ноутбучной RTX 5090 — оценка, не замер.
Основание:
- в CUDA-таблице llama.cpp ноутбучная 5090 ≈0,51–0,53× десктопной (Llama-2-7B Q4_0, FA: pp512 7642 против 14970, tg128 158 против 300);
- десктоп 5090, gpt-oss-20b MXFP4: tg128 282 (авг. 2025), 313–324 ток/с (2026), pp2048 9848, pp32768 6291;
- десктоп 5090, Gemma 4 26B-A4B Q4_K_M: tg128 231–265, pp512 11,4–12,5 тыс.; на глубине 16K tg 213 / pp 9262; на 128K pp 2839 / tg 130; на 256K pp 1707 / tg 106 (сборка 8639, апрель, до сентябрьского FA-тюнинга под Gemma 4).
Оценка для ноутбука:
- gpt-oss-20b ≈150–170 ток/с и ≈4,5–5 тыс. ток/с префилла (≈3 тыс. на 32K);
- Gemma 4 26B ≈115–135 ток/с и ≈5,5–6,5 тыс. ток/с.
Прямые точки на ноутбучной 5090: Gemma 4 E4B pp512 9556 / tg128 165,6; 26B-A4B с MTP (форк AtomicChat) 140 ток/с при 78% принятия.
  evidence: https://github.com/ggml-org/llama.cpp/discussions/15013; https://github.com/ggml-org/llama.cpp/discussions/15396 (2025-08); https://llm-speed.com/m/gpt-oss-20b (Oct 2026); https://localllmadvisor.com/can-run/gemma-4-26b-a4b-on-rtx-5090-32gb.html; https://gpubattle.com/guides/gemma-4-26b-a4b-gpu-benchmarks (2026-09-29..10-25); https://markaicode.com/benchmarks/gemma-4-llamacpp-benchmark/ (2026-05-29); https://blog.majid.info/llama-bench/ (2026-04-13); https://github.com/aamsellem/olares-one-market/blob/main/reddit_post_gemma4_mtp_blackwell_mobile.md
- [medium] Бюджет VRAM 24 ГБ (расчёт без compute-буферов, их размер не мерился).
gpt-oss-20b:
- веса 11,3 ГиБ (ggml-org MXFP4) или 12,8 ГиБ (локальный unsloth: MXFP4-эксперты 9,46 ГиБ + F16 3,34 ГиБ);
- KV 3 ГиБ на слот при 128K (это и предел модели); SWA ≈50 МиБ;
- итого 1 слот ≈16–18 ГиБ, 2 слота по 128K ≈20–22 ГиБ.
Gemma 4 26B-A4B:
- веса Q4_0 14,6 ГБ / MXFP4_MOE 16,6 / UD-Q4_K_XL 17 / UD-IQ4_XS 13,6 ГБ;
- full KV 2,5 ГиБ (128K) или 5 ГиБ (256K) в f16; SWA ≈0,3–0,6 ГиБ на слот;
- итого 128K f16 ≈20–21 ГиБ; 256K — только с q8_0-KV или Q4_0.
Обе модели одновременно в 24 ГБ не помещаются.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-gguf/gpt-oss-20b-F16.gguf (gguf-py: MXFP4 72 тензора 9.455 GiB, F16 98 тензоров 3.344 GiB); https://huggingface.co/ggml-org/gemma-4-26B-A4B-it-GGUF/tree/main; https://huggingface.co/unsloth/gemma-4-26B-A4B-it-GGUF/tree/main; /home/kristian/Рабочий стол/llama.cpp/src/llama-kv-cache-iswa.cpp:73-80
- [high] Спец-токены Gemma 4: <|tool> 46, <tool|> 47, <|tool_call> 48, <tool_call|> 49, <|tool_response> 50 (это EOG), <tool_response|> 51, <|"|> 52 (разделитель строк в DSL), <|think|> 98, <|channel> 100, <channel|> 101, <|turn> 105, <turn|> 106. EOS = [1, 106, 50]. GBNF в llama.cpp умеет матчить токены (<[id]>, <token>, отрицание !<[id]>), поэтому собственная токен-грамматика для DSL Gemma со схемой аргументов реализуема.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/tokenizer.json (added_tokens); /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/generation_config.json; /home/kristian/Рабочий стол/llama.cpp/grammars/README.md:70-90
- [medium] Схемы инструментов рендерятся с потерями.
- gpt-oss описывает инструменты как TypeScript: oneOf с объектами превращается в any, вложенные типы длиннее 50 символов — в any[].
- DSL Gemma 4 переносит только description/type/enum/items/properties/required/nullable; anyOf, $ref, default, format, pattern, minimum выпадают.
- Инструмент с именем call вызывал бесконечную рекурсию (исправлено в #30088, 07.10.2026).
Вывод: агенту нужно упрощать схемы и нормализовать имена под каждое семейство.
  evidence: /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gpt-oss-20b-bf16/chat_template.jinja:10-147; /run/media/kristian/b39e73c3-74a9-42e9-bffb-28c79f61cc2e/models/gemma4-26b-nvfp4/chat_template.jinja:1-117; https://prismix.dev/news/5edae4d8a042 (b11484, 2026-10-07)
- [high] Документация отстаёт от кода. docs/function-calling.md описывает формат Generic и таблицы старых обработчиков, которых больше нет. Пример /slots в README показывает chat_format GPT-OSS, хотя сейчас это peg-native. Источник истины — код и tests/test-chat*.cpp: тест-кейсы Gemma 4 на параллельные вызовы, числа, массивы, продолжение.
  evidence: /home/kristian/Рабочий стол/llama.cpp/docs/function-calling.md:12-29; /home/kristian/Рабочий стол/llama.cpp/tools/server/README.md:1027; /home/kristian/Рабочий стол/llama.cpp/common/chat.h:235-245; /home/kristian/Рабочий стол/llama.cpp/tests/test-chat.cpp:3002-3240
