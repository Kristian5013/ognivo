import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import {
  autoChip,
  byThinking,
  chipKey,
  compaction,
  contextChoices,
  contextSize,
  editorChips,
  estimateTokens,
  failureChip,
  levelHint,
  mentionAt,
  mentionChoices,
  nativeFor,
  profileLevels,
  sessionFiles,
  startLevel,
  windowChoice,
} from "./context.ts";
import type { EditorState } from "../../shared/messages.ts";
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

  it("takes a path's letters of any script", () => {
    expect(mentionAt("see @docs/заметки", 17)).toEqual({ start: 4, query: "docs/заметки" });
  });
});

describe("mentionChoices", () => {
  it("offers the session's files first, then the workspace's, each once", () => {
    const session = ["src/parse.rs", "src/main.rs", "Cargo.toml"];
    const found = ["src/parse.rs", "tests/parse.rs", "benches/parse.rs"];
    expect(mentionChoices("parse", session, found)).toEqual(["src/parse.rs", "tests/parse.rs", "benches/parse.rs"]);
    expect(mentionChoices("", session, [], 2)).toEqual(["src/parse.rs", "src/main.rs"]);
    expect(mentionChoices("CARGO", session, [])).toEqual(["Cargo.toml"]);
  });
});

describe("the composer's own chips", () => {
  const editor = (selection: EditorState["selection"] = null): EditorState => ({
    path: "src/parse.rs",
    abs: "/home/u/tally/src/parse.rs",
    language: "rust",
    selection,
  });
  const lines = { start: 4, end: 8, text: "fn parse_amount() {}" };

  it("are the editor's file, and its selection when something is selected", () => {
    expect(editorChips(null)).toEqual({});
    expect(editorChips(editor()).selection).toBeUndefined();
    expect(editorChips(editor({ start: 3, end: 3, text: "  \n" })).selection).toBeUndefined();
    expect(editorChips(editor(lines)).selection).toEqual({
      kind: "selection",
      path: "src/parse.rs",
      abs: "/home/u/tally/src/parse.rs",
      start: 4,
      end: 8,
      language: "rust",
      text: "fn parse_amount() {}",
    });
  });

  it("show the selection, else the file, and nothing the user took away or a chip names", () => {
    const sel = editorChips(editor(lines)).selection!;
    const file = editorChips(editor()).file!;
    expect(autoChip(editor(lines), new Set(), [])).toEqual(sel);
    expect(autoChip(editor(lines), new Set([chipKey(sel)]), [])).toEqual(file);
    expect(autoChip(editor(lines), new Set([chipKey(sel), chipKey(file)]), [])).toBeUndefined();
    expect(autoChip(null, new Set(), [])).toBeUndefined();
    // Ctrl+I's selection of the same file is the message's already
    expect(autoChip(editor(), new Set(), [{ ...sel, start: 10, end: 12 }])).toBeUndefined();
    expect(autoChip(editor(), new Set(), [failureChip({ id: "1", command: "cargo run", exitCode: 101, cwd: null, output: "", at: 0 })])).toEqual(file);
  });

  it("tell the same chip from another", () => {
    const sel = editorChips(editor(lines)).selection!;
    expect(chipKey(sel)).toBe(chipKey({ ...sel, text: "other" }));
    expect(chipKey(sel)).not.toBe(chipKey({ ...sel, end: 9 }));
    expect(chipKey({ kind: "file", path: "a", abs: "C:\\P\\a.rs" })).toBe(chipKey({ kind: "file", path: "a", abs: "c:/p/a.rs" }));
  });

  it("take a failed command with what it wrote", () => {
    expect(failureChip({ id: "7", command: "cargo run -- x.csv", exitCode: 101, cwd: "/home/u/tally", output: "panicked", at: 5 })).toEqual({
      kind: "terminal",
      command: "cargo run -- x.csv",
      exitCode: 101,
      cwd: "/home/u/tally",
      output: "panicked",
    });
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

describe("reasoning levels", () => {
  it("go from the least thinking to the most, whatever order a profile lists them in", () => {
    expect(byThinking(profiles["qwen3.8-27b"].reasoning.levels)).toEqual(["off", "low", "medium", "xhigh"]);
    expect(byThinking(profiles["gemma-4-31b"].reasoning.levels)).toEqual(["off", "on"]);
    expect(byThinking(["high", "turbo", "low"])).toEqual(["low", "high", "turbo"]);
  });

  it("each have a word on what they do", () => {
    expect(levelHint("gpt-oss-20b", "high")).toBe("The level its best agent scores were measured at.");
    expect(levelHint("gemma-4-31b", "off")).toBe("No thinking. The fastest.");
    expect(levelHint("gemma-4-31b", "on")).toBe("Thinks before each step, as its model card measured it.");
  });

  it("are a profile's own, and a new session asks only for one its model's profile has", () => {
    expect(profileLevels("gpt-oss-20b")).toEqual(["low", "medium", "high"]);
    expect(profileLevels("no-such-profile")).toBeUndefined();
    expect(profileLevels("")).toBeUndefined();
    expect(profileLevels(null)).toBeUndefined();
    // the level chosen for Qwen, then gpt-oss picked: its default, as the popover shows it
    expect(startLevel("xhigh", profileLevels("gpt-oss-20b"))).toBeNull();
    expect(startLevel("xhigh", profileLevels("qwen3.8-27b"))).toBe("xhigh");
    expect(startLevel("", profileLevels("qwen3.8-27b"))).toBeNull();
    // levels nobody knows yet (no answer of the server): the core says whether it has it
    expect(startLevel("xhigh", undefined)).toBe("xhigh");
    expect(startLevel("xhigh", [])).toBe("xhigh");
  });
});

describe("the Native path", () => {
  it("takes a session where it reads its model's format: the profile's, else the one the server runs", () => {
    expect(nativeFor(true, "gpt-oss-20b")).toBe(true);
    expect(nativeFor(true, "qwen3.8-27b")).toBe(true);
    expect(nativeFor(true, "gemma-4-31b")).toBe(false);
    expect(nativeFor(false, "gpt-oss-20b")).toBe(false);
    // no profile chosen: the model the server runs, as its detection says; a Gemma 4 model
    // of the router goes on the Generic path, as the settings show it
    expect(nativeFor(true, "", false)).toBe(false);
    expect(nativeFor(true, "", true)).toBe(true);
    expect(nativeFor(false, "", true)).toBe(false);
    // a profile chosen goes by its own format, whatever the server runs
    expect(nativeFor(true, "gemma-4-31b", true)).toBe(false);
    expect(nativeFor(true, "qwen3.8-27b", false)).toBe(true);
    // nothing known of the model: as the user wants it, and the core says
    expect(nativeFor(true, null)).toBe(true);
    expect(nativeFor(true, "no-such-profile")).toBe(true);
    expect(nativeFor(true, "no-such-profile", false)).toBe(false);
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
