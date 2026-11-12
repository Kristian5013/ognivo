#!/usr/bin/env node
// THIRD-PARTY-LICENSES.txt of the packages (scripts/package.sh): the code of others that
// they carry, with its licenses. ognivo links crates, and the window bundles npm packages
// in its UI; Electron's and Chromium's notices come with Electron itself.
//
//   node scripts/licenses.mjs [TARGET] > THIRD-PARTY-LICENSES.txt
//
// Crates: what ognivo-cli links for TARGET (x86_64-unknown-linux-gnu by default;
// x86_64-pc-windows-msvc for Windows), from `cargo metadata` and the lockfile:
// normal dependencies, not those of build scripts or tests, and no proc macros, which run
// in the compiler. npm: the workspace's packages that are not only for development
// (package-lock.json), which are the UI's.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const TARGET = process.argv[2] ?? "x86_64-unknown-linux-gnu";
const FILES = /^(licen[cs]e|copying|notice|copyright|unlicense)/i;

/** The license files in `dir`, as texts. */
function texts(dir, extra = []) {
  const names = existsSync(dir) ? readdirSync(dir).filter((n) => FILES.test(n)) : [];
  const paths = [...new Set([...names.map((n) => join(dir, n)), ...extra.map((f) => join(dir, f))])];
  return paths
    .filter((p) => existsSync(p))
    .sort()
    .map((p) =>
      readFileSync(p, "utf8")
        .split("\n")
        .map((l) => l.trimEnd())
        .join("\n")
        .trim(),
    )
    .filter(Boolean);
}

function crates() {
  const meta = JSON.parse(
    execFileSync(
      "cargo",
      ["metadata", "--format-version", "1", "--locked", "--filter-platform", TARGET],
      { cwd: ROOT, maxBuffer: 1 << 28, encoding: "utf8" },
    ),
  );
  const byId = new Map(meta.packages.map((p) => [p.id, p]));
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const start = meta.packages.find((p) => p.name === "ognivo-cli" && !p.source);
  const linked = [];
  const seen = new Set([start.id]);
  const queue = [start.id];
  while (queue.length > 0) {
    const id = queue.pop();
    const p = byId.get(id);
    if (p.targets.some((t) => t.kind.includes("proc-macro"))) continue;
    if (p.source) linked.push(p);
    for (const d of nodes.get(id).deps) {
      if (!d.dep_kinds.some((k) => k.kind === null) || seen.has(d.pkg)) continue;
      seen.add(d.pkg);
      queue.push(d.pkg);
    }
  }
  return linked
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version))
    .map((p) => ({
      name: p.name,
      version: p.version,
      license: p.license ?? (p.license_file ? `see ${p.license_file}` : "not stated"),
      where: p.repository ?? "",
      authors: p.authors.join(", "),
      texts: texts(dirname(p.manifest_path), p.license_file ? [p.license_file] : []),
    }));
}

/** The author of an npm package, from its package.json. */
function author(dir) {
  try {
    const a = JSON.parse(readFileSync(join(dir, "package.json"), "utf8")).author;
    return typeof a === "string" ? a : (a?.name ?? "");
  } catch {
    return "";
  }
}

function npm() {
  const lock = JSON.parse(readFileSync(join(ROOT, "package-lock.json"), "utf8"));
  return Object.entries(lock.packages)
    .filter(([path, p]) => path.startsWith("node_modules/") && !p.dev && !p.devOptional && !p.link)
    .map(([path, p]) => ({
      name: path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length),
      version: p.version ?? "",
      license: p.license ?? "not stated",
      where: "",
      authors: author(join(ROOT, path)),
      texts: texts(join(ROOT, path)),
    }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
}

const groups = [
  ["Crates that ognivo links", crates()],
  ["npm packages that the window bundles", npm()],
];
const out = [
  "The code of others that ognivo's packages carry, and its licenses.",
  "",
  "ognivo links the crates below, and its window bundles the npm packages below in its",
  "UI. The window is Electron, with Chromium: their notices are Electron's LICENSE and",
  "LICENSES.chromium.html, beside the window's binary.",
];
const byText = new Map();
for (const [title, list] of groups) {
  out.push("", `${title} (${list.length})`, "");
  for (const p of list) {
    out.push(`  ${p.name} ${p.version}: ${p.license}${p.where ? `, ${p.where}` : ""}`);
    if (p.texts.length === 0) {
      out.push(`    no license file in the package: ${p.license}${p.authors ? `, by ${p.authors}` : ""}; the license's text is below with others'`);
    }
    for (const t of p.texts) {
      if (!byText.has(t)) byText.set(t, []);
      byText.get(t).push(`${p.name} ${p.version}`);
    }
  }
}
out.push("", "The license texts, each with the packages that carry it");
for (const [text, who] of byText) {
  out.push("", "=".repeat(80), `${who.join(", ")}:`, "", text);
}
process.stdout.write(`${out.join("\n")}\n`);
