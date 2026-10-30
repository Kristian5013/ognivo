#!/usr/bin/env python3
"""Task selection and run bookkeeping for run-suite.sh and run-night.sh (M0 baseline).

  plan.py tasks TASKS_ROOT [--role ROLE|any] [ID...]
        Valid task ids, one per line. Without ids: every TASKS_ROOT/*/task.json whose "role" is
        ROLE (default "selected"), sorted. A task directory needs setup.sh, verify.sh and a
        prompt (prompt.txt, task.json "prompt_file" or "prompt"). Problems go to stderr; exit 1
        when an explicitly named task is not valid.
  plan.py task-env TASK_DIR PROMPT_OUT
        Writes the task's prompt to PROMPT_OUT and prints shell assignments: TASK_TITLE,
        TASK_ROLE, TASK_AGENT_TIMEOUT, TASK_VERIFY_SECONDS, TASK_SETUP_SECONDS, TASK_PROMPT_SHA256,
        TASK_PROMPT_SHA256_EXPECTED (empty when unknown).
  plan.py pending MODEL_DIR ID...
        The ids without a final result (MODEL_DIR/ID/result.json with "final": true).
  plan.py campaign-new CAMPAIGN_DIR --plan SPEC --window W --tasks ID...
        SPEC: phases separated by ';', models by ',', e.g. "gpt-oss-20b,gemma-4-26b-a4b;qwen3.8-27b".
  plan.py campaign-tonight CAMPAIGN_DIR --date YYYY-MM-DD [--greedy]
        Lines "MODEL ID ID ..." with what is left for tonight, in plan order. Night k (k-th
        distinct date, counting tonight) may run phases 1..k; --greedy allows all of them.
  plan.py campaign-record CAMPAIGN_DIR --date D --result R [--started T] [--model M --exit N]...
  plan.py campaign-status CAMPAIGN_DIR      human-readable progress; exit 0 when complete, 1 otherwise
  plan.py campaign-latest RUNS_ROOT         stamp of the newest unfinished campaign (exit 1: none)
"""
import datetime as dt
import glob
import hashlib
import json
import os
import shlex
import sys

FINAL = ("solved", "failed", "timeout", "agent_error", "stack_error", "server_crash", "abandoned")   # = summarize_run.py


def load_json(path, default=None):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def write_json(path, obj):
    tmp = f"{path}.tmp{os.getpid()}"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=1, ensure_ascii=False)
        f.write("\n")
    os.replace(tmp, path)


def utc():
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def prompt_source(task_dir, meta):
    p = os.path.join(task_dir, "prompt.txt")
    if os.path.isfile(p):
        return ("file", p)
    pf = meta.get("prompt_file") if isinstance(meta, dict) else None
    if isinstance(pf, str) and os.path.isfile(pf):
        return ("file", pf)
    if isinstance(meta, dict) and isinstance(meta.get("prompt"), str) and meta["prompt"].strip():
        return ("inline", meta["prompt"])
    return (None, None)


def check_task(task_dir):
    """-> (meta, problems)"""
    problems = []
    meta = {}
    tj = os.path.join(task_dir, "task.json")
    if os.path.exists(tj):
        meta = load_json(tj)
        if not isinstance(meta, dict):
            problems.append("task.json is not a JSON object")
            meta = {}
    for f in ("setup.sh", "verify.sh"):
        if not os.path.isfile(os.path.join(task_dir, f)):
            problems.append(f"no {f}")
    if prompt_source(task_dir, meta)[0] is None:
        problems.append("no prompt (prompt.txt, task.json prompt_file or prompt)")
    tid = meta.get("id")
    if tid and tid != os.path.basename(os.path.normpath(task_dir)):
        problems.append(f"task.json id {tid!r} differs from the directory name")
    return meta, problems


