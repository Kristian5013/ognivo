// The recorded sessions of fixtures/. Once the core speaks the frozen event model (M2),
// sessions come from it instead.

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Item, Thread } from "./model.ts";

const files = import.meta.glob<AtifTrace>("../fixtures/*.json", {
  eager: true,
  import: "default",
});

export interface Session {
  /** The fixture's name, e.g. `qwen.long-file`. */
  key: string;
  thread: Thread;
}

export const sessions: Session[] = Object.entries(files)
  .map(([path, trace]) => ({
    key: path.slice(path.lastIndexOf("/") + 1, -".json".length),
    thread: fromAtif(trace),
  }))
  .sort((a, b) => a.key.localeCompare(b.key));

/** `thread` to follow `by.turns` turns and `by.generations` generations of another: turns
 * and generations renumbered, ids prefixed. */
export function shifted(
  thread: Thread,
  by: { turns: number; generations: number; prefix: string },
): Thread {
  return {
    ...thread,
    generations: thread.generations.map((g) => ({ ...g, index: g.index + by.generations })),
    turns: thread.turns.map((t) => ({
      ...t,
      index: t.index + by.turns,
      items: t.items.map((i): Item => {
        const item = { ...i, id: by.prefix + i.id };
        if (i.generation !== undefined) item.generation = i.generation + by.generations;
        return item;
      }),
    })),
  };
}

/** A thread of at least `items` items: the recorded sessions over and over (the perf
 * budget of §10 is set on a thread of 10k). */
export function bench(items: number): Thread {
  const base = sessions[0]?.thread;
  if (!base) throw new Error("no fixtures");
  const thread: Thread = { ...base, id: "bench", title: `${items} items`, turns: [], generations: [] };
  let count = 0;
  for (let round = 0; count < items; round++) {
    for (const s of sessions) {
      const t = shifted(s.thread, {
        turns: thread.turns.length,
        generations: thread.generations.length,
        prefix: `r${round}.${s.key}.`,
      });
      thread.generations.push(...t.generations);
      thread.turns.push(...t.turns);
      count += t.turns.reduce((n, turn) => n + turn.items.length, 0);
    }
  }
  return thread;
}
