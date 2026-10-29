"""Writes the fixed test conversations (OpenAI Chat Completions wire format) to _scenarios/*.json."""
import copy
import json
import os

OUT = '/tmp/claude-1000/-home-kristian--------------cforge/3ff4d0f1-90d6-4891-b9a2-ce2beecbc699/scratchpad/templates/_scenarios'

TOOLS = [
    {"type": "function", "function": {
        "name": "read_file",
        "description": "Read a UTF-8 file from the workspace.\nReturns numbered lines.",
        "parameters": {"type": "object", "properties": {
            "path": {"type": "string", "description": "Workspace-relative path"},
            "start_line": {"type": "integer", "description": "First line (1-based)"},
            "end_line": {"type": ["integer", "null"], "description": "Last line or null for EOF"}},
            "required": ["path"]}}},
    {"type": "function", "function": {
        "name": "apply_patch",
        "description": "Apply a patch in the *** Begin Patch format.",
        "parameters": {"type": "object", "properties": {
            "patch": {"type": "string", "description": "Patch text"},
            "dry_run": {"type": "boolean", "description": "Only validate", "default": False}},
            "required": ["patch"]}}},
    {"type": "function", "function": {
        "name": "run_command",
        "description": "Run a shell command.",
        "parameters": {"type": "object", "properties": {
            "cmd": {"type": "string", "description": "Command line"},
            "Cwd": {"type": "string", "description": "Working directory (mixed-case key on purpose)"},
            "timeout_s": {"type": "integer", "description": "Timeout in seconds"},
            "env": {"type": "object", "description": "Extra environment", "additionalProperties": {"type": "string"}}},
            "required": ["cmd"]}}},
]

SYSTEM = {"role": "system", "content": "You are a coding agent working in /repo. Keep answers short."}
PATCH = '*** Begin Patch\n*** Update File: src/lib.rs\n-    a - b // "bug"\n+    a + b\n*** End Patch'


def call(cid, name, args):
    return {"id": cid, "type": "function", "function": {"name": name, "arguments": args}}


SEQ = [
    SYSTEM,
    {"role": "user", "content": "The test `add_works` fails. Fix it."},
    {"role": "assistant", "reasoning_content": "I should read src/lib.rs first.", "content": "",
     "tool_calls": [call("call_1", "read_file", {"path": "src/lib.rs", "start_line": 1, "end_line": None})]},
    {"role": "tool", "tool_call_id": "call_1",
     "content": "1| pub fn add(a: i32, b: i32) -> i32 {\n2|     a - b // \"bug\" ünï → ✓ 😀\n3| }\n"},
    {"role": "assistant", "reasoning_content": "It subtracts. Patch it to add.", "content": "",
     "tool_calls": [call("call_2", "apply_patch", {"patch": PATCH, "dry_run": False})]},
    {"role": "tool", "tool_call_id": "call_2", "content": "{\"ok\": true, \"files\": [\"src/lib.rs\"]}"},
    {"role": "assistant", "reasoning_content": "Patched; report back.", "content": "Fixed: `add` now returns `a + b`."},
    {"role": "user", "content": "Run the tests."},
    {"role": "assistant", "reasoning_content": "Run cargo test.", "content": "",
     "tool_calls": [call("call_3", "run_command", {"cmd": "cargo test -q", "Cwd": "/repo", "timeout_s": 120, "env": {"RUST_BACKTRACE": "1"}})]},
    {"role": "tool", "tool_call_id": "call_3", "content": "running 1 test\n.\ntest result: ok. 1 passed; 0 failed"},
]


def with_string_args(msgs):
    out = copy.deepcopy(msgs)
    for m in out:
        for tc in m.get("tool_calls", []):
            tc["function"]["arguments"] = json.dumps(tc["function"]["arguments"], ensure_ascii=False)
    return out


def with_null_content(msgs):
    out = copy.deepcopy(msgs)
    for m in out:
        if m.get("tool_calls"):
            m["content"] = None
    return out


PARALLEL = [
    SYSTEM,
    {"role": "user", "content": "Compare Cargo.toml and README.md."},
    {"role": "assistant", "reasoning_content": "Read both files in one step.", "content": "",
     "tool_calls": [call("call_a", "read_file", {"path": "Cargo.toml"}), call("call_b", "read_file", {"path": "README.md"})]},
    {"role": "tool", "tool_call_id": "call_a", "content": "[package]\nname = \"demo\""},
    {"role": "tool", "tool_call_id": "call_b", "content": "# demo"},
]

PREAMBLE_LAST = [
    SYSTEM,
    {"role": "user", "content": "The test `add_works` fails. Fix it."},
    {"role": "assistant", "reasoning_content": "I should read src/lib.rs first.", "content": "Let me look at the code first.",
     "tool_calls": [call("call_1", "read_file", {"path": "src/lib.rs"})]},
    {"role": "tool", "tool_call_id": "call_1", "content": "1| pub fn add(a: i32, b: i32) -> i32 { a - b }"},
]

FINAL_REASONING = [
    SYSTEM,
    {"role": "user", "content": "What is 2+2?"},
    {"role": "assistant", "reasoning_content": "Trivial arithmetic.", "content": "4"},
    {"role": "user", "content": "And 3+5?"},
]

NO_TOOLS = [SYSTEM, {"role": "user", "content": "Hi"}]

MULTIMODAL = [
    {"role": "user", "content": [{"type": "text", "text": "What is on this screenshot?"},
                                 {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]},
    {"role": "assistant", "reasoning_content": "Need a second capture.", "content": "",
     "tool_calls": [call("call_s", "read_file", {"path": "shot2.png"})]},
    {"role": "tool", "tool_call_id": "call_s", "content": [{"type": "text", "text": "image follows"},
                                                           {"type": "image_url", "image_url": {"url": "data:image/png;base64,BBBB"}}]},
]

SCENARIOS = {
    "seq": {"messages": SEQ, "tools": TOOLS},
    "seq_str_args": {"messages": with_string_args(SEQ), "tools": TOOLS},
    "seq_null_content": {"messages": with_null_content(SEQ), "tools": TOOLS},
    "parallel": {"messages": PARALLEL, "tools": TOOLS},
    "preamble_last": {"messages": PREAMBLE_LAST, "tools": TOOLS},
    "final_reasoning": {"messages": FINAL_REASONING, "tools": TOOLS},
    "no_tools": {"messages": NO_TOOLS},
    "multimodal": {"messages": MULTIMODAL, "tools": TOOLS},
}

os.makedirs(OUT, exist_ok=True)
for name, sc in SCENARIOS.items():
    with open(os.path.join(OUT, name + '.json'), 'w', encoding='utf-8') as fh:
        json.dump(sc, fh, ensure_ascii=False, indent=1)
print(sorted(SCENARIOS))
