// The agent's pages (docs/architecture.md §10): the panel in the secondary side bar, and
// tabs among the editors (`ognivo: Open the Agent in an Editor Tab`). Each page has a
// connection of its own to the core: what the core sends goes to the page as it is, and
// what the page sends goes to the core once core.ts lets it, a call that names a server
// with that server's API key put in (keys.ts). Besides, a page asks the
// extension for its storage (in globalState: a webview's own storage is not kept for sure),
// the workspace, the settings of a new session, the file the user works in and the commands
// that failed in the user's terminals, the workspace's files for `@`, to open files, diffs
// and the review of a session's changes in the editor, the terminal of the agent's commands,
// and to open links in the browser.
//
// What the extension sends a page before the page said it is ready waits for that: a
// webview's script drops a message that comes before it listens.

import { randomBytes } from "node:crypto";
import { realpathSync } from "node:fs";
import type { Socket } from "node:net";
import { isAbsolute, join } from "node:path";

import * as vscode from "vscode";

import { allowed, connectCore, lines, refusal } from "./core.ts";
import * as instructions from "./instructions.ts";
import { KeyedLines, type ServerKeys } from "./keys.ts";
import {
  type EditorState,
  type FromPage,
  type NewSession,
  type Place,
  type Requests,
  SESSION_SETTINGS,
  type TerminalFailure,
  type ToPage,
} from "./shared/messages.ts";
import { type Spelling, respelled, spellingsOf } from "./shared/text.ts";
import { unlinked } from "./wording.ts";

const STATE = "ognivo.page";
/** The extension's commands a page may run. */
const COMMANDS = new Set(["openSession", "chooseModel", "vscodeSettings", "newSession", "showLog"]);
/** The files `files/find` gives when the page does not say how many. */
const FIND_LIMIT = 20;
/** Messages that wait for a page to be ready, at most; the oldest go first. */
const OUTBOX = 200;

/** What the pages need of the rest of the extension. */
export interface Host {
  /** Opens a changed file's change in the editor's diff. */
  diff(thread: number, path: string, project: string): Promise<void>;
  /** Keeps a file's change as it is, or every file's (`null`); whether it did. */
  acceptOf(thread: number, path: string | null): Promise<boolean>;
  /** Undoes a file's change, or every file's (`null`), once the user agrees; whether it did. */
  revertOf(thread: number, path: string | null): Promise<boolean>;
  /** Deletes a session once the user agrees; whether it did. */
  deleteSession(thread: number, title: string): Promise<boolean>;
  /** Opens the review of a session's unreviewed changes: every file's in one tab (`path`
   * `null`), or a file's diff. */
  review(thread: number, path: string | null): Promise<void>;
  /** The workspace's files that match `query`, the best first, at most `limit`. */
  findFiles(query: string, limit: number): Promise<string[]>;
  /** Shows the "ognivo" terminal of the agent's commands. */
  showCommands(): void;
  /** The file the user works in now. */
  editor(): EditorState | null;
  /** The commands that failed in the user's terminals, the newest first. */
  failures(): TerminalFailure[];
}

