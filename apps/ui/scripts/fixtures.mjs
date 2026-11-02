// Copies recorded sessions (ATIF traces of T2-mini nights) into fixtures/, the sessions
// the UI kit is built and measured on, and what the UI needs of the profiles into
// src/profiles.json: their control tokens, which the Inspector marks, and their context
// budgets, which the composer counts against.
//
//   npm run fixtures            from ~/.local/share/ognivo/night, or OGNIVO_NIGHT_DIR
//
// Each fixture also gets what its generations were asked exactly: the session's
// conversations as the store kept them (sessions.db next to the traces) and, for each
// generation, the messages of its request. That is the prefix of its conversation whose
// sha256 is the prompt_hash it recorded (ognivo-core agent.rs: prompt_hash); a generation
// without one gets none.
//
// The fixtures are committed, so the machine they were recorded on is taken out: the
// project path, the home directory and the user name. `<` and `>` are written as
// \u003c and \u003e, the same JSON, so no control token of a model stands in a file of
// the repository in one piece (docs/architecture.md §5.3).

import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { dirname, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
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

const here = dirname(fileURLToPath(import.meta.url));
const night = process.env.OGNIVO_NIGHT_DIR ?? join(homedir(), ".local/share/ognivo/night");
const out = join(here, "../fixtures");
const home = homedir();
const user = userInfo().username;
const word = new RegExp(`\\b${user.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "g");

/** JSON with `<` and `>` escaped, so control tokens never stand in one piece (§5.3). */
function safe(value, indent) {
  return JSON.stringify(value, null, indent).replace(/</g, "\\u003c").replace(/>/g, "\\u003e");
}

/** Adds the session's conversations and each generation's request to `trace`. */
function addRequests(trace, db) {
  const session = Number(trace.extra?.ognivo?.session ?? trace.session_id);
  /** @type {Record<number, unknown[]>} */
  const conversations = {};
  const rows = db
    .prepare("select conversation, body from messages where session = ? order by conversation, idx")
    .all(session);
  for (const row of rows) (conversations[row.conversation] ??= []).push(JSON.parse(row.body));
  const tools = (trace.agent?.tool_definitions ?? []).map((d) => ({
    name: d.function.name,
    description: d.function.description,
    parameters: d.function.parameters,
  }));
  // serde_json's compact form, which JSON.stringify writes the same
  const hash = (messages) =>
    `sha256:${createHash("sha256").update(JSON.stringify({ messages, tools })).digest("hex")}`;
  let conversation = 1;
  let from = 0;
  let found = 0;
  let asked = 0;
  for (const step of trace.steps) {
    const n = step.extra?.conversation;
    if (step.source === "system" && n && n !== conversation) {
      conversation = n;
      from = 0;
    }
    const want = step.extra?.prompt_hash;
    if (step.source !== "agent" || !want) continue;
    asked++;
    const messages = conversations[conversation] ?? [];
    for (let k = from; k <= messages.length; k++) {
      if (hash(messages.slice(0, k)) === want) {
        step.extra.request = { conversation, messages: k };
        from = k;
        found++;
        break;
      }
    }
  }
  trace.extra.ognivo.conversations = conversations;
  return `${found} of ${asked} requests`;
}

mkdirSync(out, { recursive: true });
for (const [name, rel] of Object.entries(SESSIONS)) {
  const path = join(night, rel);
  const trace = JSON.parse(readFileSync(path, "utf8"));
  const db = new DatabaseSync(join(dirname(dirname(path)), "sessions.db"), { readOnly: true });
  const requests = addRequests(trace, db);
  db.close();
  const raw = JSON.stringify(trace);
  /** @type {string | undefined} */
  const project = trace.extra?.ognivo?.project;
  let text = project ? raw.replaceAll(project, "/home/dev/demo") : raw;
  text = text.replaceAll(home, "/home/dev").replace(word, "dev");
  if (text.includes(home) || word.test(text)) {
    throw new Error(`${rel}: the machine is still in it`);
  }
  word.lastIndex = 0;
  writeFileSync(join(out, `${name}.json`), `${safe(JSON.parse(text), 1)}\n`);
  console.log(`${name}: ${rel}, ${requests}`);
}

// Each profile's control tokens (framing ones frame the model's own messages and calls,
// §5.3) and context budget, by profile id
const profiles = join(here, "../../../profiles");
/** @type {Record<string, { tokens: { framing: string[]; other: string[] }; context: { working_max: number; compact_at: number } }>} */
const table = {};
for (const file of readdirSync(profiles).filter((f) => f.endsWith(".toml"))) {
  const toml = readFileSync(join(profiles, file), "utf8");
  const id = /^id = "([^"]+)"/m.exec(toml)?.[1];
  const section = /^\[tokens\]\n([\s\S]*?)(?=^\[)/m.exec(toml)?.[1] ?? "";
  const list = (key) => {
    const body = new RegExp(`^${key} = \\[([\\s\\S]*?)\\]`, "m").exec(section)?.[1] ?? "";
    return [...body.matchAll(/"((?:[^"\\]|\\.)*)"|'([^']*)'/g)].map((m) =>
      m[1] !== undefined ? JSON.parse(`"${m[1]}"`) : m[2],
    );
  };
  if (!id) continue;
  const framing = list("framing");
  const other = [...new Set([...list("structural"), ...list("specials")])].filter(
    (t) => !framing.includes(t),
  );
  const context = /^\[context\]\n([\s\S]*?)(?=^\[)/m.exec(toml)?.[1] ?? "";
  const number = (key) => Number(new RegExp(`^${key} = ([0-9.]+)`, "m").exec(context)?.[1]);
  table[id] = {
    tokens: { framing, other },
    context: { working_max: number("working_max"), compact_at: number("compact_at") },
  };
}
writeFileSync(join(here, "../src/profiles.json"), `${safe(table, 1)}\n`);
console.log(`profiles: ${Object.keys(table).join(", ")}`);
