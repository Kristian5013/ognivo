// The workspace's files for `@` in the message (`files/find`, src/files.ts): which files to
// leave out, as VS Code's settings and a project's .gitignore say, and how well a path
// matches what the user typed. Nothing here knows VS Code, so it is tested on its own
// (fuzzy.test.ts).

/** Characters after which a word starts in a path. */
const WORD_START = /[/\\_\-. ]/;

/** The positions of `query`'s characters in `text` from `from` on, in a window as short as
 * the first match allows: forward to where the query first ends, then back to the latest
 * start that still holds it all. `undefined` when they are not all there in order. */
function matched(text: string, query: string, from: number): number[] | undefined {
  let j = 0;
  let end = -1;
  for (let i = from; i < text.length && end < 0; i++) {
    if (text[i] === query[j]) {
      j++;
      if (j === query.length) end = i;
    }
  }
  if (end < 0) return undefined;
  let start = end;
  for (let k = query.length - 1, i = end; i >= from; i--) {
    if (text[i] === query[k]) {
      start = i;
      if (--k < 0) break;
    }
  }
  const at: number[] = [];
  for (let i = start, k = 0; i <= end && k < query.length; i++) {
    if (text[i] === query[k]) {
      at.push(i);
      k++;
    }
  }
  return at;
}

/** How well `path` matches `query`: its characters in order, case aside, with more for a
 * match in the file's own name, for characters in a row and at the starts of words, and a
 * little less for a long path. `undefined` when it does not match. Spaces in the query match
 * anything. */
export function score(path: string, query: string): number | undefined {
  const q = query.toLowerCase().replace(/\s+/g, "");
  if (!q) return -path.length / 4;
  const p = path.toLowerCase();
  const nameAt = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")) + 1;
  const inName = matched(p, q, nameAt);
  const at = inName ?? matched(p, q, 0);
  if (!at) return undefined;
  let s = 0;
  for (let n = 0; n < at.length; n++) {
    const i = at[n]!;
    s += 16;
    const before = path[i - 1];
    const camel = before !== undefined && /[a-z0-9]/.test(before) && /[A-Z]/.test(path[i]!);
    if (i === 0 || i === nameAt || (before !== undefined && WORD_START.test(before)) || camel) s += 8;
    if (n > 0 && at[n - 1] === i - 1) s += 6;
  }
  // characters between the first match and the last
  s -= Math.min(30, at[at.length - 1]! - at[0]! + 1 - at.length);
  const name = p.slice(nameAt);
  if (inName) {
    s += 30;
    if (name.startsWith(q)) s += 25;
    const dot = name.lastIndexOf(".");
    if (name === q) s += 50;
    else if (dot > 0 && name.slice(0, dot) === q) s += 40;
  }
  return s - path.length / 4;
}

/** The paths that match `query`, the best first: at most `limit`. Equal scores go shorter
 * path first, then in the order of the alphabet. */
export function rank(paths: readonly string[], query: string, limit: number): string[] {
  const scored: Array<{ path: string; s: number }> = [];
  for (const path of paths) {
    const s = score(path, query);
    if (s !== undefined) scored.push({ path, s });
  }
  scored.sort((a, b) => b.s - a.s || a.path.length - b.path.length || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  return scored.slice(0, Math.max(0, limit)).map((x) => x.path);
}

/** A glob with one group in braces as the globs it stands for; a glob with none, or with
 * groups in groups, as it is. */
function expand(glob: string): string[] {
  const m = /^([^{}]*)\{([^{}]*)\}([^{}]*)$/.exec(glob);
  if (!m) return glob.includes("{") || glob.includes("}") ? [] : [glob];
  return m[2]!.split(",").map((alt) => `${m[1]}${alt}${m[3]}`);
}

/** The globs of an exclude setting (`files.exclude`, `search.exclude`) that are on. A glob
 * whose value is a condition on its siblings (`{ "when": … }`) is not taken. */
export function enabledGlobs(setting: Record<string, unknown> | undefined): string[] {
  return Object.entries(setting ?? {})
    .filter(([, on]) => on === true)
    .map(([glob]) => glob);
}

/** The globs of exclude settings that are on, and the folders' contents of each, in one
 * pattern for findFiles; `undefined` when there are none. */
export function excludeGlob(...settings: ReadonlyArray<Record<string, unknown> | undefined>): string | undefined {
  return joinGlobs(settings.flatMap(enabledGlobs));
}

/** `globs` and the contents of the folders they name, in one pattern. */
export function joinGlobs(globs: readonly string[]): string | undefined {
  const all = new Set<string>();
  for (const glob of globs.flatMap(expand)) {
    const g = glob.trim().replace(/\/+$/, "");
    if (!g || g.includes(",")) continue;
    all.add(g);
    if (!g.endsWith("/**")) all.add(`${g}/**`);
  }
  if (all.size === 0) return undefined;
  return all.size === 1 ? [...all][0] : `{${[...all].join(",")}}`;
}

/** Lines of a .gitignore taken at most. */
const GITIGNORE_LINES = 200;

/** What a .gitignore at a folder's root leaves out, as globs relative to that folder. Only
 * the plain lines are taken: no negations, no escapes, no character classes. A name without
 * a slash goes at any depth; one with a slash, from the root. */
export function gitignoreGlobs(text: string): string[] {
  const globs: string[] = [];
  for (const raw of text.split(/\r?\n/).slice(0, GITIGNORE_LINES)) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || line.startsWith("!") || /[\\[\]{},]/.test(line)) continue;
    const name = line.replace(/\/+$/, "");
    if (!name || name === "*" || name === "**") continue;
    const inner = name.replace(/^\/+/, "");
    if (!inner) continue;
    globs.push(name.includes("/") ? inner : `**/${inner}`);
  }
  return globs;
}
