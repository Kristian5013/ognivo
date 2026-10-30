#!/usr/bin/env python3
"""Regression tests for the scorer (scripts/summarize_run.py) on synthetic run directories.

  python3 -I tests/scoring_test.py SANDBOX_DIR     prints one "ok"/"FAIL" line per case; exit 1 on a failure

Covers the paths the end-to-end stages do not reach: files the agent forges through /out
(trajectory/verify_exit, run.env, infra_error), OOM and early kills, the disk guard, the last
LLM request failing (also when it was the agent's own curl), M0_MAX_ATTEMPTS with and without
interrupted attempts, server_crash, and the table's denominator.
"""
import importlib.util
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("summarize_run", os.path.join(HERE, "..", "scripts", "summarize_run.py"))
sr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sr)

FAILS = 0


def run_dir(root, name, *, container_exit=0, verify=1, suite=None, runenv=None, meta=None, traj=None,
            logs=None, llm=None):
    d = os.path.join(root, name)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(os.path.join(d, "trajectory"))
    os.makedirs(os.path.join(d, "logs"))
    s = {"task_id": "t", "model": "m", "agent_mode": "container", "attempt": "1", "setup_exit": "0",
         "interrupted": "0", "server_ok_after": "1", "agent_timeout": "480"}
    s.update(suite or {})
    with open(os.path.join(d, "logs", "suite.env"), "w") as f:
        f.write("".join(f"{k}={v}\n" for k, v in s.items()))
    e = {"container_exit": str(container_exit), "oom_killed": "false"}
    e.update(runenv or {})
    with open(os.path.join(d, "logs", "run.env"), "w") as f:
        f.write("".join(f"{k}={v}\n" for k, v in e.items()))
    if verify is not None:
        open(os.path.join(d, "logs", "verify_exit"), "w").write(f"{verify}\n")
    m = {"exit_code": container_exit, "wall_s": 480.0, "agent_timeout_s": 480}
    m.update(meta or {})
    json.dump(m, open(os.path.join(d, "trajectory", "meta.json"), "w"))
    for k, v in (traj or {}).items():
        open(os.path.join(d, "trajectory", k), "w").write(v)
    for k, v in (logs or {}).items():
        open(os.path.join(d, "logs", k), "w").write(v)
    if llm is not None:
        with open(os.path.join(d, "trajectory", "llm.jsonl"), "w") as f:
            for r in llm:
                f.write(json.dumps(r) + "\n")
    return d


def req(status=200, ua="opencode/latest/2.2.1/cli", error=False):
    raw = 'data: {"error": {"message": "boom"}}\n\n' if error else 'data: {"choices": []}\n\ndata: [DONE]\n\n'
    return {"t_start": 0.0, "t_end": 2.0, "method": "POST", "path": "/v1/chat/completions", "status": status,
            "user_agent": ua, "request": {"messages": [{"role": "system", "content": "s"}], "tools": []},
            "timings": {"predicted_n": 10}, "response_raw": raw}


def expect(name, d, status, **fields):
    global FAILS
    r = sr.result(d)
    got = {"status": r["status"]}
    bad = r["status"] != status
    for k, v in fields.items():
        cur = r
        for part in k.split("."):
            cur = (cur or {}).get(part) if isinstance(cur, dict) else None
        got[k] = cur
        bad = bad or cur != v
    if bad:
        FAILS += 1
        print(f"  FAIL  scoring: {name}: expected {status} {fields}, got {got}")
    else:
        print(f"  ok    scoring: {name} -> {status}")


def main(root):
    os.makedirs(root, exist_ok=True)
    expect("plain failure", run_dir(root, "fail"), "failed")
    expect("forged trajectory/verify_exit=0 is ignored",
           run_dir(root, "forge-verify", traj={"verify_exit": "0\n"}), "failed")
    expect("forged trajectory/run.env interrupted=1 is ignored",
           run_dir(root, "forge-runenv", traj={"run.env": "interrupted=1\ncontainer_exit=0\n"}), "failed")
    expect("forged trajectory/infra_error is ignored",
           run_dir(root, "forge-infra", traj={"infra_error": "x\n"}), "failed")
    expect("solved although an OOM happened in the container",
           run_dir(root, "oom-solved", container_exit=137, verify=0, runenv={"oom_killed": "true"},
                   meta={"exit_code": 137, "wall_s": 200.0}), "solved")
    expect("killed early (OOM) is the agent's, not a timeout",
           run_dir(root, "oom-early", container_exit=137, runenv={"oom_killed": "true"},
                   meta={"exit_code": 137, "wall_s": 200.0}), "agent_error", **{"agent.timed_out": False})
    expect("124 at the budget is a timeout",
           run_dir(root, "timeout", container_exit=124, meta={"exit_code": 124, "wall_s": 480.2}),
           "timeout", **{"agent.timed_out": True})
    expect("disk budget", run_dir(root, "disk-budget", container_exit=143,
                                  runenv={"disk_limit": "budget: the run used 41 GB on /w"}), "agent_error")
    expect("disk floor", run_dir(root, "disk-floor", container_exit=143,
                                 runenv={"disk_limit": "floor: 20 GB left on /w"}), "infra_error", final=False)
    expect("run ended on a llama-server 500", run_dir(root, "stack", container_exit=1,
                                                      llm=[req(), req(status=500)]), "stack_error")
    expect("run ended on an SSE error object", run_dir(root, "stack-sse", container_exit=1,
                                                       llm=[req(), req(error=True)]), "stack_error")
    expect("the agent's own failing curl is not a stack error",
           run_dir(root, "foreign", container_exit=0, llm=[req(), req(status=500, ua="curl/8.17.0")]),
           "failed", **{"summary.llm.foreign_requests": 1, "summary.llm.requests": 1})
    expect("3rd infra_error attempt, server down after it: server_crash",
           run_dir(root, "crash", container_exit=0, suite={"attempt": "3", "attempts_toward_cap": "3",
                                                           "server_ok_after": "0"}),
           "server_crash", final=True, cause="infra_error")
    expect("3rd verify_error attempt: abandoned",
           run_dir(root, "abandon", verify=2, suite={"attempt": "3", "attempts_toward_cap": "3"}),
           "abandoned", final=True, cause="verify_error")
    expect("attempt 3 after two interrupted ones: still re-run",
           run_dir(root, "not-yet", verify=2, suite={"attempt": "3", "attempts_toward_cap": "1"}),
           "verify_error", final=False)
    # table: abandoned is kept out of the solved/final denominator
    stamp = os.path.join(root, "stamp")
    shutil.rmtree(stamp, ignore_errors=True)
    for name, d in (("a", run_dir(root, "t-solved", verify=0)), ("b", run_dir(root, "t-abandon", verify=2,
                                                                              suite={"attempt": "3"}))):
        os.makedirs(os.path.join(stamp, "m", name))
        json.dump(sr.result(d), open(os.path.join(stamp, "m", name, "result.json"), "w"))
    import contextlib
    import io
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        sr.table(stamp)
    out = buf.getvalue()
    line = next((ln for ln in out.splitlines() if ln.startswith("solved/final")), "")
    global FAILS
    if "1/1 (2)" in line and "abandoned (no score)" in out:
        print("  ok    scoring: table keeps abandoned out of solved/final")
    else:
        FAILS += 1
        print(f"  FAIL  scoring: table: {line!r}")
    return 1 if FAILS else 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1]))
