import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import {
  compaction,
  contextChoices,
  contextSize,
  estimateTokens,
  mentionAt,
  sessionFiles,
  windowChoice,
} from "./context.ts";
import profiles from "../../shared/profiles.json";

describe("sessionFiles", () => {
  it("lists the files a session touched, each with what its last whole read cost", () => {
    const path = join(import.meta.dirname, "../../../fixtures/gpt-oss.edit-refused.json");
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

describe("compaction", () => {
  const gemma = profiles["gemma-4-26b-a4b"].context;
  it("works with the server's window, and compacts at the profile's share of it", () => {
    // the owner's llama-server: 262,144 a slot; the core says 170,393 (compact.rs tests)
    expect(compaction(gemma, undefined, 262_144)).toEqual({ working: 262_144, compactsAt: 170_393 });
  });
  it("works with a chosen context, and a window too small to compact in is none", () => {
    expect(compaction(gemma, 32_768, 262_144).working).toBe(32_768);
    expect(compaction(gemma, undefined, 4096).working).toBe(65_536);
    expect(compaction(gemma, undefined, undefined).working).toBe(65_536);
  });
  it("never goes past what the model works with, nor so near the window that a step cannot generate", () => {
    const gptOss = profiles["gpt-oss-20b"].context;
    expect(compaction(gptOss, undefined, 262_144).working).toBe(gptOss.ceiling);
    // 32K: 16,384 of output and the summary request leave 15,872
    expect(compaction(gemma, undefined, 32_768)).toEqual({ working: 32_768, compactsAt: 15_872 });
  });
});

describe("contextChoices", () => {
  it("offers the sizes the model and the server hold, and the profile's own", () => {
    const gemma = profiles["gemma-4-26b-a4b"].context;
    expect(contextChoices(gemma, 131_072).map(contextSize)).toEqual(["16K", "32K", "64K", "96K", "128K"]);
    expect(contextChoices(gemma, undefined).at(-1)).toBe(262_144);
    // gpt-oss stops at its ceiling, 128K, whatever the server holds; Qwen3.8 goes to 256K
    expect(contextChoices(profiles["gpt-oss-20b"].context, 262_144).slice(-2).map(contextSize)).toEqual(["96K", "128K"]);
    expect(contextChoices(profiles["qwen3.8-27b"].context, 262_144).at(-1)).toBe(262_144);
  });
});

describe("windowChoice", () => {
  it("names the window, the model's ceiling under a larger one, or the profile's context where the window is too small", () => {
    const gemma = profiles["gemma-4-26b-a4b"].context;
    expect(windowChoice(gemma, 262_144)).toBe("the server's window: 256K");
    expect(windowChoice(gemma, 4096)).toBe("the profile's 64K: the server's 4K is too small to compact in");
    // gpt-oss stops at 128K: the server's 256K is said, not named by what it is cut to
    const gptOss = profiles["gpt-oss-20b"].context;
    expect(windowChoice(gptOss, 262_144)).toBe("128K of the server's 256K: the most the model works with");
    expect(windowChoice(gptOss, 131_072)).toBe("the server's window: 128K");
    expect(windowChoice(gptOss, 65_536)).toBe("the server's window: 64K");
    expect(windowChoice(profiles["qwen3.8-27b"].context, 262_144)).toBe("the server's window: 256K");
    expect(windowChoice(undefined, undefined)).toBe("the server's window");
  });
});
