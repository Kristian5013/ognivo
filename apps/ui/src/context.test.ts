import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import { estimateTokens, mentionAt, sessionFiles } from "./context.ts";

describe("sessionFiles", () => {
  it("lists the files a session touched, each with what its last whole read cost", () => {
    const path = join(import.meta.dirname, "../fixtures/gpt-oss.edit-refused.json");
    const t = fromAtif(JSON.parse(readFileSync(path, "utf8")) as AtifTrace);
    const files = sessionFiles(
      t.turns.flatMap((turn) => turn.items),
      t.project,
    );
    // read whole at 11 lines and ~30 tokens, then whole again after the edit at 8 and ~20
    expect(files.get("src/lib.rs")).toBe(20);
    expect([...files.keys()].every((p) => !p.startsWith("/home/dev/demo/"))).toBe(true);
  });
});

describe("mentionAt", () => {
  it("finds the mention being written before the caret, not an address", () => {
    expect(mentionAt("look at @src/l", 14)).toEqual({ start: 8, query: "src/l" });
    expect(mentionAt("@", 1)).toEqual({ start: 0, query: "" });
    expect(mentionAt("mail me at a@b", 14)).toBeUndefined();
    expect(mentionAt("@src/lib.rs and", 15)).toBeUndefined();
  });
});

describe("estimateTokens", () => {
  it("counts about four bytes a token", () => {
    expect(estimateTokens("")).toBe(0);
    expect(estimateTokens("abcd")).toBe(1);
    expect(estimateTokens("привет")).toBe(3); // 12 bytes
  });
});
