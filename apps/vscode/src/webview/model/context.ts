// What the composer counts: the files a session touched and what reading them cost, the
// mention being written and the files it offers, a message's tokens; and what it takes
// from the user's VS Code by itself: the file and the selection the user works in, and a
// command that failed in the user's terminal.

import type { ContextChip, EditorState, TerminalFailure } from "../../shared/messages.ts";
import profiles from "../../shared/profiles.json";
import { samePath } from "../../shared/text.ts";
import type { Item } from "./model.ts";
import { relativeTo } from "./paths.ts";

/** A rough count, about four bytes a token, until a core can ask the server's tokenizer. */
export function estimateTokens(text: string): number {
  return Math.ceil(new TextEncoder().encode(text).length / 4);
}

const READ = /^\[read: (.+?) — lines (\d+)-(\d+) of (\d+) lines, .*?~(\d+) tokens\]/;

/** The files a session touched, relative to its project, with what reading each whole
 * cost when it did: the read tool says `~N tokens` of the lines it returned. */
export function sessionFiles(items: Item[], project: string): Map<string, number | undefined> {
  const rel = (p: string) => relativeTo(project, p);
  const files = new Map<string, number | undefined>();
  const see = (p: string) => {
    const path = rel(p.trim());
    if (path && !files.has(path)) files.set(path, undefined);
  };
  for (const i of items) {
    if (i.kind !== "tool" || !i.call || i.result?.outcome !== "ok") continue;
    const c = i.call;
    if (c.name === "Read" || c.name === "Edit" || c.name === "Write") see(c.path);
    if (c.name === "Glob") i.result.text.split("\n").slice(1).forEach(see);
    const whole = c.name === "Read" ? READ.exec(i.result.text) : null;
    if (whole && whole[2] === "1" && whole[3] === whole[4]) files.set(rel(whole[1]!), Number(whole[5]));
  }
  return files;
}

/** `@query` right before the caret, the start of a mention being written: a path's
 * letters of any script, digits, `_`, `.`, `/` and `-`. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | undefined {
  const m = /(^|\s)@([\p{L}\p{N}_./-]*)$/u.exec(text.slice(0, caret));
  return m ? { start: caret - m[2]!.length - 1, query: m[2]! } : undefined;
}

/** The files a mention offers, at most `most`: the session's that hold `query` first, then
 * the workspace's that `files/find` found for it, each once. */
export function mentionChoices(query: string, session: readonly string[], found: readonly string[], most = 10): string[] {
  const q = query.toLowerCase();
  const mine = session.filter((p) => p.toLowerCase().includes(q));
  const seen = new Set(mine);
  return [...mine, ...found.filter((p) => !seen.has(p))].slice(0, most);
}

/** The editor's chips: its selection when something is selected, and its file. */
export function editorChips(editor: EditorState | null): {
  file?: Extract<ContextChip, { kind: "file" }>;
  selection?: Extract<ContextChip, { kind: "selection" }>;
} {
  if (!editor) return {};
  const file = { kind: "file", path: editor.path, abs: editor.abs } as const;
  const s = editor.selection;
  if (!s || s.text.trim() === "") return { file };
  return {
    file,
    selection: { kind: "selection", path: editor.path, abs: editor.abs, start: s.start, end: s.end, language: editor.language, text: s.text },
  };
}

/** A command that failed in the user's terminal, as a chip of a message. */
export function failureChip(f: TerminalFailure): ContextChip {
  return { kind: "terminal", command: f.command, exitCode: f.exitCode, cwd: f.cwd, output: f.output };
}

/** What tells a chip from another: the same file, the same lines, the same command. */
export function chipKey(c: ContextChip): string {
  switch (c.kind) {
    case "file":
      return `file:${samePath(c.abs)}`;
    case "selection":
      return `selection:${samePath(c.abs)}:${c.start}-${c.end}`;
    case "terminal":
      return `terminal:${c.command}:${c.exitCode}:${c.cwd ?? ""}`;
    case "problem":
      return `problem:${samePath(c.abs)}:${c.line}:${c.message}`;
  }
}

/** The chip the composer shows by itself: the editor's selection, or else its file; none
 * the user took away from this message, and none of a file a chip of the message names
 * already. */
export function autoChip(editor: EditorState | null, dismissed: ReadonlySet<string>, attached: readonly ContextChip[]): ContextChip | undefined {
  const { file, selection } = editorChips(editor);
  const named = new Set(attached.flatMap((c) => (c.kind === "terminal" ? [] : [samePath(c.abs)])));
  return [selection, file].find((c) => c !== undefined && !dismissed.has(chipKey(c)) && !named.has(samePath(c.abs)));
}

/** A profile's context budget, as profiles.json has it ([context] of its TOML). */
export interface Budget {
  working_max: number;
  compact_at: number;
  max_tokens: number;
  ceiling: number;
}

/** Tokens of a summary request beyond the history, and the least room compaction needs
 * (ognivo-core compact.rs). */
const SUMMARY_PROMPT = 512;
const MIN_ROOM = 8192;

/** The working context of a thread and what its conversation may hold before it
 * compacts, as the core reckons them (compact.rs: `session`, `limit`): the chosen context,
 * or else the server's window, or else the profile's, never past what the model works
 * with; compaction at the profile's share of it, and never so near the window that a
 * step could not generate. A window too small to compact in is no working context. */
