import * as Collapsible from "@radix-ui/react-collapsible";
import { memo, useCallback, useContext, useState, useSyncExternalStore } from "react";
import { Virtuoso } from "react-virtuoso";
import { Streamdown } from "streamdown";

import { ending, seconds } from "../format.ts";
import type { VoidItem } from "../model.ts";
import type { Row, ThreadStore } from "../store.ts";
import { Follow, tail } from "./follow.tsx";
import { SubagentCard, ThreadNavContext } from "./subagent.tsx";
import { DraftCard, ToolCard } from "./tool.tsx";

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
    generation === undefined ? undefined : store.generation(generation)?.generationMs,
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
  // while it streams, a window on it; once it ends, a line that opens to all of it
  if (row.live) {
    const text = row.item.text.trim();
    return (
      <div>
        <div className="flex items-center gap-2 text-muted-foreground">
          <span className="spinner" aria-hidden />
          <span className="ember-text">Thinking…</span>
        </div>
        {text && (
          <Follow
            content={row.item.text}
            label="The model's reasoning as it comes"
            className="mt-1.5 max-h-40 border-l-2 border-spark/40 pl-3"
          >
            <p className="leading-5 whitespace-pre-wrap text-muted-foreground">{tail(text)}</p>
          </Follow>
        )}
      </div>
    );
  }
  return (
    <Collapsible.Root open={row.open} onOpenChange={(o) => store.setOpen(row.item.id, o)}>
      <Collapsible.Trigger className="flex cursor-pointer items-center gap-2 text-muted-foreground hover:text-foreground">
        <Chevron open={row.open} />
        {ms ? `Thought for ${seconds(ms)}` : "Thought"}
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
  if (item.why === "restarted") {
    return <p className="text-warn">The server rejected the output's format, and the step started over.</p>;
  }
  if (item.why === "truncated" || item.why === "repeating") {
    const how = item.why === "truncated" ? "reached its token limit" : "kept repeating itself and was stopped";
    return <p className="text-warn">The generation {how}: nothing ran.</p>;
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

/** The user's message; on the core, written anew from here, the turns after it leave the
 * conversation. */
function UserMessage({ row, dropped }: { row: Extract<Row, { kind: "user" }>; dropped: boolean }) {
  const nav = useContext(ThreadNavContext);
  const [text, setText] = useState<string>();
  const [error, setError] = useState<string>();
  const [sending, setSending] = useState(false);
  const edit = nav?.edit;
  if (text !== undefined && edit) {
    const send = () => {
      if (!text.trim() || sending) return;
      setSending(true);
      setError(undefined);
      edit(row.turn, text)
        .then(() => setText(undefined))
        .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
        .finally(() => setSending(false));
    };
    return (
      <form
        className="mt-5 rounded-lg border border-spark/50 bg-raised p-3"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        <textarea
          autoFocus
          aria-label="The message, written anew"
          value={text}
          rows={Math.min(12, text.split("\n").length + 1)}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") setText(undefined);
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              send();
            }
          }}
          className="composer block w-full resize-y rounded-md border border-border bg-panel px-3 py-2 leading-5 outline-none"
        />
        <div className="mt-2 flex flex-wrap items-center gap-2 text-[12px]">
          <span className="min-w-0 flex-1 text-muted-foreground">
            The turns from this one on leave the conversation; what they changed in files stays. Ctrl+Enter sends.
          </span>
          {error && <span className="w-full text-err">{error}</span>}
          <button
            type="button"
            onClick={() => setText(undefined)}
            className="rounded border border-border px-2 py-0.5 hover:bg-panel"
          >
            Cancel
          </button>
          <button
            type="submit"
            disabled={sending || !text.trim() || nav?.busy}
            className="rounded-md bg-spark px-3 py-0.5 font-medium text-background hover:opacity-90 disabled:opacity-40"
          >
            Send
          </button>
        </div>
      </form>
    );
  }
  return (
    <div className="group/user relative mt-5 rounded-lg bg-raised px-4 py-3">
      <Streamdown className="leading-6">{row.text}</Streamdown>
      {edit && !dropped && !nav?.busy && (
        <button
          type="button"
          onClick={() => setText(row.text)}
          className="absolute top-2 right-2 rounded border border-border bg-panel px-2 py-0.5 text-[12px] text-muted-foreground opacity-0 group-hover/user:opacity-100 hover:text-foreground focus:opacity-100"
        >
          Edit
        </button>
      )}
    </div>
  );
}

function Content({ store, row, dropped }: { store: ThreadStore; row: Row; dropped: boolean }) {
  switch (row.kind) {
    case "user":
      return <UserMessage row={row} dropped={dropped} />;
    case "rewound":
      return (
        <div className="mt-2 mb-4 flex items-center gap-3 text-[12px] text-muted-foreground">
          <span className="h-px flex-1 bg-border" />
          <span>
            ↶{" "}
            {row.from === row.to
              ? `Turn ${row.from} taken back: what it changed in files stays`
              : `Turns ${row.from}–${row.to} taken back: what they changed in files stays`}
          </span>
          <span className="h-px flex-1 bg-border" />
        </div>
      );
    case "draft":
      return <DraftCard draft={row.draft} state={row.state} />;
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
        case "subagent":
          return <SubagentCard item={item} live={row.live} />;
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
  // a row that comes while the model works rises into place; one read back does not
  const [rising] = useState(() => (row?.kind === "item" && row.live) || row?.kind === "draft");
  if (!row) return null;
  const generation = row.kind === "item" ? row.item.generation : undefined;
  // a turn taken back stays to be read, out of the way
  const dropped = store.isDropped(index);
  return (
    <div className={`row group relative mx-auto max-w-3xl px-6 ${rising ? "rise" : ""} ${dropped ? "opacity-45" : ""}`}>
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
      <Content store={store} row={row} dropped={dropped} />
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