def cmd_tasks(args):
    root, role, ids = args[0], "selected", []
    i = 1
    while i < len(args):
        if args[i] == "--role" and i + 1 < len(args):
            role = args[i + 1]
            i += 2
            continue
        ids.append(args[i])
        i += 1
    rc = 0
    if ids:
        for tid in ids:
            d = os.path.join(root, tid)
            if not os.path.isdir(d):
                sys.stderr.write(f"plan: no task directory {d}\n")
                rc = 1
                continue
            _, problems = check_task(d)
            if problems:
                sys.stderr.write(f"plan: {tid}: {'; '.join(problems)}\n")
                rc = 1
                continue
            print(tid)
        return rc
    for d in sorted(glob.glob(os.path.join(root, "*", ""))):
        tid = os.path.basename(os.path.normpath(d))
        if tid.startswith(("_", ".")):
            continue
        meta, problems = check_task(d)
        if not os.path.exists(os.path.join(d, "task.json")):
            sys.stderr.write(f"plan: {tid}: no task.json, skipped\n")
            continue
        if role != "any" and meta.get("role") != role:
            continue
        if problems:
            sys.stderr.write(f"plan: {tid}: {'; '.join(problems)}, skipped\n")
            continue
        print(tid)
    return 0


def cmd_task_env(args):
    task_dir, out = args[0], args[1]
    meta, problems = check_task(task_dir)
    if problems:
        sys.stderr.write(f"plan: {task_dir}: {'; '.join(problems)}\n")
        return 1
    kind, src = prompt_source(task_dir, meta)
    if kind == "file":
        with open(src, "rb") as f:
            data = f.read()
    else:
        data = src.encode("utf-8")
    with open(out, "wb") as f:
        f.write(data)
    def num(key):
        v = meta.get(key)
        return str(int(v)) if isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0 else ""
    vals = {"TASK_TITLE": str(meta.get("title") or ""), "TASK_ROLE": str(meta.get("role") or ""),
            "TASK_AGENT_TIMEOUT": num("agent_timeout_s"), "TASK_VERIFY_SECONDS": num("verify_seconds"),
            "TASK_SETUP_SECONDS": num("setup_seconds"),
            "TASK_PROMPT_SHA256": hashlib.sha256(data).hexdigest(),
            "TASK_PROMPT_SHA256_EXPECTED": str(meta.get("prompt_sha256") or "")}
    for k, v in vals.items():
        print(f"{k}={shlex.quote(v)}")
    return 0


def is_final(model_dir, tid):
    r = load_json(os.path.join(model_dir, tid, "result.json"))
    return isinstance(r, dict) and r.get("final") is True


def cmd_pending(args):
    model_dir, ids = args[0], args[1:]
    for tid in ids:
        if not is_final(model_dir, tid):
            print(tid)
    return 0


def parse_plan(spec):
    phases = [[m.strip() for m in p.split(",") if m.strip()] for p in spec.split(";")]
    phases = [p for p in phases if p]
    if not phases:
        raise ValueError(f"empty plan {spec!r}")
    return phases


def opts(args, multi=("--tasks",), repeat=("--model", "--exit")):
    o, pos, i, cur = {}, [], 0, None
    while i < len(args):
        a = args[i]
        if a.startswith("--"):
            if a in ("--greedy",):
                o[a] = True
                cur = None
            elif a in multi:
                o[a] = []
                cur = a
            elif a in repeat:
                o.setdefault(a, []).append(args[i + 1])
                i += 1
                cur = None
            else:
                o[a] = args[i + 1]
                i += 1
                cur = None
        elif cur:
            o[cur].append(a)
        else:
            pos.append(a)
        i += 1
    return o, pos


def campaign_path(cdir):
    return os.path.join(cdir, "campaign.json")


def cmd_campaign_new(args):
    o, pos = opts(args)
    cdir = pos[0]
    os.makedirs(cdir, exist_ok=True)
    if os.path.exists(campaign_path(cdir)):
        sys.stderr.write(f"plan: {campaign_path(cdir)} exists\n")
        return 1
    tasks = o.get("--tasks") or []
    if not tasks:
        sys.stderr.write("plan: campaign-new needs --tasks\n")
        return 1
    c = {"schema": "ognivo.m0.campaign/1", "stamp": os.path.basename(os.path.normpath(cdir)),
         "created_utc": utc(), "plan": parse_plan(o["--plan"]), "window": o.get("--window"),
         "tasks": tasks, "nights": [], "complete": False}
    write_json(campaign_path(cdir), c)
    return 0