/** What the extension answers a page's request with. */
async function answer<M extends keyof Requests>(
  host: Host,
  keys: ServerKeys,
  method: M,
  params: Requests[M]["params"],
): Promise<Requests[M]["result"]> {
  switch (method) {
    case "server/key": {
      const p = params as Requests["server/key"]["params"];
      await keys.set(p.url, p.key ?? undefined);
      return null as Requests[M]["result"];
    }
    case "settings/set": {
      const p = params as Requests["settings/set"]["params"];
      if (!(SESSION_SETTINGS as readonly string[]).includes(p.key)) throw new Error(`no setting ognivo.${p.key}`);
      await vscode.workspace.getConfiguration("ognivo").update(p.key, p.value, vscode.ConfigurationTarget.Global);
      return null as Requests[M]["result"];
    }
    case "instructions/list":
      return instructions.list((params as Requests["instructions/list"]["params"]).project) as Requests[M]["result"];
    case "instructions/save": {
      const p = params as Requests["instructions/save"]["params"];
      instructions.save(p.path, p.text);
      return null as Requests[M]["result"];
    }
    case "instructions/open":
      await instructions.open((params as Requests["instructions/open"]["params"]).path);
      return null as Requests[M]["result"];
    case "changes/accept": {
      const p = params as Requests["changes/accept"]["params"];
      return (await host.acceptOf(p.thread, p.path)) as Requests[M]["result"];
    }
    case "changes/revert": {
      const p = params as Requests["changes/revert"]["params"];
      return (await host.revertOf(p.thread, p.path)) as Requests[M]["result"];
    }
    case "sessions/delete": {
      const p = params as Requests["sessions/delete"]["params"];
      return (await host.deleteSession(p.thread, p.title)) as Requests[M]["result"];
    }
    case "exa/check":
      return (await checkExa((params as Requests["exa/check"]["params"]).key)) as Requests[M]["result"];
    case "files/find": {
      const p = params as Requests["files/find"]["params"];
      const limit = typeof p.limit === "number" && p.limit > 0 ? Math.min(Math.floor(p.limit), 200) : FIND_LIMIT;
      return (await host.findFiles(String(p.query ?? ""), limit)) as Requests[M]["result"];
    }
    case "review/open": {
      const p = params as Requests["review/open"]["params"];
      await host.review(p.thread, p.path ?? null);
      return null as Requests[M]["result"];
    }
    case "terminal/show":
      host.showCommands();
      return null as Requests[M]["result"];
    default:
      throw new Error(`no request ${String(method)}`);
  }
}

/** The workspace's folders on disk: the projects of this window. */
export function folders(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === "file").map((f) => f.uri.fsPath);
}

/** The folders' real paths, and the folders (`key`) they were found for. */
let spelled: { key: string; folders: Spelling[] } | undefined;

/** Each folder of the workspace with its real path, found again when the folders change. */
function spellings(): Spelling[] {
  const list = folders();
  const key = JSON.stringify(list);
  if (spelled?.key !== key) spelled = { key, folders: spellingsOf(list, (p) => realpathSync.native(p)) };
  return spelled.folders;
}

/** A path of the core's, a project or a file, as this window's folders spell it. The core
 * names a project by its real path (links, junctions, subst and mapped drives and 8.3 names
 * resolved), and VS Code a file by the folder's spelling the user opened: only in that
 * spelling is it the file the editors have open. */
export function local(path: string): string {
  return respelled(path, spellings());
}

/** The settings of a new session; `keys` say whether its server has a key kept. */
export function newSession(keys?: Pick<ServerKeys, "has">): NewSession {
  const c = vscode.workspace.getConfiguration("ognivo");
  const server = c.get("server", "http://127.0.0.1:8080");
  return {
    server,
    model: c.get("model", ""),
    profile: c.get("profile", ""),
    reasoning: c.get("reasoning", ""),
    context: c.get("context", 0),
    mode: c.get("mode", "edit_automatically"),
    native: c.get("native", false),
    subagents: c.get("subagents", false),
    key: keys?.has(server) ?? false,
  };
}

/** A session a page shows: its thread, title and project. */
export interface Shown {
  thread: number;
  title: string;
  project: string;
}

/** One page: its webview and its connection to the core. */
class Page {
  socket?: Socket;
  private queue: string[] = [];
  private connecting = false;
  /** Whether its script said it listens: what comes before waits in `outbox`. */
  private ready = false;
  private outbox: ToPage[] = [];
  /** The thread it shows, with its title and project. */
  shown?: Shown;
  /** What it sends the core, in order, with the servers' keys put in. */
  private readonly lines: KeyedLines;

