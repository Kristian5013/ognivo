// What the agent of each session does now, from the events of the extension's own client
// (src/client.ts gets every thread's): whether a turn runs, the newest progress of its
// generation, the call that runs; and whose subagent a thread is, so that a subagent's
// work counts as its session's. The review (src/review.ts), the status bar (src/status.ts)
// and the terminal of the agent's commands (src/terminals.ts) read it. Nothing here knows
// VS Code, so it is tested on its own (turns.test.ts).

import type * as P from "./shared/protocol.ts";
import { type Activity, callText } from "./wording.ts";

type Listener = (thread: number) => void;

export class Turns {
  private readonly running = new Set<number>();
  private readonly activity = new Map<number, Activity>();
  /** A subagent's thread, and the thread it works for. */
  private readonly parents = new Map<number, number>();
  /** The calls that run, by thread: their items' ids and how the status says them. */
  private readonly calls = new Map<number, Map<number, string>>();
  private readonly runningListeners = new Set<Listener>();
  private readonly activityListeners = new Set<Listener>();

  /** The session a thread is of: itself, or the session whose subagent it is. */
  root(thread: number): number {
    let t = thread;
    for (let hops = 0; hops < 16; hops++) {
      const parent = this.parents.get(t);
      if (parent === undefined) return t;
      t = parent;
    }
    return t;
  }

  /** Whether a turn of the session runs. */
  isRunning(thread: number): boolean {
    return this.running.has(thread);
  }

  /** What the session's agent does now. */
  activityOf(thread: number): Activity | undefined {
    return this.activity.get(thread);
  }

  /** A turn of a session started or ended. */
  onRunning(l: Listener): () => void {
    this.runningListeners.add(l);
    return () => this.runningListeners.delete(l);
  }

  /** What a session's agent does changed: its turn, its progress, its call. */
  onActivity(l: Listener): () => void {
    this.activityListeners.add(l);
    return () => this.activityListeners.delete(l);
  }

  /** The sessions as the core lists them, when the client connects: the turns that run
   * started before it did. */
  seed(threads: readonly Pick<P.ThreadSummary, "thread" | "running">[]): void {
    for (const t of threads) this.setRunning(t.thread, t.running);
  }

  /** The client lost the core: nothing runs that it knows of. */
  clear(): void {
    for (const t of [...this.running]) this.setRunning(t, false);
  }

  event(e: P.Event): void {
    switch (e.event) {
      case "thread_started":
        if (e.info.parent) this.parents.set(e.thread, e.info.parent.thread);
        break;
      case "turn_started":
        // a subagent's turn is a part of its session's
        if (this.parents.has(e.thread)) break;
        this.calls.delete(e.thread);
        this.setRunning(e.thread, true);
        break;
      case "turn_completed":
        if (this.parents.has(e.thread)) break;
        this.calls.delete(e.thread);
        this.setRunning(e.thread, false);
        break;
      case "progress":
        if (this.parents.has(e.thread)) break;
        // a turn that started before the client connected
        if (!this.running.has(e.thread)) this.setRunning(e.thread, true);
        this.update(e.thread, { progress: e.progress });
        break;
      case "generation":
        if (this.parents.has(e.thread)) break;
        this.update(e.thread, { progress: null });
        break;
      case "item_started":
      case "item_completed": {
        if (e.item.kind !== "tool_call" || this.parents.has(e.thread)) break;
        const calls = this.calls.get(e.thread) ?? new Map<number, string>();
        this.calls.set(e.thread, calls);
        if (e.event === "item_started" && !e.item.result) {
          if (!this.running.has(e.thread)) this.setRunning(e.thread, true);
          calls.set(e.item.id, callText(e.item.tool, e.item.call));
        } else calls.delete(e.item.id);
        this.update(e.thread, { call: [...calls.values()].pop() ?? null });
        break;
      }
      default:
        break;
    }
  }

  private setRunning(thread: number, running: boolean): void {
    const was = this.running.has(thread);
    if (running) this.running.add(thread);
    else this.running.delete(thread);
    this.activity.set(thread, { running, progress: null, call: null });
    if (was !== running) for (const l of this.runningListeners) l(thread);
    for (const l of this.activityListeners) l(thread);
  }

  private update(thread: number, change: Partial<Activity>): void {
    const now = this.activity.get(thread) ?? { running: this.running.has(thread), progress: null, call: null };
    this.activity.set(thread, { ...now, ...change });
    for (const l of this.activityListeners) l(thread);
  }
}
