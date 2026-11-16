// Where the agent's unreviewed changes are in a file as it is on disk now (src/review.ts):
// the core's hunks (`changes/list`, crates/ognivo-tools/src/diff.rs) are against what the
// file held before the session, without context lines, with lines from 1. The editor counts
// lines from 0, and so does everything here. Nothing here knows VS Code, so it is tested on
// its own (hunks.test.ts).

import type { ChangedFile, Hunk } from "./shared/protocol.ts";

/** Lines from 0, both ends inclusive. */
export interface Span {
  start: number;
  end: number;
}

/** The lines a hunk added, in the file as it is now; none for a hunk that only removed. */
export function addedSpan(h: Hunk): Span | undefined {
  if (h.added.length === 0) return undefined;
  const start = h.new_start - 1;
  return { start, end: start + h.added.length - 1 };
}

/** Where a hunk's removed lines stood: above the line that took their place or follows them
 * now, or, when they were the file's end, below its last line. None for a hunk that only
 * added. */
export function removedMark(h: Hunk, lineCount: number): { line: number; where: "above" | "below" } | undefined {
  if (h.removed.length === 0) return undefined;
  const line = h.new_start - 1;
  const last = Math.max(0, lineCount - 1);
  if (line > last) return { line: last, where: "below" };
  return { line: Math.max(0, line), where: "above" };
}

/** The line a hunk's lens stands above: its first added line, or the line its removal is
 * marked on. */
export function hunkLine(h: Hunk, lineCount: number): number {
  return addedSpan(h)?.start ?? removedMark(h, lineCount)?.line ?? 0;
}

/** The lines a hunk takes in the editor: its added lines, or the one line its removal is
 * marked on. */
export function hunkSpan(h: Hunk, lineCount: number): Span {
  const added = addedSpan(h);
  if (added) return added;
  const line = hunkLine(h, lineCount);
  return { start: line, end: line };
}

/** The hunk that `line` is in, by its index; -1 when it is in none. */
export function hunkAt(hunks: readonly Hunk[], line: number, lineCount: number): number {
  return hunks.findIndex((h) => {
    const s = hunkSpan(h, lineCount);
    return line >= s.start && line <= s.end;
  });
}

/** The index of the hunk after `line` (`direction` 1) or before it (-1), by where each one
 * starts; -1 when there is none that way. From inside a hunk, the previous one is the hunk
 * before it. */
export function nextHunk(hunks: readonly Hunk[], line: number, lineCount: number, direction: 1 | -1): number {
  const starts = hunks.map((h) => hunkSpan(h, lineCount).start);
  if (direction === 1) {
    let best = -1;
    starts.forEach((s, i) => {
      if (s > line && (best < 0 || s < starts[best]!)) best = i;
    });
    return best;
  }
  const here = hunkAt(hunks, line, lineCount);
  const from = here >= 0 ? starts[here]! : line;
  let best = -1;
  starts.forEach((s, i) => {
    if (s < from && (best < 0 || s > starts[best]!)) best = i;
  });
  return best;
}

/** The lines a file's change adds and removes. */
export function counts(f: Pick<ChangedFile, "hunks">): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const h of f.hunks) {
    added += h.added.length;
    removed += h.removed.length;
  }
  return { added, removed };
}

/** Lines a hover shows of what a change removed, before it says how many more there are. */
const HOVER_LINES = 40;

/** What a change removed, for a hover: its lines, the first ones when there are many. */
export function removedText(lines: readonly string[], most = HOVER_LINES): { text: string; more: number } {
  const shown = lines.slice(0, most);
  return { text: shown.join("\n"), more: lines.length - shown.length };
}

/** A fence for `text` that no run of backticks in it closes. */
export function fence(text: string): string {
  let longest = 2;
  for (const run of text.match(/`+/g) ?? []) longest = Math.max(longest, run.length);
  return "`".repeat(longest + 1);
}

/** The title of the tab that reviews every unreviewed file of a session at once. */
export function reviewTitle(files: number): string {
  return `ognivo: ${files} ${files === 1 ? "file" : "files"} changed`;
}

/** Whether a tab's label is a review tab's, which a new review replaces. VS Code adds the
 * count of the tab's files in its own words: `ognivo: 3 files changed (3 files)`. */
export function isReviewTitle(label: string): boolean {
  return /^ognivo: \d+ files? changed(?: \([^)]*\))?$/.test(label);
}

/** The lines of a selection, from 1, as people count them: a selection that ends at the
 * start of a line ends on the line before. `start` and `end` are the editor's, from 0. */
export function selectedLines(start: { line: number }, end: { line: number; character: number }): { start: number; end: number } {
  const last = end.character === 0 && end.line > start.line ? end.line - 1 : end.line;
  return { start: start.line + 1, end: last + 1 };
}
