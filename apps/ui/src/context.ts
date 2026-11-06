// What the composer counts: the files a session touched and what reading them cost, the
// mention being written, and a message's tokens.

import type { Item } from "./model.ts";

/** A rough count, about four bytes a token, until a core can ask the server's tokenizer. */
export function estimateTokens(text: string): number {
  return Math.ceil(new TextEncoder().encode(text).length / 4);
}

const READ = /^\[read: (.+?) — lines (\d+)-(\d+) of (\d+) lines, .*?~(\d+) tokens\]/;

/** The files a session touched, relative to its project, with what reading each whole
 * cost when it did: the read tool says `~N tokens` of the lines it returned. */
export function sessionFiles(items: Item[], project: string): Map<string, number | undefined> {
  const rel = (p: string) => (project && p.startsWith(`${project}/`) ? p.slice(project.length + 1) : p);
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

/** `@query` right before the caret, the start of a mention being written. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | undefined {
  const m = /(^|\s)@([\w./-]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[2]!.length - 1, query: m[2]! } : undefined;
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

/** What the form's first context choice stands for: the server's window, or the profile's
 * context where the window is too small to compact in. */
export function windowChoice(budget: Budget | undefined, window: number | undefined): string {
  if (window === undefined) return "the server's window";
  if (!budget) return `the server's window: ${contextSize(window)}`;
  const { working } = compaction(budget, undefined, window);
  return working === Math.min(window, budget.ceiling)
    ? `the server's window: ${contextSize(working)}`
    : `the profile's ${contextSize(working)}: the server's ${contextSize(window)} is too small to compact in`;
}

/** `64K`, `256K`: a context size as servers are given it; `98.3k` for one off the grid. */
export function contextSize(n: number): string {
  return n % 1024 === 0 ? `${n / 1024}K` : `${(n / 1000).toFixed(1)}k`;
}
