// Copies recorded sessions (ATIF traces of T2-mini nights) into fixtures/, the sessions
// the UI kit is built and measured on.
//
//   npm run fixtures            from ~/.local/share/ognivo/night, or OGNIVO_NIGHT_DIR
//
// The fixtures are committed, so the machine they were recorded on is taken out: the
// project path, the home directory and the user name. `<` and `>` are written as
// < and >, the same JSON, so no control token of a model stands in a file of
// the repository in one piece (docs/architecture.md §5.3).

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** Fixture name → trace, relative to the night directory. @type {Record<string, string>} */
const SESSIONS = {
  "gpt-oss.edit-refused": "2026-11-02-2228/gpt-oss-20b/traces/edit-delete-function.1.json",
  "gpt-oss.escaping": "2026-11-02/gpt-oss-20b/traces/arguments-write-json.1.json",
  "gpt-oss.step-limit": "2026-11-02/gpt-oss-20b/traces/edit-fix-operator.1.json",
  "gpt-oss.empty-answers": "2026-11-02-2228/gpt-oss-20b/traces/arguments-write-framing-token.2.json",
  "gpt-oss.empty-grep-filter": "2026-11-02-2228/gpt-oss-20b/traces/edit-rename-function.2.json",
  "gemma.arithmetic": "2026-11-02/gemma-4-26b-a4b/traces/irrelevance-arithmetic.1.json",
  "gemma.ambiguous-edit": "2026-11-02/gemma-4-26b-a4b/traces/edit-fix-operator.1.json",
  "gemma.failing-build": "2026-11-02/gemma-4-26b-a4b/traces/loop-failing-build.1.json",
  "gemma.unicode": "2026-11-02/gemma-4-26b-a4b/traces/arguments-write-unicode.1.json",
  "qwen.malformed": "2026-11-02/qwen3.8-27b/traces/arguments-write-framing-token.1.json",
  "qwen.probing": "2026-11-02/qwen3.8-27b/traces/arguments-write-special-token.2.json",
  "qwen.long-file": "2026-11-02/qwen3.8-27b/traces/context-edit-long-file.1.json",
  "qwen.project-script": "2026-11-02/qwen3.8-27b/traces/shell-run-script.1.json",
  "qwen.rename": "2026-11-02/qwen3.8-27b/traces/edit-rename-function.1.json",
  "qwen.time-limit": "2026-11-02-2228/qwen3.8-27b/traces/arguments-write-framing-token.3.json",
};

const night = process.env.OGNIVO_NIGHT_DIR ?? join(homedir(), ".local/share/ognivo/night");
const out = join(dirname(fileURLToPath(import.meta.url)), "../fixtures");
const home = homedir();
const user = userInfo().username;
const word = new RegExp(`\\b${user.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "g");

mkdirSync(out, { recursive: true });
for (const [name, rel] of Object.entries(SESSIONS)) {
  const raw = readFileSync(join(night, rel), "utf8");
  /** @type {string | undefined} */
  const project = JSON.parse(raw).extra?.ognivo?.project;
  let text = project ? raw.replaceAll(project, "/home/dev/demo") : raw;
  text = text.replaceAll(home, "/home/dev").replace(word, "dev");
  if (text.includes(home) || word.test(text)) {
    throw new Error(`${rel}: the machine is still in it`);
  }
  word.lastIndex = 0;
  const json = JSON.stringify(JSON.parse(text), null, 1)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e");
  writeFileSync(join(out, `${name}.json`), json + "\n");
  console.log(`${name}: ${rel}`);
}
