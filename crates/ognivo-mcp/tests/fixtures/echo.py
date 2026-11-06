# A stdio MCP server of the initialize era, as most servers still are: three tools and a
# prompt, for the tests of ognivo's gateway. `server/discover` gets "method not found", as such a server
# answers it.
import json
import sys

TOOLS = [
    {
        "name": "echo",
        "description": "Says the text back.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
        "annotations": {"readOnlyHint": True},
    },
    {"name": "fail", "description": "Always fails.", "inputSchema": {"type": "object", "properties": {}}},
    {
        "name": "add-up",
        "description": "Adds numbers up.",
        "inputSchema": {"type": "object", "properties": {"numbers": {"type": "array", "items": {"type": "number"}}}},
    },
]


PROMPTS = [
    {
        "name": "review",
        "description": "Reviews a file for bugs.",
        "arguments": [
            {"name": "file", "description": "The file to review.", "required": True},
            {"name": "focus", "description": "What to look at most."},
        ],
    },
]


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    method, i, p = m.get("method"), m["id"], m.get("params") or {}
    if method == "initialize":
        result = {
            "protocolVersion": p.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}, "prompts": {}},
            "serverInfo": {"name": "echo", "version": "1"},
            "instructions": "Use echo to say a text back.",
        }
    elif method == "tools/list":
        result = {"tools": TOOLS}
    elif method == "tools/call":
        name, args = p["name"], p.get("arguments") or {}
        if name == "echo":
            result = {"content": [{"type": "text", "text": args.get("text", "")}]}
        elif name == "add-up":
            result = {"content": [], "structuredContent": {"sum": sum(args.get("numbers", []))}}
        else:
            result = {"isError": True, "content": [{"type": "text", "text": "it broke"}]}
    elif method == "prompts/list":
        result = {"prompts": PROMPTS}
    elif method == "prompts/get":
        args = p.get("arguments") or {}
        if p.get("name") != "review" or not args.get("file"):
            send({"jsonrpc": "2.0", "id": i, "error": {"code": -32602, "message": "review needs a file"}})
            continue
        focus = f", above all {args['focus']}" if args.get("focus") else ""
        result = {
            "messages": [
                {"role": "user", "content": {"type": "text", "text": f"Review {args['file']} for bugs{focus}."}},
                {"role": "assistant", "content": {"type": "text", "text": "I will read it first."}},
            ]
        }
    elif method == "ping":
        result = {}
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
        continue
    send({"jsonrpc": "2.0", "id": i, "result": result})
