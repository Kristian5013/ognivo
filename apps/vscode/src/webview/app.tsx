// The agent's panel (docs/architecture.md §10): before a session, what a new one works with
// and the workspace's last sessions; in a session, its conversation, the approvals it waits
// on, and the message being written. The first message starts a session with the
// extension's settings; a session the core does not have open picks up again with the
// first message to it. The session's changes are in the Changes view, its list in
// `ognivo: Open a Session…`.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";

import * as P from "../shared/protocol.ts";
import profiles from "../shared/profiles.json";
import { type Core, CoreError } from "../shared/rpc.ts";
import { samePath } from "../shared/text.ts";
import { type ApprovalAsk, ApprovalCard } from "./approval.tsx";
import { Composer, type Sending } from "./composer.tsx";
import { type Hello, load, newSession, onNew, onOpen, run, save, shown, subscribeFolders, subscribeSession, workspaceFolders } from "./host.ts";
import { ending } from "./model/format.ts";
import { DEFAULT_MODE, isMode, modeInfo } from "./model/modes.ts";
import type { ThreadStore } from "./model/store.ts";
import type { Threads } from "./model/threads.ts";
import { ThreadView } from "./thread.tsx";
import { Welcome } from "./welcome.tsx";

/** Whether the Native path reads a profile's format (scripts/profiles.mjs). */
const NATIVE: Record<string, boolean | undefined> = Object.fromEntries(
  Object.entries(profiles as Record<string, { native: boolean }>).map(([id, p]) => [id, p.native]),
);

