// The conversation of a thread: the user's messages, which can be written again; the
// model's reasoning and answers; its calls (tool.tsx); ognivo's notes; the subagents'
// tasks; and how each turn ended, with what the facts say of its answer. The list renders
// the rows in sight; a row re-renders when it changes, the list when rows come (§10).

import { createContext, memo, useCallback, useContext, useRef, useState, useSyncExternalStore } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";

import { copyText } from "./clipboard.ts";
import { Icon, IconButton } from "./icon.tsx";
import { Markdown } from "./markdown.tsx";
import { duration, ending } from "./model/format.ts";
import type { CompactionItem, Item, SubagentItem } from "./model/model.ts";
import type { Row, ThreadStore } from "./model/store.ts";
import { DraftView, ToolView, shape } from "./tool.tsx";

const VOID: Record<string, string> = {
  malformed: "its calls did not parse",
  empty: "it answered nothing",
  truncated: "it was cut at its token limit",
  repeating: "it repeated itself",
  restarted: "the server's format check restarted it",
};

/** What the user can do to the thread's messages: write one again, from its turn on. */
export interface Editing {
  edit: (turn: number, text: string) => Promise<unknown>;
}

/** The turn that stopped short at the thread's end, which can go on from there. */
export const TurnActions = createContext<{ stopped?: number; onContinue?: () => void }>({});

function Reasoning({ text, live, ms }: { text: string; live: boolean; ms?: number }) {
  const [open, setOpen] = useState(false);
  const label = live ? "Thinking" : ms ? `Thought for ${duration(ms)}` : "Thought";
  return (
    <div className={`think${live ? " live" : ""}`}>
      <button type="button" className="think-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className={live ? "shimmer" : undefined}>{label}</span>
        <Icon name={open ? "chevron-down" : "chevron-right"} className="chev" />
      </button>
      {(open || live) && text.trim() && <div className={`think-text${open ? "" : " tail"}`}>{text.trim()}</div>}
    </div>
  );
}

function UserMessage({ row, editing }: { row: Extract<Row, { kind: "user" }>; editing?: Editing }) {
  const [draft, setDraft] = useState<string>();
  const [error, setError] = useState<string>();
  const [more, setMore] = useState(false);
  const long = row.text.split("\n").length > 8 || row.text.length > 700;
  if (draft !== undefined && editing) {
    const send = () => {
      if (!draft.trim()) return;
      editing.edit(row.turn, draft).then(
        () => setDraft(undefined),
        (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
      );
    };
    return (
      <div className="msg-user editing">
        <textarea
          autoFocus
          aria-label="The message, written again"
          value={draft}
          rows={Math.min(10, Math.max(2, draft.split("\n").length))}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              send();
            } else if (e.key === "Escape") {
              e.preventDefault();
              setDraft(undefined);
            }
          }}
        />
        {error && <p className="error">{error}</p>}
        <div className="msg-edit-bar">
          <span className="muted">The conversation goes back to this message and goes on from it.</span>
          <span className="spacer" />
          <button type="button" className="btn ghost" onClick={() => setDraft(undefined)}>
            Cancel
          </button>
          <button type="button" className="btn primary" disabled={!draft.trim()} onClick={send}>
            Send
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className="msg-user">
      {row.images.length > 0 && (
        <div className="pictures">
          {row.images.map((p, i) => (
            <img key={i} src={`data:${p.mime};base64,${p.data}`} alt={`image ${i + 1} of the message`} />
          ))}
        </div>
      )}
      <div className={`msg-user-text${long && !more ? " clamped" : ""}`}>{row.text}</div>
      {long && (
        <button type="button" className="more" onClick={() => setMore(!more)}>
          {more ? "Show less" : "Show more"}
        </button>
      )}
      <div className="msg-actions">
        <IconButton icon="copy" title="Copy" onClick={() => void copyText(row.text)} />
        {editing && <IconButton icon="edit" title="Edit and send again" onClick={() => setDraft(row.text)} />}
      </div>
    </div>
  );
}

function AgentMessage({ text }: { text: string }) {
  return (
    <div className="msg-agent">
      <Markdown text={text} />
      <div className="msg-actions">
        <IconButton icon="copy" title="Copy" onClick={() => void copyText(text)} />
      </div>
    </div>
  );
}

