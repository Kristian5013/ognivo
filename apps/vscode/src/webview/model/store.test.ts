import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Item, Thread, ToolItem } from "./model.ts";
import { timeline } from "./replay.ts";
import type { Progress } from "../../shared/protocol.ts";
import { type Row, ThreadStore, type TurnEnd } from "./store.ts";

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

const info = { id: "t", title: "", project: "/p", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] };
const generation = (index: number, outcome: string, turn?: number) =>
  ({
    type: "generation",
    generation: { index, outcome, prompt: 0, cached: 0, completion: 0, promptMs: 0, generationMs: 0 },
    turn,
  }) as const;
const read = (id: string, gen: number, path: string, done = true): ToolItem => ({
  kind: "tool",
  id,
  generation: gen,
  shown: "read",
  raw: {},
  call: { name: "Read", path },
  ...(done ? { result: { outcome: "ok", text: `[read: ${path} — lines 1-3 of 3 lines]` } } : {}),
});
const grep = (id: string, gen: number): ToolItem => ({
  kind: "tool",
  id,
  generation: gen,
  shown: "grep",
  raw: {},
  call: { name: "Grep", pattern: "fn main" },
  result: { outcome: "ok", text: "[grep: 1 match in 1 file for /fn main/ under .]\nsrc/main.rs:1:fn main() {" },
});
const edit = (id: string, gen: number, path: string): ToolItem => ({
  kind: "tool",
  id,
  generation: gen,
  shown: "edit",
  raw: {},
  call: { name: "Edit", path, old: "a", new: "b", replaceAll: false },
  result: { outcome: "ok", text: `[edit: applied exact] ${path}` },
});
const thought = (id: string, gen: number) => ({ kind: "reasoning", id, generation: gen, text: "Hm." }) as const;
const progress = (gen: number, phase: Progress["phase"], generated = 0): Progress => ({
  generation: gen,
  phase,
  prompt_total: 1000,
  prompt_cached: 600,
  prompt_done: 800,
  generated,
  elapsed_ms: 100,
  tokens_per_second: generated ? 30 : null,
});

/** Starts and ends each item, as a thread that is read back has them. */
function play(store: ThreadStore, ...items: Item[]): void {
  for (const item of items) {
    store.apply({ type: "item_started", item });
    store.apply({ type: "item_completed", id: item.id });
  }
}

describe("calls being written", () => {
  const draft = (generation: number, field: string | undefined, text: string, index = 0) =>
    ({ type: "draft", generation, index, tool: "Write", field, text }) as const;

  it("are the live state's, not rows: they end the reasoning before them and grow field by field", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply({ type: "item_started", item: thought("r", 1) });
    expect(store.row(1)).toMatchObject({ live: true });
    expect(store.live.streaming).toBe("reasoning");
    store.apply(draft(1, undefined, ""));
    store.apply(draft(1, "path", "a.rs"));
    store.apply(draft(1, "content", "fn a"));
    store.apply(draft(1, "content", "() {}"));
    expect(store.row(1)).toMatchObject({ kind: "item", live: false });
    expect(store.live.streaming).toBeUndefined();
    expect(store.length).toBe(2);
    expect(store.live.drafts).toEqual([
      { state: "writing", draft: { generation: 1, index: 0, tool: "Write", fields: [["path", "a.rs"], ["content", "fn a() {}"]] } },
    ]);
    // a second call of the generation
    store.apply(draft(1, "path", "b.rs", 1));
    expect(store.live.drafts).toHaveLength(2);
    // the generation ends: they wait to run, and each goes as its call starts, in order
    store.apply(generation(1, "calls"));
    expect(store.live.drafts.map((d) => d.state)).toEqual(["written", "written"]);
    const running = { ...edit("w1", 1, "a.rs"), result: undefined };
    store.apply({ type: "item_started", item: running });
    expect(store.live.drafts.map((d) => d.draft.index)).toEqual([1]);
    expect(store.row(2)).toMatchObject({ kind: "item", item: { id: "w1" }, live: true });
    expect(store.live.calls.map((c) => c.id)).toEqual(["w1"]);
    store.apply({ type: "item_completed", id: "w1", result: { outcome: "ok", text: "[edit: applied exact] a.rs" } });
    expect(store.live.calls).toEqual([]);
  });

  it("that come to nothing go", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply(draft(1, "path", "a.rs"));
    store.apply(generation(1, "malformed"));
    expect(store.live.drafts).toEqual([]);
    // a step the server's format check restarted: what it wrote goes, the next try is new
    store.apply(draft(2, "path", "b.rs"));
    store.apply({ type: "item_started", item: { kind: "void", id: "v", generation: 2, why: "restarted", calls: [] } });
    expect(store.live.drafts).toEqual([]);
    store.apply(draft(2, "path", "c.rs"));
    expect(store.live.drafts).toMatchObject([{ state: "writing", draft: { fields: [["path", "c.rs"]] } }]);
    // and what waits to run when the turn ends never runs
    store.apply(generation(2, "calls"));
    expect(store.live.drafts).toMatchObject([{ state: "written" }]);
    store.apply({ type: "turn_completed", index: 1, end: "interrupted" });
    expect(store.live).toMatchObject({ running: false, drafts: [] });
    // no row was ever a draft's
    expect(rows(store).map((r) => r.kind)).toEqual(["user", "item", "end"]);
  });
});

