// The conversation of a thread: the user's messages, which can be written again, with what
// they carried from the editor; the model's reasoning and answers, a line a step; its calls
// (tool.tsx), those that only look folded into explorations; ognivo's notes, muted; what
// the user wrote while it worked; the subagents' tasks; how each turn ended, with what the
// facts say of its answer, the changes that wait for a review, and what the turn cost; and,
// while a turn runs, a line at the end that says what it does. The list renders the rows in
// sight; a row re-renders when it changes, the list when rows come, the last line as the
// model goes (§10).

import { Fragment, createContext, memo, useCallback, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { type Components, Virtuoso, type VirtuosoHandle } from "react-virtuoso";

import { type ShownChip, splitContext } from "../shared/context.ts";
import * as P from "../shared/protocol.ts";
import { CoreError } from "../shared/rpc.ts";
import { copyText } from "./clipboard.ts";
import { openFile, openReview, request } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { Markdown } from "./markdown.tsx";
import { hunkLines, splitPath } from "./model/diff.ts";
import { duration, ending, explored, plural, saying, short, speed } from "./model/format.ts";
import type { CompactionItem, Item, Picture, SubagentItem } from "./model/model.ts";
import type { Row, Step, ThreadStore, TurnCost } from "./model/store.ts";
import { Delta, DraftPreview, ToolLine, ToolView, callState, shape } from "./tool.tsx";

const VOID: Record<string, string> = {
  malformed: "its calls did not parse",
  empty: "it answered nothing",
  truncated: "it was cut at its token limit",
  repeating: "it repeated itself",
  restarted: "the server's format check restarted it",
};

/** What the user can do to the thread's messages: write one again, from its turn on, with
 * the images it had. */
export interface Editing {
  edit: (turn: number, text: string, images: Picture[]) => Promise<unknown>;
}

/** The turn that stopped short at the thread's end, which can go on from there. */
export const TurnActions = createContext<{ stopped?: number; onContinue?: () => void }>({});

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The model's reasoning: a line that says how long it thought, open to what it thought. */
function Reasoning({ text, live, ms }: { text: string; live: boolean; ms?: number }) {
  const [open, setOpen] = useState(false);
  const label = live ? "Thinking" : ms ? `Thought for ${duration(ms)}` : "Thought";
  const said = text.trim();
  return (
    <div className={`think${live ? " live" : ""}`}>
      <button type="button" className="think-head" aria-expanded={open} disabled={!said} onClick={() => setOpen(!open)}>
        <Icon name="lightbulb" className="think-icon" />
        <span className={live ? "shimmer" : undefined}>{label}</span>
        {said && <Icon name="chevron-right" className="chev" />}
      </button>
      {open && said && <div className="think-text">{said}</div>}
    </div>
  );
}

const CHIP_ICON: Record<ShownChip["kind"], string> = {
  file: "file",
  selection: "selection",
  terminal: "terminal",
  problem: "warning",
};

function UserMessage({ row, editing }: { row: Extract<Row, { kind: "user" }>; editing?: Editing }) {
  // what the user wrote, and the chips of what the message carried from the editor
  const { text, chips } = useMemo(() => splitContext(row.text), [row.text]);
  const block = row.text.slice(text.length).replace(/^\s+/, "");
  const [draft, setDraft] = useState<string>();
  const [error, setError] = useState<string>();
  const [more, setMore] = useState(false);
  const [detail, setDetail] = useState<number>();
  const long = text.split("\n").length > 8 || text.length > 700;
  if (draft !== undefined && editing) {
    const send = () => {
      if (!draft.trim()) return;
      // the message goes again with what it carried, and its images
      editing.edit(row.turn, block ? `${draft.trimEnd()}\n\n${block}` : draft, row.images).then(
        () => setDraft(undefined),
        (e: unknown) => setError(message(e)),
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
        {chips.length > 0 && (
          <div className="msg-chips editing-chips" title="What the message carried goes with it again">
            {chips.map((c, i) => (
              <span key={i} className="chip msg-chip">
                <Icon name={CHIP_ICON[c.kind]} />
                <span className="chip-label">{c.label}</span>
              </span>
            ))}
          </div>
        )}
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
  const shown = detail === undefined ? undefined : chips[detail];
  return (
    <div className="msg-user">
      {row.images.length > 0 && (
        <div className="pictures">
          {row.images.map((p, i) => (
            <img key={i} src={`data:${p.mime};base64,${p.data}`} alt={`image ${i + 1} of the message`} />
          ))}
        </div>
      )}
      {text && <div className={`msg-user-text${long && !more ? " clamped" : ""}`}>{text}</div>}
      {long && (
        <button type="button" className="more" onClick={() => setMore(!more)}>
          {more ? "Show less" : "Show more"}
        </button>
      )}
      {chips.length > 0 && (
        <div className="msg-chips">
          {chips.map((c, i) => (
            <button
              key={i}
              type="button"
              className={`chip msg-chip${detail === i ? " on" : ""}`}
              aria-expanded={detail === i}
              title={detail === i ? "Hide what it carried" : "Show what it carried"}
              onClick={() => setDetail(detail === i ? undefined : i)}
            >
              <Icon name={CHIP_ICON[c.kind]} />
              <span className="chip-label">{c.label}</span>
            </button>
          ))}
        </div>
      )}
      {shown && (
        <div className="msg-chip-detail">
          <Markdown text={shown.detail} />
        </div>
      )}
      <div className="msg-actions">
        <IconButton icon="copy" title="Copy" onClick={() => void copyText(text || row.text)} />
        {editing && <IconButton icon="edit" title="Edit and send again" onClick={() => setDraft(text)} />}
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

/** A note of ognivo's to the model: a muted line, all of it on a click. */
function Notice({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const clean = text.replace(/^\[ognivo\]\s*/, "");
  return (
    <button
      type="button"
      className={`note notice${open ? " open" : ""}`}
      aria-expanded={open}
      onClick={() => setOpen(!open)}
      title={open ? undefined : clean}
    >
      <Icon name="info" />
      <span className="note-text">{clean}</span>
    </button>
  );
}

/** What the user wrote while the turn ran, where the model got it. */
function Steer({ text }: { text: string }) {
  const { text: said, chips } = useMemo(() => splitContext(text), [text]);
  return (
    <div className="steer" title="You wrote this while the agent worked: it read it before its next step">
      <span className="steer-mark" aria-hidden="true">
        ↳
      </span>
      {/* the words and the chips wrap beside the mark */}
      <span className="steer-body">
        <span className="steer-text">{said}</span>
        {chips.map((c, i) => (
          <span key={i} className="steer-chip">
            <Icon name={CHIP_ICON[c.kind]} />
            {c.label}
          </span>
        ))}
      </span>
    </div>
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

/** How long a generation's reasoning took, about: the generation's time. */
function thoughtMs(store: ThreadStore, item: Item): number | undefined {
  return item.generation === undefined ? undefined : store.generation(item.generation)?.generationMs;
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
      return <Reasoning text={item.text} live={live} ms={thoughtMs(store, item)} />;
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
    // the question itself is the card above the message; here, what it asked and the answer
    case "approval":
      return (
        <p className={`note quiet${item.allowed === false ? " warn" : ""}`}>
          <Icon name={item.allowed === undefined ? "shield" : item.allowed ? "pass" : "circle-slash"} />
          <span className="note-text">
            {item.agent ? `Subagent «${item.agent}»: ` : ""}
            {item.allowed === undefined ? "Asked" : item.allowed ? "Allowed" : "Denied"} <code>{item.action}</code>
          </span>
        </p>
      );
    case "instructions":
      return (
        <p className="note quiet">
          <Icon name="book" />
          <span className="note-text">
            Instructions: {item.files.join(", ")}
            {item.skipped.length > 0 && <span className="muted"> · not read: {item.skipped.map((s) => `${s.path} (${s.why})`).join(", ")}</span>}
          </span>
        </p>
      );
    case "compaction":
      return <Compaction item={item} live={live} />;
    case "steer":
      return <Steer text={item.text} />;
  }
}

/** An exploration: a line that says what the calls looked at, open to each of them and to
 * the thoughts between them. */
function Steps({ row, store }: { row: Extract<Row, { kind: "steps" }>; store: ThreadStore }) {
  const calls = row.steps.flatMap((s) => (s.item.kind === "tool" ? [s.item] : []));
  const states = calls.map(callState);
  const running = states.includes("running");
  const failed = states.filter((s) => s === "failed").length;
  const refused = states.filter((s) => s === "refused").length;
  const open = Boolean(row.open);
  return (
    <div className={`steps${running ? " is-running" : ""}`}>
      <button type="button" className="steps-head" aria-expanded={open} onClick={() => store.openSteps(row.key, !open)}>
        <Icon name={running ? "loading" : "search"} spin={running} className="steps-icon" />
        <span className="steps-say">
          {running ? "Exploring" : "Explored"} {explored(calls)}
        </span>
        {failed > 0 && <span className="tag err">{failed} failed</span>}
        {refused > 0 && <span className="tag warn">{refused} not allowed</span>}
        <Icon name="chevron-right" className="chev" />
      </button>
      {open && (
        <div className="steps-body">
          {row.steps.map((s) => (
            <StepView key={s.item.id} step={s} store={store} />
          ))}
        </div>
      )}
    </div>
  );
}

function StepView({ step, store }: { step: Step; store: ThreadStore }) {
  const { item } = step;
  if (item.kind === "tool") {
    return <ToolLine item={item} open={step.open} onOpen={(o) => store.setOpen(item.id, o)} project={store.info.project} />;
  }
  if (item.kind === "reasoning") return <Reasoning text={item.text} live={step.live} ms={thoughtMs(store, item)} />;
  return null;
}

/** What the facts of a turn say of its answer: `Verified`, or `Not verified` as a warning,
 * and the facts. */
function Verdict({ text }: { text: string }) {
  const not = /^not verified/i.test(text);
  const said = text.replace(/^(not )?verified:?\s*/i, "");
  return (
    <>
      <Icon name={not ? "warning" : "pass-filled"} className="verdict-icon" />
      <span className="verdict-text">
        <strong>{not ? "Not verified" : "Verified"}</strong>
        {said && (
          <>
            {" "}
            <Markdown text={said} inline />
          </>
        )}
      </span>
    </>
  );
}

function notVerified(text: string | undefined): boolean {
  return text !== undefined && /^not verified/i.test(text);
}

/** The thread's changes that wait for a review, while `active`: from the core, again as
 * turns end, changes are kept or undone, and the window comes back. */
function useUnreviewed(store: ThreadStore, thread: number | undefined, active: boolean) {
  const version = useSyncExternalStore(
    useCallback((l: () => void) => store.subscribeChanges(l), [store]),
    () => store.changesVersion,
  );
  const [files, setFiles] = useState<P.ChangedFile[]>([]);
  const [tick, setTick] = useState(0);
  const core = store.source?.core;
  useEffect(() => {
    if (!active || !core || thread === undefined) return;
    let live = true;
    core.call("changes/list", { thread }).then(
      (c) => live && setFiles(c.files),
      (e: unknown) => {
        // a core that holds the journal while a turn runs: what showed stays
        if (live && !(e instanceof CoreError && e.code === P.ERRORS.busy)) setFiles([]);
      },
    );
    return () => {
      live = false;
    };
  }, [core, thread, active, version, tick]);
  // the editor may have kept or undone some meanwhile
  useEffect(() => {
    if (!active) return;
    const again = () => setTick((t) => t + 1);
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, [active]);
  return { files: active ? files : [], again: () => setTick((t) => t + 1) };
}

/** What the user did to the changes, said once they are gone from the card. */
interface Done {
  icon: string;
  said: string;
}

/** The changes that wait for a review at the end of the thread's last turn: each file, open
 * on a click; all of them in the editor's review, undone or kept. */
function Review({
  files,
  thread,
  project,
  verification,
  onDone,
}: {
  files: P.ChangedFile[];
  thread: number;
  project: string;
  verification?: string;
  onDone: (done: Done) => void;
}) {
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const n = plural(files.length, "file");
  const act = (method: "changes/accept" | "changes/revert", done: Done) => {
    setBusy(true);
    request(method, { thread, path: null }).then(
      (did) => {
        setBusy(false);
        setError(undefined);
        if (did) onDone(done);
      },
      (e: unknown) => {
        setBusy(false);
        setError(message(e));
      },
    );
  };
  return (
    <div className={`summary${notVerified(verification) ? " is-warn" : ""}`}>
      <div className="summary-head">
        {verification ? (
          <Verdict text={verification} />
        ) : (
          <>
            <Icon name="diff" className="verdict-icon" />
            <span className="verdict-text">
              <strong>{n} changed</strong> <span className="muted">not reviewed yet</span>
            </span>
          </>
        )}
      </div>
      <ul className="summary-files">
        {files.map((f) => {
          const { name, dir } = splitPath(f.path);
          const lines = hunkLines(f.hunks);
          // a file that is gone opens as its change
          const open = () => (f.deleted ? void openReview(thread, f.path).catch((e: unknown) => setError(message(e))) : openFile(f.path, project, f.hunks[0]?.new_start || undefined));
          return (
            <li key={f.path}>
              <button type="button" className="summary-file" title={`Open ${f.path}`} onClick={open}>
                <Icon name={f.deleted ? "trash" : f.created ? "new-file" : "file"} className="summary-icon" />
                <span className="summary-name">{name}</span>
                {dir && <span className="fdir">{dir}</span>}
                {f.created && <span className="tag">new</span>}
                {f.deleted && <span className="tag">deleted</span>}
                {f.drifted && <Icon name="warning" className="warn" title="Something else changed it too since the agent's edit" />}
                <span className="spacer" />
                <Delta added={lines.added} removed={lines.removed} />
              </button>
              <IconButton
                icon="diff"
                title={`Review the change to ${name}`}
                className="summary-act"
                onClick={() => void openReview(thread, f.path).catch((e: unknown) => setError(message(e)))}
              />
            </li>
          );
        })}
      </ul>
      <div className="summary-actions">
        <button
          type="button"
          className="btn secondary small"
          title="Every changed file's diff in one tab of the editor"
          onClick={() => void openReview(thread, null).catch((e: unknown) => setError(message(e)))}
        >
          Review all
        </button>
        <span className="spacer" />
        <button
          type="button"
          className="btn ghost small"
          disabled={busy}
          title="Put every file back as it was before the session"
          onClick={() => act("changes/revert", { icon: "discard", said: `Undid the changes to ${n}` })}
        >
          Undo all
        </button>
        <button
          type="button"
          className="btn primary small"
          disabled={busy}
          title="Keep every change as it is"
          onClick={() => act("changes/accept", { icon: "check", said: `Kept the changes to ${n}` })}
        >
          Keep all
        </button>
      </div>
      {error && <p className="summary-error">{error}</p>}
    </div>
  );
}

/** What a turn cost: the model, how big the conversation was at its end and how much of its
 * prompts came from the server's cache, and how fast the model wrote. */
function TurnMeta({ cost, model }: { cost: TurnCost; model: string }) {
  const tokens = cost.last.prompt + cost.last.completion;
  const cached = cost.prompt ? Math.round((cost.cached / cost.prompt) * 100) : 0;
  const perSecond = cost.generationMs > 0 && cost.completion > 0 ? cost.completion / (cost.generationMs / 1000) : undefined;
  const said = [model, `${short(tokens)} tokens, ${cached}% from cache`, perSecond !== undefined && speed(perSecond)];
  const title =
    `The conversation held ${tokens.toLocaleString("en")} tokens at the turn's end. ` +
    `${plural(cost.generations, "request")} sent ${cost.prompt.toLocaleString("en")} prompt tokens, ${cached}% of them from the server's cache` +
    `; the model wrote ${cost.completion.toLocaleString("en")} tokens${perSecond !== undefined ? `, ${speed(perSecond)}` : ""}.`;
  return (
    <p className="turn-meta" title={title}>
      {said
        .filter((s): s is string => Boolean(s))
        .map((s, i) => (
          <Fragment key={i}>
            {i > 0 && " · "}
            {/* a figure stays on one line with its unit; the line breaks between them */}
            <span className={i > 0 ? "turn-meta-fact" : undefined}>{s}</span>
          </Fragment>
        ))}
    </p>
  );
}

/** How a turn ended: what the facts say of its answer, the changes that wait for a review
 * when it is the thread's last, and what it cost; a turn that stopped short says why and
 * can go on. */
function TurnEnd({
  row,
  index,
  store,
  thread,
}: {
  row: Extract<Row, { kind: "end" }>;
  index: number;
  store: ThreadStore;
  thread?: number;
}) {
  const actions = useContext(TurnActions);
  const last = useSyncExternalStore(
    useCallback((l: () => void) => store.subscribeList(l), [store]),
    () => index === store.length - 1,
  );
  const review = useUnreviewed(store, thread, last && !store.isDropped(index));
  const [done, setDone] = useState<Done>();
  const cost = useMemo(() => store.turnCost(row.turn), [store, row.turn]);
  const files = review.files;
  return (
    <div className="turn-end">
      {row.end !== "answer" && (
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
      )}
      {files.length > 0 && thread !== undefined ? (
        <Review
          files={files}
          thread={thread}
          project={store.info.project}
          verification={row.verification}
          onDone={(d) => {
            setDone(d);
            review.again();
          }}
        />
      ) : (
        row.verification && (
          <p className={`verdict ${notVerified(row.verification) ? "warn" : "ok"}`}>
            <Verdict text={row.verification} />
          </p>
        )
      )}
      {done && files.length === 0 && (
        <p className="summary-done">
          <Icon name={done.icon} /> {done.said}
        </p>
      )}
      {cost && <TurnMeta cost={cost} model={store.info.model} />}
    </div>
  );
}

function RowContent({
  row,
  index,
  store,
  thread,
  onThread,
  editing,
}: {
  row: Row;
  index: number;
  store: ThreadStore;
  thread?: number;
  onThread: (t: number) => void;
  editing?: Editing;
}) {
  switch (row.kind) {
    case "user":
      return <UserMessage row={row} editing={editing} />;
    case "item":
      return <ItemView item={row.item} open={row.open} live={row.live} store={store} thread={thread} onThread={onThread} />;
    case "steps":
      return <Steps row={row} store={store} />;
    case "end":
      return <TurnEnd row={row} index={index} store={store} thread={thread} />;
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
    case "steps":
      return "line";
    case "end":
      return "verdict";
    case "rewound":
      return "divider";
    case "item":
      switch (row.item.kind) {
        case "message":
          return "text";
        case "tool":
          return shape(row.item.call?.name) === "change" ? "card" : "line";
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
      <RowContent row={row} index={index} store={store} thread={thread} onThread={onThread} editing={editing} />
    </div>
  );
});

/** Re-renders every `ms` while it is not 0: a clock that shows. */
function useTick(ms: number): void {
  const [, setTick] = useState(0);
  useEffect(() => {
    if (!ms) return;
    const id = setInterval(() => setTick((t) => t + 1), ms);
    return () => clearInterval(id);
  }, [ms]);
}

/** ognivo's spark, which turns while the agent works. */
function Spark() {
  return (
    <svg className="live-spark" viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
      <path d="M12 1.5 14.1 9.9 22.5 12 14.1 14.1 12 22.5 9.9 14.1 1.5 12 9.9 9.9Z" />
    </svg>
  );
}

interface Context {
  store: ThreadStore;
  thread?: number;
  /** Keeps the end in sight, where the user is at it, when the last line grows. */
  follow: () => void;
}

/** The line at the end of a running turn: what it does, its facts and its time, and the
 * last lines of a call being written. It alone hears the model's progress. */
function LiveLine({ store, thread, follow }: Context) {
  const live = useSyncExternalStore(
    useCallback((l: () => void) => store.subscribeLive(l), [store]),
    () => store.live,
  );
  const core = store.source?.core;
  // a question of the thread's that waits for the user: the card above the message
  const asked = useSyncExternalStore(
    useCallback((l: () => void) => core?.subscribe(l) ?? (() => {}), [core]),
    () => core !== undefined && thread !== undefined && [...core.asks.values()].some((a) => a.method === "approval/ask" && a.params.thread === thread),
  );
  useTick(live.running ? 1000 : 0);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = box.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const sizes = new ResizeObserver(() => follow());
    sizes.observe(el);
    return () => sizes.disconnect();
  }, [follow]);
  const s = live.running ? saying(live, { asking: asked, now: Date.now(), project: store.info.project }) : undefined;
  return (
    <div className="thread-bottom live-box" ref={box}>
      {s && (
        <>
          <div className={`live-line${s.tone === "ask" ? " is-asking" : ""}`}>
            {s.tone === "ask" ? <Icon name="bell-dot" className="live-icon" /> : <Spark />}
            {/* the facts wrap under the words, not under the spark */}
            <span className="live-text">
              <span className="live-say">{s.say}</span>
              {s.facts.length > 0 && <span className="live-facts">{s.facts.join(" · ")}</span>}
            </span>
          </div>
          {s.writing && <DraftPreview draft={s.writing} />}
        </>
      )}
    </div>
  );
}

/** How near the end, in pixels, the user still is at it: the end follows what comes. */
const AT_END = 160;

const Top = () => <div className="thread-top" />;
const Bottom = ({ context }: { context?: Context }) => (context ? <LiveLine {...context} /> : <div className="thread-bottom" />);
const COMPONENTS: Components<unknown, Context> = { Header: Top, Footer: Bottom };

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
  // the list as it changes, not only its length: an exploration that takes in the thought
  // before it and a row that comes in the same frame leave the length as it was, and the
  // row that held the thought must go to the row that came
  useSyncExternalStore(subscribe, () => store.listVersion);
  const length = store.length;
  const list = useRef<VirtuosoHandle>(null);
  const [atEnd, setAtEnd] = useState(true);
  const following = useRef(true);
  const onAtEnd = useCallback((at: boolean) => {
    following.current = at;
    setAtEnd(at);
  }, []);
  // the last line grows as a call's lines show, which adds no row for Virtuoso to follow:
  // where the user is at the end, the end, all of it, stays in sight
  const follow = useCallback(() => {
    if (following.current) list.current?.scrollTo({ top: Number.MAX_SAFE_INTEGER });
  }, []);
  const context = useMemo<Context>(() => ({ store, thread, follow }), [store, thread, follow]);
  // The dock below grows as the user writes a long message, a question comes or messages
  // queue, and the list gets as much shorter, from below: where the user was at the end, the
  // end, the last line with it, stays in sight above the dock. Where the user was is what
  // the last scroll left (a shorter list scrolls nothing), not Virtuoso's at-the-end, which
  // the shrinking itself may already have turned off.
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const onScroller = useCallback((el: HTMLElement | Window | null) => setScroller(el instanceof HTMLElement ? el : null), []);
  useEffect(() => {
    if (!scroller || typeof ResizeObserver === "undefined") return;
    let fromEnd = 0;
    let height = scroller.clientHeight;
    const onScroll = () => {
      fromEnd = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
    };
    const sizes = new ResizeObserver(() => {
      const now = scroller.clientHeight;
      if (now < height && fromEnd <= AT_END) list.current?.scrollTo({ top: Number.MAX_SAFE_INTEGER });
      height = now;
    });
    scroller.addEventListener("scroll", onScroll, { passive: true });
    sizes.observe(scroller);
    return () => {
      sizes.disconnect();
      scroller.removeEventListener("scroll", onScroll);
    };
  }, [scroller]);
  return (
    <div className="thread-wrap">
      <Virtuoso
        ref={list}
        scrollerRef={onScroller}
        className="thread"
        context={context}
        totalCount={length}
        computeItemKey={(index) => store.key(index)}
        increaseViewportBy={600}
        initialTopMostItemIndex={Math.max(0, length - 1)}
        components={COMPONENTS}
        // the end follows what comes while the user is near it: a card below the list (an
        // approval) shrinks the list without taking the user away from the end
        atBottomThreshold={AT_END}
        atBottomStateChange={onAtEnd}
        followOutput={(atBottom) => (atBottom ? "auto" : false)}
        itemContent={(index) => <RowView store={store} index={index} thread={thread} onThread={onThread} editing={editing} />}
      />
      {!atEnd && length > 0 && (
        <button
          type="button"
          className="to-end"
          title="To the end of the conversation"
          aria-label="To the end of the conversation"
          onClick={() => list.current?.scrollTo({ top: Number.MAX_SAFE_INTEGER, behavior: "smooth" })}
        >
          <Icon name="arrow-down" />
        </button>
      )}
    </div>
  );
}
