// What a message carries besides its text (messages.ts `ContextChip`): the file the user
// works in, a selection, a command that failed in the user's terminal, a problem. The core
// takes a message's text and images only, so the chips go into the text, as a block at its
// end that the model reads as markdown; the page shows that block again as the chips it was
// made of (`splitContext`).
//
//   {text}
//
//   ---
//   Context from VS Code:
//   - Open file: `src/parse.rs`
//   - Selection in `src/parse.rs`, lines 4–8:
//     ```rust
//     …
//     ```
//   - Terminal: `cargo run -- expenses.csv` exited with 101 in `/home/u/tally`:
//     ```text
//     …
//     ```
//   - Problem in `src/parse.rs` line 6 (error, rustc): mismatched types …
//
// A fence is the shortest run of three backticks or more that is longer than every run in
// its body, so that nothing in the body ends it, and inline code is delimited the same way;
// a fenced body is indented two spaces, as the list item's own text. A file's chip and a
// selection's in the same file make the selection's entry only. Line ends in bodies become
// LF; what goes on one line (paths, a command, a problem's message) is made one line.
//
// The extension names a file relative to the window's first folder, but a message may go to
// a session of another folder or project, whose agent reads a relative path against its own
// root: a chip names its file for the session it goes to (`chipFor`), relative to that
// session's root where the file is inside it, and by its path on disk where it is not, or
// where the page does not know that root yet.

import type { ContextChip } from "./messages.ts";
import { samePath, within } from "./text.ts";

/** A chip of a message that was sent, as the page shows it again: `label` on the chip, and
 * `detail`, the entry's markdown as the message holds it, when the user opens it. */
export interface ShownChip {
  kind: ContextChip["kind"];
  label: string;
  detail: string;
}

const HEAD = "Context from VS Code:";
/** The block's start: after the text and a blank line, or at the message's start; then its
 * head. Only the blank line is taken, so that the head of a text that ends in one, as the
 * user wrote it, does not hide the block after it. */
const BLOCK = /(^|\r?\n\r?\n)(?=(---\r?\nContext from VS Code:\r?\n))/g;
/** A label's room for a command's words. */
const COMMAND_ROOM = 24;

/** `text` with the context of `chips` below it; `text` as it is when there are none. */
export function withContext(text: string, chips: ContextChip[]): string {
  const selected = new Set(chips.flatMap((c) => (c.kind === "selection" ? [samePath(c.abs)] : [])));
  const kept = chips.filter((c) => c.kind !== "file" || !selected.has(samePath(c.abs)));
  // the same chip twice says it once
  const entries = [...new Set(kept.map(entry))];
  if (entries.length === 0) return text;
  const block = ["---", HEAD, ...entries].join("\n");
  return text === "" ? block : `${text}\n\n${block}`;
}

/** A message as the page shows it: its text, and the chips of the context block at its
 * end as `withContext` wrote it. A message without such a block is all text. */
export function splitContext(message: string): { text: string; chips: ShownChip[] } {
  let last: RegExpExecArray | undefined;
  for (const m of message.matchAll(BLOCK)) last = m;
  const chips = last && entries(message.slice(last.index + last[1]!.length + last[2]!.length));
  if (!last || !chips) return { text: message, chips: [] };
  return { text: message.slice(0, last.index), chips };
}

/** What the page calls a chip: `parse.rs`, `parse.rs:4–8`, `cargo run · exit 101`,
 * `parse.rs:6`; a message's chips are called so again once it is sent. */
export function chipLabel(chip: ContextChip): string {
  switch (chip.kind) {
    case "file":
      return base(chip.path);
    case "selection":
      return `${base(chip.path)}:${chip.start === chip.end ? chip.start : `${chip.start}–${chip.end}`}`;
    case "terminal":
      return terminalLabel(chip.command, String(chip.exitCode));
    case "problem":
      return `${base(chip.path)}:${chip.line}`;
  }
}

/** `abs`, a file's path on disk, as a session that works in `root` names it: relative to
 * `root`, with `/`, where the file is inside it; else `abs` as it is. */