describe("explorations", () => {
  it("fold the calls of a turn that only look, with the thoughts between them", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    play(store, thought("r1", 1), read("a", 1, "src/a.rs"), thought("r2", 2), read("b", 2, "src/b.rs"), grep("g", 2));
    expect(rows(store).map((r) => r.kind)).toEqual(["user", "item", "steps"]);
    const steps = store.row(2) as Extract<Row, { kind: "steps" }>;
    // the first call's row grew into the exploration, under its key
    expect(steps.key).toBe("a");
    expect(steps.steps.map((s) => s.item.id)).toEqual(["a", "r2", "b", "g"]);
    // a call that changes something ends it; the thought before that call stays out
    play(store, thought("r3", 3), edit("e", 3, "src/a.rs"), read("c", 4, "src/c.rs"));
    expect(rows(store).map((r) => (r.kind === "item" ? r.item.id : r.kind))).toEqual(["user", "r1", "steps", "r3", "e", "c"]);
    expect(store.items().map((i) => i.id)).toEqual(["r1", "a", "r2", "b", "g", "r3", "e", "c"]);
    expect(store.itemsOf(2).map((i) => i.id)).toEqual(["r2", "b", "g"]);
  });

  it("stay within a turn, and keep a lone call a row of its own", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "one" });
    play(store, read("a", 1, "a.rs"));
    store.apply({ type: "turn_completed", index: 1, end: "answer" });
    store.apply({ type: "turn_started", index: 2, user: "two" });
    play(store, read("b", 2, "b.rs"));
    expect(rows(store).map((r) => r.kind)).toEqual(["user", "item", "end", "user", "item"]);
    // a notice between two reads parts them
    play(store, { kind: "notice", id: "n", text: "[ognivo] 3 steps left" }, read("c", 3, "c.rs"));
    expect(rows(store).map((r) => r.kind)).toEqual(["user", "item", "end", "user", "item", "item", "item"]);
  });

  it("change the list as they take in a thought, though a row that comes in the same frame keeps its length", () => {
    const clock = manual();
    const store = new ThreadStore(info, { schedule: clock.schedule });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    play(store, read("a", 1, "a.rs"), thought("r", 2));
    clock.run();
    const [length, version] = [store.length, store.listVersion];
    play(store, read("b", 2, "b.rs"), { kind: "steer", id: "s", text: "also c" });
    clock.run();
    // the view follows the version: the row that held the thought holds the steer now
    expect([store.length, store.listVersion]).toEqual([length, version + 1]);
    expect(store.row(2)).toMatchObject({ kind: "item", item: { id: "s" } });
    expect(store.row(1)).toMatchObject({ kind: "steps", steps: [{ item: { id: "a" } }, { item: { id: "r" } }, { item: { id: "b" } }] });
  });

  it("take results as they come, open as a whole and call by call, and tell their row", () => {
    const clock = manual();
    const store = new ThreadStore(info, { schedule: clock.schedule });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    play(store, read("a", 1, "a.rs"));
    store.apply({ type: "item_started", item: thought("r", 2) });
    clock.run();
    let row = 0;
    let list = 0;
    store.subscribeRow(1, () => row++);
    store.subscribeList(() => list++);
    store.apply({ type: "item_completed", id: "r" });
    store.apply({ type: "item_started", item: read("b", 2, "b.rs", false) });
    expect(store.live.calls.map((c) => c.id)).toEqual(["b"]);
    clock.run();
    expect([row, list, store.length]).toEqual([1, 1, 2]);
    store.apply({ type: "item_completed", id: "b", result: { outcome: "ok", text: "[read: b.rs — lines 1-3 of 3 lines]" } });
    clock.run();
    expect(row).toBe(2);
    expect(store.live.calls).toEqual([]);
    expect(store.row(1)).toMatchObject({ kind: "steps", steps: [{ live: false }, { live: false }, { live: false, item: { result: { outcome: "ok" } } }] });
    store.openSteps("a", true);
    store.setOpen("b", true);
    expect(store.row(1)).toMatchObject({ open: true, steps: [{ open: undefined }, {}, { open: true }] });
    // the exploration's key opens it, not its first call
    store.setOpen("a", true);
    expect(store.row(1)).toMatchObject({ open: true, steps: [{ open: true }, {}, { open: true }] });
  });
});

