import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif, toCall, toResult } from "./atif.ts";

describe("toCall", () => {
  it("reads the arguments of every family as one call", () => {
    const edit = { name: "Edit", path: "src/a.rs", old: "x", new: "y", replaceAll: false };
    expect(toCall("edit", { path: "src/a.rs", old_string: "x", new_string: "y" })).toEqual(edit);
    expect(
      toCall("edit", { file: "src/a.rs", find: "x", replace: "y", replace_all: "false" }),
    ).toEqual(edit);
    expect(toCall("Edit", { file_path: "src/a.rs", old_string: "x", new_string: "y" })).toEqual(
      edit,
    );
    expect(toCall("write", { file: "a.txt", text: "hi" })).toEqual({
      name: "Write",
      path: "a.txt",
      content: "hi",
    });
    expect(toCall("Read", { file_path: "a.txt", offset: "5", limit: 3 })).toEqual({
      name: "Read",
      path: "a.txt",
      offset: 5,
      limit: 3,
    });
  });

  it("takes each family's time limit in milliseconds", () => {
    expect(toCall("shell", { command: "ls", timeout_ms: 5000 })).toMatchObject({ timeoutMs: 5000 });
    expect(toCall("Bash", { command: "ls", timeout: "5000" })).toMatchObject({ timeoutMs: 5000 });
    expect(toCall("bash", { command: "ls", timeout_s: 7 })).toMatchObject({ timeoutMs: 7000 });
  });

  it("treats an empty file filter as none, as ognivo does", () => {
    expect(toCall("grep", { pattern: "calc_total", glob: "" })).toEqual({
      name: "Grep",
      pattern: "calc_total",
      path: undefined,
      glob: undefined,
    });
  });

  it("makes no call of what ognivo would not run", () => {
    expect(toCall("rm", { path: "x" })).toBeUndefined();
    expect(toCall("edit", { path: "x", old_string: "a" })).toBeUndefined();
    expect(toCall("Write", "not an object")).toBeUndefined();
  });
});

describe("toResult", () => {
  it("reads refusals, errors and how a command ended", () => {
    expect(toResult("[refused] rm -rf x needs the user's approval")).toMatchObject({
      outcome: "refused",
    });
    expect(toResult("[error] src/a.rs: no such file")).toMatchObject({ outcome: "error" });
    expect(toResult("[shell: exit 101, 2.5 s]\nerror[E0425]")).toMatchObject({
      outcome: "ok",
      exit: 101,
      seconds: 2.5,
    });
    // killed on its time limit: no exit of its own
    const killed = toResult("[shell: timed out after 60 s and was killed; exit 124]");
    expect(killed.seconds).toBe(60);
    expect(killed.exit).toBeUndefined();
  });
});

