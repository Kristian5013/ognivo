#!/usr/bin/env python3
"""Probe what a running llama.cpp-family server can do for the agent (M0, dev-only).

Answers the capability questions from docs/architecture.md §4/§5.4 against one live
server: model family, whether structural tokens are recognised with
parse_special=false (injection risk), whether streamed /completion delivers every
sampled token id (PR #31244), whether n_probs carries ids, lazy-grammar and
reasoning-budget acceptance, and a basic OpenAI-style tool call.

Usage: python3 -I scripts/probe/server_caps.py --url http://127.0.0.1:8080 --out report.json
Stdlib only.
"""

import argparse
import hashlib
import http.client
import json
import sys
import urllib.parse

FAMILIES = {
    # family: (probe string that must be a single structural token, structural strings)
    "harmony": ("<|channel|>", ["<|start|>", "<|end|>", "<|message|>", "<|channel|>", "<|constrain|>", "<|call|>", "<|return|>"]),
    "gemma4": ("<|tool_call>", ["<|turn>", "<turn|>", "<|tool_call>", "<tool_call|>", "<|tool_response>", '<|"|>', "<|channel>", "<channel|>"]),
    "qwen-xml": ("<tool_call>", ["<|im_start|>", "<|im_end|>", "<tool_call>", "</tool_call>", "<think>", "</think>", "<tool_response>"]),
}

EMOJI_TEXT = "Repeat the following line exactly three times and output nothing else:\n🚀 ⚠️ └── ├── │ ✅ 🔥 привет 日本語"


def render(family, text):
    """Minimal single-turn prompt in the family's native format (thinking off / low)."""
    if family == "harmony":
        return ("<|start|>system<|message|>You are ChatGPT, a large language model trained by OpenAI.\n"
                "Knowledge cutoff: 2024-06\nCurrent date: 2026-10-29\n\nReasoning: low\n\n"
                "# Valid channels: analysis, commentary, final. Channel must be included for every message.<|end|>"
                f"<|start|>user<|message|>{text}<|end|><|start|>assistant")
    if family == "gemma4":
        return f"<bos><|turn>user\n{text}<turn|>\n<|turn>model\n<|channel>thought\n<channel|>"
    if family == "qwen-xml":
        return f"<|im_start|>user\n{text}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
    raise ValueError(family)


class Server:
    def __init__(self, url, api_key=None, timeout=600):
        u = urllib.parse.urlparse(url)
        self.host, self.port = u.hostname, u.port or (443 if u.scheme == "https" else 80)
        self.https = u.scheme == "https"
        self.headers = {"Content-Type": "application/json"}
        if api_key:
            self.headers["Authorization"] = f"Bearer {api_key}"
        self.timeout = timeout

    def _conn(self):
        cls = http.client.HTTPSConnection if self.https else http.client.HTTPConnection
        return cls(self.host, self.port, timeout=self.timeout)

    def request(self, method, path, body=None):
        c = self._conn()
        c.request(method, path, body=json.dumps(body) if body is not None else None, headers=self.headers)
        r = c.getresponse()
        data = r.read().decode("utf-8", errors="replace")
        c.close()
        try:
            return r.status, json.loads(data) if data else None
        except json.JSONDecodeError:
            return r.status, data

    def stream(self, path, body):
        """POST and yield parsed SSE `data:` payloads."""
        c = self._conn()
        c.request("POST", path, body=json.dumps(body), headers=self.headers)
        r = c.getresponse()
        if r.status != 200:
            yield {"_http_status": r.status, "_body": r.read().decode("utf-8", errors="replace")[:2000]}
            c.close()
            return
        buf = b""
        while True:
            chunk = r.read1(65536) if hasattr(r, "read1") else r.read(65536)
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                line = line.strip()
                if not line.startswith(b"data:"):
                    continue
                payload = line[5:].strip()
                if payload == b"[DONE]":
                    c.close()
                    return
                try:
                    yield json.loads(payload)
                except json.JSONDecodeError:
                    yield {"_unparsed": payload[:500].decode("utf-8", errors="replace")}
        c.close()

    def tokenize(self, text, parse_special):
        status, data = self.request("POST", "/tokenize", {"content": text, "parse_special": parse_special, "add_special": False})
        if status != 200 or not isinstance(data, dict):
            return None
        return [t if isinstance(t, int) else t.get("id") for t in data.get("tokens", [])]


def detect_family(srv):
    for fam, (probe, _) in FAMILIES.items():
        toks = srv.tokenize(probe, parse_special=True)
        if toks is not None and len(toks) == 1:
            return fam
    return None


def probe_special_tokens(srv, family):
    out = {}
    for s in FAMILIES[family][1]:
        special = srv.tokenize(s, parse_special=True)
        plain = srv.tokenize(s, parse_special=False)
        out[s] = {
            "id": special[0] if special and len(special) == 1 else None,
            "parse_special_false": plain,
            "recognised_without_parse_special": bool(plain and special and plain == special and len(plain) == 1),
        }
    return out


def stream_completion(srv, prompt_ids, extra, n_predict=200):
    body = {"prompt": prompt_ids, "stream": True, "return_tokens": True, "cache_prompt": False,
            "n_predict": n_predict, "temperature": 0.0, "seed": 1}
    body.update(extra)
    ids, probs_ids, content, final, errors = [], [], [], None, []
    for ev in srv.stream("/completion", body):
        if "_http_status" in ev or "_unparsed" in ev or "error" in ev:
            errors.append(ev)
            continue
        ids.extend(ev.get("tokens") or [])
        content.append(ev.get("content") or "")
        for p in ev.get("completion_probabilities") or []:
            if "id" in p:
                probs_ids.append(p["id"])
        if ev.get("stop"):
            final = ev
    predicted = None
    if final:
        predicted = (final.get("timings") or {}).get("predicted_n", final.get("tokens_predicted"))
    return {"ids": ids, "probs_ids": probs_ids, "content": "".join(content), "predicted_n": predicted,
            "stop_type": final.get("stop_type") if final else None, "errors": errors}


