import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Thread } from "./model.ts";
import { timeline } from "./replay.ts";
import { type Row, ThreadStore } from "./store.ts";

const dir = join(import.meta.dirname, "../../../fixtures");
const threads: [string, Thread][] = readdirSync(dir)
  .filter((f) => f.endsWith(".json"))
  .map((f) => [f, fromAtif(JSON.parse(readFileSync(join(dir, f), "utf8")) as AtifTrace)]);

/** Flushes only when the test says so. */
function manual() {
  const pending: (() => void)[] = [];
  return {
    schedule: (flush: () => void) => void pending.push(flush),
    run: () => pending.splice(0).forEach((f) => f()),
  };
}

function rows(store: ThreadStore): Row[] {
  return Array.from({ length: store.length }, (_, i) => store.row(i)!);
}

describe("ThreadStore", () => {
  it("tells a row's listeners once a frame, however many deltas came", () => {
    const clock = manual();
    const store = new ThreadStore(
      {
        id: "t",
        title: "",
        project: "",
        profile: "",
        model: "",
        server: "",
        system: "",
        conversations: {},
        tools: [],
      },
      { schedule: clock.schedule },
    );
    let list = 0;
    store.subscribeList(() => list++);
    store.apply({ type: "turn_started", index: 1, user: "hi" });
    store.apply({ type: "item_started", item: { kind: "message", id: "m", text: "" } });
    clock.run();
    expect(list).toBe(1);

    let row = 0;
    store.subscribeRow(1, () => row++);
    for (const text of ["Hel", "lo", ", world"]) store.apply({ type: "item_delta", id: "m", text });
    expect(row).toBe(0);
    clock.run();
    expect(row).toBe(1);
    expect(list).toBe(1);
    expect(store.row(1)).toMatchObject({ item: { text: "Hello, world" } });
  });

  it("keeps a row's identity until the row changes", () => {
    const clock = manual();
    const [, thread] = threads[0]!;
    const store = ThreadStore.of(thread, { schedule: clock.schedule });
    const before = rows(store);
    const last = before.findLastIndex((r) => r.kind === "item");
    const id = (before[last] as Extract<Row, { kind: "item" }>).item.id;
    store.setOpen(id, true);
    store.setOpen(id, true);
    const after = rows(store);
    expect(after.filter((r, i) => r !== before[i])).toHaveLength(1);
  });
});

describe("calls being written", () => {
  const info = { id: "t", title: "", project: "", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] };
  const draft = (generation: number, field: string | undefined, text: string, index = 0) =>
    ({ type: "draft", generation, index, tool: "Write", field, text }) as const;
  const generation = (index: number, outcome: string) =>
    ({
      type: "generation",
      generation: { index, outcome, prompt: 0, cached: 0, completion: 0, promptMs: 0, generationMs: 0 },
    }) as const;

  it("end the reasoning before them, and grow field by field", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply({ type: "item_started", item: { kind: "reasoning", id: "r", generation: 1, text: "Hm." } });
    expect(store.row(1)).toMatchObject({ live: true });
    store.apply(draft(1, undefined, ""));
    store.apply(draft(1, "path", "a.rs"));
    store.apply(draft(1, "content", "fn a"));
    store.apply(draft(1, "content", "() {}"));
    expect(store.row(1)).toMatchObject({ kind: "item", live: false });
    expect(store.row(2)).toMatchObject({
      kind: "draft",
      state: "writing",
      draft: { fields: [["path", "a.rs"], ["content", "fn a() {}"]] },
    });
    // a second call of the generation has a row of its own
    store.apply(draft(1, "path", "b.rs", 1));
    expect(store.length).toBe(4);
  });

  it("that come to nothing stay as not run", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply(draft(1, "path", "a.rs"));
    store.apply(generation(1, "malformed"));
    expect(store.row(1)).toMatchObject({ kind: "draft", state: "void" });
    // a step the server's format check restarted: what it wrote is void, the next try new
    store.apply(draft(2, "path", "b.rs"));
    store.apply({ type: "item_started", item: { kind: "void", id: "v", generation: 2, why: "restarted", calls: [] } });
    expect(store.row(2)).toMatchObject({ kind: "draft", state: "void" });
    store.apply(draft(2, "path", "c.rs"));
    expect(store.row(4)).toMatchObject({ kind: "draft", state: "writing", draft: { fields: [["path", "c.rs"]] } });
    // and what waits to run when the turn ends never runs
    store.apply(generation(2, "calls"));
    expect(store.row(4)).toMatchObject({ state: "written" });
    store.apply({ type: "turn_completed", index: 1, end: "interrupted" });
    expect(store.row(4)).toMatchObject({ state: "void" });
  });
});

describe("the side panels' indexes", () => {
  it("know the files a session changed and the calls that waited for an approval", () => {
    const [, thread] = threads.find(([f]) => f === "gpt-oss.edit-refused.json")!;
    const store = ThreadStore.of(thread, { schedule: manual().schedule });
    const files = store.changedFiles();
    expect(files.map((f) => f.path)).toEqual(["src/lib.rs"]);
    expect(files[0]!.calls.map((c) => c.call?.name)).toEqual(["Edit"]);
    expect(store.refusedCalls().map((c) => c.call)).toMatchObject([
      { name: "Bash", command: "cargo test --quiet" },
    ]);
  });

  it("leave out edits that did not apply", () => {
    const [, thread] = threads.find(([f]) => f === "gemma.ambiguous-edit.json")!;
    const store = ThreadStore.of(thread, { schedule: manual().schedule });
    const edits = thread.turns.flatMap((t) => t.items).filter((i) => i.kind === "tool" && i.call?.name === "Edit");
    const applied = store.changedFiles().flatMap((f) => f.calls);
    expect(edits.length).toBeGreaterThan(applied.length);
    expect(applied.every((c) => c.result?.text.startsWith("[edit: applied"))).toBe(true);
  });
});

describe("timeline", () => {
  it.each(threads)("%s played back makes the same rows as the thread", (_, thread) => {
    const whole = ThreadStore.of(thread, { schedule: manual().schedule });
    const played = new ThreadStore(whole.info, { schedule: manual().schedule });
    const events = timeline(thread);
    for (const { event } of events) played.apply(event);
    expect(rows(played)).toEqual(rows(whole));
    expect(played.generations).toEqual(whole.generations);
    expect(played.changedFiles()).toEqual(whole.changedFiles());
    expect(played.refusedCalls()).toEqual(whole.refusedCalls());
    // and in order of time
    expect(events.every((e, i) => i === 0 || e.at >= events[i - 1]!.at)).toBe(true);
  });

  it("takes as long as the session's generations took, and its calls", () => {
    for (const [, thread] of threads) {
      const events = timeline(thread);
      const spent = thread.generations.reduce((n, g) => n + g.promptMs + g.generationMs, 0);
      expect(events.at(-1)!.at).toBeGreaterThanOrEqual(Math.floor(spent));
    }
  });
});
