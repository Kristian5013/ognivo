#!/usr/bin/env python3
"""The pinned llama-server command line for one M0 model, built from its ognivo profile.

  llama_cmd.py argv  MODEL [--port N] [--no-file-check]  NUL-separated argv (bash: mapfile -d '')
  llama_cmd.py show  MODEL [--port N] [--no-file-check]  the same as one shell-quoted line
  llama_cmd.py info  MODEL                                shell assignments: gguf, profile, timeout
  llama_cmd.py check MODEL PROPS_JSON [MODELS_JSON]       compare a running server (its /props and
                                                          /v1/models) with the profile; prints a
                                                          JSON report, exit 1 on a mismatch

argv = <llama-server> -m <gguf> --alias MODEL
       + [server].recommended_flags of <ognivo_repo>/<profile>, with --chat-template-file made
         absolute under the ognivo repo (that file's sha256 must equal [template].file_sha256)
       + sampling flags from [sampling.main] (Claude Code sends no sampling, OpenCode sends the same)
       + reasoning flags from [reasoning]
       + --jinja --metrics --no-webui --log-timestamps --host 127.0.0.1 --port N (last, so a
         profile cannot override them)

`check` hashes the served chat template the way ognivo does (crates/ognivo-models/src/
template.rs: served_form) and looks it up in [template].known: anything but "canonical" is a
mismatch (TEMPLATE_CHECK=warn turns that into a warning). The alias must be served, the model
path must be the profile's gguf; a build other than models.json llama_build is only a warning.

Environment: M0_MODELS_JSON (default ../config/models.json), OGNIVO_REPO, LLAMA_SERVER_BIN.
"""
import hashlib
import json
import os
import shlex
import sys
import tomllib

HERE = os.path.dirname(os.path.abspath(__file__))

GPT_OSS_RAISE = ('{%- if "<|channel|>analysis<|message|>" in message.content or '
                 '"<|channel|>final<|message|>" in message.content %}')
MISTRAL_RAISE = ("{%- if (message['content'] is none or message['content'] == '' or "
                 "message['content']|length == 0) and (message['tool_calls'] is not defined or "
                 "message['tool_calls'] is none or message['tool_calls']|length == 0) %}")


def served_form(t):
    """Port of ognivo-models template::served_form (llama.cpp chat.cpp patches + lexer)."""
    if "<|channel|>" in t and "in message.content or" in t:
        t = t.replace(GPT_OSS_RAISE, "{%- if false %}")
    if "[TOOL_CALLS]" in t and "if (message['content'] is none or" in t:
        t = t.replace(MISTRAL_RAISE, "{%- if false %}")
    strip_newline = t.endswith("\n")
    t = t.replace("\r\n", "\n").replace("\r", "\n")
    if strip_newline:
        t = t[:-1]
    return t


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


class Problem(Exception):
    pass


def load_config():
    path = os.environ.get("M0_MODELS_JSON") or os.path.join(HERE, "..", "config", "models.json")
    with open(path, encoding="utf-8") as f:
        cfg = json.load(f)
    cfg["_path"] = os.path.abspath(path)
    return cfg


def model_entry(cfg, model):
    name = model.split("/", 1)[1] if model.startswith("llama/") else model
    models = cfg.get("models") or {}
    if name not in models:
        raise Problem(f"unknown model {model!r}; known: {', '.join(sorted(models))}")
    return name, models[name]


def repo_dir(cfg):
    return os.environ.get("OGNIVO_REPO") or cfg["ognivo_repo"]


def load_profile(cfg, entry):
    path = os.path.join(repo_dir(cfg), entry["profile"])
    with open(path, "rb") as f:
        return path, tomllib.load(f)


def num(v):
    return repr(v) if isinstance(v, float) else str(v)


def sampling_flags(prof):
    s = (prof.get("sampling") or {}).get("main") or {}
    names = [("temperature", "--temp"), ("top_p", "--top-p"), ("top_k", "--top-k"), ("min_p", "--min-p")]
    out = []
    for key, flag in names:
        if key in s:
            out += [flag, num(s[key])]
    return out


def reasoning_flags(prof):
    r = prof.get("reasoning") or {}
    control, default, levels = r.get("control"), r.get("default"), r.get("levels") or []
    if control == "toggle" and default:
        return ["--reasoning", default]                       # Gemma 4: on | off for the session
    if control == "effort" and default:
        out = []
        if "off" in levels:                                    # the template can switch thinking off
            out += ["--reasoning", "off" if default == "off" else "on"]
        if default != "off":
            out += ["--reasoning-effort", default]
        return out
    return []


