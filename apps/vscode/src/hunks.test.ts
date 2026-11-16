import { describe, expect, it } from "vitest";

import {
  addedSpan,
  counts,
  fence,
  hunkAt,
  hunkLine,
  isReviewTitle,
  nextHunk,
  removedMark,
  removedText,
  reviewTitle,
  selectedLines,
} from "./hunks.ts";
import type { Hunk } from "./shared/protocol.ts";

const hunk = (old_start: number, new_start: number, removed: string[], added: string[]): Hunk => ({ old_start, new_start, removed, added });

describe("where a change is in the file as it is now", () => {
  // before: a b c d e f; after: a B c d e e2 f (diff.rs's own example), then g goes
  const replaced = hunk(2, 2, ["b"], ["B"]);
  const inserted = hunk(6, 6, [], ["e2"]);
  const deleted = hunk(7, 8, ["g"], []);

  it("marks the lines a hunk added", () => {
    expect(addedSpan(replaced)).toEqual({ start: 1, end: 1 });
    expect(addedSpan(hunk(3, 3, [], ["x", "y", "z"]))).toEqual({ start: 2, end: 4 });
    expect(addedSpan(deleted)).toBeUndefined();
  });

  it("marks removed lines on the line that follows them, and below the last one at the end", () => {
    expect(removedMark(replaced, 8)).toEqual({ line: 1, where: "above" });
    expect(removedMark(inserted, 8)).toBeUndefined();
    // the file's 7 lines, and the empty one the editor has after its final newline
    expect(removedMark(deleted, 8)).toEqual({ line: 7, where: "above" });
    // no final newline: nothing follows the last line
    expect(removedMark(deleted, 7)).toEqual({ line: 6, where: "below" });
    // everything removed from a file now empty
    expect(removedMark(hunk(1, 1, ["a", "b"], []), 1)).toEqual({ line: 0, where: "above" });
  });

  it("puts a lens above the first added line, or where the lines went", () => {
    expect(hunkLine(replaced, 8)).toBe(1);
    expect(hunkLine(inserted, 8)).toBe(5);
    expect(hunkLine(deleted, 7)).toBe(6);
  });

  it("finds the hunk a line is in", () => {
    const hunks = [replaced, hunk(4, 4, [], ["x", "y"]), deleted];
    expect(hunkAt(hunks, 0, 10)).toBe(-1);
    expect(hunkAt(hunks, 1, 10)).toBe(0);
    expect(hunkAt(hunks, 3, 10)).toBe(1);
    expect(hunkAt(hunks, 4, 10)).toBe(1);
    expect(hunkAt(hunks, 5, 10)).toBe(-1);
    expect(hunkAt(hunks, 7, 10)).toBe(2);
  });

  it("goes to the next and the previous hunk", () => {
    const hunks = [hunk(8, 9, [], ["late"]), replaced, hunk(4, 4, [], ["x", "y"])];
    // the order of the list does not matter, only where each one starts
    expect(nextHunk(hunks, 0, 20, 1)).toBe(1);
    expect(nextHunk(hunks, 1, 20, 1)).toBe(2);
    expect(nextHunk(hunks, 4, 20, 1)).toBe(0);
    expect(nextHunk(hunks, 8, 20, 1)).toBe(-1);
    expect(nextHunk(hunks, 19, 20, -1)).toBe(0);
    expect(nextHunk(hunks, 8, 20, -1)).toBe(2);
    // from inside a hunk, back is the hunk before it, not its own start
    expect(nextHunk(hunks, 4, 20, -1)).toBe(1);
    expect(nextHunk(hunks, 1, 20, -1)).toBe(-1);
  });
});

describe("the words of a review", () => {
  it("counts a file's lines", () => {
    expect(counts({ hunks: [hunk(2, 2, ["b"], ["B", "B2"]), hunk(6, 7, ["f"], [])] })).toEqual({ added: 2, removed: 2 });
    expect(counts({ hunks: [] })).toEqual({ added: 0, removed: 0 });
  });

  it("shows the first removed lines and how many more there are", () => {
    expect(removedText(["a", "b", "c"], 2)).toEqual({ text: "a\nb", more: 1 });
    expect(removedText(["a"])).toEqual({ text: "a", more: 0 });
  });

  it("fences text that has backticks of its own", () => {
    expect(fence("plain")).toBe("```");
    expect(fence("a ``` b")).toBe("````");
  });

  it("names the review tab and knows it again", () => {
    expect(reviewTitle(1)).toBe("ognivo: 1 file changed");
    expect(reviewTitle(3)).toBe("ognivo: 3 files changed");
    expect(isReviewTitle(reviewTitle(12))).toBe(true);
    // as VS Code 1.146 labels the tab
    expect(isReviewTitle("ognivo: 1 file changed (1 file)")).toBe(true);
    expect(isReviewTitle("ognivo: 3 files changed (3 файла)")).toBe(true);
    expect(isReviewTitle("parse.rs (before ognivo ↔ now)")).toBe(false);
    expect(isReviewTitle("ognivo: 3 files changed and more")).toBe(false);
  });

  it("counts a selection's lines as people do", () => {
    expect(selectedLines({ line: 3 }, { line: 7, character: 5 })).toEqual({ start: 4, end: 8 });
    // to the start of line 9: the selection ends on line 8
    expect(selectedLines({ line: 3 }, { line: 8, character: 0 })).toEqual({ start: 4, end: 8 });
    expect(selectedLines({ line: 3 }, { line: 3, character: 0 })).toEqual({ start: 4, end: 4 });
  });
});