export function pathFor(abs: string, root: string): string {
  const file = abs.replace(/\\/g, "/");
  const top = root.replace(/\\/g, "/").replace(/\/+$/, "");
  if (top === "" || samePath(file) === samePath(top) || !within(file, top)) return abs;
  return file.slice(top.length + 1);
}

/** `chip` as the agent of a session that works in `root` reads it: its file relative to
 * `root` where it is inside it, else by its path on disk; by its path on disk too where the
 * root is not known yet (`""`: the session's history is still on its way to the page). A
 * command's chip names no file, and a chip without a path on disk stays as it is. */
export function chipFor(chip: ContextChip, root: string): ContextChip {
  if (chip.kind === "terminal" || !chip.abs) return chip;
  const path = pathFor(chip.abs, root);
  return path === chip.path ? chip : { ...chip, path };
}

/** A workspace file as the extension names it (`files/find`: relative to `home`, the
 * window's first folder, with `/`; by its path on disk outside it), as a session that works
 * in `root` names it; by its path on disk where the root is not known yet (`""`). Without a
 * folder in the window, it stays as it is. */
export function fromWindow(path: string, home: string, root: string): string {
  if (!home || samePath(home) === samePath(root)) return path;
  return pathFor(onDisk(path) ? path : joined(home, path), root);
}

/** Whether `path` is a path on disk, not one relative to a folder. */
function onDisk(path: string): boolean {
  return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);
}

/** `rel`, relative with `/`, inside folder `dir`, with the separator `dir` is written with. */
function joined(dir: string, rel: string): string {
  const sep = /^([A-Za-z]:)?\\|^\\\\/.test(dir) ? "\\" : "/";
  return `${dir.replace(/[\\/]+$/, "")}${sep}${rel.split("/").join(sep)}`;
}

/** A chip's entry in the block. */
function entry(c: ContextChip): string {
  switch (c.kind) {
    case "file":
      return `- Open file: ${code(c.path)}`;
    case "selection": {
      const lines = c.start === c.end ? `line ${c.start}` : `lines ${c.start}–${c.end}`;
      return `- Selection in ${code(c.path)}, ${lines}:\n${fenced(c.language.replace(/[^\w#+.-]/g, "") || "text", c.text)}`;
    }
    case "terminal": {
      const where = c.cwd ? ` in ${code(c.cwd)}` : "";
      return `- Terminal: ${code(c.command)} exited with ${c.exitCode}${where}:\n${fenced("text", c.output)}`;
    }
    case "problem": {
      const said = c.source ? `${c.severity}, ${oneLine(c.source)}` : c.severity;
      return `- Problem in ${code(c.path)} line ${c.line} (${said}): ${oneLine(c.message)}`.trimEnd();
    }
  }
}

