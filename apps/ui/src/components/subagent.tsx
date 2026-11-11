import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from "react";

import { summary } from "../format.ts";
import type { Picture, SubagentItem } from "../model.ts";
import type { ThreadStore } from "../store.ts";

/** What a thread's rows can do on the core: lead to a subagent's thread, and write a turn's
 * message anew. */
export interface ThreadNav {
  store(thread: number): ThreadStore | undefined;
  open(thread: number): void;
  /** Takes the conversation back to before `turn` and sends `text`, with `images`, in its
   * place. */
  edit?(turn: number, text: string, images: Picture[]): Promise<void>;
  /** A turn runs: nothing is written anew meanwhile. */
  busy?: boolean;
  /** The thread's model takes images. */
  vision?: boolean;
}

export const ThreadNavContext = createContext<ThreadNav | undefined>(undefined);

const NOTHING = () => () => {};

/** What a thread does now: its last call, or that it thinks or writes. */
function activity(store: ThreadStore | undefined): string | undefined {
  if (!store) return undefined;
  for (let i = store.length - 1; i >= 0; i--) {
    const row = store.row(i);
    if (row?.kind !== "item") continue;
    const item = row.item;
    if (item.kind === "tool") return item.call ? `${item.call.name} ${summary(item.call)}` : item.shown;
    if (item.kind === "reasoning") return row.live ? "thinking…" : undefined;
    if (item.kind === "message") return item.text.split("\n", 1)[0];
  }
  return undefined;
}

function ending(end: string): [string, string] {
  switch (end) {
    case "answer":
      return ["done", "text-ok"];
    case "interrupted":
      return ["stopped", "text-muted-foreground"];
    case "step_limit":
      return ["step limit", "text-warn"];
    case "time_limit":
      return ["time limit", "text-warn"];
    case "stuck":
      return ["stuck", "text-warn"];
    default:
      return ["failed", "text-err"];
  }
}

/** A subagent's lane: its task, what it does now, and how it ended; its thread a click
 * away. */
export function SubagentCard({ item, live }: { item: SubagentItem; live: boolean }) {
  const nav = useContext(ThreadNavContext);
  const child = item.thread === undefined ? undefined : nav?.store(item.thread);
  const subscribe = useCallback((l: () => void) => (child ? child.subscribeList(l) : NOTHING()), [child]);
  const version = useSyncExternalStore(subscribe, () => child?.listVersion ?? 0);
  const now = useMemo(() => (live ? activity(child) : undefined), [child, live, version]);
  const [label, tone] = item.end ? ending(item.end) : ["working", "text-spark"];
  const canOpen = nav !== undefined && item.thread !== undefined && child !== undefined;
  return (
    <div className={`lane ${live ? "lane-live" : ""} flex min-w-0 items-center gap-3 rounded-md border border-border bg-panel px-3 py-1.5`}>
      <span className="grid size-5 shrink-0 place-items-center rounded-full bg-raised font-mono text-[11px] text-spark">
        {item.index + 1}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-baseline gap-2">
          <span className="truncate font-medium">{item.title}</span>
          <span className="shrink-0 text-[11px] text-muted-foreground">
            {item.write ? `writes ${item.write.join(", ")}` : "reads"}
          </span>
        </span>
        {now && <span className="block truncate font-mono text-[11px] text-muted-foreground">{now}</span>}
        {!live && item.changed.length > 0 && (
          <span className="block truncate font-mono text-[11px] text-muted-foreground">changed {item.changed.join(", ")}</span>
        )}
      </span>
      {item.steps !== undefined && <span className="shrink-0 text-[11px] text-muted-foreground">{item.steps} steps</span>}
      <span className={`shrink-0 text-[12px] font-medium ${tone} ${live ? "ember-text" : ""}`}>{label}</span>
      {canOpen && (
        <button
          type="button"
          onClick={() => nav.open(item.thread!)}
          className="shrink-0 rounded border border-border px-2 py-0.5 text-[12px] hover:border-spark/60 hover:bg-raised"
        >
          Open
        </button>
      )}
    </div>
  );
}
