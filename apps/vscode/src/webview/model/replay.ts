// A recorded thread played back as it was produced: the events of its turns at the times
// they would have come, from the session's own timings. Each generation waits for its
// prefill, then streams its text at its own speed; its calls run after it ends, as
// ognivo runs them.

import type { Item, Thread, ToolItem } from "./model.ts";
import type { LiveEvent, ThreadStore } from "./store.ts";

export interface Timed {
  /** Milliseconds from the start. */
  at: number;
  event: LiveEvent;
}

/** Characters a delta carries: a few tokens, as a server streams them. */
const CHUNK = 12;
/** How long a call runs when its result does not say. */
const CALL_MS = 60;

/** The characters an item took to generate. */
function size(item: Item): number {
  switch (item.kind) {
    case "reasoning":
    case "message":
      return item.text.length;
    case "tool":
      return JSON.stringify(item.raw ?? null).length;
    case "void":
      return item.calls.reduce((n, c) => n + c.arguments.length, 0);
    default:
      return 0;
  }
}

export function timeline(thread: Thread): Timed[] {
  const out: Timed[] = [];
  // by number: a thread may follow another and number its generations on from it
  const gens = new Map(thread.generations.map((g) => [g.index, g]));
  let t = 0;
  const emit = (event: LiveEvent) => out.push({ at: Math.round(t), event });
  for (const turn of thread.turns) {
    emit({ type: "turn_started", index: turn.index, user: turn.user });
    let gen: number | undefined;
    let msPerChar = 0;
    let calls: ToolItem[] = [];
    // a generation ends: what it cost, then its calls run one after another
    const end = () => {
      const g = gen === undefined ? undefined : gens.get(gen);
      if (g) emit({ type: "generation", generation: g });
      for (const c of calls) {
        t += c.result?.seconds !== undefined ? c.result.seconds * 1000 : CALL_MS;
        emit({ type: "item_completed", id: c.id, result: c.result });
      }
      gen = undefined;
      calls = [];
    };
    for (const item of turn.items) {
      if (item.generation !== gen) {
        end();
        const g = item.generation === undefined ? undefined : gens.get(item.generation);
        if (g) {
          gen = g.index;
          t += g.promptMs;
          const chars = turn.items
            .filter((i) => i.generation === g.index)
            .reduce((n, i) => n + size(i), 0);
          msPerChar = chars ? g.generationMs / chars : 0;
        }
      }
      switch (item.kind) {
        case "reasoning":
        case "message":
          emit({ type: "item_started", item: { ...item, text: "" } });
          for (let i = 0; i < item.text.length; i += CHUNK) {
            const text = item.text.slice(i, i + CHUNK);
            t += text.length * msPerChar;
            emit({ type: "item_delta", id: item.id, text });
          }
          emit({ type: "item_completed", id: item.id });
          break;
        case "tool":
          t += size(item) * msPerChar;
          emit({ type: "item_started", item: { ...item, result: undefined } });
          calls.push(item);
          break;
        default:
          t += size(item) * msPerChar;
          emit({ type: "item_started", item });
      }
    }
    end();
    if (turn.end) emit({ type: "turn_completed", index: turn.index, end: turn.end, why: turn.why });
  }
  return out;
}

/** Plays `events` into `store` at `speed` times their pace, a frame at a time; the
 * returned function stops it. */
export function play(
  store: ThreadStore,
  events: Timed[],
  speed: number,
  done?: () => void,
): () => void {
  let next = 0;
  let start: number | undefined;
  let id = 0;
  const tick = (now: number) => {
    start ??= now;
    const elapsed = (now - start) * speed;
    while (next < events.length && events[next]!.at <= elapsed) store.apply(events[next++]!.event);
    // what this frame applied shows in this frame
    store.flush();
    if (next < events.length) id = requestAnimationFrame(tick);
    else done?.();
  };
  id = requestAnimationFrame(tick);
  return () => cancelAnimationFrame(id);
}
