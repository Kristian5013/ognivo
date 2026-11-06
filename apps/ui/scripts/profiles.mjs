// What the UI needs of the profiles (profiles/*.toml), into src/profiles.json by profile
// id: their control tokens, which the Inspector marks; their context budgets, which the
// composer counts against; their reasoning levels, and whether the Native path reads their
// format, which the form of a new thread offers.
//
//   npm run profiles            (npm run fixtures writes it too)
//
// `<` and `>` are written as \u003c and \u003e, the same JSON, so no control token of a
// model stands in a file of the repository in one piece (docs/architecture.md §5.3). A
// test of ognivo-models (native.rs) holds the file to the profiles.

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const profiles = join(here, "../../../profiles");
/** The families whose format the Native path reads (ognivo-models native.rs: `reads`). */
const NATIVE = ["harmony", "qwen-xml"];

/** JSON with `<` and `>` escaped, so control tokens never stand in one piece (§5.3). */
function safe(value, indent) {
  return JSON.stringify(value, null, indent).replace(/</g, "\\u003c").replace(/>/g, "\\u003e");
}

/** @type {Record<string, { tokens: { framing: string[]; other: string[] }; context: { working_max: number; compact_at: number; max_tokens: number; ceiling: number }; reasoning: { levels: string[]; default: string }; native: boolean }>} */
const table = {};
for (const file of readdirSync(profiles)
  .filter((f) => f.endsWith(".toml"))
  .sort()) {
  const toml = readFileSync(join(profiles, file), "utf8");
  const id = /^id = "([^"]+)"/m.exec(toml)?.[1];
  const family = /^family = "([^"]+)"/m.exec(toml)?.[1] ?? "";
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
  const reasoning = /^\[reasoning\]\n([\s\S]*?)(?=^\[)/m.exec(toml)?.[1] ?? "";
  const levels = [...(/^levels = \[([^\]]*)\]/m.exec(reasoning)?.[1] ?? "").matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  table[id] = {
    tokens: { framing, other },
    context: {
      working_max: number("working_max"),
      compact_at: number("compact_at"),
      max_tokens: number("max_tokens"),
      ceiling: number("ceiling"),
    },
    reasoning: { levels, default: /^default = "([^"]*)"/m.exec(reasoning)?.[1] ?? "" },
    native: NATIVE.includes(family),
  };
}
writeFileSync(join(here, "../src/profiles.json"), `${safe(table, 1)}\n`);
console.log(`profiles: ${Object.keys(table).join(", ")}`);
