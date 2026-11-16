import { describe, expect, it } from "vitest";

import type { Progress } from "../../shared/protocol.ts";
import { clock, draftText, explored, lineDelta, refusalReason, saying, short, speed } from "./format.ts";
import type { ToolItem } from "./model.ts";
import type { Live } from "./store.ts";

describe("lineDelta", () => {
  it("counts the lines a change adds and removes, not the lines it keeps", () => {
    expect(lineDelta("a\nb\nc", "a\nB\nc")).toEqual({ added: 1, removed: 1 });
    expect(lineDelta("fn x() {\n    2\n}", "")).toEqual({ added: 0, removed: 3 });
    expect(lineDelta("", "one\ntwo\n")).toEqual({ added: 3, removed: 0 });
    expect(lineDelta("same", "same")).toEqual({ added: 0, removed: 0 });
  });
});

describe("refusalReason", () => {
  it("reads why a call needed an approval, or the rule that denied it", () => {
    expect(
      refusalReason(
        "[refused] cargo test needs the user's approval (it runs project code while there are unreviewed changes), and this run has no user. Do not retry it",
      ),
    ).toBe("it runs project code while there are unreviewed changes");
    expect(
      refusalReason(
        "[refused] rm -f made.txt is denied by the rule Bash(rm:*) in .ognivo/config.toml. Do not retry it; do the task another way",
      ),
    ).toBe("denied by the rule Bash(rm:*) in .ognivo/config.toml");
  });
});

describe("numbers", () => {
  it("say tokens in few figures, time as a clock, and speed", () => {
    expect([418, 1234, 12_400, 31_000, 99_960, 128_000, 1_250_000].map(short)).toEqual(["418", "1.2K", "12.4K", "31K", "100K", "128K", "1.3M"]);
    expect([0, 7_900, 134_000, 3_723_000].map(clock)).toEqual(["0:00", "0:07", "2:14", "1:02:03"]);
    expect([speed(34.4), speed(7.25)]).toEqual(["34 tok/s", "7.3 tok/s"]);
  });
});

const call = (id: string, c: ToolItem["call"], result = true): ToolItem => ({
  kind: "tool",
  id,
  shown: c?.name.toLowerCase() ?? "x",
  raw: {},
  call: c,
  ...(result ? { result: { outcome: "ok", text: "" } } : {}),
});

describe("an exploration", () => {
  it("counts the files it read once each, its searches, servers and skills", () => {
    const calls = [
      call("1", { name: "Read", path: "src/a.rs" }),
      call("2", { name: "Grep", pattern: "x" }),
      call("3", { name: "Read", path: "src/a.rs", offset: 40 }),
      call("4", { name: "Read", path: "src/b.rs" }),
      call("5", { name: "Glob", pattern: "**/*.rs" }),
    ];
    expect(explored(calls)).toBe("2 files, 2 searches");
    expect(explored([call("1", { name: "Mcp", server: "exa", tool: "web_search", arguments: "{}" }), call("2", { name: "Skill", skill: "pdf" })])).toBe("1 MCP call, 1 skill");
  });
});

describe("the line at the end of a running turn", () => {
  const idle: Live = { running: true, since: 1_000, drafts: [], calls: [], asking: false, compacting: false };
  const progress = (phase: Progress["phase"], over: Partial<Progress> = {}): Progress => ({
    generation: 2,
    phase,
    prompt_total: 13_000,
    prompt_cached: 600,
    prompt_done: 8_412,
    generated: 418,
    elapsed_ms: 900,
    tokens_per_second: 34.2,
    ...over,
  });
  const say = (live: Partial<Live>, asking = false) => saying({ ...idle, ...live }, { asking, now: 13_000, project: "/p" });
  const writing = (tool: string, fields: [string, string][]) => ({ state: "writing" as const, draft: { generation: 2, index: 0, tool, fields } });

  it("says each phase of the model's work, with its facts and the turn's time", () => {
    expect(say({ progress: progress("waiting") })).toEqual({ say: "Waiting for the model", tone: "work", facts: ["0:12"] });
    // of what the cache did not have
    expect(say({ progress: progress("prompt") })).toMatchObject({ say: "Reading the conversation", facts: ["63% of 12.4K", "0:12"] });
    expect(say({ progress: progress("prompt", { prompt_cached: 13_000 }) }).facts).toEqual(["13K from the cache", "0:12"]);
    expect(say({ progress: progress("prompt", { prompt_total: null }) }).facts).toEqual(["0:12"]);
    expect(say({ progress: progress("reasoning") })).toMatchObject({ say: "Thinking", facts: ["418 tokens", "34 tok/s", "0:12"] });
    expect(say({ progress: progress("writing", { tokens_per_second: null }), streaming: "message" })).toMatchObject({ say: "Writing the reply", facts: ["418 tokens", "0:12"] });
  });

  it("names the call being written, and gives it for its lines to show", () => {
    const file = writing("Write", [["path", "/p/tests/parse.rs"], ["content", "fn a() {}\n"]]);
    expect(say({ progress: progress("writing"), drafts: [file] })).toMatchObject({ say: "Writing tests/parse.rs", writing: file.draft });
    expect(say({ drafts: [writing("Bash", [["command", "cargo te"]])] }).say).toBe("Writing a command");
    // a patch's file once its line is whole
    expect(say({ drafts: [writing("Edit", [["path", "src/parse.rs"], ["old", "x"]])] }).say).toBe("Writing src/parse.rs");
    expect(say({ drafts: [writing("ApplyPatch", [["patch", "*** Begin Patch\n*** Update File: src/a.rs\n@@"]])] }).say).toBe("Writing src/a.rs");
    expect(say({ drafts: [writing("ApplyPatch", [["patch", "*** Begin Patch\n*** Update File: src/a"]])] }).say).toBe("Writing a patch");
    // what waits to run says nothing of itself
    expect(say({ drafts: [{ ...file, state: "written" }] }).say).toBe("Working");
  });

  it("puts a question first, then what runs", () => {
    const bash = call("b", { name: "Bash", command: "cargo test\n--quiet" }, false);
    expect(say({ calls: [bash], progress: progress("reasoning") })).toMatchObject({ say: "Running cargo test", facts: ["0:12"] });
    expect(say({ calls: [bash] }, true)).toMatchObject({ say: "Waiting for your answer", tone: "ask" });
    expect(say({ asking: true }).tone).toBe("ask");
    expect(say({ calls: [call("t", { name: "Task", tasks: [{ title: "a", write: false, files: [] }, { title: "b", write: false, files: [] }] }, false)] }).say).toBe("Waiting for 2 subagents");
    expect(say({ compacting: true, progress: progress("writing") })).toMatchObject({ say: "Compacting the conversation", facts: ["418 tokens", "34 tok/s", "0:12"] });
    expect(say({ since: undefined })).toEqual({ say: "Working", tone: "work", facts: [] });
  });

  it("shows the text of a file, a patch or a command being written, and nothing of a read", () => {
    expect(draftText({ generation: 1, index: 0, tool: "Edit", fields: [["path", "a.rs"], ["old", "x"], ["new", "y\nz"]] })).toEqual({ text: "y\nz", kind: "file", path: "a.rs" });
    expect(draftText({ generation: 1, index: 0, tool: "Bash", fields: [["command", "ls"]] })).toEqual({ text: "ls", kind: "command", path: undefined });
    expect(draftText({ generation: 1, index: 0, tool: "Read", fields: [["path", "a.rs"]] })).toBeUndefined();
    expect(draftText({ generation: 1, index: 0, tool: "Write", fields: [["path", "a.rs"]] })).toBeUndefined();
  });
});
