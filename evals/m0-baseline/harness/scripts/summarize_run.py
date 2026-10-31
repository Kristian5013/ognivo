#!/usr/bin/env python3
"""Rows, results and tables for M0 baseline run directories (made by run-task.sh / run-suite.sh).

  summarize_run.py RUN_DIR [RUN_DIR...]      one JSON line per run (failure-catalogue input)
  summarize_run.py --result RUN_DIR          the run's result.json (classification + summary)
  summarize_run.py --table STAMP_DIR [--json]   status matrix model x task of runs/<stamp>/

A run directory holds trajectory/ (events.jsonl, session*.json or stream.jsonl, diff.patch,
meta.json, llm.jsonl ...) and logs/ (run.env, suite.env, verify.log, verify_exit, ...); the
flat layout of the first drafts (everything in RUN_DIR) is read as well.

Row fields: task, model, exit/verify codes, outcome, steps, tool calls per tool, failed tool
calls (first 200 chars of their output), finish reasons, tokens (input/output/reasoning/cache
read), compactions, the last error event, raw tool-call markup leaked into text (a format
failure), wall time. Heuristic flags: timeout, infra, overflow, format_leak, loop, solved.

Statuses (result.json "status"; "final" says whether a resumed suite keeps the run):
  solved        verify passed (whatever the agent's exit code was)             final
  failed        verify failed after the agent finished normally                final
  timeout       verify failed and the agent ran into its time limit            final
  agent_error   verify failed and the agent stopped on an error event, wrote   final
                DISK_BUDGET_GB, or was killed before its budget ran out (an
                OOM kill: agent.oom_killed)
  stack_error   verify failed and the last LLM request failed (HTTP >= 400 or  final
                an SSE error, from trajectory/llm.jsonl): llama-server or the
                OpenCode-llama.cpp pairing broke, not the model's coding
  server_crash  infra_error with llama-server down after the run, still after   final
                M0_MAX_ATTEMPTS (3) attempts: the run's own trajectory takes the
                server down; scored as unsolved
  abandoned     not final after M0_MAX_ATTEMPTS (3) attempts for any other      final
                reason; "cause" keeps the last status; not scored
  setup_error   setup.sh failed: the agent never ran                           re-run
  infra_error   llama-server unreachable or gone during the run, the tap died,  re-run
                or the disk guard's floor
  harness_error the container could not run (image, engine)                    re-run
  verify_error  verify.sh could not run (exit 2) or the verify engine failed   re-run
  interrupted   stopped at the night's deadline or by a signal                 re-run
"""
import collections
import glob
import hashlib
import json
import os
import re
import sys

LEAK = re.compile(r"<\|channel\|>|<\|call\|>|<\|start\|>|<tool_call>|</tool_call>|<\|tool_call>|<tool_call\|>|"
                  r"<function=|<\|\"\|>|to=functions\.")
FINAL = ("solved", "failed", "timeout", "agent_error", "stack_error", "server_crash", "abandoned")
SHORT = {"solved": "PASS", "failed": "fail", "timeout": "TIME", "agent_error": "err", "setup_error": "SETUP",
         "infra_error": "INFRA", "harness_error": "HARN", "verify_error": "VERR", "interrupted": "intr",
         "stack_error": "STACK", "server_crash": "CRASH", "abandoned": "ABND"}
MAX_ATTEMPTS = int(os.environ.get("M0_MAX_ATTEMPTS") or 3)


def find(run, name):
    """An agent-side file: trajectory/ first (a copy of the container's /out), then the rest."""
    for sub in ("trajectory", "logs", ""):
        p = os.path.join(run, sub, name) if sub else os.path.join(run, name)
        if os.path.exists(p):
            return p
    return os.path.join(run, name)


def host_file(run, name):
    """A file the host writes (run.env, verify_exit, infra_error ...): logs/ or the flat layout of
    the first drafts, never trajectory/, which the agent can write through /out."""
    for p in (os.path.join(run, "logs", name), os.path.join(run, name)):
        if os.path.exists(p):
            return p
    return os.path.join(run, "logs", name)


