// The agent's panel (docs/architecture.md §10): before a session, what a new one works with
// and the workspace's last sessions; in a session, its title (a click lists the sessions),
// its conversation, and below it the dock: the question the agent asks, the messages that
// wait for the turn to end, and the message being written, with the session's notes by it.
// The first message starts a session with the extension's settings; a session the core
// does not have open picks up again with the first message to it.
//
// While a turn runs, a message waits for it to end, or goes into it (Ctrl+Enter or "Send
// now": `turn/steer`, which the model reads before its next step; a turn that ended
// meanwhile takes it as the next turn). When a turn ends, the core's stream says so
// (`turn_completed`, read here as it comes): what this page sent into it that the model
// never read goes to the front of the queue, and the first message waiting goes as the
// next turn, unless the user stopped the turn. Messages from elsewhere in VS Code (Ctrl+I,
// a Quick Fix, the welcome page's suggestions) come through the page's workspace store,
// to the composer or straight on their way. The session's changes are reviewed in the
// editor (the extension's review), not here.

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { chipFor } from "../shared/context.ts";
import type { SettingsTab } from "../shared/messages.ts";
import * as P from "../shared/protocol.ts";
import { type Core, CoreError } from "../shared/rpc.ts";
import { samePath } from "../shared/text.ts";
import { type ApprovalAsk, ApprovalCard } from "./approval.tsx";
import { Composer, type Incoming, NotesButton, QueueList } from "./composer.tsx";
import { detect, detectAgain, forget, useDetected } from "./detect.ts";
import { forgetDraft, keepUnsent } from "./drafts.ts";
import {
  type Hello,
  load,
  newSession,
  onDeleted,
  onKeys,
  onNew,
  onOpen,
  onSettings,
  run,
  save,
  shown,
  subscribeFolders,
  subscribeSession,
  workspaceFolders,
} from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { nativeFor, profileLevels, startLevel } from "./model/context.ts";
import type { Picture } from "./model/model.ts";
import { DEFAULT_MODE, isMode, modeInfo } from "./model/modes.ts";
import type { ThreadStore } from "./model/store.ts";
import type { Threads } from "./model/threads.ts";
import workspace, { type Prompt } from "./model/workspace.ts";
import { ModelPill, type SessionModel } from "./modelpop.tsx";
import { type Message, Queue, type Queued, wire } from "./queue.ts";
import { Settings } from "./settings.tsx";
import { type Editing, ThreadView, TurnActions } from "./thread.tsx";
import { Welcome, workspaceSessions } from "./welcome.tsx";

/** The thread a workspace's page showed last, to show again. */
const LAST = "ognivo.last";

function message(e: unknown): string {
  // the core's words, and where the user gives the server its key
  if (e instanceof CoreError && e.code === P.ERRORS.unauthorized) return `${e.message}. Set its API key in the settings, under Model.`;
  return e instanceof Error ? e.message : String(e);
}

const busy = (e: unknown) => e instanceof CoreError && e.code === P.ERRORS.busy;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The composer of thread `t`, or of a new session, by its draft's key. */
const keyOf = (t: number | undefined) => String(t ?? "new");

