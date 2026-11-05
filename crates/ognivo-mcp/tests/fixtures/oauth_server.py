# A remote MCP server behind OAuth, for the tests of ognivo's sign-in. One process is the
# resource (/mcp, which wants a bearer token and says where its metadata is) and its own
# authorization server: metadata, dynamic client registration, /authorize with PKCE (S256)
# and /token with refresh tokens that work once. It prints its port first.
#
# The test plays the browser: /authorize answers at once with the redirect to the client's
# loopback callback, as a user who says yes. Tokens live `--lifetime` seconds (3600).
import base64
import hashlib
import json
import secrets
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

LIFETIME = int(sys.argv[sys.argv.index("--lifetime") + 1]) if "--lifetime" in sys.argv else 3600
CLIENTS, CODES, TOKENS, REFRESH = {}, {}, {}, {}
ISSUED = [0]
TOOLS = [{"name": "whoami", "description": "Says which token is signed in.", "inputSchema": {"type": "object", "properties": {}}}]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def base(self):
        return f"http://127.0.0.1:{self.server.server_address[1]}"

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
        u = urlparse(self.path)
        b = self.base()
        if u.path.startswith("/.well-known/oauth-protected-resource"):
            return self.reply(200, {"resource": b + "/mcp", "authorization_servers": [b], "scopes_supported": ["tools"]})
        if u.path in ("/.well-known/oauth-authorization-server", "/.well-known/openid-configuration"):
            return self.reply(200, {
                "issuer": b,
                "authorization_endpoint": b + "/authorize",
                "token_endpoint": b + "/token",
                "registration_endpoint": b + "/register",
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "scopes_supported": ["tools"],
            })
        if u.path == "/authorize":
            q = {k: v[0] for k, v in parse_qs(u.query).items()}
            redirect = q.get("redirect_uri", "")
            if (q.get("client_id") not in CLIENTS or q.get("code_challenge_method") != "S256"
                    or redirect not in CLIENTS[q["client_id"]]):
                return self.reply(400, {"error": "invalid_request"})
            code = secrets.token_urlsafe(16)
            CODES[code] = (q["client_id"], q.get("code_challenge"), redirect)
            back = redirect + "?" + urlencode({"code": code, "state": q.get("state", ""), "iss": b})
            return self.reply(302, None, [("Location", back)])
        if u.path == "/mcp":
            return self.reply(405)
        self.reply(404)

    def do_DELETE(self):
        self.reply(200)

    def issue(self, client):
        ISSUED[0] += 1
        n = ISSUED[0]
        TOKENS[f"tok-{n}"] = time.time() + LIFETIME
        REFRESH[f"ref-{n}"] = client
        return {"access_token": f"tok-{n}", "token_type": "Bearer", "expires_in": LIFETIME,
                "refresh_token": f"ref-{n}", "scope": "tools"}

    def do_POST(self):
        u = urlparse(self.path)
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if u.path == "/register":
            m = json.loads(body)
            client = "client-" + secrets.token_hex(4)
            CLIENTS[client] = m.get("redirect_uris", [])
            return self.reply(201, {
                "client_id": client,
                "client_name": m.get("client_name"),
                "redirect_uris": CLIENTS[client],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
            })
        if u.path == "/token":
            f = {k: v[0] for k, v in parse_qs(body.decode()).items()}
            if f.get("grant_type") == "authorization_code":
                client, challenge, redirect = CODES.pop(f.get("code"), (None, None, None))
                verifier = f.get("code_verifier", "")
                proof = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
                if client is None or proof != challenge or f.get("redirect_uri") != redirect:
                    return self.reply(400, {"error": "invalid_grant"})
                return self.reply(200, self.issue(client))
            if f.get("grant_type") == "refresh_token":
                client = REFRESH.pop(f.get("refresh_token"), None)
                if client is None:
                    return self.reply(400, {"error": "invalid_grant"})
                return self.reply(200, self.issue(client))
            return self.reply(400, {"error": "unsupported_grant_type"})
        if u.path != "/mcp":
            return self.reply(404)
        auth = self.headers.get("Authorization", "")
        token = auth[len("Bearer "):] if auth.startswith("Bearer ") else None
        if token not in TOKENS or TOKENS[token] < time.time():
            challenge = f'Bearer resource_metadata="{self.base()}/.well-known/oauth-protected-resource/mcp", scope="tools"'
            return self.reply(401, {"error": "invalid_token"}, [("WWW-Authenticate", challenge)])
        m = json.loads(body)
        if "id" not in m:
            return self.reply(202)
        method, i, p = m.get("method"), m["id"], m.get("params") or {}
        if method == "initialize":
            result = {"protocolVersion": p.get("protocolVersion", "2025-06-18"), "capabilities": {"tools": {}},
                      "serverInfo": {"name": "signed", "version": "1"}}
            return self.reply(200, {"jsonrpc": "2.0", "id": i, "result": result}, [("Mcp-Session-Id", "s1")])
        if method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            result = {"content": [{"type": "text", "text": f"signed in with {token}"}]}
        elif method == "ping":
            result = {}
        else:
            return self.reply(200, {"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
        self.reply(200, {"jsonrpc": "2.0", "id": i, "result": result})


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
