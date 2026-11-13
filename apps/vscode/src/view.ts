// The agent's pages (docs/architecture.md §10): the panel in the side bar, and tabs among
// the editors (`ognivo: Open the Agent in an Editor Tab`). Each page has a connection of
// its own to the core: what the core sends goes to the page as it is, and what the page
// sends goes to the core once core.ts lets it. Besides, a page asks the extension for its
// storage (in globalState: a webview's own storage is not kept for sure), the workspace,
// the settings of a new session, to open files and changes in the editor, and to open
// links in the browser.

import { randomBytes } from "node:crypto";
import type { Socket } from "node:net";
import { isAbsolute, join } from "node:path";

import * as vscode from "vscode";

import { allowed, connectCore, lines, refusal } from "./core.ts";
import * as instructions from "./instructions.ts";
import { type FromPage, type NewSession, type Place, type Requests, SESSION_SETTINGS, type ToPage } from "./shared/messages.ts";

const STATE = "ognivo.page";
/** The extension's commands a page may run. */
const COMMANDS = new Set(["openSession", "chooseModel", "vscodeSettings", "newSession", "showLog"]);

/** What the extension answers a page's request with. */
async function answer<M extends keyof Requests>(method: M, params: Requests[M]["params"]): Promise<Requests[M]["result"]> {
  switch (method) {
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
    default:
      throw new Error(`no request ${String(method)}`);
  }
}

/** The workspace's folders on disk: the projects of this window. */
export function folders(): string[] {
  return (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === "file").map((f) => f.uri.fsPath);
}

export function newSession(): NewSession {
  const c = vscode.workspace.getConfiguration("ognivo");
  return {
    server: c.get("server", "http://127.0.0.1:8080"),
    model: c.get("model", ""),
    profile: c.get("profile", ""),
    reasoning: c.get("reasoning", ""),
    context: c.get("context", 0),
    mode: c.get("mode", "edit_automatically"),
    native: c.get("native", false),
    subagents: c.get("subagents", false),
  };
}

/** One page: its webview and its connection to the core. */
class Page {
  socket?: Socket;
  private queue: string[] = [];
  private connecting = false;
  /** The thread it shows, with its title and project. */
  shown?: { thread: number; title: string; project: string };

  constructor(
    readonly webview: vscode.Webview,
    readonly place: Place,
    readonly visible: () => boolean,
  ) {}

  post(m: ToPage): void {
    void this.webview.postMessage(m);
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
    if (this.socket) this.socket.write(`${data}\n`);
    else this.queue.push(data);
  }

  dispose(): void {
    this.socket?.destroy();
    this.socket = undefined;
  }
}

export interface Opener {
  /** Opens a changed file's change in the editor's diff. */
  diff(thread: number, path: string, project: string): Promise<void>;
}

