import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { toLive } from "./live.ts";
import type { Event } from "./protocol.ts";
import { type Row, ThreadStore } from "./store.ts";

// the stream the core's projector makes of a turn with every kind of item
// (crates/ognivo-core/src/protocol.rs writes it)
const stream: Event[] = readFileSync(join(import.meta.dirname, "../fixtures/stream.jsonl"), "utf8")
  .split("\n")
  .filter(Boolean)
  .map((l) => JSON.parse(l) as Event);

function store(thread: number): ThreadStore {
  const s = new ThreadStore(
    { id: String(thread), title: "", project: "", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] },
    { schedule: (flush) => flush() },
  );
  for (const e of stream.filter((e) => e.thread === thread)) for (const l of toLive(e)) s.apply(l);
  return s;
}

function rows(s: ThreadStore): Row[] {
  return Array.from({ length: s.length }, (_, i) => s.row(i)!);
}

describe("the core's stream", () => {
  it("is numbered in order and starts each thread before its turns", () => {
    expect(stream.map((e) => e.seq)).toEqual(stream.map((_, i) => i + 1));
    for (const thread of new Set(stream.map((e) => e.thread))) {
      expect(stream.find((e) => e.thread === thread)?.event).toBe("thread_started");
    }
  });

  it("fills a thread's store: the turn, its items with their results, its end", () => {
    const s = store(11);
    const all = rows(s);
    expect(all[0]).toMatchObject({ kind: "user", text: expect.stringContaining("Say hello") });
    expect(all.at(-1)).toMatchObject({ kind: "end", end: "answer" });
    const items = all.flatMap((r) => (r.kind === "item" ? [r] : []));
    // nothing is left streaming or running
    expect(items.filter((r) => r.live)).toEqual([]);
    const reasoning = items.find((r) => r.item.kind === "reasoning")?.item;
    expect(reasoning).toMatchObject({ text: "The greeting is in main.rs.", generation: 1 });
    const tools = items.flatMap((r) => (r.item.kind === "tool" ? [r.item] : []));
    expect(tools.map((t) => t.call?.name ?? t.shown)).toEqual(["Read", "Edit", "Bash", "Task"]);
    const task = tools.at(-1)?.call;
    expect(task?.name === "Task" && task.tasks.map((t) => t.title)).toEqual(["Check the README"]);
    expect(tools.find((t) => t.call?.name === "Bash")?.result).toMatchObject({ outcome: "ok", exit: 0 });
    expect(items.some((r) => r.item.kind === "void" && r.item.why === "malformed")).toBe(true);
    expect(s.generations.map((g) => g.index)).toEqual([1, 2, 3, 4, 5, 6, 7]);
    // the edit is the session's change
    expect(s.changedFiles().map((f) => f.path)).toEqual(["main.rs"]);
  });

  it("keeps the turns taken back, out of the way, and marks where", () => {
    const s = new ThreadStore(
      { id: "5", title: "", project: "", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] },
      { schedule: (flush) => flush() },
    );
    const message = { id: 1, turn: 2, generation: 1, kind: "agent_message", text: "Two." } as const;
    const ended = (turn: number) => ({ event: "turn_completed", turn, end: "answer", why: null, answer: "x", verification: null });
    const events = [
      { event: "turn_started", turn: 1, user: "first" },
      ended(1),
      { event: "turn_started", turn: 2, user: "second" },
      { event: "item_started", item: message },
      { event: "item_completed", item: message },
      ended(2),
      { event: "thread_rewound", turn: 2 },
      { event: "turn_started", turn: 3, user: "second, better" },
    ];
    events.forEach((e, i) => {
      for (const l of toLive({ seq: i + 1, thread: 5, ...e } as Event)) s.apply(l);
    });
    expect(rows(s).map((r) => r.kind)).toEqual(["user", "end", "user", "item", "end", "rewound", "user"]);
    expect(rows(s).map((_, i) => s.isDropped(i))).toEqual([false, false, true, true, true, false, false]);
    expect(s.row(5)).toMatchObject({ kind: "rewound", from: 2, to: 2 });
  });

  it("gives a subagent a thread of its own", () => {
    const sub = stream.find((e) => e.event === "thread_started" && e.info.kind === "subagent");
    expect(sub?.event === "thread_started" && sub.info.parent).toMatchObject({ thread: 11 });
    const s = store(sub!.thread);
    const all = rows(s);
    expect(all[0]).toMatchObject({ kind: "user", text: expect.stringContaining("Task from the main agent") });
    expect(all.at(-1)).toMatchObject({ kind: "end", end: "answer" });
  });
});