/** The entries of a block, or `undefined` when what follows its head is not entries. */
function entries(rest: string): ShownChip[] | undefined {
  const lines = rest.split(/\r?\n/);
  while (lines.length > 0 && lines[lines.length - 1]!.trim() === "") lines.pop();
  if (lines.length === 0) return undefined;
  const chips: ShownChip[] = [];
  for (let i = 0; i < lines.length; ) {
    const head = header(lines[i]!);
    if (!head) return undefined;
    let end = i + 1;
    if (head.fenced) {
      const fence = /^ {2}(`{3,})/.exec(lines[i + 1] ?? "")?.[1];
      if (!fence) return undefined;
      // nothing in the body is as long a run as its fence
      const close = lines.findIndex((l, j) => j > i + 1 && l.trimEnd() === `  ${fence}`);
      if (close < 0) return undefined;
      end = close + 1;
    }
    chips.push({ kind: head.kind, label: head.label, detail: lines.slice(i, end).join("\n") });
    i = end;
  }
  return chips;
}

/** What an entry's first line says: its kind, its chip's label, and whether a fenced body
 * follows. */
function header(line: string): { kind: ContextChip["kind"]; label: string; fenced: boolean } | undefined {
  const file = codeAfter(line, "- Open file: ");
  if (file) return file.rest.trim() === "" ? { kind: "file", label: base(file.code), fenced: false } : undefined;
  const selection = codeAfter(line, "- Selection in ");
  if (selection) {
    const m = /^, lines? (\d+)(?:–(\d+))?:\s*$/.exec(selection.rest);
    return m ? { kind: "selection", label: `${base(selection.code)}:${m[2] ? `${m[1]}–${m[2]}` : m[1]}`, fenced: true } : undefined;
  }
  const terminal = codeAfter(line, "- Terminal: ");
  if (terminal) {
    const m = /^ exited with (-?\d+)(?: in .*)?:\s*$/.exec(terminal.rest);
    return m ? { kind: "terminal", label: terminalLabel(terminal.code, m[1]!), fenced: true } : undefined;
  }
  const problem = codeAfter(line, "- Problem in ");
  if (problem) {
    const m = /^ line (\d+) \(/.exec(problem.rest);
    return m ? { kind: "problem", label: `${base(problem.code)}:${m[1]}`, fenced: false } : undefined;
  }
  return undefined;
}

/** The inline code right after `prefix` at the start of `line`, and what follows it. */
function codeAfter(line: string, prefix: string): { code: string; rest: string } | undefined {
  if (!line.startsWith(prefix)) return undefined;
  const s = line.slice(prefix.length);
  const ticks = /^`+/.exec(s)?.[0];
  if (!ticks) return undefined;
  // it ends at a run of as many backticks, no more
  const close = new RegExp(`(?<!\`)${ticks}(?!\`)`, "g");
  close.lastIndex = ticks.length;
  const end = close.exec(s);
  if (!end) return undefined;
  let code = s.slice(ticks.length, end.index);
  if (code.length > 1 && code.startsWith(" ") && code.endsWith(" ") && code.trim() !== "") code = code.slice(1, -1);
  return { code, rest: s.slice(end.index + ticks.length) };
}

/** `s` as inline code on one line, between backtick runs longer than any in it; with a
 * space inside them where it begins or ends with a backtick or a space, which markdown
 * takes away again. Nothing is a space as code: two backticks alone are no code. */
function code(s: string): string {
  const one = oneLine(s);
  if (one === "") return "` `";
  const ticks = "`".repeat(longestRun(one) + 1);
  const pad = /^[` ]|[` ]$/.test(one) ? " " : "";
  return `${ticks}${pad}${one}${pad}${ticks}`;
}

/** `body` fenced as code with `info`, each line indented as the list item's text. */
function fenced(info: string, body: string): string {
  const text = body.replace(/\r\n?/g, "\n").replace(/\n+$/, "");
  const ticks = "`".repeat(Math.max(3, longestRun(text) + 1));
  const lines = text === "" ? [] : text.split("\n");
  return [`${ticks}${info}`, ...lines, ticks].map((l) => (l === "" ? l : `  ${l}`)).join("\n");
}

/** The longest run of backticks in `s`. */
function longestRun(s: string): number {
  let longest = 0;
  for (const run of s.match(/`+/g) ?? []) longest = Math.max(longest, run.length);
  return longest;
}

function oneLine(s: string): string {
  return s.replace(/\s*[\r\n]+\s*/g, " ").trim();
}

/** A path's last name. */
function base(path: string): string {
  const one = oneLine(path);
  return one.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || one;
}

/** A failed command's chip: `cargo run · exit 101`, or `exit 101` when the shell did not
 * say the command. */
function terminalLabel(command: string, exitCode: string): string {
  const words = commandLabel(command);
  return words === "" ? `exit ${exitCode}` : `${words} · exit ${exitCode}`;
}

/** A command's first words, up to its first option and about 24 characters: `cargo run` of
 * `cargo run -- expenses.csv`. */
function commandLabel(command: string): string {
  const [first = "", ...words] = command.trim().split(/\s+/);
  let label = first;
  for (const w of words) {
    if (w.startsWith("-") || label.length + 1 + w.length > COMMAND_ROOM) break;
    label += ` ${w}`;
  }
  return label.length > COMMAND_ROOM ? `${label.slice(0, COMMAND_ROOM - 1)}…` : label;
}
