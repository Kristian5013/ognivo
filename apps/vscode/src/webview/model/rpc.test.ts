import { readFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { Core, CoreError, type Port } from "../../shared/rpc.ts";
import type { Event } from "../../shared/protocol.ts";
import { Threads } from "./threads.ts";

const stream: Event[] = readFileSync(join(import.meta.dirname, "../../../fixtures/stream.jsonl"), "utf8")
  .split("\n")
  .filter(Boolean)
  .map((l) => JSON.parse(l) as Event);

type Message = { id?: number; method?: string; params?: Record<string, unknown>; result?: unknown };

/** A port whose other end is the test: what the client sent, and a way to answer. */
function wire(answer: (m: Message) => unknown = () => ({})) {
  const sent: Message[] = [];
  const port: Port = {
    onmessage: null,
    postMessage(text: string) {
      const m = JSON.parse(text) as Message;
      sent.push(m);
      // a request gets its answer later, as from a socket
      if (m.method) queueMicrotask(() => deliver({ jsonrpc: "2.0", id: m.id, result: answer(m) }));
    },
  };
  const deliver = (m: unknown) => port.onmessage?.({ data: typeof m === "string" ? m : JSON.stringify(m) } as MessageEvent);
  return { port, sent, deliver };
}

const event = (e: Event) => ({ jsonrpc: "2.0", method: "event", params: e });
const sync = { schedule: (flush: () => void) => flush() };

describe("the core's connection", () => {
  it("answers calls by their ids, and errors as such", async () => {
    const w = wire((m) => (m.method === "initialize" ? { protocol: 1, core: "0.0.1", seq: 9 } : undefined));
    const core = new Core(w.port);
    expect(await core.initialize("test")).toEqual({ protocol: 1, core: "0.0.1", seq: 9 });
    expect(core.status).toEqual({ state: "ready", core: "0.0.1" });
    expect(w.sent[0]).toMatchObject({ jsonrpc: "2.0", id: 1, method: "initialize", params: { client: "test", protocol: 1 } });
    w.port.postMessage = (text) => {
      const m = JSON.parse(text) as Message;
      queueMicrotask(() => w.deliver({ jsonrpc: "2.0", id: m.id, error: { code: -32002, message: "a turn is running" } }));
    };
    const failed = core.call("turn/start", { thread: 1, text: "go" });
    await expect(failed).rejects.toEqual(new CoreError(-32002, "a turn is running"));
    await expect(failed).rejects.toMatchObject({ code: -32002 });
  });

  it("keeps the core's questions until they are answered or withdrawn", () => {
    const w = wire();
    const core = new Core(w.port);
    const seen: Event[] = [];
    core.onEvent((e) => seen.push(e));
    w.deliver(event(stream[0]!));
    expect(seen).toEqual([stream[0]]);
    const params = { thread: 11, item: null, tool: "shell", action: "cargo test", reason: "runs project code", unreviewed: [], agent: null };
    w.deliver({ jsonrpc: "2.0", id: 7, method: "approval/ask", params });
    w.deliver({ jsonrpc: "2.0", id: 8, method: "rules/trust", params: { project: "/p", rules: [] } });
    expect([...core.asks.keys()]).toEqual([7, 8]);
    core.answer<"approval/ask">(7, { decision: "allow_session", instead: null });
    expect(w.sent.at(-1)).toEqual({ jsonrpc: "2.0", id: 7, result: { decision: "allow_session", instead: null } });
    // answered elsewhere
    w.deliver({ jsonrpc: "2.0", method: "$/cancelRequest", params: { id: 8 } });
    expect(core.asks.size).toBe(0);
    // an answer to a question no longer there goes nowhere
    core.answer<"rules/trust">(8, { trust: true });
    expect(w.sent.at(-1)).toMatchObject({ id: 7 });
  });

  it("is down when main says the core closed it", async () => {
    const w = wire(() => undefined);
    w.port.postMessage = (text) => void w.sent.push(JSON.parse(text) as Message);
    const core = new Core(w.port);
    const pending = core.call("thread/list", { project: null, limit: null });
    w.deliver({ jsonrpc: "2.0", id: 9, method: "approval/ask", params: {} });
    w.port.onmessage?.({ data: { core: "closed", why: "the core closed the connection" } } as MessageEvent);
    await expect(pending).rejects.toThrow("the core closed the connection");
    expect(core.status).toEqual({ state: "down", why: "the core closed the connection" });
    expect(core.asks.size).toBe(0);
    await expect(core.call("thread/list", { project: null, limit: null })).rejects.toThrow("closed");
  });
});

describe("the core's threads", () => {
  it("fill from the stream when they start while the client is there", () => {
    const w = wire();
    const threads = new Threads(new Core(w.port), sync);
    for (const e of stream) w.deliver(event(e));
    const s = threads.store(11)!;
    expect(s.info).toMatchObject({ id: "11", project: "/home/user/proj", profile: "gpt-oss-20b" });
    expect(s.row(0)).toMatchObject({ kind: "user" });
    expect(s.row(s.length - 1)).toMatchObject({ kind: "end", end: "answer" });
    expect(threads.running(11)).toBe(false);
    // the subagent's own thread
    expect(threads.info(12)?.parent).toEqual({ thread: 11, item: 10 });
  });

  it("read what was kept, then the stream, each event once", async () => {
    const half = stream.findIndex((e) => e.event === "item_started" && e.item.kind === "tool_call");
    const kept = stream.filter((e) => e.event !== "item_delta").slice(0, half);
    const w = wire((m) => (m.method === "thread/read" ? { events: kept } : undefined));
    const threads = new Threads(new Core(w.port), sync);
    const opened = threads.open(11);
    // the stream goes on while the thread loads, from a little before where the read ends
    for (const e of stream.slice(half - 3)) w.deliver(event(e));
    const s = await opened;
    expect(w.sent[0]).toMatchObject({ method: "thread/read", params: { thread: 11, after: null } });
    // the same thread as a client that was there all along has it
    const all = wire();
    const reference = new Threads(new Core(all.port), sync);
    for (const e of stream) all.deliver(event(e));
    const rows = (x: typeof s) => Array.from({ length: x.length }, (_, i) => x.row(i));
    expect(rows(s)).toEqual(rows(reference.store(11)!));
    expect(await threads.open(11)).toBe(s);
  });

  it("number their generations on from a trajectory when nothing was kept", async () => {
    const trajectory = {
      schema_version: "ATIF-v1.7",
      session_id: "5",
      steps: [
        { step_id: 1, source: "user", message: "first" },
        { step_id: 2, source: "agent", message: "One.", metrics: { prompt_tokens: 10, completion_tokens: 2 } },
        { step_id: 3, source: "agent", message: "Two.", metrics: { prompt_tokens: 12, completion_tokens: 2 } },
      ],
    };
    const w = wire((m) =>
      m.method === "thread/read" ? { events: [] } : m.method === "thread/trajectory" ? { trajectory } : undefined,
    );
    const threads = new Threads(new Core(w.port), sync);
    const s = await threads.open(5, "first");
    expect(s.generations.map((g) => g.index)).toEqual([0, 1]);
    // the session goes on in the core, which numbers its generations from 1
    const info = { kind: "chat", project: "/p", profile: "gpt-oss-20b", level: null, server: "u", model: null, parent: null };
    const on: Event[] = [
      { seq: 40, thread: 5, event: "thread_started", info },
      { seq: 41, thread: 5, event: "turn_started", turn: 2, user: "second" },
      {
        seq: 42,
        thread: 5,
        event: "item_started",
        item: { id: 1, turn: 2, generation: 1, kind: "agent_message", text: "" },
      },
    ];
    for (const e of on) w.deliver(event(e));
    expect(threads.running(5)).toBe(true);
    const last = s.row(s.length - 1);
    expect(last).toMatchObject({ kind: "item", item: { kind: "message", generation: 3 } });
  });
});
