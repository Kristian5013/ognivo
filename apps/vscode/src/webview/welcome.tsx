// Before a session: what to work on, from where the user is in VS Code (the file and the
// selection in the editor, the last command that failed in a terminal: model/workspace.ts),
// the server a new session works with, and the last sessions to go on with, this
// workspace's first. A suggestion hands the composer its message with the context it is
// about as chips (`workspace.emitPrompt`); the composer sends it, or, for one the user is
// to finish, shows it with the focus in the message.

import { useEffect, useMemo, useReducer, useRef, useState, useSyncExternalStore } from "react";

import type { ContextChip, EditorState, NewSession, SettingsTab, TerminalFailure } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";
import { ago, samePath } from "../shared/text.ts";
import { type Detection, keyRefused, useDetected } from "./detect.ts";
import { request, run, subscribeFolders, workspaceFolders } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { profileLevels, startLevel } from "./model/context.ts";
import workspace, { type Prompt } from "./model/workspace.ts";

/** ognivo's spark. */
export function Spark({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden className="spark">
      <path d="M12 1.5 14.1 9.9 22.5 12 14.1 14.1 12 22.5 9.9 14.1 1.5 12 9.9 9.9Z" />
      <circle cx="19.4" cy="4.6" r="1.35" />
      <circle cx="4.9" cy="19.1" r="0.85" />
    </svg>
  );
}

/** A thing to do that the empty state offers, and the message it gives the composer. */
export interface Suggestion {
  key: string;
  kind: "failure" | "selection" | "explain" | "tests";
  /** What it does; `code`, when there is one, follows it in the editor's font. */
  title: string;
  code?: string;
  /** A line under it: what it is about, or what the agent does. */
  detail: string;
  /** All of what it is about, on hover. */
  hint: string;
  prompt: Prompt;
}

/** VS Code's languages of files that are not code: prose, settings, data. */
const NOT_CODE = new Set(["markdown", "plaintext", "json", "jsonc", "json5", "yaml", "toml", "ini", "properties", "csv", "tsv", "log", "ignore", "dotenv", "xml"]);

/** The empty state's suggestions, from where the user is: the newest command that failed
 * in the user's terminals, the selection in the editor, and its file. A change of the
 * selection needs the user's words, so it only fills the composer; the others send. */
export function suggestions(editor: EditorState | null, failures: readonly TerminalFailure[], now = Date.now()): Suggestion[] {
  const found: Suggestion[] = [];
  const failure = failures[0];
  if (failure) {
    const command = oneLine(failure.command);
    const chip: ContextChip = {
      kind: "terminal",
      command: failure.command,
      exitCode: failure.exitCode,
      cwd: failure.cwd,
      output: failure.output,
    };
    found.push({
      key: `failure ${failure.id}`,
      kind: "failure",
      title: command ? "Fix" : "Fix the command that failed",
      code: command || undefined,
      detail: `exit ${failure.exitCode} · ${placeIn(failure.output, failure.cwd) ?? ago(failure.at, now)}`,
      hint: [command, failure.cwd ? `in ${failure.cwd}` : "", `exited with ${failure.exitCode} ${ago(failure.at, now)}`].filter(Boolean).join("\n"),
      prompt: {
        text: command ? `Fix ${inlineCode(command)} (exit ${failure.exitCode}).` : `Fix the command that failed (exit ${failure.exitCode}).`,
        chips: [chip],
        send: true,
      },
    });
  }
  if (editor) {
    const name = baseName(editor.path);
    const selection = editor.selection;
    if (selection && selection.text.trim() !== "") {
      const lines = selection.start === selection.end ? `line ${selection.start}` : `lines ${selection.start}–${selection.end}`;
      const first = selection.text
        .split(/\r?\n/)
        .map((l) => l.trim())
        .find((l) => l !== "");
      found.push({
        key: "selection",
        kind: "selection",
        title: `Change the selection in ${name}`,
        detail: first ? `${lines} · ${first}` : lines,
        hint: `${editor.path}, ${lines}: say what to change in the message`,
        prompt: {
          text: "",
          chips: [
            {
              kind: "selection",
              path: editor.path,
              abs: editor.abs,
              start: selection.start,
              end: selection.end,
              language: editor.language,
              text: selection.text,
            },
          ],
          send: false,
        },
      });
    }
    const file: ContextChip = { kind: "file", path: editor.path, abs: editor.abs };
    found.push({
      key: "explain",
      kind: "explain",
      title: `Explain ${name}`,
      detail: "What it does and how it works",
      hint: editor.path,
      prompt: { text: `Explain ${editor.path}: what it does and how it works.`, chips: [file], send: true },
    });
    // tests are for code: not for a README, a config, data
    if (!NOT_CODE.has(editor.language)) {
      found.push({
        key: "tests",
        kind: "tests",
        title: `Write tests for ${name}`,
        detail: "Then runs them",
        hint: editor.path,
        prompt: { text: `Write tests for ${editor.path}, and run them.`, chips: [file], send: true },
      });
    }
  }
  return found;
}

