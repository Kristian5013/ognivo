// The GUI on ognivo's core (docs/architecture.md §3, §10): the threads of every project, a
// new one, and the open one with its stream, the questions it waits on and its changes. A
// thread from the list shows what happened in it; the first message to one the core does
// not have open picks it up again with the server it last had.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";

import { Hud } from "./app.tsx";
import { Composer, type Sending } from "./components/composer.tsx";
import { type ApprovalAsk, ApprovalCard, type Panel, SidePanel } from "./components/panels.tsx";
import { ThreadView } from "./components/thread.tsx";
import { type Ask, type Core, CoreError } from "./core.ts";
import profiles from "./profiles.json";
import * as P from "./protocol.ts";
import type { ThreadStore } from "./store.ts";
import type { Threads } from "./threads.ts";

const PROFILES = Object.keys(profiles);
const LEVELS = ["low", "medium", "high"];
const SETTINGS = "ognivo.settings";

/** What the form for a new thread starts with: what it was last time. */
interface Settings {
  url: string;
  profile: string;
  level: string;
  native: boolean;
  subagents: boolean;
  /** Projects started last, the newest first. */
  projects: string[];
}

// the owner's llama-server; the nights' runs on 8090
const DEFAULTS: Settings = { url: "http://127.0.0.1:8080", profile: "", level: "", native: false, subagents: false, projects: [] };

function loadSettings(): Settings {
  try {
    return { ...DEFAULTS, ...(JSON.parse(localStorage.getItem(SETTINGS) ?? "{}") as Partial<Settings>) };
  } catch {
    return DEFAULTS;
  }
}