describe("fromAtif", () => {
  const trace: AtifTrace = {
    schema_version: "ATIF-v1.7",
    session_id: "7",
    agent: { model_name: "gpt-oss-20b" },
    extra: {
      ognivo: {
        profile: "gpt-oss-20b",
        title: "Fix add",
        turns: [{ turn: 1, end: "step_limit", why: "the step limit" }],
      },
    },
    steps: [
      { step_id: 1, source: "system", message: "You are ognivo.", extra: { reason: "start" } },
      { step_id: 2, source: "user", message: "Fix add" },
      {
        step_id: 3,
        source: "agent",
        message: "",
        reasoning_content: "Read it first.",
        tool_calls: [{ tool_call_id: "c1", function_name: "read", arguments: { path: "a.rs" } }],
        observation: { results: [{ source_call_id: "c1", content: "[read: a.rs — lines 1-2]\n1│x" }] },
        metrics: { prompt_tokens: 900, cached_tokens: 100, completion_tokens: 40, extra: { generation_ms: 400 } },
        extra: { outcome: "calls" },
      },
      {
        step_id: 4,
        source: "agent",
        extra: {
          outcome: "malformed",
          rejected_calls: [{ name: "write", arguments: '{"path":"a.rs"' }],
        },
      },
      { step_id: 5, source: "system", message: "[ognivo] Your last tool call could not be used." },
      { step_id: 6, source: "agent", message: "", reasoning_content: "Hmm", extra: { outcome: "answer" } },
    ],
  };

  it("makes turns of the user's messages and items of the steps", () => {
    const t = fromAtif(trace);
    expect(t).toMatchObject({ id: "7", title: "Fix add", model: "gpt-oss-20b", system: "You are ognivo." });
    expect(t.generations.map((g) => g.outcome)).toEqual(["calls", "malformed", "answer"]);
    expect(t.generations[0]).toMatchObject({ prompt: 900, cached: 100, generationMs: 400 });
    expect(t.turns).toHaveLength(1);
    const [turn] = t.turns;
    expect(turn).toMatchObject({ index: 1, user: "Fix add", end: "step_limit", why: "the step limit" });
    expect(turn?.items.map((i) => i.kind)).toEqual([
      "reasoning",
      "tool",
      "void",
      "notice",
      "reasoning",
      "void",
    ]);
    expect(turn?.items[1]).toMatchObject({
      call: { name: "Read", path: "a.rs" },
      result: { outcome: "ok" },
      generation: 0,
    });
    expect(turn?.items[2]).toMatchObject({ why: "malformed", calls: [{ name: "write" }] });
    expect(turn?.items[5]).toMatchObject({ why: "empty", generation: 2 });
  });
});

describe("the fixtures", () => {
  const dir = join(import.meta.dirname, "../../../fixtures");
  const names = readdirSync(dir).filter((f) => f.endsWith(".json"));

  it("are there", () => {
    expect(names.length).toBeGreaterThan(10);
  });

  it.each(names)("%s reads as a thread whose calls ognivo knows", (name) => {
    const trace = JSON.parse(readFileSync(join(dir, name), "utf8")) as AtifTrace;
    const t = fromAtif(trace);
    const agentSteps = trace.steps.filter((s) => s.source === "agent").length;
    expect(t.generations).toHaveLength(agentSteps);
    expect(t.turns.length).toBeGreaterThan(0);
    for (const turn of t.turns) {
      expect(turn.end).toBeDefined();
      for (const item of turn.items) {
        if (item.kind === "tool") {
          // every recorded call ran or was refused, and its arguments made a call
          expect(item.result, `${item.shown} ${JSON.stringify(item.raw)}`).toBeDefined();
          expect(item.call, `${item.shown} ${JSON.stringify(item.raw)}`).toBeDefined();
        }
      }
    }
  });
});

describe("the fixtures' requests", () => {
  const dir = join(import.meta.dirname, "../../../fixtures");
  const names = readdirSync(dir).filter((f) => f.endsWith(".json"));

  it.each(names)("%s: every request is in the record, and a usable answer follows its own", (name) => {
    const t = fromAtif(JSON.parse(readFileSync(join(dir, name), "utf8")) as AtifTrace);
    const items = t.turns.flatMap((turn) => turn.items);
    let previous = 0;
    for (const g of t.generations) {
      expect(g.request, `generation ${g.index}`).toBeDefined();
      const conversation = t.conversations[g.request!.conversation]!;
      expect(g.request!.messages).toBeGreaterThanOrEqual(previous);
      expect(g.request!.messages).toBeLessThanOrEqual(conversation.length);
      previous = g.request!.messages;
      const made = items.filter((i) => i.generation === g.index);
      // what ran nothing is not in the history; what did is the message after the request
      if (made.some((i) => i.kind === "void")) continue;
      const answer = conversation[g.request!.messages];
      expect(answer?.role, `generation ${g.index}`).toBe("assistant");
      const calls = made.filter((i) => i.kind === "tool").map((i) => (i.kind === "tool" ? i.shown : ""));
      expect((answer?.tool_calls ?? []).map((c) => c.name)).toEqual(calls);
    }
  });
});
