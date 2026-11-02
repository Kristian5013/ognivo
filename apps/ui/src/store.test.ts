import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Thread } from "./model.ts";
import { timeline } from "./replay.ts";
import { type Row, ThreadStore } from "./store.ts";

const dir = join(import.meta.dirname, "../fixtures");
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