function saveSettings(s: Settings): void {
  try {
    localStorage.setItem(SETTINGS, JSON.stringify(s));
  } catch {
    // a private window: the form starts from the defaults next time
  }
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function basename(path: string): string {
  return path.replace(/\/+$/, "").split("/").at(-1) || path;
}

function ThreadList({
  list,
  selected,
  creating,
  waiting,
  running,
  onSelect,
  onNew,
}: {
  list: P.ThreadSummary[];
  selected?: number;
  creating: boolean;
  /** Questions waiting, by thread. */
  waiting: Map<number, number>;
  running: (thread: number) => boolean;
  onSelect: (t: P.ThreadSummary) => void;
  onNew: () => void;
}) {
  const groups = new Map<string, P.ThreadSummary[]>();
  for (const t of list) groups.set(t.project, [...(groups.get(t.project) ?? []), t]);
  return (
    <nav className="flex w-72 shrink-0 flex-col border-r border-border bg-panel">
      <div className="flex items-center px-4 py-3">
        <span className="text-[15px] font-semibold tracking-tight text-spark">ognivo</span>
        <button
          type="button"
          onClick={onNew}
          className={`ml-auto rounded border px-2 py-0.5 text-[12px] hover:bg-raised ${creating ? "border-spark/60" : "border-border"}`}
        >
          New session
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {[...groups].map(([project, threads]) => (
          <section key={project}>
            <h2
              title={project}
              className="truncate px-4 pt-3 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase"
            >
              {basename(project)}
            </h2>
            {threads.map((t) => {
              const asks = waiting.get(t.thread) ?? 0;
              const runs = running(t.thread) || t.running;
              return (
                <button
                  key={t.thread}
                  type="button"
                  onClick={() => onSelect(t)}
                  className={`block w-full px-4 py-1.5 text-left hover:bg-raised ${t.thread === selected && !creating ? "bg-raised" : ""}`}
                >
                  <span className="line-clamp-2 leading-5">{t.title || "New session"}</span>
                  <span className="flex gap-2 text-[11px] text-muted-foreground">
                    <span>{t.profile}</span>
                    <span>
                      {t.turns} {t.turns === 1 ? "turn" : "turns"}
                    </span>
                    {t.unreviewed > 0 && <span className="text-warn">{t.unreviewed} unreviewed</span>}
                    {runs && <span className="animate-pulse text-spark">running</span>}
                    {asks > 0 && <span className="text-spark">asks</span>}
                  </span>
                </button>
              );
            })}
          </section>
        ))}
      </div>
    </nav>
  );
}

/** A new thread: the project, the server, and how the agent works. */
function NewThread({ settings, onStart }: { settings: Settings; onStart: (s: Settings, project: string) => Promise<void> }) {
  const [form, setForm] = useState(settings);
  const [project, setProject] = useState(settings.projects[0] ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const field = "w-full rounded border border-border bg-panel px-2 py-1 outline-none focus:border-spark/60";
  const label = "mb-1 block text-[12px] text-muted-foreground";
  return (
    <form
      className="m-auto w-full max-w-lg space-y-4 px-6"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError(undefined);
        onStart(form, project.trim())
          .catch((e: unknown) => setError(message(e)))
          .finally(() => setBusy(false));
      }}
    >
      <h1 className="text-[15px] font-semibold">New session</h1>
      <div>
        <label className={label} htmlFor="project">
          Project: its root directory
        </label>
        <input
          id="project"
          list="projects"
          required
          value={project}
          onChange={(e) => setProject(e.target.value)}
          placeholder="/home/you/code/project"
          className={`${field} font-mono`}
        />
        <datalist id="projects">
          {settings.projects.map((p) => (
            <option key={p} value={p} />
          ))}
        </datalist>
      </div>
      <div>
        <label className={label} htmlFor="server">
          llama-server
        </label>
        <input
          id="server"
          required
          value={form.url}
          onChange={(e) => setForm({ ...form, url: e.target.value })}
          className={`${field} font-mono`}
        />
      </div>
      <div className="flex gap-3">
        <div className="flex-1">
          <label className={label} htmlFor="profile">
            Profile
          </label>
          <select
            id="profile"
            value={form.profile}
            onChange={(e) => setForm({ ...form, profile: e.target.value })}
            className={field}
          >
            <option value="">the server's model</option>
            {PROFILES.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
        <div className="flex-1">
          <label className={label} htmlFor="level">
            Reasoning
          </label>
          <select
            id="level"
            value={form.level}
            onChange={(e) => setForm({ ...form, level: e.target.value })}
            className={field}
          >
            <option value="">the profile's default</option>
            {LEVELS.map((l) => (
              <option key={l} value={l}>
                {l}
              </option>
            ))}
          </select>
        </div>
      </div>
      <label className="flex items-center gap-2 text-[13px]">
        <input type="checkbox" checked={form.native} onChange={(e) => setForm({ ...form, native: e.target.checked })} />
        Native path: ognivo renders the model's format itself (gpt-oss)
      </label>
      <label className="flex items-center gap-2 text-[13px]">
        <input
          type="checkbox"
          checked={form.subagents}
          onChange={(e) => setForm({ ...form, subagents: e.target.checked })}
        />
        Subagents: the agent may hand parts of the task to agents of their own
      </label>
      <div className="flex items-center gap-3">
        <button
          type="submit"
          disabled={busy}
          className="rounded border border-spark/60 px-3 py-1 hover:bg-raised disabled:opacity-40"
        >
          {busy ? "Connecting…" : "Start"}
        </button>
        {error && <span className="text-[12px] text-err">{error}</span>}
      </div>
    </form>
  );
}

/** The project's settings allow actions without a question: they count once trusted. */
function TrustDialog({ ask, core }: { ask: Extract<Ask, { method: "rules/trust" }>; core: Core }) {
  const answer = (trust: boolean) => core.answer<"rules/trust">(ask.id, { trust });
  return (
    <div className="fixed inset-0 z-10 flex items-center justify-center bg-black/50">
      <div role="dialog" aria-label="Trust the project's rules" className="w-[520px] rounded-lg border border-border bg-panel p-4">
        <p className="font-medium">{basename(ask.params.project)}: the project's settings allow these without a question</p>
        <ul className="my-3 space-y-1 font-mono text-[12px]">
          {ask.params.rules.map((r) => (
            <li key={`${r.source}:${r.text}`} className={r.any ? "text-warn" : undefined}>
              {r.text} <span className="text-muted-foreground">({r.source})</span>
              {r.any && ": any command at all"}
            </li>
          ))}
        </ul>
        <p className="mb-3 text-[12px] text-muted-foreground">The answer is kept until the rules change.</p>
        <div className="flex gap-2 text-[13px]">
          <button type="button" onClick={() => answer(true)} className="rounded border border-border px-3 py-1 hover:bg-raised">
            Use them
          </button>
          <button type="button" onClick={() => answer(false)} className="rounded border border-border px-3 py-1 hover:bg-raised">
            Ask anyway
          </button>
        </div>
      </div>
    </div>
  );
}

export function LiveApp({ core, threads }: { core: Core; threads: Threads }) {
  useSyncExternalStore(
    useCallback((l: () => void) => core.subscribe(l), [core]),
    () => core.stateVersion,
  );
  const threadsVersion = useSyncExternalStore(
    useCallback((l: () => void) => threads.subscribe(l), [threads]),
    () => threads.stateVersion,
  );
  const [list, setList] = useState<P.ThreadSummary[]>([]);
  const [selected, setSelected] = useState<number>();
  const shown = useRef<number>(undefined);
  const [creating, setCreating] = useState(true);
  const [store, setStore] = useState<ThreadStore>();
  const [notes, setNotes] = useState(new Map<number, string[]>());
  const [open, setOpen] = useState(new Set<number>());
  const [panel, setPanel] = useState<Panel>();
  const [generation, setGeneration] = useState<number>();
  const [error, setError] = useState<string>();
  const [settings, setSettings] = useState(loadSettings);

  // the list follows the threads' starts and turns
  useEffect(() => {
    let current = true;
    core.call("thread/list", { project: null, limit: 200 }).then(
      (r) => current && setList(r.threads),
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [core, threadsVersion]);

  const select = (thread: number, title = "") => {
    shown.current = thread;
    setSelected(thread);
    setCreating(false);
    setGeneration(undefined);
    setError(undefined);
    setStore(threads.store(thread));
    threads.open(thread, title).then(
      (s) => shown.current === thread && setStore(s),
      (e: unknown) => shown.current === thread && setError(message(e)),
    );
  };

  const opened = (o: P.Opened) => {
    setNotes((n) => new Map(n).set(o.thread, o.notes));
    setOpen((s) => new Set(s).add(o.thread));
  };

  const start = async (form: Settings, project: string) => {
    const o = await core.call("thread/start", {
      project,
      server: { url: form.url.trim(), api_key: null, model: null },
      profile: form.profile || null,
      level: form.level || null,
      native: form.native,
      priors: false,
      subagents: form.subagents,
    });
    const next = { ...form, projects: [project, ...form.projects.filter((p) => p !== project)].slice(0, 12) };
    setSettings(next);
    saveSettings(next);
    opened(o);
    select(o.thread);
  };

  const summary = list.find((t) => t.thread === selected);
  const isOpen = selected !== undefined && (open.has(selected) || Boolean(summary?.open));

  const sending: Sending | undefined =
    selected === undefined
      ? undefined
      : {
          running: threads.running(selected),
          send: async (text) => {
            const turn = () => core.call("turn/start", { thread: selected, text });
            try {
              await turn();
            } catch (e) {
              if (!(e instanceof CoreError) || e.code !== P.ERRORS.noThread) throw e;
              // a thread the core does not have open: picked up again where it ended
              opened(
                await core.call("thread/resume", {
                  thread: selected,
                  server: null,
                  native: settings.native,
                  priors: false,
                  subagents: settings.subagents,
                }),
              );
              await turn();
            }
          },
          stop: () => void core.call("turn/interrupt", { thread: selected }).catch((e: unknown) => setError(message(e))),
        };

  const asks = [...core.asks.values()];
  const approvals = asks.filter((a): a is ApprovalAsk => a.method === "approval/ask");
  const here = approvals.filter((a) => a.params.thread === selected);
  const trust = asks.find((a): a is Extract<Ask, { method: "rules/trust" }> => a.method === "rules/trust");
  const waiting = new Map<number, number>();
  for (const a of approvals) waiting.set(a.params.thread, (waiting.get(a.params.thread) ?? 0) + 1);

  if (core.status.state === "down") {
    return (
      <div className="m-auto max-w-lg space-y-3 px-6 text-center">
        <p>The connection to the core is gone: {core.status.why}.</p>
        <button type="button" onClick={() => location.reload()} className="rounded border border-border px-3 py-1 hover:bg-raised">
          Connect again
        </button>
      </div>
    );
  }

  const shownNotes = selected === undefined ? undefined : notes.get(selected);
  return (
    <div className="flex h-full">
      <ThreadList
        list={list}
        selected={selected}
        creating={creating}
        waiting={waiting}
        running={(t) => threads.running(t)}
        onSelect={(t) => select(t.thread, t.title)}
        onNew={() => setCreating(true)}
      />
      <main className="flex min-w-0 flex-1 flex-col">
        {creating || selected === undefined ? (
          <NewThread key={settings.projects[0] ?? ""} settings={settings} onStart={start} />
        ) : store ? (
          <>
            <Hud store={store} panel={panel} onPanel={setPanel} waiting={here.length} />
            {(shownNotes?.length || error) && (
              <div className="border-b border-border px-6 py-1.5 text-[12px] text-muted-foreground">
                {shownNotes?.map((n) => (
                  <p key={n}>· {n}</p>
                ))}
                {error && <p className="text-err">{error}</p>}
              </div>
            )}
            <ThreadView
              key={store.info.id}
              store={store}
              atEnd
              onInspect={(g) => {
                setGeneration(g);
                setPanel("inspector");
              }}
            />
            {here.length > 0 && (
              <div className="px-6 pt-2">
                <div className="mx-auto max-w-3xl">
                  {here.map((a) => (
                    <ApprovalCard key={a.id} ask={a} core={core} />
                  ))}
                </div>
              </div>
            )}
            {summary && !isOpen && (
              <p className="px-6 pt-1 text-center text-[12px] text-muted-foreground">
                The core does not have this session open: a message picks it up again on {summary.server}
              </p>
            )}
            <Composer key={store.info.id} store={store} sending={sending} />
          </>
        ) : (
          <p className="m-auto text-muted-foreground">{error ?? "Loading…"}</p>
        )}
      </main>
      {store && panel && !creating && selected !== undefined && (
        <SidePanel
          store={store}
          panel={panel}
          onPanel={setPanel}
          generation={generation}
          onGeneration={setGeneration}
          live={{ core, thread: selected, open: isOpen, version: threadsVersion, asks: here }}
        />
      )}
      {trust && <TrustDialog ask={trust} core={core} />}
    </div>
  );
}
