#!/usr/bin/env python3
"""The agent's source change set without build output.

  filter_patch.py DIFF_PATCH OUT_PATCH

trajectory/diff.patch is `git add -A` against the base commit, so in workspaces without a
.gitignore (flowcoin, misc) every cmake tree, object file and __pycache__ the agent produced
is in it, up to tens of MB. OUT_PATCH (trajectory/diff.src.patch) keeps every file section
except NEW files that are build output: a path component build, out, target, CMakeFiles,
__pycache__, .pytest_cache, .cache, a name ending in .o/.a/.so/.pyc/.d, CMakeCache.txt, or any
new binary file. Changes to tracked files always stay, so a real edit is never dropped.
Prints what was dropped as one summary line on stderr.
"""
import re
import sys

BUILD_DIRS = {"build", "out", "target", "CMakeFiles", "__pycache__", ".pytest_cache", ".cache"}
BUILD_EXT = (".o", ".a", ".so", ".pyc", ".d")
HEADER = re.compile(rb"^diff --git a/(.*) b/(.*)$")


def is_build_output(path, section):
    new = b"\nnew file mode " in section[:400]
    if not new:
        return False
    parts = path.split("/")
    if any(p in BUILD_DIRS for p in parts[:-1]) or parts[-1].endswith(BUILD_EXT) or parts[-1] == "CMakeCache.txt":
        return True
    return b"\nGIT binary patch\n" in section or b"\nBinary files " in section


def main(src, dst):
    data = open(src, "rb").read()
    starts = [m.start() for m in re.finditer(rb"^diff --git ", data, re.M)]
    out, dropped, dropped_bytes = [data[:starts[0]] if starts else data], 0, 0
    for i, s in enumerate(starts):
        section = data[s:starts[i + 1] if i + 1 < len(starts) else len(data)]
        m = HEADER.match(section.split(b"\n", 1)[0])
        path = m.group(2).decode("utf-8", "replace") if m else ""
        if m and is_build_output(path, section):
            dropped += 1
            dropped_bytes += len(section)
            continue
        out.append(section)
    with open(dst, "wb") as f:
        f.write(b"".join(out))
    sys.stderr.write(f"filter_patch: dropped {dropped} new build-output file(s), {dropped_bytes} bytes\n")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