def load(path, default=None):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def read_text(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read().strip()
    except OSError:
        return None


def read_env(path):
    out = {}
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                k, sep, v = line.rstrip("\n").partition("=")
                if sep and k:
                    out[k] = v
    except OSError:
        pass
    return out


def as_int(v):
    try:
        return int(str(v).strip())
    except (TypeError, ValueError):
        return None


def session_files(run):
    base = os.path.dirname(find(run, "session.json"))
    return [os.path.join(base, "session.json")] + sorted(glob.glob(os.path.join(base, "session-*.json")))


def summarize_claude(run):
    """Minimal summary of a Claude Code stream-json transcript (HARNESS=claude)."""
    path = find(run, "stream.jsonl")
    if not os.path.isfile(path):
        return None
    tools, tok, steps, outcome = collections.Counter(), collections.Counter(), 0, None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if e.get("type") == "assistant":
                steps += 1
                for b in (e.get("message") or {}).get("content") or []:
                    if isinstance(b, dict) and b.get("type") == "tool_use":
                        tools[b.get("name")] += 1
            elif e.get("type") == "result":
                outcome = e.get("subtype")
                for k, v in (e.get("usage") or {}).items():
                    if isinstance(v, (int, float)):
                        tok[k] += v
    return {"steps": steps, "tools": dict(tools), "tokens": dict(tok), "outcome": outcome}


def summarize_ognivo(run):
    """Summary of an `ognivo run --jsonl` event stream (HARNESS=ognivo), with ognivo's own
    counters (malformed and cut generations, edits, refusals, answers sent back) and the
    verification status of its final answer."""
    path = find(run, "ognivo.jsonl")
    if not os.path.isfile(path):
        return None
    tools, failed, calls, end = collections.Counter(), [], [], None
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            t = e.get("type")
            if t == "tool_started":
                tools[e.get("tool")] += 1
                try:
                    args = json.dumps(json.loads(e.get("arguments") or "null"), sort_keys=True)
                except ValueError:
                    args = e.get("arguments")
                calls.append((e.get("tool"), args))
            elif t == "tool_finished":
                r = e.get("result") or ""
                if r.startswith(("[error]", "[refused]")):
                    failed.append({"tool": e.get("tool"), "status": r[1:r.index("]")], "output": r[:200]})
            elif t == "end":
                end = e
    stats = (end or {}).get("stats") or {}
    answer = (end or {}).get("answer") or {}
    verification = answer.get("verification") or {}
    return {"steps": stats.get("steps", 0), "tools": dict(tools), "failed": failed, "calls": calls,
            "tokens": {k: stats[k] for k in ("prompt_tokens_processed", "prompt_tokens_cached",
                                             "completion_tokens") if k in stats},
            "outcome": verification.get("status") or ("no_answer" if end else "no_end_event"),
            "ognivo": {"exit": (end or {}).get("exit"), "seed": (end or {}).get("seed"), "stats": stats,
                       "verification": verification or None,
                       "unsupported_claim": answer.get("unsupported_claim")}}


def llm_wire(run):
    """What trajectory/llm.jsonl (the host-side tap) saw: failed requests and the prompt shape."""
    path = find(run, "llm.jsonl")
    if not os.path.isfile(path):
        return None
    n = 0
    errors, last_failed, first_req = [], False, None
    model_s, predicted, foreign = 0.0, 0, 0
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                continue
            if not str(r.get("path", "")).startswith("/v1/chat/completions"):
                continue
            ua = r.get("user_agent")
            if ua is not None and not str(ua).startswith(("opencode/", "claude-cli/")):
                foreign += 1   # the agent's own call to llama-server (curl ...), not the harness's
                continue
            n += 1
            if isinstance(r.get("t_end"), (int, float)) and isinstance(r.get("t_start"), (int, float)):
                model_s += r["t_end"] - r["t_start"]
            predicted += ((r.get("timings") or {}).get("predicted_n") or 0)
            if first_req is None and isinstance(r.get("request"), dict):
                first_req = r["request"]
            raw = r.get("response_raw") or ""
            msg = None
            if (r.get("status") or 0) >= 400:
                msg = raw[:300]
            else:
                for ln in raw.splitlines():
                    if ln.startswith("data: ") and '"error"' in ln:
                        try:
                            e = json.loads(ln[6:]).get("error")
                        except ValueError:
                            continue
                        if e:
                            msg = json.dumps(e)[:300]
            last_failed = msg is not None
            if msg is not None:
                errors.append({"request": n, "status": r.get("status"), "message": msg})
    out = {"requests": n, "foreign_requests": foreign, "model_s": round(model_s, 1), "predicted_n": predicted,
           "errors": len(errors), "last_request_failed": last_failed,
           "first_error": errors[0] if errors else None, "last_error": errors[-1] if len(errors) > 1 else None}
    if first_req:   # OpenCode picks the system prompt and tool set by model name
        sys_txt = "".join(m["content"] if isinstance(m.get("content"), str) else json.dumps(m.get("content"))
                          for m in first_req.get("messages") or [] if m.get("role") == "system")
        out["system_prompt_sha256"] = hashlib.sha256(sys_txt.encode()).hexdigest()
        out["tools"] = [t.get("function", {}).get("name") for t in first_req.get("tools") or []]
    return out


def summarize(run):
    meta = load(find(run, "meta.json"), {}) or {}
    row = {"run": run, "task": meta.get("task_id"), "model": meta.get("model"),
           "exit": meta.get("exit_code"), "verify": read_text(host_file(run, "verify_exit")),
           "wall_s": meta.get("wall_s"), "infra_error": read_text(host_file(run, "infra_error"))}
    sessions = [s for s in (load(p) for p in session_files(run)) if s]
    tools, failed, finishes = collections.Counter(), [], collections.Counter()
    tok = collections.Counter()
    steps = compactions = 0
    leaks = []
    calls = []
    for s in sessions:
        for m in s.get("messages", []):
            t = m.get("type")
            if t == "compaction" or "compaction" in str(t):
                compactions += 1
            if t != "assistant":
                continue
            steps += 1
            finishes[m.get("finish") or "none"] += 1
            for k, v in (m.get("tokens") or {}).items():
                if isinstance(v, (int, float)):
                    tok[k] += v
                elif isinstance(v, dict):
                    for kk, vv in v.items():
                        tok[f"{k}_{kk}"] += vv or 0
            for part in m.get("content", []):
                if part.get("type") == "tool":
                    name = part.get("name")
                    tools[name] += 1
                    st = part.get("state") or {}
                    calls.append((name, json.dumps(st.get("input"), sort_keys=True)))
                    if st.get("status") not in (None, "completed"):
                        out = " ".join(c.get("text", "") for c in st.get("content") or [] if isinstance(c, dict))
                        failed.append({"tool": name, "status": st.get("status"), "output": out[:200]})
                elif part.get("type") == "text" and LEAK.search(part.get("text") or ""):
                    leaks.append((part.get("text") or "")[:200])
    last_error = None
    try:
        with open(find(run, "events.jsonl"), encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                if e.get("type") == "error":
                    last_error = e.get("error")
    except OSError:
        pass
    claude = summarize_claude(run)
    if claude:
        steps = steps or claude["steps"]
        tools.update(claude["tools"])
        tok.update(claude["tokens"])
    og = summarize_ognivo(run)
    if og:
        steps = steps or og["steps"]
        tools.update(og["tools"])
        tok.update(og["tokens"])
        failed.extend(og["failed"])
        calls.extend(og["calls"])
    repeats = max(collections.Counter(calls).values(), default=0)
    root = sessions[0]["info"] if sessions else {}
    wire = llm_wire(run)
    row.update({"outcome": root.get("outcome") or (claude or {}).get("outcome") or (og or {}).get("outcome"),
                "sessions": len(sessions),
                "steps": steps, "tools": dict(tools), "failed_tools": failed[:20], "finish": dict(finishes),
                "tokens": dict(tok), "compactions": compactions, "last_error": last_error,
                "format_leaks": leaks[:5], "max_identical_calls": repeats, "llm": wire})
    if og:
        row["ognivo"] = og["ognivo"]
    err = last_error.get("type", "") if isinstance(last_error, dict) else ""
    row["flags"] = [f for f, on in [
        ("timeout", row["exit"] == 124),
        ("infra", bool(row["infra_error"]) or err in ("provider.transport", "provider.internal")),
        ("overflow", err == "provider.invalid-request" and "context" in json.dumps(last_error)),
        ("format_leak", bool(leaks)),
        ("loop", repeats >= 4),
        ("server_errors", bool(wire and wire["errors"])),
        ("solved", row["verify"] == "0"),
    ] if on]
    return row


def classify(suite, runenv, agent_rc, verify_rc, infra, wire=None, early_kill=False):
    disk = runenv.get("disk_limit", "")   # run-task.sh's disk guard stopped the container
    if as_int(suite.get("setup_exit")) not in (None, 0):
        return "setup_error"
    if suite.get("interrupted") == "1" or runenv.get("interrupted") == "1":
        return "interrupted"
    if agent_rc == 3 or infra or disk.startswith("floor"):
        return "infra_error"
    if agent_rc is None or agent_rc in (2, 125, 126, 127):
        return "harness_error"
    if suite.get("server_ok_after") == "0":
        return "infra_error"
    if verify_rc is None or verify_rc in (2, 125, 126, 127):
        return "verify_error"
    if verify_rc == 0:
        return "solved"
    if disk.startswith("budget"):   # the agent itself wrote DISK_BUDGET_GB: not a time limit
        return "agent_error"
    if wire and wire.get("last_request_failed"):   # the run ended on a llama-server error
        return "stack_error"
    if agent_rc in (137, 143) and early_kill:
        # killed before its budget ran out: an OOM kill (run.env oom_killed says so) by the
        # agent's own builds, or the agent killing its own process; not the clock
        return "agent_error"
    if agent_rc in (124, 137, 143):
        return "timeout"
    if agent_rc == 0:
        return "failed"
    return "agent_error"


def result(run):
    suite = read_env(os.path.join(run, "logs", "suite.env"))
    runenv = read_env(host_file(run, "run.env"))
    meta = load(find(run, "meta.json"), {}) or {}
    agent_rc = as_int(runenv.get("container_exit"))
    if agent_rc is None:
        agent_rc = as_int(meta.get("exit_code"))
    oc_rc = as_int(read_text(find(run, "exit_code")))   # the agent's own exit, written by the entrypoint
    if agent_rc in (137, 143) and oc_rc in (0, 1):
        agent_rc = oc_rc   # the agent had finished; the container was killed later (export, git)
    verify_rc = as_int(read_text(host_file(run, "verify_exit")))
    infra = read_text(host_file(run, "infra_error"))
    row = None
    if suite.get("agent_mode", "container") == "container":
        row = summarize(run)
        for k in ("run", "task", "model", "exit", "verify", "infra_error"):
            row.pop(k, None)
    # 137/143 with the agent's wall time clearly under its budget: it did not run out of time
    w, b = meta.get("wall_s"), meta.get("agent_timeout_s")
    early_kill = isinstance(w, (int, float)) and isinstance(b, (int, float)) and w < b - 1
    status = classify(suite, runenv, agent_rc, verify_rc, infra, (row or {}).get("llm"), early_kill)

    def num(d, k, cast=int):
        try:
            return cast(d[k]) if d.get(k) not in (None, "") else None
        except ValueError:
            return None

    # A run that is still not final after MAX_ATTEMPTS attempts (the same setup, infra or verify
    # error every night) is closed as "abandoned" instead of being re-run forever.
    cause = None
    attempt = num(suite, "attempt")
    counted = num(suite, "attempts_toward_cap") or attempt   # interrupted attempts do not count
    if status not in FINAL and status != "interrupted" and counted and counted >= MAX_ATTEMPTS:
        # a run that kept taking llama-server down is charged to the model (unsolved, scored);
        # setup, harness and verify trouble says nothing about it (abandoned, not scored)
        crash = status == "infra_error" and suite.get("server_ok_after") == "0"
        cause, status = status, ("server_crash" if crash else "abandoned")
    exp = suite.get("prompt_sha256_expected")
    return {
        "schema": "ognivo.m0.result/1",
        "task_id": suite.get("task_id") or meta.get("task_id"),
        "model": suite.get("model") or meta.get("model"),
        "harness": suite.get("harness") or runenv.get("harness"),
        "agent_mode": suite.get("agent_mode", "container"),
        "status": status,
        "cause": cause,
        "final": status in FINAL,
        "attempt": attempt,
        "stamp": suite.get("stamp"),
        "started_utc": suite.get("started_utc"),
        "finished_utc": suite.get("finished_utc"),
        "setup": {"exit": num(suite, "setup_exit"), "wall_s": num(suite, "setup_wall_s", float)},
        "agent": {"exit": agent_rc, "timeout_s": num(suite, "agent_timeout"),
                  "timed_out": agent_rc in (124, 137, 143) and not runenv.get("disk_limit")
                               and not early_kill,
                  "disk_limit": runenv.get("disk_limit") or None,
                  "oom_killed": {"true": True, "false": False}.get(runenv.get("oom_killed", "")),
                  "wall_s": meta.get("wall_s"), "session_id": meta.get("session_id") or None,
                  "version": meta.get("opencode") or meta.get("claude") or meta.get("ognivo"),
                  "infra_error": infra},
        "verify": {"exit": verify_rc, "timeout_s": num(suite, "verify_timeout"),
                   "wall_s": num(runenv, "verify_wall_s", float)},
        "server": {"managed": suite.get("server_managed") == "1", "url": suite.get("server_url"),
                   "ok_after_run": {"1": True, "0": False}.get(suite.get("server_ok_after"))},
        "image": {"name": runenv.get("image"), "id": runenv.get("image_id")},
        "container": runenv.get("container"),
        "network": runenv.get("network"),
        "versions": {"suite_sha256": suite.get("suite_sha256") or None,
                     "task_sha256": suite.get("task_sha256") or None},
        "host": {"ac_online": {"1": True, "0": False}.get(suite.get("host_ac_online", "")),
                 "platform_profile": suite.get("host_platform_profile") or None,
                 "gpu_at_start": suite.get("host_gpu") or None,
                 "loadavg_at_start": suite.get("host_loadavg") or None},
        "prompt_sha256": suite.get("prompt_sha256"),
        "prompt_matches_task_json": (suite.get("prompt_sha256") == exp) if exp else None,
        "deadline_hit": suite.get("deadline_hit") == "1",
        "paths": {"task_dir": suite.get("task_dir"), "workspace": suite.get("workspace"),
                  "workspace_kept": suite.get("workspace_kept") == "1"},
        "summary": row,
    }


def table(stamp_dir, as_json=False):
    models = sorted(d for d in os.listdir(stamp_dir)
                    if os.path.isdir(os.path.join(stamp_dir, d)) and not d.startswith(("_", ".")))
    grid, tasks = {}, set()
    for m in models:
        for p in glob.glob(os.path.join(stamp_dir, m, "*", "result.json")):
            r = load(p)
            if not isinstance(r, dict):
                continue
            t = os.path.basename(os.path.dirname(p))
            tasks.add(t)
            grid[(m, t)] = r
    tasks = sorted(tasks)
    if as_json:
        for (m, t), r in sorted(grid.items()):
            s = r.get("summary") or {}
            print(json.dumps({"model": m, "task": t, "status": r.get("status"), "final": r.get("final"),
                              "agent_exit": (r.get("agent") or {}).get("exit"),
                              "verify_exit": (r.get("verify") or {}).get("exit"),
                              "wall_s": (r.get("agent") or {}).get("wall_s"), "steps": s.get("steps"),
                              "tokens": s.get("tokens"), "flags": s.get("flags")}, ensure_ascii=False))
        return 0
    if not grid:
        print(f"no results under {stamp_dir}")
        return 0
    models = [m for m in models if any(mm == m for mm, _ in grid)]
    w = max([len(t) for t in tasks] + [19])
    print(f"{'task':{w}}  " + "  ".join(f"{m[:15]:>15}" for m in models))
    for t in tasks:
        cells = []
        for m in models:
            r = grid.get((m, t))
            if not r:
                cells.append(f"{'-':>15}")
                continue
            a = (r.get("agent") or {}).get("wall_s")
            cell = SHORT.get(r.get("status"), r.get("status") or "?")
            if isinstance(a, (int, float)):
                cell += f" {a:.0f}s"
            cells.append(f"{cell:>15}")
        print(f"{t:{w}}  " + "  ".join(cells))
    tot, gave_up = [], []
    for m in models:
        rs = [r for (mm, _), r in grid.items() if mm == m]
        # "abandoned" is final (no more re-runs) but says nothing about the model: kept out of
        # the denominator and counted on its own line
        scored = [r for r in rs if r.get("final") and r.get("status") != "abandoned"]
        tot.append(f"{sum(r.get('status') == 'solved' for r in rs)}/{len(scored)} ({len(rs)})")
        gave_up.append(str(sum(r.get("status") == "abandoned" for r in rs)))
    print(f"{'solved/final (runs)':{w}}  " + "  ".join(f"{c:>15}" for c in tot))
    if any(g != "0" for g in gave_up):
        print(f"{'abandoned (no score)':{w}}  " + "  ".join(f"{c:>15}" for c in gave_up))
    print("PASS solved, fail failed, TIME timeout, err agent error, STACK ended on a llama-server error, "
          "CRASH kept taking the server down, ABND abandoned after M0_MAX_ATTEMPTS (not scored), "
          "intr interrupted; INFRA/HARN/VERR/SETUP/intr are re-run on resume")
    return 0


if __name__ == "__main__":
    a = sys.argv[1:]
    if not a:
        sys.stderr.write(__doc__)
        sys.exit(2)
    if a[0] == "--result" and len(a) == 2:
        json.dump(result(a[1]), sys.stdout, indent=1, ensure_ascii=False)
        sys.stdout.write("\n")
    elif a[0] == "--table" and len(a) >= 2:
        sys.exit(table(a[1], as_json="--json" in a[2:]))
    elif a[0].startswith("--"):
        sys.stderr.write(__doc__)
        sys.exit(2)
    else:
        for d in a:
            print(json.dumps(summarize(d), ensure_ascii=False))
