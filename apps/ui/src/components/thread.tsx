import * as Collapsible from "@radix-ui/react-collapsible";
import { memo, useCallback, useSyncExternalStore } from "react";
import { Virtuoso } from "react-virtuoso";
import { Streamdown } from "streamdown";

import { ending, seconds } from "../format.ts";
import type { VoidItem } from "../model.ts";
import type { Row, ThreadStore } from "../store.ts";
import { ToolCard } from "./tool.tsx";

function Chevron({ open }: { open: boolean }) {
  return (
    <span aria-hidden className={`inline-block transition-transform ${open ? "rotate-90" : ""}`}>
      ›
    </span>
  );
}

/** Generations end after their reasoning: the duration reads the list, which they change. */
function useGenerationMs(store: ThreadStore, generation?: number): number | undefined {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  return useSyncExternalStore(subscribe, () =>
    generation === undefined ? undefined : store.generations[generation]?.generationMs,
  );
}

function Reasoning({
  store,
  row,
}: {
  store: ThreadStore;
  row: Extract<Row, { kind: "item" }> & { item: { kind: "reasoning" } };
}) {
  const ms = useGenerationMs(store, row.item.generation);
  return (
    <Collapsible.Root open={row.open} onOpenChange={(o) => store.setOpen(row.item.id, o)}>
      <Collapsible.Trigger className="flex cursor-pointer items-center gap-2 text-muted-foreground hover:text-foreground">
        <Chevron open={row.open} />
        {row.live ? (
          <span className="animate-pulse">Thinking…</span>
        ) : ms ? (
          `Thought for ${seconds(ms)}`
        ) : (
          "Thought"
        )}
      </Collapsible.Trigger>
      <Collapsible.Content>
        {row.open && (
          <p className="mt-1.5 border-l-2 border-border pl-3 leading-5 whitespace-pre-wrap text-muted-foreground">
            {row.item.text}
          </p>
        )}
      </Collapsible.Content>
    </Collapsible.Root>
  );
}

function Void({ item }: { item: VoidItem }) {
  if (item.why === "empty") {
    return <p className="text-warn">The model stopped inside its reasoning: nothing ran.</p>;
  }
  return (
    <div className="rounded-md border border-warn/40 px-3 py-2">
      <p className="text-warn">The calls of this generation could not be used, so none ran.</p>
      {item.calls.map((c, i) => (
        <pre key={i} className="mt-1 font-mono text-[12px] break-all whitespace-pre-wrap text-muted-foreground">
          {c.name} {c.arguments}
        </pre>
      ))}
    </div>
  );
}

function Content({ store, row }: { store: ThreadStore; row: Row }) {
  switch (row.kind) {
    case "user":
      return (
        <div className="mt-5 rounded-lg bg-raised px-4 py-3">
          <Streamdown className="leading-6">{row.text}</Streamdown>
        </div>
      );
    case "end":
      return (
        <div className="mt-2 mb-6 flex items-center gap-3 text-[12px] text-muted-foreground">
          <span className="h-px flex-1 bg-border" />
          <span className={row.end === "answer" ? undefined : "text-warn"}>
            Turn {row.turn} {ending(row.end)}
            {row.why ? `: ${row.why}` : ""}
          </span>
          <span className="h-px flex-1 bg-border" />
        </div>
      );
    case "item": {
      const item = row.item;
      switch (item.kind) {
        case "reasoning":
          return <Reasoning store={store} row={{ ...row, item }} />;
        case "message":
          return (
            <Streamdown className="leading-6" parseIncompleteMarkdown isAnimating={row.live}>
              {item.text}
            </Streamdown>
          );
        case "tool":
          return (
            <ToolCard item={item} open={row.open} onOpenChange={(o) => store.setOpen(item.id, o)} />
          );
        case "notice":
          return (
            <p className="text-[12px] leading-5 text-muted-foreground">
              <span className="mr-2 font-medium text-spark">ognivo</span>
              {item.text.replace(/^\[ognivo\]\s*/, "")}
            </p>
          );
        case "void":
          return <Void item={item} />;
      }
    }
  }
}

/** One row, re-rendered when that row changes and only then. What a generation made has
 * a way into the Inspector in its margin. */
const RowView = memo(function RowView({
  store,
  index,
  onInspect,
}: {
  store: ThreadStore;
  index: number;
  onInspect?: (generation: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeRow(index, l), [store, index]);
  const row = useSyncExternalStore(subscribe, () => store.row(index));
  if (!row) return null;
  const generation = row.kind === "item" ? row.item.generation : undefined;
  return (
    <div className="group relative mx-auto max-w-3xl px-6 py-1">
      {onInspect && generation !== undefined && (
        <button
          type="button"
          title="Inspect this generation"
          aria-label="Inspect this generation"
          onClick={() => onInspect(generation)}
          className="absolute top-1.5 left-0.5 rounded px-1 text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-spark focus:opacity-100"
        >
          ⌕
        </button>
      )}
      <Content store={store} row={row} />
    </div>
  );
});

/** A thread, turn by turn: only what is on screen is rendered, and it follows the end
 * while the end is in view. `atEnd` opens it at its end, where a stream goes on. */
export function ThreadView({
  store,
  atEnd,
  onInspect,
}: {
  store: ThreadStore;
  atEnd?: boolean;
  onInspect?: (generation: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  const length = useSyncExternalStore(subscribe, () => store.length);
  return (
    <Virtuoso
      className="min-h-0 flex-1"
      totalCount={length}
      computeItemKey={(index) => store.key(index)}
      increaseViewportBy={800}
      initialTopMostItemIndex={atEnd ? Math.max(0, length - 1) : 0}
      followOutput={(atBottom) => (atBottom ? "auto" : false)}
      itemContent={(index) => <RowView store={store} index={index} onInspect={onInspect} />}
    />
  );
}