def build_argv(cfg, model, port, file_check=True):
    name, entry = model_entry(cfg, model)
    ppath, prof = load_profile(cfg, entry)
    server = os.path.expanduser(os.environ.get("LLAMA_SERVER_BIN") or cfg["llama_server"])
    flags = shlex.split(((prof.get("server") or {}).get("recommended_flags")) or "")
    tmpl = None
    for i, a in enumerate(flags):
        if a == "--chat-template-file" and i + 1 < len(flags):
            if not os.path.isabs(flags[i + 1]):
                flags[i + 1] = os.path.join(repo_dir(cfg), flags[i + 1])
            tmpl = flags[i + 1]
    if tmpl is None:
        raise Problem(f"{ppath}: recommended_flags has no --chat-template-file")
    want = (prof.get("template") or {}).get("file_sha256")
    if not os.path.isfile(tmpl):
        raise Problem(f"chat template not found: {tmpl}")
    got = sha256_file(tmpl)
    if want and got != want:
        raise Problem(f"{tmpl}: sha256 {got} != [template].file_sha256 {want} in {ppath}")
    if file_check:
        if not (os.path.isfile(server) and os.access(server, os.X_OK)):
            raise Problem(f"llama-server not found or not executable: {server}")
        if not os.path.isfile(entry["gguf"]):
            raise Problem(f"model file not found (is the model disk mounted?): {entry['gguf']}")
    host = cfg.get("host", "127.0.0.1")
    if host != "127.0.0.1":
        raise Problem("models.json host must be 127.0.0.1 (the containers reach the host loopback only)")
    return ([server, "-m", entry["gguf"], "--alias", name] + flags + sampling_flags(prof)
            + reasoning_flags(prof)
            + ["--jinja", "--metrics", "--no-webui", "--log-timestamps",
               "--host", host, "--port", str(port)])


def cmd_info(cfg, model):
    name, entry = model_entry(cfg, model)
    ppath, prof = load_profile(cfg, entry)
    ctx = (prof.get("context") or {}).get("working_max", "")
    vals = {"M0_MODEL_ALIAS": name, "M0_MODEL_GGUF": entry["gguf"], "M0_MODEL_PROFILE": ppath,
            "M0_MODEL_TIMEOUT": str(entry.get("agent_timeout_s", 600)), "M0_MODEL_CTX": str(ctx),
            "M0_LLAMA_SERVER": os.path.expanduser(os.environ.get("LLAMA_SERVER_BIN") or cfg["llama_server"]),
            "M0_LLAMA_BUILD": cfg.get("llama_build", ""), "M0_LLAMA_PORT": str(cfg.get("port", 8080))}
    for k, v in vals.items():
        print(f"{k}={shlex.quote(v)}")


def cmd_check(cfg, model, props_path, models_path):
    name, entry = model_entry(cfg, model)
    _, prof = load_profile(cfg, entry)
    with open(props_path, encoding="utf-8") as f:
        props = json.load(f)
    served_ids = []
    if models_path:
        try:
            with open(models_path, encoding="utf-8") as f:
                served_ids = [m.get("id") for m in (json.load(f).get("data") or []) if isinstance(m, dict)]
        except (OSError, ValueError):
            served_ids = []
    known = {k.get("sha256"): k for k in (prof.get("template") or {}).get("known") or []}
    tpl = props.get("chat_template")
    sha = hashlib.sha256(served_form(tpl).encode("utf-8")).hexdigest() if isinstance(tpl, str) else None
    verdict = (known.get(sha) or {}).get("verdict", "unknown") if sha else "missing"
    gen = props.get("default_generation_settings") or {}
    report = {"model": name, "build_info": props.get("build_info"), "model_path": props.get("model_path"),
              "model_alias": props.get("model_alias"), "served_ids": served_ids,
              "total_slots": props.get("total_slots"), "n_ctx": gen.get("n_ctx"),
              "chat_template_sha256": sha, "template_verdict": verdict,
              "template_note": (known.get(sha) or {}).get("note"),
              "chat_template_caps": props.get("chat_template_caps"),
              "problems": [], "warnings": []}
    strict = os.environ.get("TEMPLATE_CHECK", "strict") != "warn"
    if verdict != "canonical":
        msg = f"served chat template is {verdict!r} (sha256 {sha}), expected the canonical one"
        (report["problems"] if strict else report["warnings"]).append(msg)
    aliases = set(served_ids) | ({props["model_alias"]} if isinstance(props.get("model_alias"), str) else set())
    if name not in aliases:
        report["problems"].append(f"server does not serve alias {name!r} (it serves {sorted(aliases)})")
    mp = props.get("model_path")
    if mp and os.path.realpath(mp) != os.path.realpath(entry["gguf"]):
        report["problems"].append(f"server loaded {mp}, expected {entry['gguf']}")
    build = cfg.get("llama_build")
    if build and props.get("build_info") != build:
        report["warnings"].append(f"build_info {props.get('build_info')!r} is not the pinned {build!r}")
    report["ok"] = not report["problems"]
    json.dump(report, sys.stdout, indent=1, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0 if report["ok"] else 1


def main(argv):
    if len(argv) < 2 or argv[0] not in ("argv", "show", "info", "check"):
        sys.stderr.write(__doc__)
        return 2
    cmd, model, rest = argv[0], argv[1], argv[2:]
    port, file_check, pos = None, True, []
    i = 0
    while i < len(rest):
        if rest[i] == "--port" and i + 1 < len(rest):
            port = int(rest[i + 1])
            i += 2
            continue
        if rest[i] == "--no-file-check":
            file_check = False
        else:
            pos.append(rest[i])
        i += 1
    try:
        cfg = load_config()
        if port is None:
            port = int(cfg.get("port", 8080))
        if cmd == "info":
            cmd_info(cfg, model)
            return 0
        if cmd == "check":
            if not pos:
                raise Problem("check needs PROPS_JSON")
            return cmd_check(cfg, model, pos[0], pos[1] if len(pos) > 1 else None)
        args = build_argv(cfg, model, port, file_check)
    except (Problem, OSError, KeyError, ValueError, tomllib.TOMLDecodeError) as e:
        sys.stderr.write(f"llama_cmd: {e}\n")
        return 1
    if cmd == "argv":
        sys.stdout.write("".join(a + "\0" for a in args))
    else:
        print(" ".join(shlex.quote(a) for a in args))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
