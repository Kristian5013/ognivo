// The core's threads as the GUI holds them: a store for each thread it shows. A thread
// opened from the list fills from what the core kept of it (`thread/read`), or from its
// trajectory when the core kept nothing (a session of `ognivo chat`, or from before), then
// from the stream; a thread that starts while the GUI is there fills from the stream. An
// event the store has had is not applied again.

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Core } from "../../shared/rpc.ts";
import { toLive } from "./live.ts";
import type * as P from "../../shared/protocol.ts";
import { type Info, ThreadStore } from "./store.ts";

interface Entry {
  store: ThreadStore;
  info?: P.ThreadInfo;
  /** The last event it took. */
  seq: number;
  /** Events that came while it loads. */
  pending?: P.Event[];
  loaded?: Promise<ThreadStore>;
  running: boolean;
  /** The generations of its trajectory: the core numbers the thread's own from 1. */
  offset: number;
}

type Schedule = (flush: () => void) => void;

export class Threads {
  private entries = new Map<number, Entry>();
  /** Each thread's mode, as it opened and as a client switched it. */
  private modes = new Map<number, P.Mode>();
  private listeners = new Set<() => void>();
  private version = 0;

  constructor(
    private readonly core: Core,
    private readonly opts: { schedule?: Schedule } = {},
  ) {
    core.onEvent((e) => this.event(e));
  }

  store(thread: number): ThreadStore | undefined {
    return this.entries.get(thread)?.store;
  }

  info(thread: number): P.ThreadInfo | undefined {
    return this.entries.get(thread)?.info;
  }

  /** The thread's mode, once it opened or a client switched it. */
  mode(thread: number): P.Mode | undefined {
    return this.modes.get(thread);
  }

  /** The thread's mode as it opened, or as this client switched it before the core heard. */
  setMode(thread: number, mode: P.Mode): void {
    if (this.modes.get(thread) === mode) return;
    this.modes.set(thread, mode);
    this.changed();
  }

  /** Whether a turn of the thread runs, as its events say. */
  running(thread: number): boolean {
    return this.entries.get(thread)?.running ?? false;
  }

  /** Changes when a thread starts, a turn starts or ends. */
  get stateVersion(): number {
    return this.version;
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Lets go of a deleted thread, and of its subagents' threads. */
  forget(thread: number): void {
    for (const [t, entry] of this.entries) {
      if (t === thread || entry.info?.parent?.thread === thread) this.entries.delete(t);
    }
    this.changed();
  }

  /** The thread's store, filled with what happened in it so far. */
  open(thread: number, title = ""): Promise<ThreadStore> {
    const have = this.entries.get(thread);
    if (have) return have.loaded ?? Promise.resolve(have.store);
    const entry: Entry = {
      store: this.newStore(thread, undefined, title),
      seq: 0,
      pending: [],
      running: false,
      offset: 0,
    };
    this.entries.set(thread, entry);
    entry.loaded = this.load(thread, entry, title).finally(() => {
      entry.loaded = undefined;
      const pending = entry.pending ?? [];
      entry.pending = undefined;
      for (const e of pending) this.apply(entry, e);
      this.changed();
    });
    return entry.loaded;
  }

  private async load(thread: number, entry: Entry, title: string): Promise<ThreadStore> {
    const { events } = await this.core.call("thread/read", { thread, after: null });
    const started = events.find((e) => e.thread === thread && e.event === "thread_started");
    if (started?.event === "thread_started") {
      entry.store = this.newStore(thread, started.info, title);
      for (const e of events) {
        if (e.thread === thread) this.apply(entry, e);
        else this.event(e);
      }
      return entry.store;
    }
    // nothing kept: what happened before, from its trajectory
    const { trajectory } = await this.core.call("thread/trajectory", { thread });
    const recorded = fromAtif(trajectory as AtifTrace);
    entry.store = ThreadStore.of({ ...recorded, title: title || recorded.title }, this.opts);
    entry.offset = recorded.generations.length;
    return entry.store;
  }

  private event(e: P.Event): void {
    let entry = this.entries.get(e.thread);
    if (!entry) {
      if (e.event !== "thread_started") return;
      entry = { store: this.newStore(e.thread, e.info), seq: 0, running: false, offset: 0 };
      this.entries.set(e.thread, entry);
    }
    if (entry.pending) entry.pending.push(e);
    else this.apply(entry, e);
  }

  private apply(entry: Entry, e: P.Event): void {
    if (e.seq <= entry.seq) return;
    entry.seq = e.seq;
    switch (e.event) {
      case "thread_started":
        entry.info = e.info;
        this.changed();
        break;
      case "turn_started":
        entry.running = true;
        this.changed();
        break;
      case "turn_completed":
        entry.running = false;
        this.changed();
        break;
      case "mode_changed":
        this.setMode(e.thread, e.mode);
        break;
    }
    for (const l of toLive(e, entry.offset)) entry.store.apply(l);
  }

  private newStore(thread: number, info?: P.ThreadInfo, title = ""): ThreadStore {
    const base: Info = {
      id: String(thread),
      title,
      project: info?.project ?? "",
      profile: info?.profile ?? "",
      model: info?.model ?? info?.profile ?? "",
      server: info?.server ?? "",
      system: "",
      conversations: {},
      tools: [],
    };
    return new ThreadStore(base, this.opts);
  }

  private changed(): void {
    this.version++;
    for (const l of this.listeners) l();
  }
}