def probe_stream_ids(srv, family):
    prompt = srv.tokenize(render(family, EMOJI_TEXT), parse_special=True)
    if not prompt:
        return {"error": "tokenize failed"}
    plain = stream_completion(srv, prompt, {})
    with_probs = stream_completion(srv, prompt, {"n_probs": 1})
    def detok(ids):
        status, data = srv.request("POST", "/detokenize", {"tokens": ids})
        return data.get("content") if status == 200 and isinstance(data, dict) else None

    detok_all = detok(plain["ids"]) if plain["ids"] else None
    detok_but_last = detok(plain["ids"][:-1]) if plain["ids"] else None
    return {
        "prompt_tokens": len(prompt),
        "stream_ids": len(plain["ids"]),
        "predicted_n": plain["predicted_n"],
        "stream_ids_complete": plain["predicted_n"] is not None and len(plain["ids"]) == plain["predicted_n"],
        "detokenize_matches_content": detok_all == plain["content"],
        # the stop (EOG) token id is streamed too, but its text is not part of `content`
        "detokenize_without_last_matches_content": detok_but_last == plain["content"],
        "last_id": plain["ids"][-1] if plain["ids"] else None,
        "last_id_text": detok(plain["ids"][-1:]) if plain["ids"] else None,
        "n_probs_ids": len(with_probs["probs_ids"]),
        "n_probs_ids_complete": with_probs["predicted_n"] is not None and len(with_probs["probs_ids"]) == with_probs["predicted_n"],
        "stop_type": plain["stop_type"],
        "sample": plain["content"][:300],
        "errors": plain["errors"] + with_probs["errors"],
    }


def probe_accepts(srv, family):
    prompt = srv.tokenize(render(family, "Say: ANSWER: ok"), parse_special=True)
    tests = {
        # after a lazy trigger fires, the grammar is matched from the trigger text itself,
        # so the grammar has to start with it
        "grammar_lazy_word_trigger": {"grammar": 'root ::= "ANSWER:" " ok"', "grammar_lazy": True,
                                      "grammar_triggers": [{"type": 1, "value": "ANSWER:"}]},
        "reasoning_budget_fields": {"reasoning_budget_tokens": 16, "reasoning_budget_start_tag": "<think>",
                                    "reasoning_budget_end_tags": ["</think>"], "reasoning_budget_message": "..."},
        "logit_bias_ban": {"logit_bias": [[0, False]]},
    }
    out = {}
    for name, extra in tests.items():
        body = {"prompt": prompt, "n_predict": 16, "temperature": 0.0, "stream": False}
        body.update(extra)
        status, data = srv.request("POST", "/completion", body)
        out[name] = {"http_status": status,
                     "error": data.get("error") if isinstance(data, dict) and "error" in data else (data if status != 200 else None)}
    return out


def probe_chat_tools(srv):
    tools = [{"type": "function", "function": {
        "name": "get_time", "description": "Return the current time in a city.",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}}]
    body = {"messages": [{"role": "user", "content": "What time is it in Paris? Use the tool."}],
            "tools": tools, "tool_choice": "auto", "stream": False, "max_tokens": 1024, "temperature": 0.0}
    status, data = srv.request("POST", "/v1/chat/completions", body)
    if status != 200 or not isinstance(data, dict):
        return {"http_status": status, "error": data}
    choice = (data.get("choices") or [{}])[0]
    msg = choice.get("message") or {}
    return {"http_status": status, "finish_reason": choice.get("finish_reason"),
            "tool_calls": msg.get("tool_calls"), "has_reasoning_content": bool(msg.get("reasoning_content")),
            "content": (msg.get("content") or "")[:300]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8080")
    ap.add_argument("--api-key")
    ap.add_argument("--out")
    ap.add_argument("--family", choices=["auto", *FAMILIES])
    args = ap.parse_args()

    srv = Server(args.url, args.api_key)
    report = {"url": args.url}
    report["health"] = srv.request("GET", "/health")
    status, props = srv.request("GET", "/props")
    if status == 200 and isinstance(props, dict):
        tpl = props.get("chat_template") or ""
        report["props"] = {k: props.get(k) for k in ("build_info", "model_path", "total_slots", "modalities", "chat_template_caps")}
        report["props"]["n_ctx"] = (props.get("default_generation_settings") or {}).get("n_ctx")
        report["props"]["chat_template_sha256"] = hashlib.sha256(tpl.encode()).hexdigest()
        settings = props.get("default_generation_settings") or {}
        params = settings.get("params") if isinstance(settings.get("params"), dict) else settings  # ik: flat
        report["props"]["default_sampling"] = {k: params.get(k) for k in ("temperature", "top_k", "top_p", "min_p", "repeat_penalty")}
    else:
        report["props"] = {"http_status": status, "body": props}
    report["models"] = srv.request("GET", "/v1/models")[1]
    report["slots_endpoint_http_status"] = srv.request("GET", "/slots")[0]

    family = args.family if args.family and args.family != "auto" else detect_family(srv)
    report["family"] = family
    if family:
        report["special_tokens"] = probe_special_tokens(srv, family)
        report["stream_ids"] = probe_stream_ids(srv, family)
        report["accepts"] = probe_accepts(srv, family)
    report["chat_tools"] = probe_chat_tools(srv)

    text = json.dumps(report, ensure_ascii=False, indent=2)
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text + "\n")
    print(text)


if __name__ == "__main__":
    sys.exit(main())
