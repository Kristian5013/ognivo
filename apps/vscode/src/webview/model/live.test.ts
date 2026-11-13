import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { toLive } from "./live.ts";
import type { Event } from "../../shared/protocol.ts";
import { type Row, ThreadStore } from "./store.ts";

// the stream the core's projector makes of a turn with every kind of item
// (crates/ognivo-core/src/protocol.rs writes it)
const stream: Event[] = readFileSync(join(import.meta.dirname, "../../../fixtures/stream.jsonl"), "utf8")
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
    // the user's screenshot, which the model saw before the text
    expect(all[0]).toMatchObject({ images: [{ mime: "image/png", width: 2, height: 1, data: expect.any(String) }] });
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

  it("gives a call's result the image file it read, and a turn of an older core no images", () => {
    const s = new ThreadStore(
      { id: "1", title: "", project: "", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] },
      { schedule: (flush) => flush() },
    );
    const picture = { mime: "image/png", data: "iVBORw0KGgo=", width: 8, height: 6 };
    const item = {
      id: 1,
      turn: 1,
      generation: 1,
      kind: "tool_call" as const,
      call_id: "c1",
      tool: "read",
      arguments: '{"path":"shot.png"}',
      call: { name: "Read" as const, path: "shot.png", offset: null, limit: null },
      changes: [],
    };
    const events: Event[] = [
      { seq: 1, thread: 1, event: "turn_started", turn: 1, user: "look" },
      { seq: 2, thread: 1, event: "item_started", item: { ...item, result: null } },
      {
        seq: 3,
        thread: 1,
        event: "item_completed",
        item: {
          ...item,
          result: {
            outcome: "ok",
            text: "[read: shot.png — an image, PNG, 8×6, 1 KB; it follows]",
            images: [picture],
            exit_code: null,
            duration_ms: 3,
          },
        },
      },
    ];
    for (const e of events) for (const l of toLive(e)) s.apply(l);
    const all = rows(s);
    expect(all[0]).toMatchObject({ kind: "user", text: "look", images: [] });
    const read = all[1]?.kind === "item" ? all[1].item : undefined;
    expect(read?.kind === "tool" && read.result).toMatchObject({ outcome: "ok", images: [picture] });
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

  it("shows a call as the model writes it, and gives its row to the call's item", () => {
    const s = new ThreadStore(
      { id: "11", title: "", project: "", profile: "", model: "", server: "", system: "", conversations: {}, tools: [] },
      { schedule: (flush) => flush() },
    );
    const own = stream.filter((e) => e.thread === 11);
    const at = (pred: (e: Event) => boolean) => own.findIndex(pred);
    const play = (from: number, to: number) => {
      for (const e of own.slice(from, to)) for (const l of toLive(e)) s.apply(l);
    };
    const drafted = at((e) => e.event === "call_draft");
    const ended = at((e) => e.event === "generation" && e.generation.index === 2);
    const edit = at((e) => e.event === "item_started" && e.item.kind === "tool_call" && e.item.tool === "edit");
    play(0, ended);
    const row = s.length - 1;
    expect(s.row(row)).toMatchObject({
      kind: "draft",
      state: "writing",
      draft: { generation: 2, index: 0, tool: "Edit", fields: [["path", "main.rs"], ["old", '"hi"'], ["new", '"hello"']] },
    });
    const key = s.key(row);
    play(ended, ended + 1);
    expect(s.row(row)).toMatchObject({ kind: "draft", state: "written" });
    play(ended + 1, edit + 1);
    // the edit's item in the draft's place, under its key
    expect(s.row(row)).toMatchObject({ kind: "item", item: { kind: "tool", call: { name: "Edit" } } });
    expect(s.key(row)).toBe(key);
    play(edit + 1, own.length);
    expect(rows(s).filter((r) => r.kind === "draft")).toEqual([]);
    expect(drafted).toBeLessThan(ended);
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