  constructor(
    readonly webview: vscode.Webview,
    readonly place: Place,
    readonly visible: () => boolean,
    /** Brings it into sight; with the focus unless `preserveFocus`. */
    readonly reveal: (preserveFocus: boolean) => void,
    keys: ServerKeys,
  ) {
    this.lines = new KeyedLines(keys, (line) => {
      if (this.socket) this.socket.write(`${line}\n`);
      else this.queue.push(line);
    });
  }

  post(m: ToPage): void {
    if (this.ready) {
      void this.webview.postMessage(m);
      return;
    }
    // the hello that the page gets says the editor and the terminals as they are then
    if (m.t === "editor" || m.t === "failures") return;
    this.outbox.push(m);
    if (this.outbox.length > OUTBOX) this.outbox.shift();
  }

  /** The page listens: its hello, then what waited for it. */
  hello(m: Extract<ToPage, { t: "hello" }>): void {
    this.ready = true;
    void this.webview.postMessage(m);
    for (const waiting of this.outbox.splice(0)) void this.webview.postMessage(waiting);
  }

  connect(bin: string): void {
    if (this.connecting || this.socket) return;
    this.connecting = true;
    connectCore(bin).then(
      (socket) => {
        this.socket = socket;
        socket.setEncoding("utf8");
        socket.on(
          "data",
          lines((line) => this.post({ t: "core", data: line })),
        );
        socket.on("error", () => {});
        socket.on("close", () => {
          this.socket = undefined;
          this.connecting = false;
          this.post({ t: "core", data: { core: "closed", why: "the core closed the connection" } });
        });
        for (const line of this.queue.splice(0)) socket.write(`${line}\n`);
      },
      (e: unknown) => {
        this.connecting = false;
        this.post({ t: "core", data: { core: "failed", why: e instanceof Error ? e.message : String(e) } });
      },
    );
  }

  send(data: unknown): void {
    if (!allowed(data)) {
      const answer = refusal(data);
      if (answer) this.post({ t: "core", data: answer });
      return;
    }
    this.lines.send(data);
  }

  dispose(): void {
    this.socket?.destroy();
    this.socket = undefined;
  }
}

/** Whether Exa takes `key`: one search of one result, as its MCP server would make it. */
async function checkExa(key: string): Promise<Requests["exa/check"]["result"]> {
  try {
    const res = await fetch("https://api.exa.ai/search", {
      method: "POST",
      headers: { "content-type": "application/json", "x-api-key": key },
      body: JSON.stringify({ query: "Exa", numResults: 1 }),
      signal: AbortSignal.timeout(15_000),
    });
    if (res.ok) return { ok: true, says: "Exa takes the key: searches work" };
    const body = (await res.text()).slice(0, 300);
    const why = /"(?:error|message)"\s*:\s*"([^"]+)"/.exec(body)?.[1] ?? body;
    if (res.status === 401 || res.status === 403) return { ok: false, says: `Exa does not take the key (${res.status}): ${why}` };
    if (res.status === 402 || res.status === 429) return { ok: false, says: `Exa takes the key but answers ${res.status}: ${why}` };
    return { ok: false, says: `Exa answers ${res.status}: ${why}` };
  } catch (e) {
    return { ok: false, says: `Exa did not answer: ${e instanceof Error ? e.message : String(e)}` };
  }
}

/** How a message to a page brings the page into sight: `reveal` shows a page that is out
 * of sight, with the focus; `focus` gives the page the focus in any case; `quiet` shows it
 * and leaves the focus where it is (the editor, where the agent's change will show). */
export type Showing = "reveal" | "focus" | "quiet";

