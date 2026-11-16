// Small words the extension and its pages share.

/** A path as one spelling for comparing: `/` for `\`, no slash at the end, one case where
 * the path is Windows'. A drive's root is Windows' too, once its slash is gone: `C:\` and
 * `c:\` are one path, and `D:\a.ts` is inside `D:\`. */
export function samePath(path: string): string {
  const p = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[A-Za-z]:(\/|$)/.test(p) ? p.toLowerCase() : p;
}

/** Whether `path` is `root` or inside it. */
export function within(path: string, root: string): boolean {
  const p = samePath(path);
  const r = samePath(root);
  return p === r || p.startsWith(`${r}/`);
}

/** How long ago `ms` (Unix milliseconds, as the core's threads have their times) was, in
 * words. */
export function ago(ms: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  const d = Math.round(s / 86400);
  return d === 1 ? "yesterday" : `${d} days ago`;
}

/** `text` with its control characters shown instead of obeyed, as the CLI prints what came
 * from the model or a tool (crates/ognivo-cli/src/view.rs `printable`): an escape sequence in
 * a file must not reach a terminal. The C0 controls but the tab and the line feed become
 * their pictures (`␛`, `␇`), DEL becomes `␡`, and the C1 controls become U+FFFD, since xterm
 * obeys them too (U+009D starts an OSC, as ESC ] does). `returns` keeps a lone carriage
 * return, with which a progress bar writes over its own line. */
export function printable(text: string, returns = false): string {
  return text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, (c) => {
    if (returns && c === "\r") return c;
    const n = c.charCodeAt(0);
    return n < 0x20 ? String.fromCharCode(0x2400 + n) : n === 0x7f ? "\u2421" : "\ufffd";
  });
}

/** `s` as markdown's inline code, on one line: between backtick runs longer than any run in
 * it, so that nothing in it ends the code and none of it is read as markdown. A space goes
 * inside the backticks where it begins or ends with a backtick or a space, as markdown takes
 * one away there again. */
export function code(s: string): string {
  const one = s.replace(/\r\n|\r|\n/g, " ");
  if (one === "") return "` `";
  let longest = 0;
  for (const run of one.match(/`+/g) ?? []) longest = Math.max(longest, run.length);
  const ticks = "`".repeat(longest + 1);
  const pad = /^[` ]|[` ]$/.test(one) ? " " : "";
  return `${ticks}${pad}${one}${pad}${ticks}`;
}

/** `s` as markdown that says just `s`, on one line: every ASCII punctuation character
 * escaped, so that no part of it makes a link, an image, HTML or emphasis. Markdown takes
 * the backslashes away again. */
export function literal(s: string): string {
  return s.replace(/\s+/g, " ").replace(/[!-/:-@[-`{-~]/g, "\\$&");
}

/** A folder of the workspace as VS Code spells it, and its real path: links, junctions,
 * subst and mapped drives and 8.3 names resolved, as the core spells the projects. */
export interface Spelling {
  folder: string;
  real: string;
}

/** Each of `folders` with its real path as `realpath` finds it; one it cannot find (a
 * folder that is not there) as it is. */
export function spellingsOf(folders: readonly string[], realpath: (path: string) => string): Spelling[] {
  return folders.map((folder) => {
    try {
      return { folder, real: realpath(folder) };
    } catch {
      return { folder, real: folder };
    }
  });
}

/** Whether `path` is a POSIX path, one that starts with `/`: there `\` is a character of a
 * name, not a separator. */
function posix(path: string): boolean {
  return path.startsWith("/");
}

/** A path's names, without its separators. */
function names(path: string): string[] {
  return path.split(posix(path) ? /\/+/ : /[\\/]+/).filter((n) => n !== "");
}

/** Whether `path` is `root` or inside it: as `within` says on Windows, and with `/` as the
 * only separator under a POSIX root, so that `/r/proj\AGENTS.md`, a file beside `/r/proj`,
 * is not inside it. */
function inside(path: string, root: string): boolean {
  if (!posix(root)) return within(path, root);
  const r = root.replace(/\/+$/, "");
  return path === r || path.startsWith(`${r}/`);
}

/** `path`, which the core spells with real paths, as the workspace's folders spell it, so
 * that it is the file VS Code has open: inside a folder as VS Code spells it, it stays; inside
 * a folder's real path, it is that folder's path from there (the deepest such folder's). Any
 * other path stays as it is. Windows' `\\?\` before a long path is no part of the name. */
export function respelled(path: string, folders: readonly Spelling[]): string {
  if (folders.some((f) => inside(path, f.folder))) return path;
  const bare = path.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
  let best: Spelling | undefined;
  for (const f of folders) {
    if (inside(bare, f.real) && (!best || names(f.real).length > names(best.real).length)) best = f;
  }
  if (!best) return path;
  const rest = names(bare).slice(names(best.real).length);
  // VS Code spells a Windows folder with `\` only; any other path has a `/`
  const sep = best.folder.includes("/") ? "/" : "\\";
  const folder = best.folder.length > 1 ? best.folder.replace(/[\\/]+$/, "") : best.folder;
  return rest.length ? `${folder}${folder.endsWith(sep) ? "" : sep}${rest.join(sep)}` : best.folder;
}
