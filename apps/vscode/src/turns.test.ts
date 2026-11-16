import { describe, expect, it } from "vitest";

import type * as P from "./shared/protocol.ts";
import { Turns } from "./turns.ts";

let seq = 0;
const ev = (thread: number, kind: P.EventKind): P.Event => ({ seq: ++seq, thread, ...kind }) as P.Event;

const info = (parent: number | null): P.ThreadInfo => ({
  kind: parent === null ? "chat" : "subagent",
  project: "/p",
  profile: "gpt-oss-20b",
  level: null,
  server: "http://127.0.0.1:8080",
  model: null,
  parent: parent === null ? null : { thread: parent, item: 7 },
});

const bash = (id: number, done: boolean): P.Item => ({
  id,
  turn: 1,
  generation: 1,
  kind: "tool_call",
  call_id: `c${id}`,
  tool: "Bash",
  arguments: "{}",
  call: { name: "Bash", command: "cargo test", timeout_ms: null },
  result: done ? { outcome: "ok", text: "[shell: exit 0, 1.0 s]\n", exit_code: 0, duration_ms: 1000 } : null,
  changes: [],
});

const progress: P.Progress = {
  generation: 2,
  phase: "writing",
  prompt_total: 100,
  prompt_cached: 0,
  prompt_done: 100,
  generated: 40,
  elapsed_ms: 1200,
  tokens_per_second: 33,
};

describe("what each session's agent does", () => {
  it("follows a turn: its progress, its call, its end", () => {
    const turns = new Turns();
    const running: number[] = [];
    turns.onRunning((t) => running.push(t));
    turns.event(ev(1, { event: "turn_started", turn: 1, user: "hi" }));
    expect(turns.isRunning(1)).toBe(true);
    turns.event(ev(1, { event: "progress", progress }));
    expect(turns.activityOf(1)).toEqual({ running: true, progress, call: null });
    turns.event(ev(1, { event: "generation", generation: {} as P.Generation }));
    expect(turns.activityOf(1)?.progress).toBeNull();
    turns.event(ev(1, { event: "item_started", item: bash(5, false) }));
    expect(turns.activityOf(1)?.call).toBe("Running cargo test");
    turns.event(ev(1, { event: "item_completed", item: bash(5, true) }));
    expect(turns.activityOf(1)?.call).toBeNull();
    turns.event(ev(1, { event: "turn_completed", turn: 1, end: "answer", why: null, answer: "ok", verification: null }));
    expect(turns.isRunning(1)).toBe(false);
    expect(running).toEqual([1, 1]);
  });

  it("counts a subagent's thread as its session's, and its turns as none of their own", () => {
    const turns = new Turns();
    turns.event(ev(2, { event: "thread_started", info: info(null) }));
    turns.event(ev(3, { event: "thread_started", info: info(2) }));
    turns.event(ev(4, { event: "thread_started", info: info(3) }));
    expect(turns.root(4)).toBe(2);
    expect(turns.root(2)).toBe(2);
    turns.event(ev(3, { event: "turn_started", turn: 1, user: "a task" }));
    expect(turns.isRunning(3)).toBe(false);
  });

  it("takes the turns that ran before it connected from the list, and a progress as one", () => {
    const turns = new Turns();
    turns.seed([
      { thread: 1, running: true },
      { thread: 2, running: false },
    ]);
    expect(turns.isRunning(1)).toBe(true);
    expect(turns.isRunning(2)).toBe(false);
    turns.event(ev(2, { event: "progress", progress }));
    expect(turns.isRunning(2)).toBe(true);
    turns.clear();
    expect(turns.isRunning(1) || turns.isRunning(2)).toBe(false);
  });
});
