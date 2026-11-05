# A stdio MCP server of the initialize era, as most servers still are: three tools, for the
# tests of ognivo's gateway. `server/discover` gets "method not found", as such a server
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
            "capabilities": {"tools": {}},
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
    elif method == "ping":
        result = {}
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
        continue
    send({"jsonrpc": "2.0", "id": i, "result": result})
