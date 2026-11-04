# A Streamable HTTP MCP server of the initialize era, for the tests of ognivo's gateway: it
# answers POSTs with JSON, wants `Authorization: Bearer t0k`, has no GET stream, and prints
# its port first.
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOLS = [
    {
        "name": "echo",
        "description": "Says the text back.",
        "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
    }
]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body=None, headers=()):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        for k, v in headers:
            self.send_header(k, v)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.reply(405)

    def do_DELETE(self):
        self.reply(200)

    def do_POST(self):
        if self.headers.get("Authorization") != "Bearer t0k":
            self.reply(401, {"error": "unauthorized"})
            return
        m = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        if "id" not in m:
            self.reply(202)
            return
        method, i, p = m.get("method"), m["id"], m.get("params") or {}
        if method == "initialize":
            result = {
                "protocolVersion": p.get("protocolVersion", "2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "http-echo", "version": "1"},
            }
            self.reply(200, {"jsonrpc": "2.0", "id": i, "result": result}, [("Mcp-Session-Id", "s1")])
            return
        if method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            result = {"content": [{"type": "text", "text": "over http: " + p["arguments"]["text"]}]}
        elif method == "ping":
            result = {}
        else:
            self.reply(200, {"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
            return
        self.reply(200, {"jsonrpc": "2.0", "id": i, "result": result})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