describe("the live state", () => {
  it("keeps the newest progress until its generation or the turn ends, and tells only its own listeners", () => {
    const clock = manual();
    const store = new ThreadStore(info, { schedule: clock.schedule });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply({ type: "item_started", item: { kind: "message", id: "m", generation: 1, text: "" } });
    clock.run();
    expect(store.live).toMatchObject({ running: true, streaming: "message", since: expect.any(Number) });
    let row = 0;
    let list = 0;
    let live = 0;
    store.subscribeRow(1, () => row++);
    store.subscribeList(() => list++);
    store.subscribeLive(() => live++);
    for (const n of [10, 20, 30]) store.apply({ type: "progress", progress: progress(1, "writing", n) });
    expect(live).toBe(0);
    clock.run();
    expect([live, row, list]).toEqual([1, 0, 0]);
    expect(store.live.progress?.generated).toBe(30);
    // the same object while nothing of it changes
    expect(store.live).toBe(store.live);
    store.apply(generation(1, "answer"));
    expect(store.live.progress).toBeUndefined();
    // a late word of the generation that ended is no one's either
    store.apply({ type: "progress", progress: progress(1, "writing", 40) });
    expect(store.live.progress).toBeUndefined();
    store.apply({ type: "progress", progress: progress(2, "waiting") });
    expect(store.live.progress?.phase).toBe("waiting");
    store.apply({ type: "turn_completed", index: 1, end: "answer" });
    clock.run();
    expect(store.live).toMatchObject({ running: false, progress: undefined, since: undefined });
    // a late word of a turn that ended is no one's
    store.apply({ type: "progress", progress: progress(2, "writing", 40) });
    expect(store.live.progress).toBeUndefined();
  });

  it("knows a question that waits and a summary being written", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    store.apply({ type: "turn_started", index: 1, user: "go" });
    const ask = { kind: "approval", id: "q", tool: "shell", action: "cargo test", reason: "runs code" } as const;
    store.apply({ type: "item_started", item: ask });
    expect(store.live.asking).toBe(true);
    store.apply({ type: "item_completed", id: "q", item: { ...ask, allowed: true } });
    expect(store.live.asking).toBe(false);
    store.apply({ type: "item_started", item: { kind: "compaction", id: "c", tokens: 9000, limit: 8000 } });
    expect(store.live.compacting).toBe(true);
  });

  it("is still for a recorded thread, whose last turn has no end", () => {
    const [, thread] = threads[0]!;
    const open = { ...thread, turns: thread.turns.map((t, i) => (i === thread.turns.length - 1 ? { ...t, end: undefined } : t)) };
    expect(ThreadStore.of(open, { schedule: manual().schedule }).live).toMatchObject({ running: false, calls: [], drafts: [] });
  });
});

describe("a turn's end", () => {
  it("keeps what the user wrote that never reached the model, and tells who listens", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    const heard: TurnEnd[] = [];
    store.onTurnEnd((e) => heard.push(e));
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply({ type: "turn_completed", index: 1, end: "answer", verification: "verified: ok" });
    store.apply({ type: "turn_started", index: 2, user: "more" });
    store.apply({ type: "turn_completed", index: 2, end: "interrupted", undelivered: ["also this", "and that"] });
    expect(heard).toEqual([
      { turn: 1, end: "answer", why: undefined, verification: "verified: ok", undelivered: [] },
      { turn: 2, end: "interrupted", why: undefined, verification: undefined, undelivered: ["also this", "and that"] },
    ]);
    expect(store.lastEnd).toEqual(heard[1]);
    expect(store.row(1)).not.toHaveProperty("undelivered");
    expect(store.row(3)).toMatchObject({ kind: "end", undelivered: ["also this", "and that"] });
  });

  it("costs what the turn's generations cost", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    const g = (index: number, prompt: number, cached: number, completion: number, generationMs: number, turn?: number) =>
      ({ type: "generation", generation: { index, outcome: "calls", prompt, cached, completion, promptMs: 10, generationMs }, turn }) as const;
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply(g(1, 1000, 0, 100, 1000, 1));
    store.apply(g(2, 1200, 1000, 50, 1000, 1));
    store.apply({ type: "turn_completed", index: 1, end: "answer" });
    store.apply({ type: "turn_started", index: 2, user: "more" });
    // an event without its turn is of the turn that runs
    store.apply(g(3, 1400, 1200, 10, 500));
    expect(store.turnCost(1)).toMatchObject({ prompt: 2200, cached: 1000, completion: 150, generationMs: 2000, generations: 2, last: { index: 2 } });
    expect(store.turnCost(2)).toMatchObject({ generations: 1, last: { index: 3 } });
    expect(store.turnCost(3)).toBeUndefined();
    // a recorded thread's generations go by its items' turns
    const [, thread] = threads[0]!;
    const recorded = ThreadStore.of(thread, { schedule: manual().schedule });
    const first = thread.turns[0]!;
    const own = new Set(first.items.flatMap((i) => (i.generation === undefined ? [] : [i.generation])));
    expect(recorded.turnCost(first.index)?.generations).toBe(own.size || undefined);
  });

  it("and the core's word that changes were kept or undone tell the review again", () => {
    const store = new ThreadStore(info, { schedule: (f) => f() });
    let heard = 0;
    store.subscribeChanges(() => heard++);
    store.apply({ type: "turn_started", index: 1, user: "go" });
    store.apply({ type: "turn_completed", index: 1, end: "answer" });
    store.apply({ type: "changes" });
    expect(heard).toBe(2);
    expect(store.changesVersion).toBe(2);
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