export class Pages implements vscode.WebviewViewProvider {
  private readonly all = new Set<Page>();
  private sidebar?: { view: vscode.WebviewView; page: Page };
  /** The page used last, which the editor's commands go to. */
  private last?: Page;
  private state: Record<string, string>;
  /** Messages for a page that is not there yet: the side bar's, while it opens. */
  private waiting: ToPage[] = [];
  private badge = 0;
  private readonly shownEmitter = new vscode.EventEmitter<Shown | undefined>();
  /** The thread of the page used last changed. */
  readonly onShown = this.shownEmitter.event;
  private readonly changeEmitter = new vscode.EventEmitter<void>();
  /** What some page shows changed, or a page came or went. */
  readonly onChange = this.changeEmitter.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly bin: () => string,
    private readonly host: Host,
    /** A page connects: the extension's own client first, for the questions only it
     * answers, and to replace a core of another build before the page reaches it. */
    private readonly onConnect: () => Promise<void>,
    private readonly keys: ServerKeys,
  ) {
    this.state = { ...context.globalState.get<Record<string, string>>(STATE, {}) };
    context.subscriptions.push(
      this.shownEmitter,
      this.changeEmitter,
      vscode.workspace.onDidChangeWorkspaceFolders(() => {
        spelled = undefined;
        this.broadcast({ t: "folders", folders: folders() });
      }),
      // a key kept or dropped here or in another window: the pages ask the servers again
      keys.onDidChange(() => {
        this.broadcast({ t: "session", session: newSession(keys) });
        this.broadcast({ t: "keys" });
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("ognivo")) this.broadcast({ t: "session", session: newSession(keys) });
      }),
    );
  }

  /** The thread the page used last shows. */
  shown(): Shown | undefined {
    return this.last?.shown;
  }

  /** The sessions the pages show, the page used last's first. */
  shownAll(): Shown[] {
    const pages = [...this.all].sort((a, b) => Number(b === this.last) - Number(a === this.last));
    const seen = new Set<number>();
    const out: Shown[] = [];
    for (const p of pages) {
      if (p.shown && !seen.has(p.shown.thread)) {
        seen.add(p.shown.thread);
        out.push(p.shown);
      }
    }
    return out;
  }

  /** Whether some page of this window is in sight, and the one showing `thread` if any. */
  inSight(thread?: number): boolean {
    for (const p of this.all) {
      if (p.visible() && (thread === undefined || p.shown?.thread === thread)) return true;
    }
    return false;
  }

  /** The number on the panel's icon: the session's files to review. */
  setBadge(files: number): void {
    this.badge = files;
    if (this.sidebar) this.applyBadge(this.sidebar.view);
  }

  private applyBadge(view: vscode.WebviewView): void {
    view.badge = this.badge ? { value: this.badge, tooltip: `${this.badge} ${this.badge === 1 ? "file" : "files"} to review` } : undefined;
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    const page = this.attach(view.webview, "sidebar", () => view.visible, (preserveFocus) => view.show(preserveFocus), view);
    this.sidebar = { view, page };
    this.applyBadge(view);
    view.onDidDispose(() => {
      if (this.sidebar?.view === view) this.sidebar = undefined;
    });
  }

  /** A tab among the editors with a page of its own. */
  openTab(): void {
    const panel = vscode.window.createWebviewPanel("ognivo.tab", "ognivo", vscode.ViewColumn.Active, {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, "dist")],
    });
    panel.iconPath = vscode.Uri.joinPath(this.context.extensionUri, "media", "ognivo.svg");
    this.attach(panel.webview, "editor", () => panel.visible, (preserveFocus) => panel.reveal(undefined, preserveFocus), undefined, panel);
  }

  /** The session the page a message goes to (`toPage`) shows, if it shows one: a file the
   * message names is named for its project. */
  targetShown(): Shown | undefined {
    return (this.target() ?? this.sidebar?.page)?.shown;
  }

  /** The page a message goes to: the page used last while it is in sight, else one that is,
   * the side bar's first. */
  private target(): Page | undefined {
    if (this.last && this.all.has(this.last) && this.last.visible()) return this.last;
    if (this.sidebar?.page.visible()) return this.sidebar.page;
    for (const p of this.all) if (p.visible()) return p;
    return undefined;
  }

  /** Sends `m` to the page used last, or to the one in sight; brings the side bar's page
   * into sight first when no page is (`showing` says how). */
  async toPage(m: ToPage, showing: Showing = "reveal"): Promise<void> {
    let page = this.target();
    if (page) {
      if (showing === "focus") page.reveal(false);
    } else if (this.sidebar) {
      this.sidebar.page.reveal(showing === "quiet");
      page = this.sidebar.page;
    } else {
      // the side bar's page is made as its view opens: the message waits for it
      this.waiting.push(m);
      await vscode.commands.executeCommand("ognivo.chat.focus");
      if (showing === "quiet") await vscode.commands.executeCommand("workbench.action.focusActiveEditorGroup");
      return;
    }
    page.post(m);
  }

  /** Brings the page that shows `thread` into sight, or opens `thread` in the page used last. */
  async showThread(thread: number): Promise<void> {
    const pages = [...this.all].filter((p) => p.shown?.thread === thread);
    const page = pages.find((p) => p === this.last) ?? pages.find((p) => p.visible()) ?? pages[0];
    if (page) {
      page.reveal(false);
      this.use(page);
      return;
    }
    await this.toPage({ t: "open", thread }, "focus");
  }

  /** Sends `m` to every page. */
  broadcast(m: ToPage): void {
    for (const p of this.all) p.post(m);
  }

  /** A session was deleted: the pages let go of it. */
  deleted(thread: number): void {
    this.broadcast({ t: "deleted", thread });
  }

  private attach(
    webview: vscode.Webview,
    place: Place,
    visible: () => boolean,
    reveal: (preserveFocus: boolean) => void,
    view?: vscode.WebviewView,
    panel?: vscode.WebviewPanel,
  ): Page {
    const dist = vscode.Uri.joinPath(this.context.extensionUri, "dist");
    webview.options = { enableScripts: true, localResourceRoots: [dist] };
    webview.html = html(webview, dist, place);
    const page = new Page(webview, place, visible, reveal, this.keys);
    this.all.add(page);
    if (!this.last) this.last = page;
    if (view) for (const m of this.waiting.splice(0)) page.post(m);
    const subscriptions = [
      webview.onDidReceiveMessage((m: FromPage) => this.receive(page, m, view, panel)),
      ...(panel ? [panel.onDidChangeViewState((e) => e.webviewPanel.active && this.use(page))] : []),
      ...(view ? [view.onDidChangeVisibility(() => view.visible && this.use(page))] : []),
    ];
    const dispose = () => {
      page.dispose();
      this.all.delete(page);
      if (this.last === page) {
        this.last = this.sidebar?.page !== page ? this.sidebar?.page : undefined;
        this.shownEmitter.fire(this.last?.shown);
      }
      for (const s of subscriptions) s.dispose();
      this.changeEmitter.fire();
    };
    if (view) view.onDidDispose(dispose);
    if (panel) panel.onDidDispose(dispose);
    this.changeEmitter.fire();
    return page;
  }

  private use(page: Page): void {
    if (this.last === page) return;
    this.last = page;
    this.shownEmitter.fire(page.shown);
    this.changeEmitter.fire();
  }

  private async receive(page: Page, m: FromPage, view?: vscode.WebviewView, panel?: vscode.WebviewPanel): Promise<void> {
    switch (m.t) {
      case "ready":
        page.hello({
          t: "hello",
          folders: folders(),
          state: this.state,
          place: page.place,
          session: newSession(this.keys),
          editor: this.host.editor(),
          failures: this.host.failures(),
        });
        break;
      case "connect":
        await this.onConnect();
        page.connect(this.bin());
        break;
      case "core":
        // a page out of sight calls too (it lists the sessions as turns end): only one in
        // sight is the page the user uses
        if (page.visible()) this.use(page);
        page.send(m.data);
        break;
      case "store":
        if (m.value === null) delete this.state[m.key];
        else this.state[m.key] = m.value;
        await this.context.globalState.update(STATE, this.state);
        break;
      case "shown": {
        page.shown = m.thread === null ? undefined : { thread: m.thread, title: m.title, project: m.project };
        const title = m.thread === null ? "" : m.title || "New session";
        if (view) view.description = title;
        if (panel) panel.title = title ? `ognivo · ${title}` : "ognivo";
        if (this.last === page) this.shownEmitter.fire(page.shown);
        // a page out of sight that picks its session up again is not the one in use
        else if (page.visible()) this.use(page);
        // the review follows every page's session
        this.changeEmitter.fire();
        break;
      }
      // the page has the core's spelling of the project
      case "open":
        await openFile(m.path, local(m.project), m.line, page.place === "editor");
        break;
      case "diff":
        await this.host.diff(m.thread, m.path, local(m.project));
        break;
      case "link":
        await openLink(m.href);
        break;
      case "command":
        if (!COMMANDS.has(m.command)) break;
        this.use(page);
        await vscode.commands.executeCommand(`ognivo.${m.command}`);
        break;
      case "request":
        try {
          const result = await answer(this.host, this.keys, m.method, m.params as never);
          page.post({ t: "response", id: m.id, result });
        } catch (e) {
          page.post({ t: "response", id: m.id, error: e instanceof Error ? e.message : String(e) });
        }
        break;
    }
  }
}