/** The first place in a command's output that names a file and a line, as
 * `src/parse.rs:6`: of `panicked at src/parse.rs:6:35`, ` --> src/main.rs:4:5`,
 * `src/app.ts(12,5)`, MSVC's `main.cpp(12)` or `File "app.py", line 12`; relative to `cwd`
 * when it is in it, a name alone for a path elsewhere. */
export function placeIn(output: string, cwd: string | null = null): string | undefined {
  // a file's name has an extension that starts with a letter: `127.0.0.1:8080` is none
  const m =
    /(?:^|[\s("'`[<])((?:[A-Za-z]:)?[\w.@~+/\\-]*?[\w@+-]+\.[A-Za-z][A-Za-z0-9]*)(?::(\d+)|\((\d+)(?:,\d+)?\))|File "([^"\n]+)", line (\d+)/m.exec(
      output,
    );
  if (!m) return undefined;
  const line = m[2] ?? m[3] ?? m[5];
  let path = (m[1] ?? m[4] ?? "").replace(/\\/g, "/").replace(/^\.\//, "");
  const root = cwd ? samePath(cwd) : undefined;
  if (root && samePath(path).startsWith(`${root}/`)) path = path.slice(root.length + 1);
  else if (/^(\/|[A-Za-z]:\/|~\/)/.test(path)) path = baseName(path);
  return `${path}:${line}`;
}

/** A session the empty state lists, and whether it is of a folder of this workspace. */
export interface Recent {
  thread: P.ThreadSummary;
  here: boolean;
}

/** The sessions the empty state lists, at most `max`: those of this workspace's folders
 * first, then the other projects', each the last updated first; no subagent's thread. A
 * session is this workspace's where the core lists it for one of its folders (`here`, by
 * thread), as the core names a project by its canonical path, which a folder opened by a
 * link, a junction, a mapped drive or an 8.3 name is not; or where its project is spelled as
 * a folder is. */
export function recentSessions(
  threads: readonly P.ThreadSummary[],
  folders: readonly string[],
  here: ReadonlySet<number> = new Set(),
  max = 5,
): Recent[] {
  const spelled = new Set(folders.map(samePath));
  const ours = (t: P.ThreadSummary) => here.has(t.thread) || spelled.has(samePath(t.project));
  const users = threads.filter((t) => t.kind !== "subagent");
  const mine = users.filter(ours);
  const others = users.filter((t) => !ours(t));
  return [...mine.map((thread) => ({ thread, here: true })), ...others.map((thread) => ({ thread, here: false }))].slice(0, max);
}

/** This workspace's sessions, from what the core lists for each of its folders (`lists`, in
 * the order of `folders`): each session once, the last updated first, with the folder it is
 * of, as the window spells it. */
export function workspaceSessions(
  folders: readonly string[],
  lists: readonly (readonly P.ThreadSummary[])[],
): { threads: P.ThreadSummary[]; folder: ReadonlyMap<number, string> } {
  const folder = new Map<number, string>();
  const threads: P.ThreadSummary[] = [];
  lists.forEach((list, i) => {
    const of = folders[i];
    if (of === undefined) return;
    for (const t of list) {
      if (folder.has(t.thread)) continue;
      folder.set(t.thread, of);
      threads.push(t);
    }
  });
  threads.sort((a, b) => b.updated - a.updated);
  return { threads, folder };
}

/** What the status line says of the server a new session works with, in three parts: what
 * it names (cut short when the line is narrow), where (left out in a narrow side bar: the
 * hint has it), and what it says of it, always whole. */
export interface ServerLine {
  state: "pending" | "ok" | "warn" | "failed";
  name: string;
  where: string;
  says: string;
  /** Its address and what the core found of it, on hover. */
  hint: string;
}

export function serverLine(detected: Detection, session: NewSession): ServerLine {
  const host = hostOf(session.server);
  const line = (state: ServerLine["state"], name: string, where: string, says: string, hint: string): ServerLine => ({ state, name, where, says, hint });
  if (detected === undefined) return line("pending", `Looking at ${host}…`, "", "", session.server);
  if (detected === "failed" && keyRefused(session.server, session.model)) {
    const says = session.key ? " does not take the API key" : " wants an API key";
    return line("failed", host, "", says, `${session.server}: set its API key in the settings, under Model`);
  }
  if (detected === "failed") return line("failed", host, "", " does not answer", `${session.server}: no llama-server answers there`);
  const profile = session.profile || detected.profile;
  // the level a new session gets: not one of another model's profile (model/context.ts)
  const reasoning = startLevel(session.reasoning, session.profile ? profileLevels(session.profile) : detected.levels) || detected.default_level;
  const hint = [
    session.server,
    profile ? `profile ${profile}` : "no profile fits the model",
    reasoning ? `reasoning ${reasoning}` : "",
    detected.window ? `${Math.round(detected.window / 1024)}K context` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  // the core names the model of llama-server's router only (`models` lists the router's): a
  // server of one model goes by the profile that fits it, the found one or the user's
  const router = detected.models.length > 0;
  const model = session.model || detected.model || (router ? null : profile);
  if (!model) {
    return router
      ? line("warn", host, "", " runs several models · choose one", hint)
      : line("warn", host, "", " · no profile fits its model", hint);
  }
  if (!profile) return line("warn", model, ` on ${host}`, " · no profile fits it", hint);
  // behind a router, a model that is not loaded yet starts with the session
  const status = detected.models.find((m) => m.id === model)?.status ?? "loaded";
  const ready = status === "loaded" ? "ready" : status === "loading" ? "loading" : "starts with the session";
  return line("ok", model, ` on ${host}`, ` · ${ready}`, hint);
}

const ICONS: Record<Suggestion["kind"], string> = {
  failure: "terminal",
  selection: "list-selection",
  explain: "book",
  tests: "beaker",
};

export function Welcome({
  core,
  session,
  project,
  here,
  version,
  onOpen,
  onSettings,
}: {
  core: Core;
  session: NewSession;
  /** The workspace's folder a new session works in; none without a folder. */
  project?: string;
  /** This workspace's sessions, as the core lists them for its folders. */
  here: ReadonlySet<number>;
  /** Changes as sessions start, turn, end and go. */
  version: number;
  onOpen: (t: P.ThreadSummary) => void;
  onSettings: (tab: SettingsTab) => void;
}) {
  const detected = useDetected(core, session.server, session.model);
  const folders = useSyncExternalStore(subscribeFolders, workspaceFolders);
  const editor = useSyncExternalStore(workspace.subscribe, () => workspace.editor);
  const failures = useSyncExternalStore(workspace.subscribe, () => workspace.failures);
  // "2 min ago" says so a minute on
  const [, tick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    const timer = setInterval(tick, 60_000);
    return () => clearInterval(timer);
  }, []);

  const [threads, setThreads] = useState<P.ThreadSummary[]>([]);
  useEffect(() => {
    let live = true;
    // every project's: this workspace's are the ones the core lists for its folders
    // (recentSessions)
    core.call("thread/list", { project: null, limit: 200 }).then(
      (r) => live && setThreads(r.threads),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [core, version]);
  const recent = useMemo(() => recentSessions(threads, folders, here), [threads, folders, here]);
  /** The sessions VS Code asks about deleting now: a second click asks nothing more. */
  const deleting = useRef(new Set<number>());
  const remove = (t: P.ThreadSummary) => {
    if (deleting.current.has(t.thread)) return;
    deleting.current.add(t.thread);
    void request("sessions/delete", { thread: t.thread, title: t.title })
      .then(
        (gone) => gone && setThreads((all) => all.filter((x) => x.thread !== t.thread)),
        () => {},
      )
      .finally(() => deleting.current.delete(t.thread));
  };

  // a session works in a folder: without one there is nothing to start from
  const offers = project ? suggestions(editor, failures) : [];
  const line = serverLine(detected, session);
  return (
    <div className="welcome">
      <header className="welcome-head">
        <Spark size={22} />
        <h1>{project ? `What should we work on in ${baseName(project)}?` : "What should we work on?"}</h1>
        <p className={`welcome-status is-${line.state}`} title={line.hint}>
          {line.state === "pending" ? <Icon name="loading" spin className="welcome-pending" /> : <span className="welcome-dot" aria-hidden />}
          <span className="welcome-status-text">
            <span className="welcome-status-name">{line.name}</span>
            {line.where && <span className="welcome-status-where">{line.where}</span>}
          </span>
          {line.says && <span className="welcome-status-says">{line.says}</span>}
          <button type="button" className="welcome-change" title="The server and the model of a new session" onClick={() => onSettings("model")}>
            Change
          </button>
        </p>
      </header>
      {!project && (
        <div className="callout warn">
          <Icon name="folder" />
          <span>Open a folder: a session works in a folder of the workspace.</span>
        </div>
      )}
      {offers.length > 0 && (
        <section className="welcome-section" aria-labelledby="welcome-here">
          <h2 id="welcome-here">From where you are</h2>
          <ul className="suggestions">
            {offers.map((s) => (
              <li key={s.key}>
                <button type="button" className={`suggestion is-${s.kind}`} title={s.hint} onClick={() => workspace.emitPrompt(s.prompt)}>
                  <Icon name={ICONS[s.kind]} className="suggestion-icon" />
                  <span className="suggestion-text">
                    <span className="suggestion-title">
                      {s.title}
                      {s.code !== undefined && (
                        <>
                          {" "}
                          <code>{s.code}</code>
                        </>
                      )}
                    </span>
                    <span className="suggestion-detail">{s.detail}</span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      {recent.length > 0 && (
        <section className="welcome-section recent" aria-labelledby="welcome-recent">
          <h2 id="welcome-recent">Recent</h2>
          <ul>
            {recent.map(({ thread: t, here }) => {
              const title = t.title || "New session";
              return (
                <li key={t.thread}>
                  <button type="button" className="recent-open" title={here ? title : `${title}\nin ${t.project}`} onClick={() => onOpen(t)}>
                    {t.running && <Icon name="loading" spin className="recent-icon" />}
                    <span className="recent-title">{title}</span>
                    {t.unreviewed > 0 && (
                      <span className="recent-review" title={`${t.unreviewed} ${t.unreviewed === 1 ? "file" : "files"} with changes to review`}>
                        {t.unreviewed} to review
                      </span>
                    )}
                    {!here && <span className="recent-project">{baseName(t.project)}</span>}
                    <span className="recent-time">{ago(t.updated)}</span>
                  </button>
                  {/* the core deletes no session while a turn of it runs */}
                  {!t.running && <IconButton icon="trash" title={`Delete «${title}»…`} className="recent-delete" onClick={() => remove(t)} />}
                </li>
              );
            })}
          </ul>
          <button type="button" className="recent-all" onClick={() => run("openSession")}>
            <Icon name="history" />
            <span>All sessions</span>
          </button>
        </section>
      )}
      <footer className="welcome-links">
        <button type="button" onClick={() => onSettings("mcp")}>
          <Icon name="plug" /> MCP servers
        </button>
        <button type="button" onClick={() => onSettings("instructions")}>
          <Icon name="book" /> Instructions
        </button>
      </footer>
    </div>
  );
}

/** A path's last name. */
function baseName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/** A server's address as the status line names it: `127.0.0.1:8080`. */
function hostOf(url: string): string {
  return url.trim().replace(/^[a-z][\w+.-]*:\/\//i, "").replace(/\/+$/, "") || url;
}

function oneLine(s: string): string {
  return s.replace(/\s*[\r\n]+\s*/g, " ").trim();
}

/** `s` as markdown's inline code, between backtick runs longer than any in it. */
function inlineCode(s: string): string {
  const longest = Math.max(0, ...(s.match(/`+/g) ?? []).map((run) => run.length));
  const ticks = "`".repeat(longest + 1);
  const pad = /^[` ]|[` ]$/.test(s) ? " " : "";
  return `${ticks}${pad}${s}${pad}${ticks}`;
}
