#!/usr/bin/env python3
"""Tiny stand-in for llama-server's OpenAI-compatible API (no model, no GPU).

It records every request (method, path, headers, JSON body) to a JSONL log and plays a
fixed 3-step agent script on /v1/chat/completions:
  step 1 (no tool results yet)  -> call the `write` tool (hello.txt)
  step 2 (one tool result)      -> call the shell tool (cat hello.txt; echo SHELL_OK)
  step 3 (two+ tool results)    -> final answer, finish_reason=stop
Requests without tools (title/summary agents) get a short plain-text answer.
Every answer streams `reasoning_content` first, like llama-server with --reasoning-format auto.
/health, /props and /v1/models answer like llama-server: /props reports the --alias, the n_ctx
and, with --chat-template-file, that file as chat_template, so run-suite.sh's server checks
(scripts/llama_cmd.py check) run against the mock exactly as against the real server.

Usage: python3 -I mock_llama.py --host 127.0.0.1 --port 18080 --log requests.jsonl
         [--alias gpt-oss-20b] [--chat-template-file T] [--delay SECONDS] [--scenario write|search|subagent]
         [--fail ctx|500|slow] [--die-after N [--die-marker FILE]]
"""
import argparse
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOCK = threading.Lock()
ARGS = None
COUNTER = {"n": 0}