/** A note of ognivo's to the model, in few lines; all of it on a click. */
function Notice({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const clean = text.replace(/^\[ognivo\]\s*/, "");
  return (
    <button type="button" className={`note${open ? " open" : ""}`} onClick={() => setOpen(!open)} title={open ? undefined : clean}>
      <Icon name="info" />
      <span className="note-text">{clean}</span>
    </button>
  );
}

function Subagent({ item, onThread }: { item: SubagentItem; onThread: (thread: number) => void }) {
  const state = !item.end ? "running" : item.end === "answer" ? "ok" : "failed";
  return (
    <button
      type="button"
      className={`sub is-${state}`}
      disabled={item.thread === undefined}
      onClick={() => item.thread !== undefined && onThread(item.thread)}
      title="Open the subagent's thread"
    >
      <Icon name={state === "running" ? "loading" : state === "ok" ? "pass" : "warning"} spin={state === "running"} className="sub-icon" />
      <span className="sub-body">
        <span className="sub-title">{item.title}</span>
        <span className="sub-meta">
          Subagent · {item.write ? "writes" : "reads"}
          {item.steps ? ` · ${item.steps} steps` : ""}
          {item.changed.length ? ` · ${item.changed.length} changed` : ""}
          {item.end && item.end !== "answer" ? ` · ${ending(item.end)}` : ""}
        </span>
      </span>
      <Icon name="chevron-right" className="chev" />
    </button>
  );
}

function Compaction({ item, live }: { item: CompactionItem; live: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="divider-block">
      <button type="button" className="divider" disabled={!item.summary} onClick={() => setOpen(!open)}>
        {live ? <Icon name="loading" spin /> : <Icon name="fold" />}
        <span>
          {live
            ? `Compacting the conversation: about ${Math.round(item.tokens / 1000)}k tokens`
            : `Conversation compacted${item.replaced ? `: ${item.replaced} messages summed up` : ""}`}
        </span>
      </button>
      {open && item.summary && <div className="think-text">{item.summary}</div>}
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
  open?: boolean;
  live: boolean;
  store: ThreadStore;
  thread?: number;
  onThread: (thread: number) => void;
}) {
  switch (item.kind) {
    case "message":
      return <AgentMessage text={item.text} />;
    case "reasoning":
      return <Reasoning text={item.text} live={live} ms={item.generation === undefined ? undefined : store.generation(item.generation)?.generationMs} />;
    case "tool":
      return <ToolView item={item} open={open} onOpen={(o) => store.setOpen(item.id, o)} project={store.info.project} thread={thread} />;
    case "notice":
      return <Notice text={item.text} />;
    case "void":
      return (
        <p className="note warn">
          <Icon name="warning" />
          <span className="note-text">The model's output ran nothing: {VOID[item.why] ?? item.why}.</span>
        </p>
      );
    case "subagent":
      return <Subagent item={item} onThread={onThread} />;
    case "approval":
      return (
        <p className={`note${item.allowed === false ? " warn" : ""}`}>
          <Icon name={item.allowed === undefined ? "shield" : item.allowed ? "pass" : "circle-slash"} />
          <span className="note-text">
            {item.agent ? `Subagent «${item.agent}»: ` : ""}
            {item.allowed === undefined ? "Waits for your answer" : item.allowed ? "Allowed" : "Denied"} <code>{item.action}</code>
          </span>
        </p>
      );
    case "instructions":
      return (
        <p className="note">
          <Icon name="book" />
          <span className="note-text">
            Instructions: {item.files.join(", ")}
            {item.skipped.length > 0 && <span className="muted"> · not read: {item.skipped.map((s) => `${s.path} (${s.why})`).join(", ")}</span>}
          </span>
        </p>
      );
    case "compaction":
      return <Compaction item={item} live={live} />;
  }
}

/** What the facts of a turn say of its answer. */
function Verification({ text }: { text: string }) {
  const not = /^not verified/i.test(text);
  const said = text.replace(/^(not )?verified:?\s*/i, "");
  return (
    <p className={`verdict ${not ? "warn" : "ok"}`}>
      <Icon name={not ? "warning" : "pass-filled"} />
      <span>
        <strong>{not ? "Not verified" : "Verified"}</strong>
        {said && (
          <>
            {": "}
            <Markdown text={said} inline />
          </>
        )}
      </span>
    </p>
  );
}

function RowContent({
  row,
  store,
  thread,
  onThread,
  editing,
}: {
  row: Row;
  store: ThreadStore;
  thread?: number;
  onThread: (t: number) => void;
  editing?: Editing;
}) {
  const actions = useContext(TurnActions);
  switch (row.kind) {
    case "user":
      return <UserMessage row={row} editing={editing} />;
    case "item":
      return <ItemView item={row.item} open={row.open} live={row.live} store={store} thread={thread} onThread={onThread} />;
    case "draft":
      return <DraftView draft={row.draft} state={row.state} project={store.info.project} />;
    case "end":
      if (row.end === "answer") return row.verification ? <Verification text={row.verification} /> : null;
      return (
        <div className="stopped">
          <p className="note warn">
            <Icon name="debug-stop" />
            <span className="note-text">
              Turn {row.turn} {ending(row.end)}
              {row.why ? `: ${row.why}` : ""}
            </span>
          </p>
          {actions.stopped === row.turn && actions.onContinue && (
            <button type="button" className="btn secondary small" onClick={actions.onContinue}>
              <Icon name="debug-continue" /> Continue
            </button>
          )}
        </div>
      );
    case "rewound":
      return (
        <div className="divider-block">
          <span className="divider">
            <Icon name="history" />
            <span>{row.from === row.to ? `Turn ${row.from} was` : `Turns ${row.from}–${row.to} were`} taken back and written anew</span>
          </span>
        </div>
      );
  }
}

/** How a row sits among the others: its room above and below follows. */
function variant(row: Row): string {
  switch (row.kind) {
    case "user":
      return "user";
    case "draft":
      return shape(row.draft.tool) === "line" ? "line" : "card";
    case "end":
      return row.end === "answer" ? "verdict" : "line";
    case "rewound":
      return "divider";
    case "item":
      switch (row.item.kind) {
        case "message":
          return "text";
        case "tool":
          return shape(row.item.call?.name) === "line" ? "line" : "card";
        case "subagent":
          return "card";
        case "compaction":
          return "divider";
        default:
          return "line";
      }
  }
}

const RowView = memo(function RowView({
  store,
  index,
  thread,
  onThread,
  editing,
}: {
  store: ThreadStore;
  index: number;
  thread?: number;
  onThread: (t: number) => void;
  editing?: Editing;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeRow(index, l), [store, index]);
  const row = useSyncExternalStore(subscribe, () => store.row(index));
  if (!row) return <div className="row" />;
  return (
    <div className={`row v-${variant(row)}${store.isDropped(index) ? " dropped" : ""}`}>
      <RowContent row={row} store={store} thread={thread} onThread={onThread} editing={editing} />
    </div>
  );
});

const Top = () => <div className="thread-top" />;
const Bottom = () => <div className="thread-bottom" />;

export function ThreadView({
  store,
  thread,
  onThread,
  editing,
}: {
  store: ThreadStore;
  thread?: number;
  onThread: (t: number) => void;
  /** None while a turn runs, or for a subagent's thread. */
  editing?: Editing;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  const length = useSyncExternalStore(subscribe, () => store.length);
  const list = useRef<VirtuosoHandle>(null);
  const [atEnd, setAtEnd] = useState(true);
  return (
    <div className="thread-wrap">
      <Virtuoso
        ref={list}
        className="thread"
        totalCount={length}
        computeItemKey={(index) => store.key(index)}
        increaseViewportBy={600}
        initialTopMostItemIndex={Math.max(0, length - 1)}
        components={{ Header: Top, Footer: Bottom }}
        // the end follows what comes while the user is near it: a card below the list (an
        // approval) shrinks the list without taking the user away from the end
        atBottomThreshold={160}
        atBottomStateChange={setAtEnd}
        followOutput={(atBottom) => (atBottom ? "auto" : false)}
        itemContent={(index) => <RowView store={store} index={index} thread={thread} onThread={onThread} editing={editing} />}
      />
      {!atEnd && length > 0 && (
        <button
          type="button"
          className="to-end"
          title="To the end of the conversation"
          aria-label="To the end of the conversation"
          onClick={() => list.current?.scrollToIndex({ index: length - 1, align: "end", behavior: "smooth" })}
        >
          <Icon name="arrow-down" />
        </button>
      )}
    </div>
  );
}
