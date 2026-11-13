// The page's side of the extension (src/view.ts, src/shared/messages.ts): the extension's
// hello comes first, with the page's storage, the workspace and the settings of a new
// session; then the core's messages, the editor's insertions and the extension's commands.
// The page asks the extension to open files, changes and links, keeps its storage there,
// and runs the extension's commands.

import type { Command, FromPage, NewSession, Place, ToPage } from "../shared/messages.ts";
import type { Port } from "../shared/rpc.ts";

interface VsCodeApi {
  postMessage(message: FromPage): void;
}

declare global {
  /** VS Code's webview API, which a page may take once. */
  function acquireVsCodeApi(): VsCodeApi;
}

const api = acquireVsCodeApi();

export interface Hello {
  place: Place;
}

let kept = new Map<string, string>();
let folders: string[] = [];
let session: NewSession | undefined;
let corePort: Port | undefined;
const listeners = {
  folders: new Set<() => void>(),
  session: new Set<() => void>(),
  insert: new Set<(text: string) => void>(),
  new: new Set<() => void>(),
  open: new Set<(thread: number) => void>(),
};
/** Insertions that came while no message was being written. */
const pending: string[] = [];

function receive(m: ToPage): void {
  switch (m.t) {
    case "folders":
      folders = m.folders;
      for (const l of listeners.folders) l();
      break;
    case "session":
      session = m.session;
      for (const l of listeners.session) l();
      break;
    case "core":
      corePort?.onmessage?.({ data: m.data });
      break;
    case "insert":
      if (listeners.insert.size === 0) pending.push(m.text);
      for (const l of listeners.insert) l(m.text);
      break;
    case "new":
      for (const l of listeners.new) l();
      break;
    case "open":
      for (const l of listeners.open) l(m.thread);
      break;
    case "hello":
      break;
  }
}

/** The extension's hello, before anything else. */
export function hello(): Promise<Hello> {
  return new Promise((resolve) => {
    window.addEventListener("message", (e: MessageEvent<ToPage>) => {
      const m = e.data;
      if (!m || typeof m !== "object" || typeof m.t !== "string") return;
      if (m.t === "hello") {
        kept = new Map(Object.entries(m.state));
        folders = m.folders;
        session = m.session;
        document.documentElement.dataset.place = m.place;
        resolve({ place: m.place });
      } else {
        receive(m);
      }
    });
    api.postMessage({ t: "ready" });
  });
}

/** A port to the core through the extension, which connects once the client starts. */
export function port(): Port {
  const p: Port = {
    onmessage: null,
    postMessage: (data) => api.postMessage({ t: "core", data }),
    start: () => api.postMessage({ t: "connect" }),
  };
  corePort = p;
  return p;
}

/** The page's storage, which the extension keeps. */
export function load(key: string): string | null {
  return kept.get(key) ?? null;
}

export function save(key: string, value: string | null): void {
  if (value === null) kept.delete(key);
  else kept.set(key, value);
  api.postMessage({ t: "store", key, value });
}

export function workspaceFolders(): string[] {
  return folders;
}

export function subscribeFolders(l: () => void): () => void {
  listeners.folders.add(l);
  return () => listeners.folders.delete(l);
}

/** The settings of a new session. */
export function newSession(): NewSession | undefined {
  return session;
}

export function subscribeSession(l: () => void): () => void {
  listeners.session.add(l);
  return () => listeners.session.delete(l);
}

/** Text the editor sends to the message being written. What came while no message was
 * being written goes to the first one that is. */
export function onInsert(l: (text: string) => void): () => void {
  listeners.insert.add(l);
  for (const text of pending.splice(0)) l(text);
  return () => listeners.insert.delete(l);
}

export function onNew(l: () => void): () => void {
  listeners.new.add(l);
  return () => listeners.new.delete(l);
}

export function onOpen(l: (thread: number) => void): () => void {
  listeners.open.add(l);
  return () => listeners.open.delete(l);
}

/** `path`, relative to `project` or absolute, in the editor at `line` (from 1). */
export function openFile(path: string, project: string, line?: number): void {
  api.postMessage({ t: "open", path, project, line });
}

/** A changed file's change in the editor's diff. */
export function openDiff(thread: number, path: string, project: string): void {
  api.postMessage({ t: "diff", thread, path, project });
}

export function openLink(href: string): void {
  api.postMessage({ t: "link", href });
}

/** Tells the extension which thread the page shows: its title goes to the view's. */
export function shown(thread: number | undefined, title: string, project: string): void {
  api.postMessage({ t: "shown", thread: thread ?? null, title, project });
}

export function run(command: Command): void {
  api.postMessage({ t: "command", command });
}