def remaining(cdir, c):
    out = []
    for pi, phase in enumerate(c["plan"]):
        for m in phase:
            left = [t for t in c["tasks"] if not is_final(os.path.join(cdir, m), t)]
            out.append((pi, m, left))
    return out


def night_number(c, date):
    dates = []
    for n in c.get("nights") or []:
        if n.get("date") not in dates:
            dates.append(n.get("date"))
    if date not in dates:
        dates.append(date)
    return dates.index(date) + 1


def cmd_campaign_tonight(args):
    o, pos = opts(args)
    cdir = pos[0]
    c = load_json(campaign_path(cdir))
    if not c:
        sys.stderr.write(f"plan: no campaign in {cdir}\n")
        return 1
    k = night_number(c, o["--date"])
    allowed = len(c["plan"]) if o.get("--greedy") else k
    for pi, m, left in remaining(cdir, c):
        if pi < allowed and left:
            print(" ".join([m] + left))
    return 0


def cmd_campaign_record(args):
    o, pos = opts(args)
    cdir = pos[0]
    c = load_json(campaign_path(cdir))
    if not c:
        sys.stderr.write(f"plan: no campaign in {cdir}\n")
        return 1
    runs = [{"model": m, "exit": int(e)} for m, e in zip(o.get("--model") or [], o.get("--exit") or [])]
    c.setdefault("nights", []).append({"date": o["--date"], "night": night_number(c, o["--date"]),
                                       "started_utc": o.get("--started"), "ended_utc": utc(),
                                       "result": o.get("--result"), "suites": runs})
    c["complete"] = all(not left for _, _, left in remaining(cdir, c))
    write_json(campaign_path(cdir), c)
    return 0


def cmd_campaign_status(args):
    cdir = args[0]
    c = load_json(campaign_path(cdir))
    if not c:
        sys.stderr.write(f"plan: no campaign in {cdir}\n")
        return 2
    print(f"campaign {c['stamp']} created {c['created_utc']}, window {c.get('window')}, "
          f"{len(c['tasks'])} tasks, nights so far: {len({n.get('date') for n in c.get('nights') or []})}")
    done_all = True
    for pi, m, left in remaining(cdir, c):
        n = len(c["tasks"])
        print(f"  night {pi + 1}  {m:18} {n - len(left):3}/{n} final" + (f"; left: {' '.join(left)}" if left else ""))
        done_all = done_all and not left
    for n in c.get("nights") or []:
        suites = ", ".join(f"{s['model']}={s['exit']}" for s in n.get("suites") or [])
        print(f"  {n.get('date')} (night {n.get('night')}): {n.get('result')} [{suites}] "
              f"{n.get('started_utc')} -> {n.get('ended_utc')}")
    return 0 if done_all else 1


def cmd_campaign_latest(args):
    root = args[0]
    best = None
    for p in sorted(glob.glob(os.path.join(root, "*", "campaign.json"))):
        c = load_json(p)
        if isinstance(c, dict) and not c.get("complete"):
            best = os.path.basename(os.path.dirname(p))
    if best:
        print(best)
        return 0
    return 1


COMMANDS = {"tasks": cmd_tasks, "task-env": cmd_task_env, "pending": cmd_pending,
            "campaign-new": cmd_campaign_new, "campaign-tonight": cmd_campaign_tonight,
            "campaign-record": cmd_campaign_record, "campaign-status": cmd_campaign_status,
            "campaign-latest": cmd_campaign_latest}

if __name__ == "__main__":
    if len(sys.argv) < 3 or sys.argv[1] not in COMMANDS:
        sys.stderr.write(__doc__)
        sys.exit(2)
    try:
        sys.exit(COMMANDS[sys.argv[1]](sys.argv[2:]))
    except (KeyError, IndexError, ValueError) as e:
        sys.stderr.write(f"plan: bad arguments: {e}\n")
        sys.exit(2)
