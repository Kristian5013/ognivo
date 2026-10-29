"""Render the `seq` scenario with the openai-harmony reference renderer (the format's source of truth),
after applying OpenAI's CoT rule manually (drop analysis before the most recent final).
usage: TIKTOKEN_ENCODINGS_BASE=<dir with o200k_base.tiktoken> uv run --with openai-harmony==0.0.8 python -I harmony_ref.py"""
import json

from openai_harmony import (Author, Conversation, DeveloperContent, HarmonyEncodingName, Message,
                            ReasoningEffort, Role, SystemContent, ToolDescription, load_harmony_encoding)

R = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/'
sc = json.load(open(R + '_scenarios/seq.json', encoding='utf-8'))
enc = load_harmony_encoding(HarmonyEncodingName.HARMONY_GPT_OSS)
tools = [ToolDescription.new(t['function']['name'], t['function']['description'], parameters=t['function']['parameters']) for t in sc['tools']]
system = SystemContent.new().with_reasoning_effort(ReasoningEffort.HIGH).with_conversation_start_date('2026-10-29')
msgs = [Message.from_role_and_content(Role.SYSTEM, system),
        Message.from_role_and_content(Role.DEVELOPER, DeveloperContent.new().with_instructions(sc['messages'][0]['content']).with_function_tools(tools))]
m = sc['messages']
# index of the most recent final (assistant without tool_calls)
last_final = max(i for i, x in enumerate(m) if x['role'] == 'assistant' and not x.get('tool_calls'))
name_of = {}
for i, x in enumerate(m[1:], start=1):
    if x['role'] == 'user':
        msgs.append(Message.from_role_and_content(Role.USER, x['content']))
    elif x['role'] == 'assistant' and x.get('tool_calls'):
        if x.get('reasoning_content') and i > last_final:
            msgs.append(Message.from_role_and_content(Role.ASSISTANT, x['reasoning_content']).with_channel('analysis'))
        tc = x['tool_calls'][0]
        name_of[tc['id']] = tc['function']['name']
        msgs.append(Message.from_role_and_content(Role.ASSISTANT, json.dumps(tc['function']['arguments'], ensure_ascii=False))
                    .with_channel('commentary').with_recipient('functions.' + tc['function']['name']).with_content_type('<|constrain|>json'))
    elif x['role'] == 'assistant':
        msgs.append(Message.from_role_and_content(Role.ASSISTANT, x['content']).with_channel('final'))
    elif x['role'] == 'tool':
        msgs.append(Message.from_author_and_content(Author.new(Role.TOOL, 'functions.' + name_of[x['tool_call_id']]), x['content'])
                    .with_channel('commentary').with_recipient('assistant'))
toks = enc.render_conversation_for_completion(Conversation.from_messages(msgs), Role.ASSISTANT)
text = enc.decode_utf8(toks)
open(R + 'gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.txt', 'w', encoding='utf-8').write(text)
json.dump(toks, open(R + 'gpt-oss-20b/rendered/seq.effort-high.harmony-reference-0.0.8.tokens.json', 'w'))
print(text)