/** `path`, relative to `project` or absolute, in the editor at `line` (from 1): beside a
 * tab of the agent, in the active group from the side bar. A path of the core's is spelled
 * as the folders spell it. */
export async function openFile(path: string, project: string, line?: number, beside = false): Promise<void> {
  const file = local(isAbsolute(path) ? path : join(project, path));
  const at = line && line > 0 ? new vscode.Position(line - 1, 0) : undefined;
  try {
    await vscode.window.showTextDocument(vscode.Uri.file(file), {
      preview: true,
      viewColumn: beside ? vscode.ViewColumn.Beside : vscode.ViewColumn.Active,
      selection: at ? new vscode.Range(at, at) : undefined,
    });
  } catch (e) {
    void vscode.window.showErrorMessage(unlinked(`ognivo: ${file} does not open: ${e instanceof Error ? e.message : String(e)}`));
  }
}

/** A link out of a page: http and https only, in the browser once the user agrees. */
async function openLink(href: string): Promise<void> {
  let uri: vscode.Uri;
  try {
    uri = vscode.Uri.parse(href, true);
  } catch {
    return;
  }
  if (uri.scheme !== "https" && uri.scheme !== "http") return;
  const open = await vscode.window.showInformationMessage(`Open ${uri.toString(true)} in the browser?`, { modal: true }, "Open");
  if (open === "Open") await vscode.env.openExternal(uri);
}

/** The page: the build's script and style, under a policy that lets in nothing else. */
function html(webview: vscode.Webview, dist: vscode.Uri, place: Place): string {
  const js = webview.asWebviewUri(vscode.Uri.joinPath(dist, "webview", "main.js"));
  const css = webview.asWebviewUri(vscode.Uri.joinPath(dist, "webview", "main.css"));
  const nonce = randomBytes(16).toString("base64");
  const csp = [
    "default-src 'none'",
    `script-src ${webview.cspSource} 'nonce-${nonce}'`,
    // React and the list set styles on elements
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `img-src ${webview.cspSource} data: blob:`,
    `font-src ${webview.cspSource}`,
    `connect-src ${webview.cspSource}`,
    "base-uri 'none'",
    "form-action 'none'",
  ].join("; ");
  return `<!doctype html>
<html lang="en" data-place="${place}">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <meta http-equiv="Content-Security-Policy" content="${csp}" />
    <link rel="stylesheet" href="${css}" />
    <title>ognivo</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" nonce="${nonce}" src="${js}"></script>
  </body>
</html>`;
}