def log(entry):
    with LOCK:
        with open(ARGS.log, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")


def pick_tool(tools, names):
    for t in tools or []:
        fn = t.get("function", {})
        if fn.get("name") in names:
            return fn
    return None


def prop(fn, candidates, default):
    props = (fn.get("parameters") or {}).get("properties") or {}
    for c in candidates:
        if c in props:
            return c
    return default


def plan(body):
    tools = body.get("tools") or []
    msgs = body.get("messages") or []
    n_tool = sum(1 for m in msgs if m.get("role") == "tool")
    if not tools:
        return {"reasoning": "", "text": "Mock title", "call": None}
    if ARGS.scenario == "subagent":
        names = [t.get("function", {}).get("name") for t in tools]
        if "subagent" not in names:
            return {"reasoning": "Child: nothing to do.", "text": "CHILD DONE: README.md only", "call": None}
        if n_tool == 0:
            return {"reasoning": "Delegate.", "text": "", "call": ("subagent", {"agent": "explore", "description": "List repo files",
                    "prompt": "List the files in the repository and report them."})}
        return {"reasoning": "Child answered.", "text": "DONE: subagent reported", "call": None}
    if ARGS.scenario == "search" and n_tool == 0:
        fn = pick_tool(tools, ["grep"])
        if fn:
            return {"reasoning": "Search first.", "text": "", "call": (fn["name"], {prop(fn, ["pattern"], "pattern"): "hello"})}
    if ARGS.scenario == "search" and n_tool == 1:
        fn = pick_tool(tools, ["glob"])
        if fn:
            return {"reasoning": "Now list files.", "text": "", "call": (fn["name"], {prop(fn, ["pattern"], "pattern"): "**/*"})}
    if ARGS.scenario == "search":
        return {"reasoning": "Done searching.", "text": "DONE: searched", "call": None}
    if n_tool == 0:
        fn = pick_tool(tools, ["write"])
        if fn:
            args = {prop(fn, ["path", "filePath", "file_path"], "path"): "hello.txt",
                    prop(fn, ["content"], "content"): "hello from mock\n"}
            return {"reasoning": "I should create the file first.", "text": "", "call": (fn["name"], args)}
    if n_tool <= 1:
        fn = pick_tool(tools, ["shell", "bash"])
        if fn:
            args = {prop(fn, ["command", "cmd"], "command"): "cat hello.txt && echo SHELL_OK"}
            desc = prop(fn, ["description"], None)
            if desc:
                args[desc] = "Show the file"
            return {"reasoning": "Now verify it with the shell.", "text": "", "call": (fn["name"], args)}
    return {"reasoning": "Both steps worked.", "text": "DONE: created hello.txt", "call": None}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *a):  # quiet
        pass

    def _send(self, code, obj):
        data = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        log({"t": time.time(), "method": "GET", "path": self.path, "headers": dict(self.headers)})
        if self.path.startswith("/v1/models") or self.path.startswith("/models"):
            self._send(200, {"object": "list", "data": [
                {"id": ARGS.model, "object": "model", "owned_by": "llamacpp", "meta": {"n_ctx_train": 131072}}]})
        elif self.path.startswith("/health") or self.path.startswith("/v1/health"):
            self._send(200, {"status": "ok"})
        elif self.path.startswith("/props"):
            props = {"model_alias": ARGS.model, "default_generation_settings": {"n_ctx": ARGS.n_ctx},
                     "build_info": "mock", "total_slots": 1}
            if ARGS.chat_template_file:
                with open(ARGS.chat_template_file, encoding="utf-8") as f:
                    props["chat_template"] = f.read()
            if ARGS.model_path:
                props["model_path"] = ARGS.model_path
            self._send(200, props)
        else:
            self._send(404, {"error": {"message": "not found"}})

    def anthropic(self, body):
        """Minimal /v1/messages (Anthropic SSE) with the same 3-step script, via the Bash tool."""
        msgs = body.get("messages") or []
        n_res = sum(1 for m in msgs if isinstance(m.get("content"), list)
                    for b in m["content"] if isinstance(b, dict) and b.get("type") == "tool_result")
        names = [t.get("name") for t in body.get("tools") or []]
        if not names:
            text, call = "Mock title", None
        elif n_res == 0 and "Bash" in names:
            text, call = "", ("Bash", {"command": "printf 'hello from mock\\n' > hello.txt", "description": "Create file"})
        elif n_res == 1 and "Bash" in names:
            text, call = "", ("Bash", {"command": "cat hello.txt && echo SHELL_OK", "description": "Show file"})
        else:
            text, call = "DONE: created hello.txt", None
        mid = f"msg_mock{COUNTER['n']}"
        usage = {"input_tokens": 1000, "output_tokens": 20, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
        content = [{"type": "thinking", "thinking": f"mock reasoning {COUNTER['n']}", "signature": ""}]
        if text:
            content.append({"type": "text", "text": text})
        if call:
            content.append({"type": "tool_use", "id": f"toolu_{COUNTER['n']}", "name": call[0], "input": call[1]})
        stop = "tool_use" if call else "end_turn"
        if not body.get("stream"):
            self._send(200, {"id": mid, "type": "message", "role": "assistant", "model": body.get("model"),
                             "content": content, "stop_reason": stop, "stop_sequence": None, "usage": usage})
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Connection", "close")
        self.end_headers()
        def ev(name, obj):
            self.wfile.write(f"event: {name}\ndata: {json.dumps(obj)}\n\n".encode()); self.wfile.flush()
        ev("message_start", {"type": "message_start", "message": {"id": mid, "type": "message", "role": "assistant",
            "model": body.get("model"), "content": [], "stop_reason": None, "stop_sequence": None, "usage": usage}})
        for i, blk in enumerate(content):
            if blk["type"] == "thinking":
                ev("content_block_start", {"type": "content_block_start", "index": i, "content_block": {"type": "thinking", "thinking": ""}})
                ev("content_block_delta", {"type": "content_block_delta", "index": i, "delta": {"type": "thinking_delta", "thinking": blk["thinking"]}})
                ev("content_block_delta", {"type": "content_block_delta", "index": i, "delta": {"type": "signature_delta", "signature": ""}})
            elif blk["type"] == "text":
                ev("content_block_start", {"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
                ev("content_block_delta", {"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": blk["text"]}})
            else:
                ev("content_block_start", {"type": "content_block_start", "index": i,
                    "content_block": {"type": "tool_use", "id": blk["id"], "name": blk["name"], "input": {}}})
                ev("content_block_delta", {"type": "content_block_delta", "index": i,
                    "delta": {"type": "input_json_delta", "partial_json": json.dumps(blk["input"])}})
            ev("content_block_stop", {"type": "content_block_stop", "index": i})
        ev("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 20}})
        ev("message_stop", {"type": "message_stop"})
        self.close_connection = True

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            body = json.loads(raw or b"{}")
        except Exception:
            body = {"_raw": raw.decode("utf-8", "replace")}
        COUNTER["n"] += 1
        log({"t": time.time(), "i": COUNTER["n"], "method": "POST", "path": self.path,
             "headers": dict(self.headers), "body": body})
        if ARGS.delay:
            time.sleep(ARGS.delay)
        if ARGS.die_after and COUNTER["n"] > ARGS.die_after and not os.path.exists(ARGS.die_marker or ""):
            if ARGS.die_marker:
                open(ARGS.die_marker, "w").close()
            sys.stderr.write("mock: crashing on purpose (--die-after)\n")
            os._exit(3)   # a crashed server: the connection drops, the port closes
        if self.path.split("?")[0].rstrip("/").endswith("/messages/count_tokens"):
            self._send(200, {"input_tokens": 1234})
            return
        if self.path.split("?")[0].rstrip("/").endswith("/v1/messages"):
            self.anthropic(body)
            return
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self._send(404, {"error": {"message": "only chat completions are mocked"}})
            return
        if ARGS.fail == "slow" and body.get("tools") and sum(1 for m in body.get("messages") or [] if m.get("role") == "tool") >= 1:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            for k in range(600):  # a 10-minute "generation" that never finishes in time
                obj = {"id": "slow", "object": "chat.completion.chunk", "created": int(time.time()), "model": ARGS.model,
                       "choices": [{"index": 0, "delta": {"reasoning_content": f"thinking {k} "}, "finish_reason": None}]}
                try:
                    self.wfile.write(b"data: " + json.dumps(obj).encode() + b"\n\n"); self.wfile.flush()
                except Exception:
                    return
                time.sleep(1)
            return
        if ARGS.fail and ARGS.fail != "slow":
            code, msg = {"ctx": (400, "the request exceeds the available context size, try increasing it"),
                         "500": (500, "Unexpected empty grammar stack")}[ARGS.fail]
            self._send(code, {"error": {"code": code, "message": msg, "type": "exceed_context_size_error" if code == 400 else "server_error"}})
            return
        p = plan(body)
        cid = f"chatcmpl-mock{COUNTER['n']}"
        created = int(time.time())
        finish = "tool_calls" if p["call"] else "stop"
        usage = {"prompt_tokens": 1000 + 10 * COUNTER["n"], "completion_tokens": 20,
                 "total_tokens": 1020 + 10 * COUNTER["n"]}
        call = None
        if p["call"]:
            call = {"id": f"call_{COUNTER['n']}", "type": "function",
                    "function": {"name": p["call"][0], "arguments": json.dumps(p["call"][1])}}
        if not body.get("stream"):
            msg = {"role": "assistant", "content": p["text"] or None, "reasoning_content": p["reasoning"] or None}
            if call:
                msg["tool_calls"] = [call]
            self._send(200, {"id": cid, "object": "chat.completion", "created": created, "model": ARGS.model,
                             "choices": [{"index": 0, "message": msg, "finish_reason": finish}], "usage": usage})
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        def chunk(delta, fr=None, extra=None):
            obj = {"id": cid, "object": "chat.completion.chunk", "created": created, "model": ARGS.model,
                   "choices": [{"index": 0, "delta": delta, "finish_reason": fr}]}
            if extra:
                obj.update(extra)
            self.wfile.write(b"data: " + json.dumps(obj).encode() + b"\n\n")
            self.wfile.flush()

        chunk({"role": "assistant", "content": None})
        for piece in (p["reasoning"][:12], p["reasoning"][12:]) if p["reasoning"] else ():
            if piece:
                chunk({"reasoning_content": piece})
        if p["text"]:
            chunk({"content": p["text"]})
        if call:
            chunk({"tool_calls": [{"index": 0, "id": call["id"], "type": "function",
                                   "function": {"name": call["function"]["name"], "arguments": ""}}]})
            a = call["function"]["arguments"]
            chunk({"tool_calls": [{"index": 0, "function": {"arguments": a[: len(a) // 2]}}]})
            chunk({"tool_calls": [{"index": 0, "function": {"arguments": a[len(a) // 2:]}}]})
        chunk({}, finish, {"timings": {"cache_n": 900, "prompt_n": 100 + COUNTER["n"], "prompt_ms": 12.5,
                                       "predicted_n": 20, "predicted_ms": 130.0, "predicted_per_second": 153.8}})
        if (body.get("stream_options") or {}).get("include_usage"):
            obj = {"id": cid, "object": "chat.completion.chunk", "created": created, "model": ARGS.model,
                   "choices": [], "usage": usage}
            self.wfile.write(b"data: " + json.dumps(obj).encode() + b"\n\n")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


def main():
    global ARGS
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=18080)
    ap.add_argument("--log", required=True)
    ap.add_argument("--model", "--alias", dest="model", default="gpt-oss-20b")
    ap.add_argument("--model-path", default=None, help="reported as /props model_path")
    ap.add_argument("--chat-template-file", default=None, help="reported as /props chat_template")
    ap.add_argument("--n-ctx", type=int, default=98304)
    ap.add_argument("--delay", type=float, default=0.0, help="seconds to wait before each POST answer")
    ap.add_argument("--die-after", type=int, default=0, help="exit (crash) on the POST after this many")
    ap.add_argument("--die-marker", default=None, help="crash only while this file does not exist; create it")
    ap.add_argument("--scenario", default="write", choices=["write", "search", "subagent"])
    ap.add_argument("--fail", default=None, choices=["ctx", "500", "slow"])
    ARGS = ap.parse_args()
    srv = ThreadingHTTPServer((ARGS.host, ARGS.port), H)
    print(f"mock llama-server on http://{ARGS.host}:{ARGS.port}", file=sys.stderr, flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
