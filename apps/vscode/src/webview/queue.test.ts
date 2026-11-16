import { describe, expect, it } from "vitest";

import type { ContextChip } from "../shared/messages.ts";
import { type Message, Queue, wire } from "./queue.ts";

const words = (text: string, chips: ContextChip[] = []): Message => ({ text, chips, images: [] });
const texts = (q: Queue, thread: number) => q.list(thread).map((m) => m.text);

describe("the queue of a thread", () => {
  it("keeps the messages written while a turn runs in their order, each thread its own", () => {
    const q = new Queue();
    q.add(1, words("first"));
    q.add(1, words("second"));
    q.add(2, words("elsewhere"));
    expect(texts(q, 1)).toEqual(["first", "second"]);
    expect(texts(q, 2)).toEqual(["elsewhere"]);
    expect(texts(q, 3)).toEqual([]);
  });

  it("sends the first message as the next turn when a turn ends, one a turn", () => {
    const q = new Queue();
    q.add(1, words("first"));
    q.add(1, words("second"));
    expect(q.ended(1, "answer", [])?.text).toBe("first");
    expect(texts(q, 1)).toEqual(["second"]);
    expect(q.ended(1, "step_limit", [])?.text).toBe("second");
    expect(q.ended(1, "answer", [])).toBeUndefined();
  });

  it("keeps every message when the user stopped the turn", () => {
    const q = new Queue();
    q.add(1, words("first"));
    expect(q.ended(1, "interrupted", [])).toBeUndefined();
    expect(texts(q, 1)).toEqual(["first"]);
  });

  it("drops a message by its id, and puts one that could not go back in front", () => {
    const q = new Queue();
    const a = q.add(1, words("a"));
    q.add(1, words("b"));
    expect(q.remove(1, a.id)?.text).toBe("a");
    expect(q.remove(1, a.id)).toBeUndefined();
    q.front(1, a);
    expect(texts(q, 1)).toEqual(["a", "b"]);
  });

  it("puts the steers the model never read in front, in their order, as the user wrote them", () => {
    const q = new Queue();
    const chip: ContextChip = { kind: "file", path: "src/parse.rs", abs: "/p/src/parse.rs" };
    q.add(1, words("queued"));
    q.steering(1, words("one", [chip]));
    q.steering(1, words("two"));
    // the core names them by the text it got, the context block too
    const next = q.ended(1, "answer", [wire(words("one", [chip])), "two"]);
    expect(next).toMatchObject({ text: "one", chips: [chip] });
    expect(texts(q, 1)).toEqual(["two", "queued"]);
  });

  it("does not send what another page steered, nor a steer the model read", () => {
    const q = new Queue();
    q.steering(1, words("read"));
    q.delivered(1, "read");
    expect(q.ended(1, "answer", ["read", "another page's"])).toBeUndefined();
    expect(texts(q, 1)).toEqual([]);
  });

  it("keeps undelivered steers when the turn was stopped, and forgets steers once a turn ended", () => {
    const q = new Queue();
    const s = q.steering(1, words("late"));
    expect(q.isSteering(1, s)).toBe(true);
    expect(q.ended(1, "interrupted", ["late"])).toBeUndefined();
    expect(texts(q, 1)).toEqual(["late"]);
    expect(q.isSteering(1, s)).toBe(false);
    // a steer of an earlier turn is not this turn's
    q.steering(1, words("earlier"));
    expect(q.ended(1, "answer", [])?.text).toBe("late");
    q.add(1, words("x"));
    expect(q.ended(1, "answer", ["earlier"])?.text).toBe("x");
    expect(texts(q, 1)).toEqual([]);
  });

  it("gives a steer the core did not take back to the page", () => {
    const q = new Queue();
    const s = q.steering(1, words("idle"));
    q.unsteer(1, s);
    expect(q.isSteering(1, s)).toBe(false);
    expect(q.ended(1, "answer", ["idle"])).toBeUndefined();
  });

  it("says what never reached a model: the steers the model has not read, then what waits", () => {
    const q = new Queue();
    q.add(1, words("queued first"));
    q.steering(1, words("steered"));
    q.add(1, words("queued second"));
    q.add(2, words("elsewhere"));
    const read = q.steering(3, words("read"));
    q.delivered(3, "read");
    expect(q.isSteering(3, read)).toBe(false);
    const unsent = q.unsent();
    expect([...unsent].map(([t, list]) => [t, list.map((m) => m.text)])).toEqual([
      [1, ["steered", "queued first", "queued second"]],
      [2, ["elsewhere"]],
    ]);
    // it only says so: the queue stays as it was
    expect(texts(q, 1)).toEqual(["queued first", "queued second"]);
    expect(new Queue().unsent().size).toBe(0);
  });

  it("tells its listeners of each change, and lets a deleted thread go", () => {
    const q = new Queue();
    let heard = 0;
    const stop = q.subscribe(() => heard++);
    const v = q.version;
    q.add(1, words("a"));
    q.add(1, words("b"));
    q.forget(1);
    expect(texts(q, 1)).toEqual([]);
    expect(heard).toBe(3);
    expect(q.version).toBe(v + 3);
    stop();
    q.add(1, words("c"));
    expect(heard).toBe(3);
  });
});
