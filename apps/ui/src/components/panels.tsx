import { McpPanel } from "./mcp.tsx";
import { useCallback, useEffect, useState, useSyncExternalStore } from "react";

import { type Ask, type Core, CoreError } from "../core.ts";
import { lineDelta, refusalReason, summary } from "../format.ts";
import type { ToolItem } from "../model.ts";
import * as P from "../protocol.ts";
import type { ThreadStore } from "../store.ts";
import { EditDiff } from "./diff.tsx";
import { Inspector } from "./inspector.tsx";
import { Resizer, useWidth } from "./resizer.tsx";

export type Panel = "changes" | "approvals" | "inspector" | "mcp";

/** The side panel's first width. */
const PANEL = 440;

export type ApprovalAsk = Extract<Ask, { method: "approval/ask" }>;

/** What the side panels do with a thread on the core: its edit journal, and the questions
 * it waits on. `open`: the core has the thread open; a thread only read has no journal
 * there. */
export interface LiveThread {
  core: Core;
  thread: number;
  /** The thread's project root. */
  project: string;
  open: boolean;
  /** Changes when the thread may have changed files: a turn's end, a call's. */
  version: number;
  asks: ApprovalAsk[];
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function Empty({ children }: { children: string }) {
  return <p className="m-auto px-6 text-center text-muted-foreground">{children}</p>;
}

/** What a file's calls added and removed, edit by edit; a write adds its lines. */
function delta(calls: ToolItem[]): { added: number; removed: number } {
  return calls.reduce(
    (sum, c) => {
      const d =
        c.call?.name === "Edit"
          ? lineDelta(c.call.old, c.call.new)
          : c.call?.name === "Write"
            ? lineDelta("", c.call.content)
            : { added: 0, removed: 0 };
      return { added: sum.added + d.added, removed: sum.removed + d.removed };
    },
    { added: 0, removed: 0 },
  );
}

function Change({ call, n }: { call: ToolItem; n: number }) {
  const c = call.call;
  const created = call.result?.text.startsWith("[write: created");
  return (
    <section className="border-b border-border">
      <header className="px-3 py-1.5 text-[11px] text-muted-foreground">
        {n}. {c?.name === "Write" ? (created ? "created" : "written over") : "edited"}
      </header>
      {c?.name === "Edit" && <EditDiff path={c.path} before={c.old} after={c.new} />}
      {c?.name === "Write" && (
        <pre className="max-h-96 overflow-auto px-3 pb-2 font-mono text-[12px] leading-5 whitespace-pre-wrap">
          {c.content}
        </pre>
      )}
    </section>
  );
}

/** The session's changes, file by file, as its calls made them. Restoring one needs the
 * core's edit journal (§8), which a session the core has open shows instead. */
function Changes({ files }: { files: { path: string; calls: ToolItem[] }[] }) {
  const [selected, setSelected] = useState<string>();
  const file = files.find((f) => f.path === selected) ?? files[0];
  if (!file) return <Empty>No changes in this session yet.</Empty>;
  return (
    <>
      <ul className="max-h-48 shrink-0 overflow-y-auto border-b border-border py-1">
        {files.map((f) => {
          const d = delta(f.calls);
          return (
            <li key={f.path}>
              <button
                type="button"
                onClick={() => setSelected(f.path)}
                className={`flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-raised ${f === file ? "bg-raised" : ""}`}
              >
                <span className="min-w-0 flex-1 truncate font-mono text-[12px]">{f.path}</span>
                <span className="text-[11px] text-ok">+{d.added}</span>
                <span className="text-[11px] text-err">−{d.removed}</span>
              </button>
            </li>
          );
        })}
      </ul>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {file.calls.map((c, i) => (
          <Change key={c.id} call={c} n={i + 1} />
        ))}
      </div>
      <footer className="flex shrink-0 items-center gap-3 border-t border-border px-3 py-2 text-[12px]">
        <button
          type="button"
          disabled
          className="rounded border border-border px-2 py-0.5 text-muted-foreground"
        >
          Restore
        </button>
        <span className="text-muted-foreground">the core's edit journal restores files of a session it has open</span>
      </footer>
    </>
  );
}

type Answer = { kind: "once" } | { kind: "session" } | { kind: "deny"; instead: string };

const BUTTON = "rounded border border-border px-2 py-0.5 hover:bg-raised";

/** A question of the core's: allow, allow for the session (this action again), or deny,
 * with what to do instead. The answer goes back to the agent that asked. */
export function ApprovalCard({ ask, core }: { ask: ApprovalAsk; core: Core }) {
  const [denying, setDenying] = useState(false);
  const [instead, setInstead] = useState("");
  const q = ask.params;
  const answer = (decision: P.Decision, text?: string) =>
    core.answer<"approval/ask">(ask.id, { decision, instead: text?.trim() || null });
  return (
    <article className="asks mb-2 rounded-md bg-background p-3">
      <div className="flex min-w-0 items-baseline gap-2">
        <span className="shrink-0 font-mono text-[11px] text-spark uppercase">{q.tool}</span>
        <span className="min-w-0 truncate font-mono text-[12px]" title={q.action}>
          {q.action}
        </span>
      </div>
      <p className="mt-1 text-[12px] text-muted-foreground">
        {q.agent ? `subagent «${q.agent}»: ` : ""}
        {q.reason}
      </p>
      {q.unreviewed.length > 0 && (
        <p className="mt-1 text-[12px] text-muted-foreground">unreviewed: {q.unreviewed.join(", ")}</p>
      )}
      {denying ? (
        <form
          className="mt-2 flex gap-2 text-[12px]"
          onSubmit={(e) => {
            e.preventDefault();
            answer("deny", instead);
          }}
        >
          <input
            autoFocus
            value={instead}
            onChange={(e) => setInstead(e.target.value)}
            placeholder="What to do instead (optional)"
            className="min-w-0 flex-1 rounded border border-border bg-panel px-2 py-0.5 outline-none focus:border-spark/60"
          />
          <button type="submit" className={BUTTON}>
            Deny
          </button>
        </form>
      ) : (
        <div className="mt-2 flex gap-2 text-[12px]">
          <button type="button" className={BUTTON} onClick={() => answer("allow")}>
            Allow
          </button>
          <button type="button" className={BUTTON} onClick={() => answer("allow_session")}>
            Allow for the session
          </button>
          <button type="button" className={BUTTON} onClick={() => setDenying(true)}>
            Deny…
          </button>
        </div>
      )}
    </article>
  );
}

function Request({ call }: { call: ToolItem }) {
  const [answer, setAnswer] = useState<Answer>();
  const [denying, setDenying] = useState(false);
  const [instead, setInstead] = useState("");
  const button = "rounded border border-border px-2 py-0.5 hover:bg-raised";
  return (
    <article className="mb-2 rounded-md border border-border bg-background p-3">
      <div className="flex min-w-0 items-baseline gap-2">
        <span className="shrink-0 font-mono text-[11px] text-spark uppercase">
          {call.call?.name ?? call.shown}
        </span>
        <span className="min-w-0 truncate font-mono text-[12px]">
          {call.call ? summary(call.call) : call.shown}
        </span>
      </div>
      <p className="mt-1 text-[12px] text-muted-foreground">{refusalReason(call.result?.text ?? "")}</p>
      {answer ? (
        <p className="mt-2 text-[12px]">
          {answer.kind === "once"
            ? "Allowed this once."
            : answer.kind === "session"
              ? "Allowed for the rest of the session."
              : `Denied${answer.instead ? `: ${answer.instead}` : "."}`}
        </p>
      ) : denying ? (
        <form
          className="mt-2 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setAnswer({ kind: "deny", instead: instead.trim() });
          }}
        >
          <input
            autoFocus
            value={instead}
            onChange={(e) => setInstead(e.target.value)}
            placeholder="What to do instead (optional)"
            className="min-w-0 flex-1 rounded border border-border bg-panel px-2 py-0.5 outline-none focus:border-spark/60"
          />
          <button type="submit" className={button}>
            Deny
          </button>
        </form>
      ) : (
        <div className="mt-2 flex gap-2 text-[12px]">
          <button type="button" className={button} onClick={() => setAnswer({ kind: "once" })}>
            Allow
          </button>
          <button type="button" className={button} onClick={() => setAnswer({ kind: "session" })}>
            Allow for the session
          </button>
          <button type="button" className={button} onClick={() => setDenying(true)}>
            Deny…
          </button>
        </div>
      )}
    </article>
  );
}