export function compaction(
  budget: Budget,
  chosen: number | undefined,
  window: number | undefined,
): { working: number; compactsAt: number } {
  const room = window === undefined ? undefined : window - budget.max_tokens - SUMMARY_PROMPT;
  const fits = room !== undefined && room >= MIN_ROOM;
  const working = Math.min(chosen ?? (fits ? window! : budget.working_max), budget.ceiling);
  const share = Math.floor(working * budget.compact_at);
  return { working, compactsAt: fits ? Math.min(share, room!) : share };
}

/** The working contexts the form of a new thread offers: sizes up to what the model works
 * with and the server holds, and the profile's own. */
export function contextChoices(budget: Budget | undefined, window: number | undefined): number[] {
  const cap = Math.min(budget?.ceiling ?? Infinity, window ?? Infinity);
  const sizes = [16_384, 32_768, 65_536, 98_304, 131_072, 196_608, 262_144];
  const all = new Set(sizes.filter((n) => n <= cap));
  if (budget && budget.working_max <= cap) all.add(budget.working_max);
  if (budget && budget.ceiling <= cap) all.add(budget.ceiling);
  return [...all].sort((a, b) => a - b);
}

/** What the form's first context choice stands for: the server's window; the model's
 * ceiling where the window is larger, so the window is not named by the size it is cut to;
 * or the profile's context where the window is too small to compact in. */
export function windowChoice(budget: Budget | undefined, window: number | undefined): string {
  if (window === undefined) return "the server's window";
  if (!budget) return `the server's window: ${contextSize(window)}`;
  const { working } = compaction(budget, undefined, window);
  if (working === window) return `the server's window: ${contextSize(window)}`;
  if (working === budget.ceiling && window > working) {
    return `${contextSize(working)} of the server's ${contextSize(window)}: the most the model works with`;
  }
  return `the profile's ${contextSize(working)}: the server's ${contextSize(window)} is too small to compact in`;
}

/** What a reasoning level does, by profile and level, as the profiles were measured. */
const LEVEL_HINTS: Record<string, string> = {
  "qwen3.8-27b:xhigh": "Very long thinking: 15K tokens before the first step have been seen. Rarely worth it.",
  "qwen3.8-27b:medium": "Thinks about 10× less than xhigh, and is as good at agent work.",
  "qwen3.8-27b:low": "Short thinking. Good for small, clear changes.",
  "qwen3.8-27b:off": "No thinking. The fastest, for one-line edits.",
  "gpt-oss-20b:high": "The level its best agent scores were measured at.",
  "gpt-oss-20b:medium": "Faster. Its SWE-bench score drops from 61 to 53.",
  "gpt-oss-20b:low": "The fastest, for lookups and small edits.",
};

/** The reasoning levels from the least thinking to the most. */
const THINKING = ["off", "on", "minimal", "low", "medium", "high", "xhigh", "max"];

/** A profile's levels from the least thinking to the most, as the model's popover shows
 * them side by side; a level of another name goes last. */
export function byThinking(levels: readonly string[]): string[] {
  const rank = (l: string) => {
    const i = THINKING.indexOf(l.toLowerCase());
    return i < 0 ? THINKING.length : i;
  };
  return [...levels].sort((a, b) => rank(a) - rank(b));
}

/** The reasoning levels of profile `id`, as profiles.json lists them; none known of a
 * profile it does not have, or of none. */
export function profileLevels(id: string | null | undefined): readonly string[] | undefined {
  return id ? (profiles as Record<string, { reasoning?: { levels: string[] } } | undefined>)[id]?.reasoning?.levels : undefined;
}

/** The reasoning level a new session asks for, of a model whose profile has `levels`: the
 * chosen one where the profile has it, or where its levels are not known (the core says
 * then); `null`, the profile's default, for none chosen and for one the profile has not, as
 * a level of the model picked before. The core refuses to start a session at such a level. */
export function startLevel(chosen: string, levels: readonly string[] | undefined): string | null {
  if (!chosen) return null;
  return !levels?.length || levels.includes(chosen) ? chosen : null;
}

/** Whether a session of profile `id` goes on the Native path, which the user wants or not
 * (`wanted`, `ognivo.native`): where the path reads its model's format, as profiles.json says
 * of the profile; of none chosen, or one this page does not know, as the server's detection
 * says of the model it runs (`detected`), where it said. The core refuses a session on the
 * Native path of a format it does not read: Gemma 4's goes on the Generic path. */
export function nativeFor(wanted: boolean, id: string | null | undefined, detected?: boolean): boolean {
  if (!wanted) return false;
  const reads = id ? (profiles as Record<string, { native?: boolean } | undefined>)[id]?.native : undefined;
  return (reads ?? detected) !== false;
}

/** A word on level `level` of profile `profile`. */
export function levelHint(profile: string, level: string): string {
  return LEVEL_HINTS[`${profile}:${level}`] ?? (level === "off" ? "No thinking. The fastest." : "Thinks before each step, as its model card measured it.");
}

/** `64K`, `256K`: a context size as servers are given it; `98.3k` for one off the grid. */
export function contextSize(n: number): string {
  return n % 1024 === 0 ? `${n / 1024}K` : `${(n / 1000).toFixed(1)}k`;
}