/** The images of turn `turn`'s message, to keep when it is written again. */
function imagesOf(store: ThreadStore | undefined, turn: number): Picture[] {
  if (!store) return [];
  for (let i = store.length - 1; i >= 0; i--) {
    const r = store.row(i);
    if (r?.kind === "user" && r.turn === turn) return r.images;
  }
  return [];
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
  const [queue] = useState(() => new Queue());
  useSyncExternalStore(queue.subscribe, () => queue.version);
  const project = folders[0];
  const [thread, setThread] = useState<number>();
  const [store, setStore] = useState<ThreadStore>();
  /** This workspace's sessions, with the folder each is of. */
  const [listed, setListed] = useState<{ threads: P.ThreadSummary[]; folder: ReadonlyMap<number, string> }>();
  const list = listed?.threads;
  const [opened, setOpened] = useState(new Map<number, P.Opened>());
  /** Whether a thread's model takes images, as the core said when asked again
   * (`thread/vision`): newer than what it said as the thread opened, or in the list. */
  const [sees, setSees] = useState(new Map<number, boolean>());
  const [startMode, setStartMode] = useState<P.Mode>(isMode(session.mode) ? session.mode : DEFAULT_MODE);
  const [error, setError] = useState<string>();
  /** The threads whose opening notes the user put away. */
  const [read, setRead] = useState(new Set<number>());
  /** The settings' tab on show; the conversation while none. */
  const [settings, setSettings] = useState<SettingsTab>();
  /** What went wrong with a message, by the composer it came from. */
  const [failed, setFailed] = useState<{ key: string; error: string }>();
  /** Messages for the composers, each by its composer's key: ones that could not go, and
   * ones from elsewhere in VS Code. A composer takes its own as it shows, in their order: one
   * for a composer that does not show waits for it, whatever comes to another meanwhile. */
  const [incoming, setIncoming] = useState<readonly Incoming[]>([]);
  /** The composers whose message is on its way. */
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  /** The thread whose running turn a message was just sent into, until the model reads it. */
  const [steered, setSteered] = useState<number>();
  const showing = useRef<number | undefined>(undefined);
  const composerArea = useRef<HTMLTextAreaElement | null>(null);
  const askCard = useRef<HTMLDivElement | null>(null);
  const incomingNo = useRef(0);
  const storeNow = useRef(store);
  storeNow.current = store;
  const detected = useDetected(core, session.server, session.model);
  useEffect(() => onSettings((tab) => setSettings((now) => tab ?? now ?? "model")), []);

  // the core's sessions of this workspace, as they start, turn and end: what it lists for
  // each folder, which it takes by its canonical path, as it names a session's project; the
  // window's spelling of a folder opened by a link, a junction, a mapped drive or an 8.3
  // name is another, and would match none
  useEffect(() => {
    let live = true;
    void Promise.allSettled(folders.map((f) => core.call("thread/list", { project: f, limit: 200 }))).then((lists) => {
      if (live) setListed(workspaceSessions(folders, lists.map((l) => (l.status === "fulfilled" ? l.value.threads : []))));
    });
    return () => {
      live = false;
    };
  }, [core, threadsVersion, folders]);
  const ours = useMemo<ReadonlySet<number>>(() => new Set(list?.map((t) => t.thread)), [list]);

  const summary = list?.find((t) => t.thread === thread);
  const info = thread === undefined ? undefined : threads.info(thread);
  // a subagent's thread is not listed: its task's title, as its session's thread says it
  const task = info?.parent
    ? threads
        .store(info.parent.thread)
        ?.items()
        .find((i) => i.kind === "subagent" && i.thread === thread)
    : undefined;
  const title = summary?.title || store?.info.title || (task?.kind === "subagent" ? task.title : "") || "";
  // a session of another project, read from its trajectory, is where its trajectory says
  const threadProject = summary?.project ?? info?.project ?? (store?.info.project || project) ?? "";

  /** Where thread `t` works, as this window names files: the folder of the window it is of,
   * for a session of this workspace; else its project as the core names it; `""` while the
   * page does not know (a session of another project whose history is on its way). A
   * message's chips name their files so for the session's agent. */
  const rootOf = (t: number): string => {
    const folder = listed?.folder.get(t);
    if (folder !== undefined) return folder;
    const root = threads.info(t)?.project || threads.store(t)?.info.project || "";
    return folders.find((f) => samePath(f) === samePath(root)) ?? root;
  };

  // the view's title says the session; the extension's review follows it
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
  // another API key: what the servers run is asked again
  useEffect(() => onKeys(forget), []);
  // a deleted session: let go of it, of the page that showed it, of its queue, and of the
  // message being written to it
  useEffect(
    () =>
      onDeleted((t) => {
        if (showing.current === t || threads.info(showing.current ?? -1)?.parent?.thread === t) fresh();
        threads.forget(t);
        queue.forget(t);
        forgetDraft(keyOf(t));
        setIncoming((list) => list.filter((i) => i.key !== keyOf(t)));
      }),
    [fresh, threads, queue],
  );

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
    // what it says as it opens is the newest
    setSees((m) => {
      if (!m.has(o.thread)) return m;
      const next = new Map(m);
      next.delete(o.thread);
      return next;
    });
  };

  /** `call`, after picking the thread up again when the core does not have it open. */
  const onCore = async (t: number, call: () => Promise<unknown>) => {
    try {
      await call();
    } catch (e) {
      if (!(e instanceof CoreError) || e.code !== P.ERRORS.noThread) throw e;
      // a session of another project is not in this workspace's list: its profile as it
      // opened, or as its trajectory says, once the page has read that (a message may come
      // while it is still on its way)
      const profileOf = () => list?.find((s) => s.thread === t)?.profile || threads.info(t)?.profile || threads.store(t)?.info.profile || "";
      const profile =
        profileOf() ||
        (await threads.open(t).then(
          () => profileOf(),
          () => "",
        ));
      // its own server, by name, so that the extension puts in that server's key (the core
      // keeps none); as the core has it when nothing here says
      const at = list?.find((s) => s.thread === t) ?? threads.info(t) ?? threads.store(t)?.info;
      opening(
        await core.call("thread/resume", {
          thread: t,
          server: at ? { url: at.server, api_key: null, model: at.model ?? null } : null,
          native: nativeFor(session.native, profile),
          priors: false,
          subagents: session.subagents,
          context: session.context || null,
          mode: threads.mode(t) ?? null,
        }),
      );
      await call();
    }
  };
  const onCoreNow = useRef(onCore);
  onCoreNow.current = onCore;

  const mark = (key: string, on: boolean) =>
    setPending((p) => {
      const next = new Set(p);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });

  /** Message `m` back in the composer `key`, with why it did not go. */
  const restore = (key: string, m: Message, why: string) => {
    const n = ++incomingNo.current;
    setIncoming((list) => [...list, { key, text: m.text, chips: m.chips, images: m.images, restore: true, n }]);
    setFailed({ key, error: why });
  };

  /** Message `m` as a turn of thread `t`. */
  const turn = (t: number, m: Message) =>
    onCoreNow.current(t, () => core.call("turn/start", { thread: t, text: wire(m), images: m.images.map((i) => ({ data: i.data })) }));

  /** A new session with the extension's settings, on show. */
  const startSession = async (): Promise<number> => {
    if (!project) throw new Error("Open a folder first: a session works in a folder of the workspace.");
    // what the server runs, where no profile is chosen: as the model's popover shows it
    const found = session.profile || !session.server.trim() ? undefined : await detect(core, session.server, session.model).catch(() => undefined);
    // the level the model's popover shows: a level of the model picked before, which this
    // model's profile has not, is the profile's default, as the core would refuse it
    const levels = session.profile ? profileLevels(session.profile) : found?.levels;
    const o = await core.call("thread/start", {
      project,
      server: { url: session.server.trim(), api_key: null, model: session.model || null },
      profile: session.profile || null,
      level: startLevel(session.reasoning, levels),
      // the Native path where it reads the model's format, as the settings show it: a Gemma
      // 4 model the server runs goes on the Generic path, which the core would refuse else
      native: nativeFor(session.native, session.profile, found?.native),
      priors: false,
      subagents: session.subagents,
      context: session.context || null,
      mode: startMode,
    });
    opening(o);
    select(o.thread);
    return o.thread;
  };

  /** Message `m` as the next turn of thread `t`, or as the first of a new session; back in
   * its composer when it could not go, and queued when a turn began meanwhile. */
  const deliver = async (t: number | undefined, m: Message): Promise<void> => {
    // the new session's composer as well, once it shows
    const keys = [keyOf(t)];
    mark(keys[0]!, true);
    try {
      if (t === undefined) {
        t = await startSession();
        keys.push(keyOf(t));
        mark(keyOf(t), true);
      }
      await turn(t, m);
    } catch (e) {
      if (t !== undefined && busy(e)) queue.add(t, m);
      else restore(keys.at(-1)!, m, message(e));
    } finally {
      for (const k of keys) mark(k, false);
    }
  };

  /** Message `m` into thread `t`'s running turn; as its next turn when none runs. */
  const steer = async (t: number, m: Message): Promise<void> => {
    const q = queue.steering(t, m);
    try {
      await core.call("turn/steer", { thread: t, text: wire(m) });
      // a turn that ended as it came has it back already, to send as the next turn
      if (queue.isSteering(t, q)) setSteered(t);
    } catch (e) {
      queue.unsteer(t, q);
      if (e instanceof CoreError && e.code === P.ERRORS.idle) return deliver(t, m);
      queue.front(t, m);
      setFailed({ key: keyOf(t), error: message(e) });
    }
  };

  /** The queue's next message as the turn after the one that ended. The core may still be
   * closing that turn as it says it ended: a moment later it takes the next. */
  const deliverQueued = async (t: number, m: Queued): Promise<void> => {
    const key = keyOf(t);
    mark(key, true);
    try {
      for (let i = 0; ; i++) {
        try {
          await turn(t, m);
          return;
        } catch (e) {
          if (!busy(e) || i >= 3) throw e;
          await sleep(250 * 2 ** i);
          // another page's message went first: this one waits for that turn to end
          if (threads.running(t)) {
            queue.front(t, m);
            return;
          }
        }
      }
    } catch (e) {
      queue.front(t, m);
      setFailed({ key, error: message(e) });
    } finally {
      mark(key, false);
    }
  };

  /** Message `m` for thread `t` (none: a new session): a turn, or into the queue or the
   * running turn while one runs. Whether it took it: one new session starts at a time. */
  const submitTo = (t: number | undefined, given: Message, now: boolean): boolean => {
    // its chips name their files for the session's agent, which may work in another folder
    // or project than the window's first, where the editor names them from (a new session
    // works there), and by their paths on disk while the page does not know where; before
    // the queue or the turn has it, so that what the core echoes of a steer is what it keeps
    const m = t === undefined ? given : { ...given, chips: given.chips.map((c) => chipFor(c, rootOf(t))) };
    if (t === undefined) {
      if (pending.has(keyOf(undefined))) return false;
      void deliver(undefined, m);
      return true;
    }
    setFailed((f) => (f?.key === keyOf(t) ? undefined : f));
    if (threads.running(t) || pending.has(keyOf(t))) {
      // only words go into a running turn
      if (now && m.images.length === 0 && wire(m).trim()) void steer(t, m);
      else queue.add(t, m);
      return true;
    }
    void deliver(t, m);
    return true;
  };

  const parent = info?.parent;
  /** A message from elsewhere in VS Code: in the composer, or on its way with `send`. A
   * subagent's thread takes none: its session does. */
  const prompt = (p: Prompt) => {
    setSettings(undefined);
    const target = parent ? parent.thread : thread;
    if (parent) select(parent.thread);
    const m: Message = { text: p.text, chips: p.chips, images: [] };
    if (p.send && submitTo(target, m, false)) return;
    const n = ++incomingNo.current;
    setIncoming((list) => [...list, { key: keyOf(target), text: p.text, chips: p.chips, images: [], restore: false, n }]);
  };

  // what the stream's events and VS Code's prompts call, as this render has it
  const later = useRef({ deliverQueued, prompt });
  later.current = { deliverQueued, prompt };

  // the turns' ends and the steers the model read, straight from the core's stream
  useEffect(
    () =>
      core.onEvent((e) => {
        if (e.event === "item_started" && e.item.kind === "steer") {
          queue.delivered(e.thread, e.item.text);
          setSteered((s) => (s === e.thread ? undefined : s));
        } else if (e.event === "turn_completed") {
          setSteered((s) => (s === e.thread ? undefined : s));
          const next = queue.ended(e.thread, e.end, e.undelivered ?? []);
          if (next) void later.current.deliverQueued(e.thread, next);
        }
      }),
    [core, queue],
  );
  useEffect(() => workspace.onPrompt((p) => later.current.prompt(p)), []);

  const running = thread !== undefined && threads.running(thread);
  const stop = () => {
    if (thread !== undefined) void core.call("turn/interrupt", { thread }).catch((e: unknown) => setError(message(e)));
  };
  const answerNow =
    thread !== undefined && (opened.get(thread)?.answer_now || summary?.answer_now)
      ? () => void core.call("turn/answer_now", { thread }).catch((e: unknown) => setError(message(e)))
      : undefined;

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
  const last = store?.row(store.length - 1);
  const stopped = !running && !parent && last?.kind === "end" && last.end !== "answer" ? last : undefined;
  const planned = !running && !parent && mode === "plan" && last?.kind === "end" && last.end === "answer";
  const context = (thread === undefined ? undefined : opened.get(thread)?.context) ?? summary?.context ?? undefined;
  const found = detected && detected !== "failed" ? detected : undefined;
  const current: SessionModel | undefined =
    thread === undefined
      ? undefined
      : {
          model: summary?.model || info?.model || info?.profile || summary?.profile || "",
          level: info?.level ?? summary?.level ?? null,
          context,
        };
  const vision = thread === undefined ? found?.vision : (sees.get(thread) ?? opened.get(thread)?.vision ?? summary?.vision);
  /** Whether the model takes images now, the core asked again: its server may have been
   * started with its projector since it last said. Where the core cannot say (an older
   * core, a thread it does not have open, no connection), the last word stands. */
  const visionNow = async (): Promise<boolean> => {
    const t = thread;
    try {
      if (t === undefined) return (await detectAgain(core, session.server, session.model)).vision !== false;
      const now = (await core.call("thread/vision", { thread: t })).vision;
      setSees((m) => new Map(m).set(t, now));
      return now;
    } catch {
      return vision !== false;
    }
  };
  const notes = thread === undefined || read.has(thread) ? [] : (opened.get(thread)?.notes ?? []);
  const composerKey = keyOf(thread);

  // a message written again: the turns from its turn on go back, and the agent goes on;
  // its images go with it again unless the new one says otherwise
  const editing = useMemo<Editing | undefined>(
    () =>
      thread === undefined || running || parent
        ? undefined
        : {
            edit: (turn: number, text: string, images?: Picture[]) => {
              const pictures = images ?? imagesOf(storeNow.current, turn);
              return onCoreNow.current(thread, () =>
                core.call("turn/edit", { thread, turn, text, images: pictures.map((p) => ({ data: p.data })) }),
              );
            },
          },
    [core, thread, running, parent],
  );
  const stoppedTurn = stopped?.turn;
  const goOn = useMemo(
    () => ({
      stopped: stoppedTurn,
      onContinue: () => {
        if (thread === undefined) return;
        onCoreNow.current(thread, () => core.call("turn/start", { thread, text: "Continue.", images: [] })).catch((e: unknown) => setError(message(e)));
      },
    }),
    [core, thread, stoppedTurn],
  );

  /** Whether a question may take the focus as it comes: not from a composer that has text
   * or the focus (in its text, or in its pills, chips and their popovers, where the next
   * Enter means something else), nor from any field the user writes in. */
  const mayFocus = () => {
    // the user works elsewhere in VS Code (the editor, a terminal): the keys stay there, or
    // the Enter they meant for it would answer a question they never saw
    if (!document.hasFocus()) return false;
    const area = composerArea.current;
    const active = document.activeElement;
    const composer = area?.closest(".composer");
    if (area && (area.value.trim() !== "" || (active && composer?.contains(active)))) return false;
    return !(active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement || (active instanceof HTMLElement && active.isContentEditable));
  };
  /** A question answered from the keyboard: the focus goes to the next question, which
   * takes it as it comes, or else back to the composer. */
  const afterAsk = () =>
    requestAnimationFrame(() => {
      if (!document.activeElement || document.activeElement === document.body) composerArea.current?.focus();
    });

  if (settings && core.status.state !== "down") {
    return (
      <Settings
        core={core}
        project={project ?? ""}
        thread={thread}
        session={session}
        tab={settings}
        onTab={setSettings}
        onClose={() => setSettings(undefined)}
      />
    );
  }

  if (core.status.state === "down") {
    // the page starts anew, and its memory with it: what no model got goes back into the
    // composers' drafts, which the page's storage keeps, images aside
    const connectAgain = () => {
      const unsent = new Map<string, Message[]>();
      const put = (key: string, m: Message) => unsent.set(key, [...(unsent.get(key) ?? []), m]);
      // the messages that came back first, as they were going
      for (const i of incoming) put(i.key, i);
      for (const [t, messages] of queue.unsent()) for (const m of messages) put(keyOf(t), m);
      keepUnsent(unsent);
      location.reload();
    };
    return (
      <div className="centered">
        <Icon name="debug-disconnect" className="centered-icon" />
        <p>The connection to the core is gone: {core.status.why}.</p>
        <div className="buttons">
          <button type="button" className="btn primary" onClick={connectAgain}>
            Connect again
          </button>
          <button type="button" className="btn ghost" onClick={() => run("showLog")}>
            The core's log
          </button>
        </div>
      </div>
    );
  }

  const titleButton = (
    <button type="button" className="head-title" title="Open a session…" onClick={() => run("openSession")}>
      <span className="head-text">{thread === undefined ? "New session" : title || "New session"}</span>
      <Icon name="chevron-down" className="head-chev" />
    </button>
  );

  return (
    <div className="app">
      {hello.place === "editor" ? (
        <div className="toolbar">
          <IconButton icon="add" title="New session" onClick={fresh} />
          <IconButton icon="history" title="Open a session…" onClick={() => run("openSession")} />
          {titleButton}
          <span className="spacer" />
          <IconButton icon="settings-gear" title="Settings" onClick={() => setSettings("model")} />
        </div>
      ) : (
        thread !== undefined && <div className="head">{titleButton}</div>
      )}
      {parent && (
        <div className="bar">
          <Icon name="organization" />
          <span className="bar-text">A subagent's thread: one task of the session</span>
          <button type="button" className="btn ghost small" onClick={() => select(parent.thread)}>
            <Icon name="arrow-left" /> The session
          </button>
        </div>
      )}
      {elsewhere && (
        <div className="bar warn">
          <Icon name="bell-dot" />
          <span className="bar-text">Another session waits for your answer</span>
          <button type="button" className="btn ghost small" onClick={() => select(elsewhere.params.thread)}>
            Open it
          </button>
        </div>
      )}
      {thread === undefined ? (
        <Welcome
          core={core}
          session={session}
          project={project}
          here={ours}
          version={threadsVersion}
          onOpen={(t) => select(t.thread, t.title)}
          onSettings={setSettings}
        />
      ) : store ? (
        <TurnActions.Provider value={goOn}>
          {/* A list for each thread: the rows' keys (`u1`, an item's id) start anew in every
              thread, so another thread's rows would take over these rows with their state (a
              message being written again, what the user did to the changes). The store that
              comes once the thread is read is the same thread's, and keeps the list. */}
          <ThreadView key={thread} store={store} thread={thread} onThread={(t) => select(t)} editing={editing} />
        </TurnActions.Provider>
      ) : (
        <div className="centered muted">{error ?? <Icon name="loading" spin />}</div>
      )}
      <div className="dock">
        {here[0] && (
          <ApprovalCard
            // The dock's children share one key space, and React compares keys as strings:
            // ask 1 and thread "1" would collide, and the card would outlive its answer.
            key={`ask:${here[0].id}`}
            ask={here[0]}
            core={core}
            project={threadProject}
            of={here.length}
            mayFocus={mayFocus}
            onLeave={afterAsk}
            rootRef={askCard}
          />
        )}
        {planned && (
          <div className="callout spark">
            <Icon name="lightbulb" />
            <span>The plan is ready: carry it out, or write what to change in it.</span>
            <div className="callout-buttons">
              {(["edit_automatically", "auto", "manual"] as const).map((m, i) => (
                <button
                  key={m}
                  type="button"
                  className={`btn small ${i === 0 ? "primary" : "secondary"}`}
                  title={modeInfo(m).says}
                  onClick={() =>
                    void switchMode(m)
                      .then(() => submitTo(thread, { text: "Carry out the plan.", chips: [], images: [] }, false))
                      .catch((e: unknown) => setError(message(e)))
                  }
                >
                  {i === 0 ? `Carry out · ${modeInfo(m).label}` : modeInfo(m).label}
                </button>
              ))}
            </div>
          </div>
        )}
        {thread !== undefined && steered === thread && (
          <p className="steered" role="status">
            <Icon name="sparkle" />
            <span>Sent: ognivo reads it before its next step</span>
          </p>
        )}
        {thread !== undefined && !parent && (
          <QueueList
            items={queue.list(thread)}
            running={running}
            onSendNow={(q) => {
              queue.remove(thread, q.id);
              if (threads.running(thread)) void steer(thread, q);
              else void deliver(thread, q);
            }}
            onRemove={(q) => queue.remove(thread, q.id)}
          />
        )}
        {parent ? (
          <p className="dock-hint">A subagent ends with its task: talk to the session that started it.</p>
        ) : (
          <Composer
            key={`composer:${composerKey}`}
            draftKey={composerKey}
            store={store}
            running={running}
            pending={pending.has(composerKey)}
            asking={here.length > 0}
            onAsk={() => askCard.current?.focus()}
            mode={mode}
            onMode={(m) => void switchMode(m)}
            context={context}
            model={<ModelPill session={session} detected={detected} current={current} onSettings={setSettings} />}
            notes={
              <NotesButton
                notes={notes}
                errors={error ? [error] : []}
                onDismiss={() => {
                  setError(undefined);
                  if (thread !== undefined) setRead((r) => new Set(r).add(thread));
                  // the notes' button goes with them: the focus to the message being written
                  composerArea.current?.focus();
                }}
              />
            }
            vision={vision}
            onVision={visionNow}
            home={project ?? ""}
            root={thread === undefined ? (project ?? "") : rootOf(thread)}
            onSubmit={(m, now) => submitTo(thread, m, now)}
            stop={stop}
            answerNow={answerNow}
            incoming={incoming.find((i) => i.key === composerKey)}
            onIncoming={(n) => setIncoming((list) => list.filter((i) => i.n !== n))}
            error={failed?.key === composerKey ? failed.error : undefined}
            onError={(e) => {
              if (e === undefined) setFailed((f) => (f?.key === composerKey ? undefined : f));
            }}
            areaRef={composerArea}
          />
        )}
      </div>
    </div>
  );
}