/** Calls waiting for the user, the subagents' too, and the answer goes back to the call;
 * then the calls the session refused. A recorded run without a user refused them all, and
 * the answers there stay in this window. */
function Approvals({ calls, live }: { calls: ToolItem[]; live?: LiveThread }) {
  const asks = live?.asks ?? [];
  if (!calls.length && !asks.length) return <Empty>Nothing waited for an approval in this session.</Empty>;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-2">
      {live && asks.map((a) => <ApprovalCard key={a.id} ask={a} core={live.core} />)}
      {calls.length > 0 && (
        <p className="px-1 pt-1 pb-2 text-[12px] text-muted-foreground">
          {live ? "Refused in this session:" : "Refused in a run without a user; answers stay in this window."}
        </p>
      )}
      {calls.map((c) =>
        live ? (
          <p key={c.id} className="mb-1 px-1 font-mono text-[12px] text-muted-foreground">
            {c.call ? summary(c.call) : c.shown} · {refusalReason(c.result?.text ?? "")}
          </p>
        ) : (
          <Request key={c.id} call={c} />
        ),
      )}
    </div>
  );
}

/** A file's change against what it was before the session, hunk by hunk. */
function Hunks({ file }: { file: P.ChangedFile }) {
  return (
    <div className="font-mono text-[12px] leading-5">
      {file.hunks.map((h, i) => (
        <div key={i} className="border-b border-border/60 py-1">
          <div className="px-3 text-muted-foreground">
            line {h.old_start}
            {h.new_start !== h.old_start ? ` → ${h.new_start}` : ""}
          </div>
          {h.removed.map((l, j) => (
            <div key={`r${j}`} className="bg-err/10 px-3 whitespace-pre-wrap break-all text-err">
              − {l}
            </div>
          ))}
          {h.added.map((l, j) => (
            <div key={`a${j}`} className="bg-ok/10 px-3 whitespace-pre-wrap break-all text-ok">
              + {l}
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

/** What the agent changed and nobody reviewed yet, from the core's edit journal (§8):
 * accept it, or put a file back as it was. */
function LiveChanges({ live, refresh }: { live: LiveThread; refresh: string }) {
  const { core, thread } = live;
  const [changes, setChanges] = useState<P.Changes>();
  const [error, setError] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const [confirm, setConfirm] = useState<string>();
  const [tick, setTick] = useState(0);
  const [running, setRunning] = useState(false);
  useEffect(() => {
    let current = true;
    core.call("changes/list", { thread }).then(
      (c) => {
        if (!current) return;
        setChanges(c);
        setRunning(false);
      },
      (e: unknown) => {
        if (!current) return;
        // the journal is the turn's while it runs: what it showed last stays
        if (e instanceof CoreError && e.code === P.ERRORS.busy) setRunning(true);
        else setError(message(e));
      },
    );
    return () => {
      current = false;
    };
  }, [core, thread, refresh, tick]);
  const act = (done: Promise<unknown>) =>
    done.then(
      () => {
        setError(undefined);
        setConfirm(undefined);
        setTick((t) => t + 1);
      },
      (e: unknown) => setError(message(e)),
    );
  if (!changes) return <Empty>{error ?? (running ? "A turn runs: its changes show when it ends." : "…")}</Empty>;
  const file = changes.files.find((f) => f.path === selected) ?? changes.files[0];
  const nothing = !changes.files.length && !changes.commands.length && !changes.inherited;
  return (
    <>
      {nothing ? (
        <Empty>Nothing unreviewed: builds and tests run without asking.</Empty>
      ) : (
        <>
          <ul className="max-h-48 shrink-0 overflow-y-auto border-b border-border py-1">
            {changes.files.map((f) => {
              const added = f.hunks.reduce((n, h) => n + h.added.length, 0);
              const removed = f.hunks.reduce((n, h) => n + h.removed.length, 0);
              return (
                <li key={f.path}>
                  <button
                    type="button"
                    onClick={() => setSelected(f.path)}
                    className={`flex w-full items-center gap-2 px-3 py-1 text-left hover:bg-raised ${f === file ? "bg-raised" : ""}`}
                  >
                    <span className="min-w-0 flex-1 truncate font-mono text-[12px]">{f.path}</span>
                    {f.created && <span className="text-[11px] text-muted-foreground">new</span>}
                    {f.deleted && <span className="text-[11px] text-muted-foreground">deleted</span>}
                    {f.drifted && <span className="text-[11px] text-warn">changed outside</span>}
                    <span className="text-[11px] text-ok">+{added}</span>
                    <span className="text-[11px] text-err">−{removed}</span>
                  </button>
                </li>
              );
            })}
          </ul>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {file && <Hunks file={file} />}
            {changes.commands.map((c) => (
              <p key={c} className="px-3 py-1 text-[12px] text-warn">
                ran <span className="font-mono">{c}</span>: its side effects are not in the journal
              </p>
            ))}
            {changes.inherited && (
              <p className="px-3 py-1 text-[12px] text-muted-foreground">
                and what the project had before this session (git status shows it)
              </p>
            )}
          </div>
        </>
      )}
      <footer className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border px-3 py-2 text-[12px]">
        {confirm ? (
          <>
            <span className="text-warn">{confirm} also changed outside ognivo since its edits: a revert loses that.</span>
            <button
              type="button"
              className={BUTTON}
              onClick={() => act(core.call("changes/revert", { thread, path: confirm, force: true }))}
            >
              Revert anyway
            </button>
            <button type="button" className={BUTTON} onClick={() => setConfirm(undefined)}>
              Keep
            </button>
          </>
        ) : (
          <>
            {file && (
              <>
                <button
                  type="button"
                  className={BUTTON}
                  onClick={() =>
                    file.drifted
                      ? setConfirm(file.path)
                      : act(core.call("changes/revert", { thread, path: file.path, force: false }))
                  }
                >
                  Revert {file.path}
                </button>
                <button
                  type="button"
                  className={BUTTON}
                  onClick={() => act(core.call("changes/accept", { thread, path: file.path }))}
                >
                  Accept it
                </button>
              </>
            )}
            {!nothing && (
              <button
                type="button"
                className={`${BUTTON} ml-auto`}
                onClick={() => act(core.call("changes/accept", { thread, path: null }))}
              >
                Accept all
              </button>
            )}
          </>
        )}
        {running && <span className="w-full text-muted-foreground">A turn runs: this is how it was before it.</span>}
        {error && <span className="w-full text-err">{error}</span>}
      </footer>
    </>
  );
}

export function SidePanel({
  store,
  panel,
  onPanel,
  generation,
  onGeneration,
  live,
}: {
  store: ThreadStore;
  panel: Panel;
  onPanel: (panel: Panel) => void;
  /** The generation the Inspector shows, by its number; the last one when none. */
  generation?: number;
  onGeneration: (generation: number) => void;
  live?: LiveThread;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeChanges(l), [store]);
  useSyncExternalStore(subscribe, () => store.changesVersion);
  const [width, setWidth] = useWidth("panel", PANEL, 300, 900);
  const files = store.changedFiles();
  const refused = store.refusedCalls();
  const tab = (id: Panel, label: string, count?: number) => (
    <button
      type="button"
      onClick={() => onPanel(id)}
      className={`border-b-2 px-3 py-1.5 ${panel === id ? "border-spark text-foreground" : "border-transparent text-muted-foreground hover:text-foreground"}`}
    >
      {label} {count !== undefined && <span className="text-muted-foreground">{count}</span>}
    </button>
  );
  return (
    <>
      <Resizer width={width} onWidth={setWidth} side="right" initial={PANEL} />
      <aside style={{ width }} className="panel-in flex shrink-0 flex-col bg-panel">
        <div className="flex shrink-0 gap-1 border-b border-border px-2 text-[12px]">
          {tab("changes", "Changes", files.length)}
          {tab("approvals", "Approvals", live ? live.asks.length : refused.length)}
          {tab("inspector", "Inspector")}
          {live?.open && tab("mcp", "MCP")}
        </div>
        {panel === "changes" ? (
          live?.open ? (
            <LiveChanges live={live} refresh={`${live.version}.${store.changesVersion}`} />
          ) : (
            <Changes files={files} />
          )
        ) : panel === "approvals" ? (
          <Approvals calls={refused} live={live} />
        ) : panel === "mcp" && live?.open ? (
          <McpPanel core={live.core} thread={live.thread} project={live.project} />
        ) : (
          <Inspector store={store} generation={generation} onGeneration={onGeneration} />
        )}
      </aside>
    </>
  );
}
