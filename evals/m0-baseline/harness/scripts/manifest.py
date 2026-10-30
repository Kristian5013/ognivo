#!/usr/bin/env python3
"""Content hashes of everything that judges a run, so a campaign's nights stay comparable.

  manifest.py suite IMAGE_ID OUT_DIR [MODEL]
                                       hash the harness tree, the ognivo profiles/ (profiles and
                                       chat templates), the pinned llama-server build directory,
                                       the image ID and the runtime (MODEL's GGUF size and mtime,
                                       the NVIDIA driver, podman/passt/crun/conmon versions, the
                                       kernel); write OUT_DIR/<combined>.json with all of it (a
                                       second file there = the suite drifted between nights) and
                                       print the combined sha256
  manifest.py task TASK_DIR            print the combined sha256 of TASK_DIR and of the
                                       ../_common/<group>/ it uses (group from task.json)

Paths come from config/models.json (ognivo_repo, llama_server), OGNIVO_REPO and
LLAMA_SERVER_BIN override them as in llama_cmd.py.
"""
import hashlib
import json
import os
import subprocess
import sys

HARNESS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def sha_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def tree(root, label, out):
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x != "__pycache__")
        for name in sorted(files):
            p = os.path.join(d, name)
            if os.path.isfile(p) and not os.path.islink(p):
                out[f"{label}/{os.path.relpath(p, root)}"] = sha_file(p)


def combined(files, extra=""):
    h = hashlib.sha256(extra.encode())
    for k in sorted(files):
        h.update(f"{k} {files[k]}\n".encode())
    return h.hexdigest()


def runtime(cfg, model):
    """What can change under a campaign without touching a file we hash: the weights at the same
    path (they were replaced on 2026-10-28), and packages unattended-upgrades may update."""
    rt = {}
    gguf = ((cfg.get("models") or {}).get(model) or {}).get("gguf")
    if gguf:
        try:
            st = os.stat(gguf)
            rt["gguf"] = {"path": gguf, "size": st.st_size, "mtime": int(st.st_mtime)}
        except OSError as e:
            rt["gguf"] = {"path": gguf, "error": str(e)}
    try:
        out = subprocess.run(["dpkg-query", "-W", "-f", "${Package} ${Version}\n", "nvidia-driver-*",
                              "libnvidia-compute-*", "podman", "passt", "crun", "conmon"],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=30).stdout
        rt["packages"] = sorted({ln for ln in out.splitlines() if ln.strip() and not ln.endswith(" ")})
    except (OSError, subprocess.SubprocessError):
        rt["packages"] = None
    rt["kernel"] = os.uname().release
    try:
        rt["nvidia"] = open("/proc/driver/nvidia/version").readline().strip()
    except OSError:
        rt["nvidia"] = None
    return rt


def cmd_suite(image_id, out_dir, model=""):
    cfg = json.load(open(os.environ.get("M0_MODELS_JSON") or os.path.join(HARNESS, "config", "models.json")))
    repo = os.environ.get("OGNIVO_REPO") or cfg["ognivo_repo"]
    server = os.path.expanduser(os.environ.get("LLAMA_SERVER_BIN") or cfg["llama_server"])
    files = {}
    tree(HARNESS, "harness", files)
    tree(os.path.join(repo, "profiles"), "profiles", files)
    tree(os.path.dirname(os.path.realpath(server)), "llama-bin", files)
    rt = runtime(cfg, model)
    total = combined(files, f"image {image_id}\nruntime {json.dumps(rt, sort_keys=True)}\n")
    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, f"{total}.json")
    if not os.path.exists(path):
        with open(path + ".tmp", "w") as f:
            json.dump({"combined_sha256": total, "image_id": image_id, "model": model, "runtime": rt,
                       "ognivo_repo": repo, "llama_server": server, "files": files}, f, indent=1, sort_keys=True)
        os.replace(path + ".tmp", path)
    print(total)


def cmd_task(task_dir):
    task_dir = os.path.abspath(task_dir)
    files = {}
    tree(task_dir, "task", files)
    group = (json.load(open(os.path.join(task_dir, "task.json"))).get("group") or "").strip()
    common = os.path.join(os.path.dirname(task_dir), "_common", group)
    if group and os.path.isdir(common):
        tree(common, f"_common/{group}", files)
    print(combined(files))


if __name__ == "__main__":
    a = sys.argv[1:]
    if len(a) in (3, 4) and a[0] == "suite":
        cmd_suite(*a[1:])
    elif len(a) == 2 and a[0] == "task":
        cmd_task(a[1])
    else:
        sys.exit(__doc__)
