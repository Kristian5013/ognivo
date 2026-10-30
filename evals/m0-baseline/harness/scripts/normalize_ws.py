#!/usr/bin/env python3
"""Make a fresh task workspace tell the agent nothing about where the task's diff was applied.

  normalize_ws.py WORKSPACE

The workspace builders unpack a tree and then apply the mutation/setup diff, so the files that
diff touched are the only ones with a fresh mtime and the highest inode numbers (`ls -lt`,
`find -newer` point at the bug), and a tree written before the diff can stay in .git as an
unreachable object (`git fsck` shows it). Run by run-suite.sh on the host right after setup.sh,
before any agent sees the workspace:
  1. every tracked file is removed and checked out again from the index, in index order;
  2. tracked files, their directories and the workspace root get one fixed mtime;
  3. `git prune --expire=now` drops objects that no ref reaches.
Untracked and ignored files (a warm target/) are left alone. A workspace with uncommitted
changes to tracked files is an error: the builders always end on a clean commit.
"""
import os
import subprocess
import sys

STAMP = 1790812800   # 2026-10-01 00:00:00 UTC: in the past, so make/cargo see no clock skew


def git(ws, *args, **kw):
    return subprocess.run(["git", "-C", ws, *args], check=True, stdout=subprocess.PIPE, **kw).stdout


def main(ws):
    if git(ws, "status", "--porcelain=v1", "--untracked-files=no").strip():
        sys.exit(f"normalize_ws: {ws} has uncommitted changes to tracked files")
    paths = []
    for rec in git(ws, "ls-files", "-z", "-s").split(b"\0"):
        if not rec:
            continue
        meta, path = rec.split(b"\t", 1)
        if meta.split()[0] == b"160000":   # a submodule: not a file of this tree
            continue
        paths.append(os.fsdecode(path))
    for p in paths:
        try:
            os.unlink(os.path.join(ws, p))
        except FileNotFoundError:
            pass
    git(ws, "checkout-index", "-a", "-f")
    dirs = {""}
    for p in paths:
        os.utime(os.path.join(ws, p), (STAMP, STAMP), follow_symlinks=False)
        d = os.path.dirname(p)
        while d and d not in dirs:
            dirs.add(d)
            d = os.path.dirname(d)
    for d in sorted(dirs, key=len, reverse=True):
        os.utime(os.path.join(ws, d), (STAMP, STAMP))
    git(ws, "update-index", "-q", "--refresh")
    git(ws, "-c", "gc.auto=0", "prune", "--expire=now")
    if git(ws, "status", "--porcelain=v1", "--untracked-files=no").strip():
        sys.exit(f"normalize_ws: {ws} is not clean after the rewrite")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
