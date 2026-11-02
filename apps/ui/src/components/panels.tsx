import { useCallback, useState, useSyncExternalStore } from "react";

import { lineDelta, refusalReason, summary } from "../format.ts";
import type { ToolItem } from "../model.ts";
import type { ThreadStore } from "../store.ts";
import { EditDiff } from "./diff.tsx";
import { Inspector } from "./inspector.tsx";

export type Panel = "changes" | "approvals" | "inspector";

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

/** The session's changes, file by file. Restoring one needs the core's edit journal (§8). */
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
        <span className="text-muted-foreground">once a core is connected: its edit journal restores files</span>
      </footer>
    </>
  );
}

type Answer = { kind: "once" } | { kind: "session" } | { kind: "deny"; instead: string };

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

/** Calls waiting for the user. A run without a user refused them; with a core, each waits
 * here, the subagents' too, and the answer goes back to the call. */
function Approvals({ calls }: { calls: ToolItem[] }) {
  if (!calls.length) return <Empty>Nothing waited for an approval in this session.</Empty>;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-2">
      <p className="px-1 pb-2 text-[12px] text-muted-foreground">
        Refused in a run without a user. With a core connected each would wait here; answers stay
        in this window for now.
      </p>
      {calls.map((c) => (
        <Request key={c.id} call={c} />
      ))}
    </div>
  );
}

export function SidePanel({
  store,
  panel,
  onPanel,
  generation,
  onGeneration,
}: {
  store: ThreadStore;
  panel: Panel;
  onPanel: (panel: Panel) => void;
  /** The generation the Inspector shows; the last one when none. */
  generation?: number;
  onGeneration: (generation: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeChanges(l), [store]);
  useSyncExternalStore(subscribe, () => store.changesVersion);
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
    <aside className="flex w-[440px] shrink-0 flex-col border-l border-border bg-panel">
      <div className="flex shrink-0 gap-1 border-b border-border px-2 text-[12px]">
        {tab("changes", "Changes", files.length)}
        {tab("approvals", "Approvals", refused.length)}
        {tab("inspector", "Inspector")}
      </div>
      {panel === "changes" ? (
        <Changes files={files} />
      ) : panel === "approvals" ? (
        <Approvals calls={refused} />
      ) : (
        <Inspector store={store} generation={generation} onGeneration={onGeneration} />
      )}
    </aside>
  );
}
