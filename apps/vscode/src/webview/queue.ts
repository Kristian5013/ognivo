// What the user wrote while a turn ran (§1.4 of the redesign): each thread's messages that
// wait for the turn to end, in the order they were written, and the messages this page
// steered into a running turn (`turn/steer`) that the model has not read yet. When a turn
// ends, the steers it never delivered (`turn_completed.undelivered`) go back to the front,
// and the first message waiting goes as the next turn; a turn the user stopped keeps them
// all, to send or to drop by hand. Only what this page wrote comes back: another page on
// the same thread sees the same `undelivered`, and sends its own.

import { withContext } from "../shared/context.ts";
import type { ContextChip } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";
import type { Attachment } from "./model/images.ts";

/** A message as the composer wrote it: the user's words, what goes with them, and its
 * images. */
export interface Message {
  text: string;
  chips: ContextChip[];
  images: Attachment[];
}

export interface Queued extends Message {
  id: number;
}

/** The message as the core takes it: its words with their context below. */
export function wire(m: Message): string {
  return withContext(m.text, m.chips);
}

/** The same text, as the core may have trimmed its ends. */
function same(a: string, b: string): boolean {
  return a.trim() === b.trim();
}

export class Queue {
  private waiting = new Map<number, Queued[]>();
  /** What this page steered into each thread's turn, and the core took, by the text the
   * core got. */
  private steered = new Map<number, { text: string; message: Queued }[]>();
  private next = 1;
  private listeners = new Set<() => void>();
  private changes = 0;

  /** The messages that wait for thread `thread`'s turn to end, the next first. */
  list(thread: number): readonly Queued[] {
    return this.waiting.get(thread) ?? [];
  }

  /** `m` waits after the others. */
  add(thread: number, m: Message): Queued {
    const q = this.entry(m);
    this.set(thread, [...this.list(thread), q]);
    return q;
  }

  /** `m` goes first: a message that could not go, or a steer the model never read. */
  front(thread: number, m: Message): Queued {
    const q = this.entry(m);
    this.set(thread, [q, ...this.list(thread)]);
    return q;
  }

  /** Message `id` out of the queue: it goes now, or the user dropped it. */
  remove(thread: number, id: number): Queued | undefined {
    const list = this.list(thread);
    const q = list.find((m) => m.id === id);
    if (q) this.set(thread, list.filter((m) => m !== q));
    return q;
  }

  /** The page steers `m` into the turn that runs: until the model reads it, or the turn
   * ends without it. */
  steering(thread: number, m: Message): Queued {
    const q = this.entry(m);
    this.steered.set(thread, [...(this.steered.get(thread) ?? []), { text: wire(m), message: q }]);
    return q;
  }

  /** Whether steer `q` still waits for the model. */
  isSteering(thread: number, q: Queued): boolean {
    return (this.steered.get(thread) ?? []).some((s) => s.message === q);
  }

  /** The core did not take steer `q` (no turn runs, or it failed): it is the page's again. */
  unsteer(thread: number, q: Queued): void {
    const left = (this.steered.get(thread) ?? []).filter((s) => s.message !== q);
    if (left.length) this.steered.set(thread, left);
    else this.steered.delete(thread);
  }

  /** The model read `text` in the turn (a `steer` item came). */
  delivered(thread: number, text: string): void {
    const mine = this.steered.get(thread);
    const i = mine?.findIndex((s) => same(s.text, text)) ?? -1;
    if (!mine || i < 0) return;
    mine.splice(i, 1);
    if (!mine.length) this.steered.delete(thread);
  }

  /** Thread `thread`'s turn ended so: this page's steers among `undelivered` go to the
   * front, in their order; the message that goes as the next turn comes out, none when the
   * user stopped the turn. */
  ended(thread: number, end: P.TurnEnd, undelivered: readonly string[]): Queued | undefined {
    const mine = this.steered.get(thread) ?? [];
    this.steered.delete(thread);
    const back: Queued[] = [];
    for (const text of undelivered) {
      const i = mine.findIndex((s) => same(s.text, text));
      if (i >= 0) back.push(mine.splice(i, 1)[0]!.message);
    }
    if (back.length) this.set(thread, [...back, ...this.list(thread)]);
    if (end === "interrupted") return undefined;
    const [first, ...rest] = this.list(thread);
    if (first) this.set(thread, rest);
    return first;
  }

  /** What of this page's never reached a model, by thread: what it steered into a turn and
   * the model has not read, then what waits, each in its order. For the composers' drafts,
   * when the core is gone and the turns with it. */
  unsent(): Map<number, Queued[]> {
    const all = new Map<number, Queued[]>();
    for (const [thread, list] of this.steered) all.set(thread, list.map((s) => s.message));
    for (const [thread, list] of this.waiting) all.set(thread, [...(all.get(thread) ?? []), ...list]);
    return all;
  }

  /** Lets go of a deleted thread's messages. */
  forget(thread: number): void {
    this.steered.delete(thread);
    if (this.waiting.delete(thread)) this.changed();
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** Changes with every queue's messages. */
  get version(): number {
    return this.changes;
  }

  private entry(m: Message): Queued {
    return { text: m.text, chips: m.chips, images: m.images, id: this.next++ };
  }

  private set(thread: number, list: Queued[]): void {
    if (list.length) this.waiting.set(thread, list);
    else this.waiting.delete(thread);
    this.changed();
  }

  private changed(): void {
    this.changes++;
    for (const l of this.listeners) l();
  }
}
