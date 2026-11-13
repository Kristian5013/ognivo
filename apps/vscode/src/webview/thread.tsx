// The conversation of a thread: the user's messages, the model's reasoning and answers,
// its calls, ognivo's notes, the subagents' tasks and how each turn ended. The list renders
// the rows in sight; a row re-renders when it changes, the list when rows come (§10).

import { memo, useCallback, useState, useSyncExternalStore } from "react";
import { Virtuoso } from "react-virtuoso";

import { Markdown } from "./markdown.tsx";
import { ending } from "./model/format.ts";
import type { Item } from "./model/model.ts";
import type { Row, ThreadStore } from "./model/store.ts";
import { DraftRow, ToolRow } from "./tool.tsx";

const VOID: Record<string, string> = {
  malformed: "calls that did not parse",
  empty: "an empty answer",
  truncated: "an output cut at its token limit",
  repeating: "an output that repeated itself",
  restarted: "an output the server's format check restarted",
};

function Reasoning({ text, live }: { text: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const words = text.trim().split(/\s+/).length;
  return (
    <div className={`reasoning ${live ? "live" : ""}`}>
      <button type="button" className="reasoning-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className={`codicon codicon-chevron-${open ? "down" : "right"}`} />
        <span>{live ? "Thinking…" : "Thought"}</span>
        <span className="muted">{words} words</span>
      </button>
      {(open || live) && <div className={`reasoning-text ${open ? "" : "clipped"}`}>{text}</div>}
    </div>
  );
}

function ItemView({
  item,
  open,
  live,
  store,
  thread,
  onThread,
}: {
  item: Item;
  open: boolean;
  live: boolean;
  store: ThreadStore;
  thread?: number;
  onThread: (thread: number) => void;
}) {
  switch (item.kind) {
    case "message":
      return <Markdown text={item.text} />;
    case "reasoning":
      return <Reasoning text={item.text} live={live} />;
    case "tool":
      return (
        <ToolRow item={item} open={open} onOpen={(o) => store.setOpen(item.id, o)} project={store.info.project} thread={thread} />
      );
    case "notice":
      return (
        <p className="notice">
          <span className="codicon codicon-info" /> {item.text}
        </p>
      );
    case "void":
      return (
        <p className="notice warn">
          <span className="codicon codicon-warning" /> The model's output ran nothing: {VOID[item.why] ?? item.why}.
        </p>
      );
    case "subagent":
      return (
        <button
          type="button"
          className="subagent"
          disabled={item.thread === undefined}
          onClick={() => item.thread !== undefined && onThread(item.thread)}
          title="Open the subagent's thread"
        >
          <span className={`codicon codicon-${item.end ? (item.end === "answer" ? "pass" : "warning") : "loading codicon-modifier-spin"}`} />
          <span className="subagent-title">{item.title}</span>
          <span className="muted">
            {item.write ? "writes" : "reads"}
            {item.steps ? ` · ${item.steps} steps` : ""}
            {item.changed.length ? ` · ${item.changed.length} changed` : ""}
            {item.end && item.end !== "answer" ? ` · ${ending(item.end)}` : ""}
          </span>
        </button>
      );
  }
}

function RowContent({ row, store, thread, onThread }: { row: Row; store: ThreadStore; thread?: number; onThread: (t: number) => void }) {
  switch (row.kind) {
    case "user":
      return (
        <div className="user">
          {row.images.length > 0 && (
            <div className="pictures">
              {row.images.map((p, i) => (
                <img key={i} src={`data:${p.mime};base64,${p.data}`} alt={`image ${i + 1} of the message`} />
              ))}
            </div>
          )}
          <div className="user-text">{row.text}</div>
        </div>
      );
    case "item":
      return <ItemView item={row.item} open={row.open} live={row.live} store={store} thread={thread} onThread={onThread} />;
    case "draft":
      return <DraftRow draft={row.draft} state={row.state} />;
    case "end":
      return row.end === "answer" ? null : (
        <p className="notice warn">
          <span className="codicon codicon-debug-stop" /> Turn {row.turn} {ending(row.end)}
          {row.why ? `: ${row.why}` : ""}
        </p>
      );
    case "rewound":
      return (
        <p className="notice">
          <span className="codicon codicon-history" />{" "}
          {row.from === row.to ? `Turn ${row.from} was` : `Turns ${row.from}–${row.to} were`} taken back and written anew.
        </p>
      );
  }
}

const RowView = memo(function RowView({
  store,
  index,
  thread,
  onThread,
}: {
  store: ThreadStore;
  index: number;
  thread?: number;
  onThread: (t: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeRow(index, l), [store, index]);
  const row = useSyncExternalStore(subscribe, () => store.row(index));
  if (!row) return <div className="row" />;
  return (
    <div className={`row row-${row.kind} ${store.isDropped(index) ? "dropped" : ""}`}>
      <RowContent row={row} store={store} thread={thread} onThread={onThread} />
    </div>
  );
});

export function ThreadView({ store, thread, onThread }: { store: ThreadStore; thread?: number; onThread: (t: number) => void }) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  const length = useSyncExternalStore(subscribe, () => store.length);
  return (
    <Virtuoso
      className="thread"
      totalCount={length}
      computeItemKey={(index) => store.key(index)}
      increaseViewportBy={600}
      initialTopMostItemIndex={Math.max(0, length - 1)}
      // the end follows what comes while the user is near it: a card below the list (an
      // approval) shrinks the list without taking the user away from the end
      atBottomThreshold={160}
      followOutput={(atBottom) => (atBottom ? "auto" : false)}
      itemContent={(index) => <RowView store={store} index={index} thread={thread} onThread={onThread} />}
    />
  );
}