/** The thread a workspace's page showed last, to show again. */
const LAST = "ognivo.last";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function App({ core, threads, hello }: { core: Core; threads: Threads; hello: Hello }) {
  const folders = useSyncExternalStore(subscribeFolders, workspaceFolders);
  const session = useSyncExternalStore(subscribeSession, newSession)!;
  useSyncExternalStore(
    useCallback((l: () => void) => core.subscribe(l), [core]),
    () => core.stateVersion,
  );
  const threadsVersion = useSyncExternalStore(
    useCallback((l: () => void) => threads.subscribe(l), [threads]),
    () => threads.stateVersion,
  );
  const project = folders[0];
  const [thread, setThread] = useState<number>();
  const [store, setStore] = useState<ThreadStore>();
  const [list, setList] = useState<P.ThreadSummary[]>();
  const [opened, setOpened] = useState(new Map<number, P.Opened>());
  const [startMode, setStartMode] = useState<P.Mode>(isMode(session.mode) ? session.mode : DEFAULT_MODE);
  const [error, setError] = useState<string>();
  /** The threads whose opening notes the user hid. */
  const [read, setRead] = useState(new Set<number>());
  const showing = useRef<number | undefined>(undefined);

  // the core's sessions of this workspace, as they start, turn and end
  useEffect(() => {
    let live = true;
    core.call("thread/list", { project: null, limit: 200 }).then(
      (r) => {
        if (!live) return;
        const here = new Set(folders.map(samePath));
        setList(r.threads.filter((t) => here.has(samePath(t.project))));
      },
      () => live && setList([]),
    );
    return () => {
      live = false;
    };
  }, [core, threadsVersion, folders]);

  const summary = list?.find((t) => t.thread === thread);
  const info = thread === undefined ? undefined : threads.info(thread);
  const title = summary?.title || store?.info.title || "";
  const threadProject = summary?.project ?? info?.project ?? project ?? "";

  // the view's title says the session; the Changes view follows it
  useEffect(() => {
    shown(thread, title, threadProject);
  }, [thread, title, threadProject]);

  const select = useCallback(
    (t: number, name = "") => {
      showing.current = t;
      setThread(t);
      setError(undefined);
      setStore(threads.store(t));
      if (project) save(`${LAST}.${samePath(project)}`, String(t));
      threads.open(t, name).then(
        (s) => showing.current === t && setStore(s),
        (e: unknown) => showing.current === t && setError(message(e)),
      );
    },
    [threads, project],
  );

  const fresh = useCallback(() => {
    showing.current = undefined;
    setThread(undefined);
    setStore(undefined);
    setError(undefined);
    if (project) save(`${LAST}.${samePath(project)}`, null);
  }, [project]);

  useEffect(() => onNew(fresh), [fresh]);
  useEffect(() => onOpen((t) => select(t)), [select]);

  // the session this workspace's page showed last, once the list says it is there
  const restored = useRef(false);
  useEffect(() => {
    if (restored.current || !list || !project) return;
    restored.current = true;
    const last = Number(load(`${LAST}.${samePath(project)}`));
    const t = list.find((s) => s.thread === last);
    if (t && showing.current === undefined) select(t.thread, t.title);
  }, [list, project, select]);

  const opening = (o: P.Opened) => {
    threads.setMode(o.thread, o.mode);
    setOpened((m) => new Map(m).set(o.thread, o));
  };

  /** `call`, after picking the thread up again when the core does not have it open. */
  const onCore = async (t: number, call: () => Promise<unknown>) => {
    try {
      await call();
    } catch (e) {
      if (!(e instanceof CoreError) || e.code !== P.ERRORS.noThread) throw e;
      const profile = list?.find((s) => s.thread === t)?.profile ?? "";
      opening(
        await core.call("thread/resume", {
          thread: t,
          server: null,
          native: session.native && NATIVE[profile] !== false,
          priors: false,
          subagents: session.subagents,
          context: session.context || null,
          mode: threads.mode(t) ?? null,
        }),
      );
      await call();
    }
  };

  const running = thread !== undefined && threads.running(thread);
  const sending: Sending = {
    running,
    send: async (text) => {
      if (thread !== undefined) {
        await onCore(thread, () => core.call("turn/start", { thread, text, images: [] }));
        return;
      }
      if (!project) throw new Error("open a folder first: a session works in a folder of the workspace");
      const o = await core.call("thread/start", {
        project,
        server: { url: session.server.trim(), api_key: null, model: session.model || null },
        profile: session.profile || null,
        level: session.reasoning || null,
        native: session.native && (session.profile ? NATIVE[session.profile] !== false : true),
        priors: false,
        subagents: session.subagents,
        context: session.context || null,
        mode: startMode,
      });
      opening(o);
      select(o.thread);
      await core.call("turn/start", { thread: o.thread, text, images: [] });
    },
    stop: () => {
      if (thread !== undefined) void core.call("turn/interrupt", { thread }).catch((e: unknown) => setError(message(e)));
    },
    answerNow:
      thread !== undefined && (opened.get(thread)?.answer_now || summary?.answer_now)
        ? () => void core.call("turn/answer_now", { thread }).catch((e: unknown) => setError(message(e)))
        : undefined,
  };

  const mode = (thread === undefined ? startMode : (threads.mode(thread) ?? summary?.mode)) ?? DEFAULT_MODE;
  const switchMode = async (m: P.Mode) => {
    if (thread === undefined) {
      setStartMode(m);
      return;
    }
    threads.setMode(thread, m);
    try {
      await core.call("thread/mode", { thread, mode: m });
    } catch (e) {
      // a thread the core does not have open gets it when it is picked up again
      if (!(e instanceof CoreError) || e.code !== P.ERRORS.noThread) setError(message(e));
    }
  };

  const asks = [...core.asks.values()].filter((a): a is ApprovalAsk => a.method === "approval/ask");
  const here = asks.filter((a) => a.params.thread === thread);
  const elsewhere = asks.find((a) => a.params.thread !== thread);
  const parent = info?.parent;
  const last = store?.row(store.length - 1);
  const stopped = !running && !parent && last?.kind === "end" && last.end !== "answer" ? last : undefined;
  const planned = !running && !parent && mode === "plan" && last?.kind === "end" && last.end === "answer";
  const context = (thread === undefined ? undefined : opened.get(thread)?.context) ?? summary?.context ?? undefined;
  const model = thread === undefined ? session.model || session.profile || "" : summary?.model || info?.model || info?.profile || "";
  const notes = thread === undefined ? undefined : opened.get(thread)?.notes;

  if (core.status.state === "down") {
    return (
      <div className="centered">
        <p>The connection to the core is gone: {core.status.why}.</p>
        <div className="buttons">
          <button type="button" className="primary" onClick={() => location.reload()}>
            Connect again
          </button>
          <button type="button" className="link" onClick={() => run("settings")}>
            Settings
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      {hello.place === "editor" && (
        <div className="toolbar">
          <button type="button" className="icon-button" title="New session" onClick={fresh}>
            <span className="codicon codicon-add" />
          </button>
          <button type="button" className="icon-button" title="Open a session…" onClick={() => run("openSession")}>
            <span className="codicon codicon-history" />
          </button>
          <span className="toolbar-title">{thread === undefined ? "New session" : title || "Session"}</span>
        </div>
      )}
      {parent && (
        <div className="banner">
          <span>A subagent's thread: it works on one task of the session.</span>
          <button type="button" className="link" onClick={() => select(parent.thread)}>
            Back to the session
          </button>
        </div>
      )}
      {elsewhere && (
        <div className="banner warn">
          <span className="codicon codicon-bell-dot" />
          <span>Another session waits for your answer.</span>
          <button type="button" className="link" onClick={() => select(elsewhere.params.thread)}>
            Open it
          </button>
        </div>
      )}
      {((notes?.length && !read.has(thread!)) || error) && (
        <div className="notes">
          <div>
            {!read.has(thread!) &&
              notes?.map((n) => (
                <p key={n} className="muted">
                  · {n}
                </p>
              ))}
            {error && <p className="error">{error}</p>}
          </div>
          <button
            type="button"
            className="icon-button"
            title="Hide"
            onClick={() => {
              setError(undefined);
              if (thread !== undefined) setRead((r) => new Set(r).add(thread));
            }}
          >
            <span className="codicon codicon-close" />
          </button>
        </div>
      )}
      {thread === undefined ? (
        <Welcome core={core} session={session} project={project} onOpen={(t) => select(t.thread, t.title)} />
      ) : store ? (
        <ThreadView store={store} thread={thread} onThread={(t) => select(t)} />
      ) : (
        <p className="centered muted">{error ?? "Loading…"}</p>
      )}
      <div className="bottom">
        {here.map((a, i) => (
          <ApprovalCard key={a.id} ask={a} core={core} first={i === 0} />
        ))}
        {stopped && (
          <div className="banner warn">
            <span>
              Turn {stopped.turn} {ending(stopped.end)}
              {stopped.why ? `: ${stopped.why}` : ""}
            </span>
            <button type="button" className="primary small" onClick={() => void sending.send("Continue.").catch((e: unknown) => setError(message(e)))}>
              Continue
            </button>
          </div>
        )}
        {planned && (
          <div className="banner spark">
            <span>The plan is ready. Carry it out, or write what to change in it.</span>
            {(["edit_automatically", "auto", "manual"] as const).map((m, i) => (
              <button
                key={m}
                type="button"
                className={i === 0 ? "primary small" : "secondary small"}
                title={modeInfo(m).says}
                onClick={() =>
                  void switchMode(m)
                    .then(() => sending.send("Carry out the plan."))
                    .catch((e: unknown) => setError(message(e)))
                }
              >
                {i === 0 ? `Carry out · ${modeInfo(m).label}` : modeInfo(m).label}
              </button>
            ))}
          </div>
        )}
        {summary && !summary.open && !opened.has(summary.thread) && (
          <p className="hint muted">The core does not have this session open: a message picks it up again on {summary.server}.</p>
        )}
        {parent ? (
          <p className="hint muted">A subagent ends with its task; talk to the session that started it.</p>
        ) : (
          <Composer
            key={thread ?? "new"}
            draftKey={String(thread ?? "new")}
            store={store}
            sending={sending}
            mode={mode}
            onMode={(m) => void switchMode(m)}
            context={context}
            model={model}
          />
        )}
      </div>
    </div>
  );
}
