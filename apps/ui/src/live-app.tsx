// The GUI on ognivo's core (docs/architecture.md §3, §10): the threads of every project, a
// new one, and the open one with its stream, the questions it waits on and its changes. A
// thread from the list shows what happened in it; the first message to one the core does
// not have open picks it up again with the server it last had.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { Hud } from "./app.tsx";
import { Composer, type Sending } from "./components/composer.tsx";
import { Logo, Wordmark } from "./components/logo.tsx";
import { type ApprovalAsk, ApprovalCard, type Panel, SidePanel } from "./components/panels.tsx";
import { Resizer, useWidth } from "./components/resizer.tsx";
import { SettingsButton } from "./components/settings.tsx";
import { ThreadNavContext } from "./components/subagent.tsx";
import { ThreadView } from "./components/thread.tsx";
import { type Ask, type Core, CoreError } from "./core.ts";
import { ending } from "./format.ts";
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

const SIDEBAR = 288;

function ThreadList({
  list,
  selected,
  creating,
  waiting,
  running,
  onSelect,
  onNew,
  onDelete,
  width,
}: {
  list: P.ThreadSummary[];
  selected?: number;
  creating: boolean;
  /** Questions waiting, by thread. */
  waiting: Map<number, number>;
  running: (thread: number) => boolean;
  onSelect: (t: P.ThreadSummary) => void;
  onNew: () => void;
  onDelete: (t: P.ThreadSummary) => Promise<void>;
  width: number;
}) {
  const [asking, setAsking] = useState<number>();
  const [error, setError] = useState<string>();
  const groups = new Map<string, P.ThreadSummary[]>();
  for (const t of list) groups.set(t.project, [...(groups.get(t.project) ?? []), t]);
  return (
    <nav style={{ width }} className="flex shrink-0 flex-col bg-panel">
      <div className="flex items-center px-4 py-3">
        <Wordmark />
        <button
          type="button"
          onClick={onNew}
          className={`ml-auto rounded border px-2 py-0.5 text-[12px] hover:bg-raised ${creating ? "border-spark/60 text-foreground" : "border-border"}`}
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
              if (asking === t.thread) {
                return (
                  <div key={t.thread} className="rise border-l-2 border-err/60 bg-raised px-4 py-2 text-[12px]">
                    <p className="line-clamp-1 font-medium">{t.title || "New session"}</p>
                    <p className="mt-0.5 text-muted-foreground">
                      Delete the session: its conversation and its edit journal
                      {t.unreviewed > 0 ? `; ${t.unreviewed} unreviewed ${t.unreviewed === 1 ? "file" : "files"} can no longer be reverted` : ""}.
                    </p>
                    {error && <p className="mt-0.5 text-err">{error}</p>}
                    <div className="mt-1.5 flex gap-2">
                      <button
                        type="button"
                        onClick={() =>
                          onDelete(t).then(
                            () => setAsking(undefined),
                            (e: unknown) => setError(message(e)),
                          )
                        }
                        className="rounded bg-err px-2 py-0.5 font-medium text-background hover:opacity-90"
                      >
                        Delete
                      </button>
                      <button
                        type="button"
                        onClick={() => setAsking(undefined)}
                        className="rounded border border-border px-2 py-0.5 hover:bg-panel"
                      >
                        Keep
                      </button>
                    </div>
                  </div>
                );
              }
              return (
                <div key={t.thread} className="group/session relative">
                  <button
                    type="button"
                    onClick={() => onSelect(t)}
                    className={`block w-full border-l-2 px-4 py-1.5 pr-8 text-left hover:bg-raised ${t.thread === selected && !creating ? "border-spark bg-raised" : "border-transparent"}`}
                  >
                    <span className="flex items-start gap-2">
                      <span className="line-clamp-2 min-w-0 flex-1 leading-5">{t.title || "New session"}</span>
                      {runs && <span className="ember-dot mt-1.5" title="A turn runs" />}
                    </span>
                    <span className="flex gap-2 text-[11px] text-muted-foreground">
                      <span>{t.profile}</span>
                      <span>
                        {t.turns} {t.turns === 1 ? "turn" : "turns"}
                      </span>
                      {t.unreviewed > 0 && <span className="text-warn">{t.unreviewed} unreviewed</span>}
                      {asks > 0 && <span className="rounded bg-spark/15 px-1 text-spark">asks</span>}
                    </span>
                  </button>
                  {!runs && (
                    <button
                      type="button"
                      aria-label="Delete the session"
                      title="Delete the session"
                      onClick={() => {
                        setError(undefined);
                        setAsking(t.thread);
                      }}
                      className="absolute top-1.5 right-2 grid size-6 place-items-center rounded text-muted-foreground opacity-0 group-hover/session:opacity-100 hover:bg-panel hover:text-err focus:opacity-100"
                    >
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
                        <path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" />
                      </svg>
                    </button>
                  )}
                </div>
              );
            })}
          </section>
        ))}
      </div>
      <div className="flex items-center border-t border-border px-3 py-2">
        <SettingsButton />
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
      className="rise m-auto w-full max-w-lg space-y-4 rounded-xl border border-border bg-panel/70 p-6 shadow-xl backdrop-blur"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        setError(undefined);
        onStart(form, project.trim())
          .catch((e: unknown) => setError(message(e)))
          .finally(() => setBusy(false));
      }}
    >
      <div className="flex items-center gap-3">
        <Logo size={30} />
        <div>
          <h1 className="text-[15px] font-semibold">New session</h1>
          <p className="text-[12px] text-muted-foreground">A coding agent on your own llama-server</p>
        </div>
      </div>
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
          className="rounded-md bg-spark px-4 py-1.5 font-medium text-background hover:opacity-90 disabled:opacity-40"
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
  /** Threads on the Native path, which can end the model's reasoning. */
  const [native, setNative] = useState(new Set<number>());
  const [panel, setPanel] = useState<Panel>();
  const [generation, setGeneration] = useState<number>();
  const [error, setError] = useState<string>();
  const [settings, setSettings] = useState(loadSettings);
  const [listed, setListed] = useState(0);
  const [sidebar, setSidebar] = useWidth("sidebar", SIDEBAR, 200, 520);

  // the list follows the threads' starts and turns, and deletes
  useEffect(() => {
    let current = true;
    core.call("thread/list", { project: null, limit: 200 }).then(
      (r) => current && setList(r.threads),
      (e: unknown) => current && setError(message(e)),
    );
    return () => {
      current = false;
    };
  }, [core, threadsVersion, listed]);

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
    if (o.answer_now) setNative((s) => new Set(s).add(o.thread));
  };

  const remove = async (t: P.ThreadSummary) => {
    await core.call("thread/delete", { thread: t.thread });
    threads.forget(t.thread);
    if (shown.current === t.thread) {
      shown.current = undefined;
      setSelected(undefined);
      setStore(undefined);
      setCreating(true);
    }
    setListed((n) => n + 1);
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
  const running = selected !== undefined && threads.running(selected);
  /** The thread a subagent's thread works for. */
  const parent = selected === undefined ? undefined : threads.info(selected)?.parent;
  // a turn that ended without an answer can go on
  const last = store?.row(store.length - 1);
  const stopped = !running && !parent && last?.kind === "end" && last.end !== "answer" ? last : undefined;

  /** `call`, after picking the thread up again when the core does not have it open. */
  const onCore = async (thread: number, call: () => Promise<unknown>) => {
    try {
      await call();
    } catch (e) {
      if (!(e instanceof CoreError) || e.code !== P.ERRORS.noThread) throw e;
      opened(
        await core.call("thread/resume", {
          thread,
          server: null,
          native: settings.native,
          priors: false,
          subagents: settings.subagents,
        }),
      );
      await call();
    }
  };

  // a subagent's lane opens its thread; a message of the user's is written anew
  const nav = useMemo(
    () => ({
      store: (t: number) => threads.store(t),
      open: (t: number) => select(t),
      edit:
        selected === undefined || parent
          ? undefined
          : (turn: number, text: string) => onCore(selected, () => core.call("turn/edit", { thread: selected, turn, text })),
      busy: running,
    }),
    [threads, core, selected, parent, running, settings],
  );

  const sending: Sending | undefined =
    selected === undefined
      ? undefined
      : {
          running,
          send: (text) => onCore(selected, () => core.call("turn/start", { thread: selected, text })),
          stop: () => void core.call("turn/interrupt", { thread: selected }).catch((e: unknown) => setError(message(e))),
          answerNow:
            native.has(selected) || summary?.answer_now
              ? () => void core.call("turn/answer_now", { thread: selected }).catch((e: unknown) => setError(message(e)))
              : undefined,
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
        onDelete={remove}
        width={sidebar}
      />
      <Resizer width={sidebar} onWidth={setSidebar} side="left" initial={SIDEBAR} />
      <main className={`flex min-w-0 flex-1 flex-col ${creating || selected === undefined ? "hearth" : ""}`}>
        {creating || selected === undefined ? (
          <NewThread key={settings.projects[0] ?? ""} settings={settings} onStart={start} />
        ) : store ? (
          <>
            <Hud store={store} panel={panel} onPanel={setPanel} waiting={here.length} />
            {parent && (
              <div className="flex items-center gap-3 border-b border-border bg-panel px-6 py-1.5 text-[12px]">
                <span className="text-muted-foreground">A subagent's thread: it works on one task of the session.</span>
                <button
                  type="button"
                  onClick={() => select(parent.thread)}
                  className="ml-auto rounded border border-border px-2 py-0.5 hover:border-spark/60 hover:bg-raised"
                >
                  ← Back to the session
                </button>
              </div>
            )}
            {(shownNotes?.length || error) && (
              <div className="border-b border-border px-6 py-1.5 text-[12px] text-muted-foreground">
                {shownNotes?.map((n) => (
                  <p key={n}>· {n}</p>
                ))}
                {error && <p className="text-err">{error}</p>}
              </div>
            )}
            <ThreadNavContext.Provider value={nav}>
              <ThreadView
                key={store.info.id}
                store={store}
                atEnd
                onInspect={(g) => {
                  setGeneration(g);
                  setPanel("inspector");
                }}
              />
            </ThreadNavContext.Provider>
            {here.length > 0 && (
              <div className="px-6 pt-2">
                <div className="mx-auto max-w-3xl">
                  {here.map((a) => (
                    <ApprovalCard key={a.id} ask={a} core={core} />
                  ))}
                </div>
              </div>
            )}
            {stopped && sending && (
              <div className="px-6 pt-2">
                <div className="rise mx-auto flex max-w-3xl items-center gap-3 rounded-md border border-warn/40 bg-panel px-3 py-1.5 text-[12px]">
                  <span className="min-w-0 flex-1 truncate text-warn">
                    Turn {stopped.turn} {ending(stopped.end)}
                    {stopped.why ? `: ${stopped.why}` : ""}
                  </span>
                  <button
                    type="button"
                    onClick={() => void sending.send("Continue.").catch((e: unknown) => setError(message(e)))}
                    className="shrink-0 rounded-md bg-spark px-3 py-0.5 font-medium text-background hover:opacity-90"
                  >
                    Continue
                  </button>
                </div>
              </div>
            )}
            {summary && !isOpen && (
              <p className="px-6 pt-1 text-center text-[12px] text-muted-foreground">
                The core does not have this session open: a message picks it up again on {summary.server}
              </p>
            )}
            {parent ? (
              <p className="border-t border-border px-6 py-3 text-center text-[12px] text-muted-foreground">
                A subagent ends with its task; talk to the session that started it.
              </p>
            ) : (
              <Composer key={store.info.id} store={store} sending={sending} />
            )}
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
