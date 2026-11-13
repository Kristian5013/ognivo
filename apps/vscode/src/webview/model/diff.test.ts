import { describe, expect, it } from "vitest";

import { diffsOf, fromHunks, fromPatch, lineDiff, splitPath } from "./diff.ts";
import type { ToolItem } from "./model.ts";

const tool = (call: ToolItem["call"], changes?: ToolItem["changes"]): ToolItem => ({
  kind: "tool",
  id: "1",
  shown: "edit",
  raw: {},
  call,
  changes,
});

describe("lineDiff", () => {
  it("keeps the shared lines as context and takes out before it puts in", () => {
    expect(lineDiff("a\nb\nc\n", "a\nB\nc\nd\n")).toEqual([
      { kind: "ctx", text: "a" },
      { kind: "del", text: "b" },
      { kind: "add", text: "B" },
      { kind: "ctx", text: "c" },
      { kind: "add", text: "d" },
    ]);
  });

  it("gives a block of new lines in place of a block of old ones", () => {
    const kinds = lineDiff("x\n1\n2\ny", "x\n3\n4\ny").map((l) => l.kind);
    expect(kinds).toEqual(["ctx", "del", "del", "add", "add", "ctx"]);
  });
});

describe("the files a call changes", () => {
  it("shows an edit's own lines, at the line the core found", () => {
    const [d] = diffsOf(
      tool({ name: "Edit", path: "src/main.rs", old: "  x: u8,\n}", new: "  x: u8,\n  y: u8,\n}", replaceAll: false }, [
        { path: "src/main.rs", created: false, hunks: [{ oldStart: 12, newStart: 12, removed: [], added: ["  y: u8,"] }] },
      ]),
    );
    expect(d).toMatchObject({ path: "src/main.rs", added: 1, removed: 0, at: 12 });
    expect(d!.lines.map((l) => l.kind)).toEqual(["ctx", "add", "ctx"]);
  });

  it("counts a replacement everywhere as often as the core made it", () => {
    const hunks = [1, 4, 5].map((n) => ({ oldStart: n, newStart: n, removed: ["calc_total"], added: ["compute_total"] }));
    const [d] = diffsOf(tool({ name: "Edit", path: "m.rs", old: "calc_total", new: "compute_total", replaceAll: true }, [{ path: "m.rs", created: false, hunks }]));
    expect(d).toMatchObject({ added: 3, removed: 3 });
  });

  it("shows a new file whole, and an overwritten one by its hunks", () => {
    const [created] = diffsOf(tool({ name: "Write", path: "a.txt", content: "one\ntwo\n" }));
    expect(created).toMatchObject({ created: true, added: 2, removed: 0 });
    const [over] = diffsOf(
      tool({ name: "Write", path: "a.txt", content: "one\nTWO\n" }, [
        { path: "a.txt", created: false, hunks: [{ oldStart: 2, newStart: 2, removed: ["two"], added: ["TWO"] }] },
      ]),
    );
    expect(over).toMatchObject({ created: false, added: 1, removed: 1, at: 2 });
  });

  it("reads a patch of gpt-oss file by file", () => {
    const files = fromPatch(
      "*** Begin Patch\n*** Update File: src/a.rs\n@@ fn main\n fn main() {\n-    hi();\n+    hello();\n }\n@@\n-x\n+y\n*** Add File: b.txt\n+new\n*** Delete File: c.txt\n*** End Patch",
    );
    expect(files.map((f) => [f.path, f.created, f.deleted, f.added, f.removed])).toEqual([
      ["src/a.rs", false, false, 2, 2],
      ["b.txt", true, false, 1, 0],
      ["c.txt", false, true, 0, 0],
    ]);
    expect(files[0]!.lines.map((l) => l.kind)).toEqual(["ctx", "del", "add", "ctx", "gap", "del", "add"]);
  });

  it("puts a gap between the core's hunks", () => {
    const d = fromHunks({
      path: "x",
      created: false,
      hunks: [
        { oldStart: 1, newStart: 1, removed: ["a"], added: ["b"] },
        { oldStart: 9, newStart: 9, removed: [], added: ["c"] },
      ],
    });
    expect(d.lines.map((l) => l.kind)).toEqual(["del", "add", "gap", "add"]);
    expect(d.at).toBe(1);
  });
});

describe("splitPath", () => {
  it("parts a file's name from its directory", () => {
    expect(splitPath("src/model/diff.ts")).toEqual({ name: "diff.ts", dir: "src/model" });
    expect(splitPath("C:\\proj\\a.rs")).toEqual({ name: "a.rs", dir: "C:/proj" });
    expect(splitPath("README.md")).toEqual({ name: "README.md", dir: "" });
  });
});
