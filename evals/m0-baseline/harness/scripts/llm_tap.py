#!/usr/bin/env python3
"""Recording pass-through proxy for llama-server (host side, optional).

Forwards every request unchanged to UPSTREAM and streams the answer back while saving one JSONL
record per request: method, path, request body, status, the raw response (SSE included) and
llama-server's `timings` (cache_n, prompt_n, predicted_n, *_per_second) from the final chunk.
That is the raw model output the failure catalogue needs when the server-side tool-call parser
fails, plus cache-hit and tokens/s per step. Bind it to 127.0.0.1 only.

  python3 -I llm_tap.py --listen 127.0.0.1:8091 --upstream http://127.0.0.1:8090 --log RUN/llm.jsonl
"""
import argparse
import http.client
import json
import sys
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOP = {"connection", "keep-alive", "proxy-connection", "transfer-encoding", "te", "trailer", "upgrade", "host",
       "content-length"}
LOCK = threading.Lock()
A = None
MAX_LOG = 8 << 20
# The client is the agent's container and this process runs on the host, outside the container's
# memory and pids limits: bound what one client can make it buffer or spawn.
MAX_BODY = 64 << 20
CONNS = threading.BoundedSemaphore(256)


def timings_from(raw):
    t = None
    for line in raw.splitlines():
        if line.startswith("data: ") and '"timings"' in line:
            try:
                t = json.loads(line[6:]).get("timings") or t
            except ValueError:
                pass
    if t is None and raw.lstrip().startswith("{"):
        try:
            t = json.loads(raw).get("timings")
        except ValueError:
            pass
    return t


class Tap(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _proxy(self):
        t0 = time.time()
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            n = -1
        if not 0 <= n <= MAX_BODY:
            self.send_response(413)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            self._log(t0, b"", 413, f"request body of {n} bytes refused (limit {MAX_BODY})", None)
            return
        body = self.rfile.read(n) if n else b""
        up = urllib.parse.urlsplit(A.upstream)
        conn = http.client.HTTPConnection(up.hostname, up.port or 80, timeout=A.timeout)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        try:
            conn.request(self.command, self.path, body=body or None, headers=headers)
            resp = conn.getresponse()
        except OSError as e:
            self.send_response(502)
            self.send_header("Content-Length", "0")
            self.end_headers()
            self._log(t0, body, 502, f"upstream error: {e}", None)
            return
        self.send_response(resp.status)
        for k, v in resp.getheaders():
            if k.lower() not in HOP:
                self.send_header(k, v)
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        buf = bytearray()
        first = None
        try:
            while True:
                chunk = resp.read1(65536)
                if not chunk:
                    break
                if first is None:
                    first = time.time()
                if len(buf) < MAX_LOG:
                    buf += chunk
                self.wfile.write(b"%x\r\n%s\r\n" % (len(chunk), chunk))
                self.wfile.flush()
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
        except OSError:
            pass  # client went away (agent killed); keep what we have
        finally:
            conn.close()
        raw = buf.decode("utf-8", "replace")
        self._log(t0, body, resp.status, raw, first)

    def _log(self, t0, body, status, raw, first):
        try:
            req = json.loads(body) if body else None
        except ValueError:
            req = body.decode("utf-8", "replace")
        # user_agent: the agent can also call llama-server itself (curl in its shell); the
        # failure catalogue keeps those requests apart from OpenCode's own
        rec = {"t_start": t0, "t_first_byte": first, "t_end": time.time(), "method": self.command,
               "path": self.path, "status": status, "user_agent": self.headers.get("User-Agent"),
               "request": req, "timings": timings_from(raw), "response_raw": raw}
        with LOCK, open(A.log, "a", encoding="utf-8") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    do_GET = do_POST = do_PUT = do_DELETE = _proxy


class Server(ThreadingHTTPServer):
    daemon_threads = True

    def process_request(self, request, client_address):
        if not CONNS.acquire(blocking=False):   # too many open connections: drop this one
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            CONNS.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            CONNS.release()


def main():
    global A
    ap = argparse.ArgumentParser()
    ap.add_argument("--listen", default="127.0.0.1:8091")
    ap.add_argument("--upstream", default="http://127.0.0.1:8090")
    ap.add_argument("--log", required=True)
    ap.add_argument("--timeout", type=float, default=3600)
    A = ap.parse_args()
    host, port = A.listen.rsplit(":", 1)
    srv = Server((host, int(port)), Tap)
    print(f"llm_tap {A.listen} -> {A.upstream}, log {A.log}", file=sys.stderr, flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