export class Pages implements vscode.WebviewViewProvider {
  private readonly all = new Set<Page>();
  private sidebar?: { view: vscode.WebviewView; page: Page };
  /** The page used last, which the editor's commands go to. */
  private last?: Page;
  private state: Record<string, string>;
  private readonly shownEmitter = new vscode.EventEmitter<Page["shown"]>();
  /** The thread of the page used last changed. */
  readonly onShown = this.shownEmitter.event;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly bin: () => string,
    private readonly opener: Opener,
    /** A page connects: the extension's own client too, for the questions only it answers. */
    private readonly onConnect: () => void,
  ) {
    this.state = { ...context.globalState.get<Record<string, string>>(STATE, {}) };
    context.subscriptions.push(
      vscode.workspace.onDidChangeWorkspaceFolders(() => this.broadcast({ t: "folders", folders: folders() })),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("ognivo")) this.broadcast({ t: "session", session: newSession() });
      }),
    );
  }

  /** The thread the page used last shows. */
  shown(): Page["shown"] {
    return this.last?.shown;
  }

  /** Whether some page of this window is in sight, and the one showing `thread` if any. */
  inSight(thread?: number): boolean {
    for (const p of this.all) {
      if (p.visible() && (thread === undefined || p.shown?.thread === thread)) return true;
    }
    return false;
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    const page = this.attach(view.webview, "sidebar", () => view.visible, view);
    this.sidebar = { view, page };
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
    this.attach(panel.webview, "editor", () => panel.visible, undefined, panel);
  }

  /** Sends `m` to the page used last, the side bar's when none was; shows that page first. */
  async toPage(m: ToPage): Promise<void> {
    const page = this.last && this.all.has(this.last) ? this.last : this.sidebar?.page;
    if (!page || !page.visible()) await vscode.commands.executeCommand("ognivo.chat.focus");
    const target = this.last && this.all.has(this.last) ? this.last : this.sidebar?.page;
    target?.post(m);
  }

  private broadcast(m: ToPage): void {
    for (const p of this.all) p.post(m);
  }

  private attach(
    webview: vscode.Webview,
    place: Place,
    visible: () => boolean,
    view?: vscode.WebviewView,
    panel?: vscode.WebviewPanel,
  ): Page {
    const dist = vscode.Uri.joinPath(this.context.extensionUri, "dist");
    webview.options = { enableScripts: true, localResourceRoots: [dist] };
    webview.html = html(webview, dist, place);
    const page = new Page(webview, place, visible);
    this.all.add(page);
    if (!this.last) this.last = page;
    const subscriptions = [
      webview.onDidReceiveMessage((m: FromPage) => this.receive(page, m, view, panel)),
      ...(panel ? [panel.onDidChangeViewState((e) => e.webviewPanel.active && this.use(page))] : []),
      ...(view ? [view.onDidChangeVisibility(() => view.visible && this.use(page))] : []),
    ];
    const dispose = () => {
      page.dispose();
      this.all.delete(page);
      if (this.last === page) this.last = this.sidebar?.page !== page ? this.sidebar?.page : undefined;
      for (const s of subscriptions) s.dispose();
    };
    if (view) view.onDidDispose(dispose);
    if (panel) panel.onDidDispose(dispose);
    return page;
  }

  private use(page: Page): void {
    if (this.last === page) return;
    this.last = page;
    this.shownEmitter.fire(page.shown);
  }

  private async receive(page: Page, m: FromPage, view?: vscode.WebviewView, panel?: vscode.WebviewPanel): Promise<void> {
    switch (m.t) {
      case "ready":
        page.post({ t: "hello", folders: folders(), state: this.state, place: page.place, session: newSession() });
        break;
      case "connect":
        page.connect(this.bin());
        this.onConnect();
        break;
      case "core":
        this.use(page);
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
        else this.use(page);
        break;
      }
      case "open":
        await openFile(m.path, m.project, m.line, page.place === "editor");
        break;
      case "diff":
        await this.opener.diff(m.thread, m.path, m.project);
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
          page.post({ t: "response", id: m.id, result: await answer(m.method, m.params as never) });
        } catch (e) {
          page.post({ t: "response", id: m.id, error: e instanceof Error ? e.message : String(e) });
        }
        break;
    }
  }
}

/** `path`, relative to `project` or absolute, in the editor at `line` (from 1): beside a
 * tab of the agent, in the active group from the side bar. */
export async function openFile(path: string, project: string, line?: number, beside = false): Promise<void> {
  const file = isAbsolute(path) ? path : join(project, path);
  const at = line && line > 0 ? new vscode.Position(line - 1, 0) : undefined;
  try {
    await vscode.window.showTextDocument(vscode.Uri.file(file), {
      preview: true,
      viewColumn: beside ? vscode.ViewColumn.Beside : vscode.ViewColumn.Active,
      selection: at ? new vscode.Range(at, at) : undefined,
    });
  } catch (e) {
    void vscode.window.showErrorMessage(`ognivo: ${file} does not open: ${e instanceof Error ? e.message : String(e)}`);
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
