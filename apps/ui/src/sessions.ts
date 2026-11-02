// The recorded sessions of fixtures/. Once the core speaks the frozen event model (M2),
// sessions come from it instead.

import { type AtifTrace, fromAtif } from "./atif.ts";
import type { Thread } from "./model.ts";

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
