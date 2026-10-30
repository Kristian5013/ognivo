#!/usr/bin/env python3
"""A few shellcheck-style checks for the harness scripts, for hosts without shellcheck.

  python3 -I tests/lint_bash.py FILE...      prints FILE:LINE: message; exit 1 when it finds any

Heuristics, line based, aimed at the hazards that matter under `set -euo pipefail`:
  E1  a one-line loop whose body ends in `[[ ... ]] && cmd` / `(( ... )) && cmd`: when the test
      is false on the last iteration the loop returns 1 and errexit ends the script
  E2  `producer | grep -q` / `| head`: the early exit SIGPIPEs the producer, which pipefail
      reports as a failure
  E3  `local var=$(...)`: local masks the exit status of the command (SC2155)
  E4  unquoted $@ or $* outside double quotes (SC2068)
  E5  backticks for command substitution (SC2006)
A trailing `# lint: ok` comment silences a line.
"""
import re
import sys

E1 = re.compile(r";\s*do\s+(?:.*;\s*)?(\[\[[^]]*\]\]|\(\([^)]*\)\))\s*&&\s*[^|;]+;\s*done\b")
E2 = re.compile(r"\|\s*(grep\s+(-[a-zA-Z]*q|--quiet)|head\b)")
E3 = re.compile(r"^\s*local\s+[A-Za-z_][A-Za-z0-9_]*=\$\(")
E5 = re.compile(r"`[^`]*`")


def strip_strings(line):
    """Drop single-quoted text and comments (roughly), keep double-quoted text marked."""
    out, q, i = [], None, 0
    while i < len(line):
        c = line[i]
        if q is None and c == "#" and (i == 0 or line[i - 1] in " \t;"):
            break
        if c == "\\" and q != "'" and i + 1 < len(line):
            out.append("__")
            i += 2
            continue
        if c in "'\"" and (q is None or q == c):
            q = None if q == c else c
            out.append(c)
        elif q == "'":
            out.append("_")
        else:
            out.append(c)
        i += 1
    return "".join(out)


def unquoted_dollar_at(line):
    q = None
    for i, c in enumerate(line):
        if c == '"':
            q = None if q else '"'
        elif c == "$" and q is None and line[i + 1:i + 2] in ("@", "*"):
            return True
    return False


def lint(path):
    problems = []
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except OSError as e:
        return [f"{path}: {e}"]
    in_heredoc = None
    for n, raw in enumerate(lines, 1):
        if in_heredoc:
            if raw.strip() == in_heredoc:
                in_heredoc = None
            continue
        m = re.search(r"<<-?\s*'?\"?([A-Za-z_]+)'?\"?", raw)
        if m and "<<<" not in raw:
            in_heredoc = m.group(1)
        if "# lint: ok" in raw:
            continue
        line = strip_strings(raw)
        if E1.search(line):
            problems.append(f"{path}:{n}: E1 loop body ends in a conditional '&&': false on the last pass -> errexit")
        if E2.search(line) and "<<<" not in line:
            problems.append(f"{path}:{n}: E2 early-exit reader in a pipeline: SIGPIPE is a failure under pipefail")
        if E3.search(line):
            problems.append(f"{path}:{n}: E3 'local x=$(...)' masks the command's exit status")
        if unquoted_dollar_at(line):
            problems.append(f"{path}:{n}: E4 unquoted $@ / $*")
        if E5.search(line):
            problems.append(f"{path}:{n}: E5 backticks; use $(...)")
    return problems


if __name__ == "__main__":
    found = [p for f in sys.argv[1:] for p in lint(f)]
    for p in found:
        print(p)
    sys.exit(1 if found else 0)
