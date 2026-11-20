// ognivo in VS Code (docs/architecture.md §3, §10): the agent's panel in the secondary side
// bar (and in editor tabs); its changes reviewed where the user works (src/review.ts); the
// status bar's word on what it does; its commands in a terminal; the editor's file and
// selection, the user's failed commands, Ctrl+I and a Quick Fix as ways to ask it; the
// sessions and the server in VS Code's pickers, the core's questions in its dialogs. The
// agent itself is the Rust core, which the extension starts when none runs and which every
// window of the user's shares.

import * as vscode from "vscode";

import { Asks } from "./asks.ts";
import { Connection } from "./client.ts";
import { binary, logPath } from "./core.ts";
import { EditorTracker, FixWithOgnivo, InlinePrompt, type Prompt, fixPrompt } from "./editor.ts";
import { WorkspaceFiles } from "./files.ts";
import { ServerKeys } from "./keys.ts";
import { BASE, type HunkArg, Review } from "./review.ts";
import { chooseModel, deleteSession, pickSession } from "./sessions.ts";
import { pathFor } from "./shared/context.ts";
import type * as P from "./shared/protocol.ts";
import { StatusItem } from "./status.ts";
import { AgentCommands, TerminalFailures } from "./terminals.ts";
import { Turns } from "./turns.ts";
import { type Host, Pages, local } from "./view.ts";
import { finishedNotice, unlinked } from "./wording.ts";

/** The globalState key that says the panel was shown once, on the first start. */
const REVEALED = "ognivo.revealed";

/** A fence around `text` that no run of backticks in it closes. */
function fenced(text: string, language: string): string {
  const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = "`".repeat(longest + 1);
  return `${fence}${language}\n${text.replace(/\n$/, "")}\n${fence}`;
}

/** The selection as a message quotes it: where it is (the file as `name` names it), and the
 * code. */
function quoted(editor: vscode.TextEditor, name: (uri: vscode.Uri) => string): string | undefined {
  const doc = editor.document;
  const parts = editor.selections
    .filter((s) => !s.isEmpty)
    .map((s) => {
      const from = s.start.line + 1;
      // a selection that ends at a line's start ends on the line before
      const to = s.end.character === 0 && s.end.line > s.start.line ? s.end.line : s.end.line + 1;
      const where = name(doc.uri);
      const lines = from === to ? `line ${from}` : `lines ${from}–${to}`;
      return `\`${where}\` ${lines}:\n${fenced(doc.getText(s), doc.languageId)}`;
    });
  return parts.length ? `${parts.join("\n\n")}\n` : undefined;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Asks for the API key of the server a new session works with, as llama-server was started
 * with it (`--api-key`), and keeps it; an empty one drops the key kept. */
async function askServerKey(keys: ServerKeys): Promise<void> {
  const server = vscode.workspace.getConfiguration("ognivo").get("server", "http://127.0.0.1:8080").trim();
  const kept = keys.has(server);
  const key = await vscode.window.showInputBox({
    title: `The API key of ${server}`,
    prompt: kept
      ? "A key is kept for this server: type another to change it, or leave it empty to drop it"
      : "The key llama-server was started with (--api-key). VS Code keeps it in its secret storage",
    password: true,
    ignoreFocusOut: true,
  });
  if (key === undefined || (!key.trim() && !kept)) return;
  await keys.set(server, key);
  void vscode.window.showInformationMessage(key.trim() ? `ognivo keeps the API key of ${server}.` : `ognivo keeps no API key for ${server} now.`);
}

export function activate(context: vscode.ExtensionContext): void {
  const bin = () => binary(vscode.workspace.getConfiguration("ognivo").get("path", ""), context.extensionPath);
  const keys = new ServerKeys(context.secrets);
  context.subscriptions.push(keys);
  const turns = new Turns();
  const asks = new Asks(
    (thread) => pages.inSight(thread),
    (thread) => pages.shownAll().some((s) => s.thread === thread),
    (thread) => pages.showThread(thread),
    () => status.changed(),
  );
  const connection = new Connection(bin, (core) => {
    asks.follow(core);
    core.onEvent((e) => onEvent(e));
    core.subscribe(() => {
      if (core.status.state === "down") {
        turns.clear();
        status.changed();
      }
    });
    // the turns that ran before this client came, and the journals as they are now
    core.call("thread/list", { project: null, limit: 200 }).then(
      ({ threads }) => turns.seed(threads),
      () => {},
    );
    review.refreshAll();
  }, keys);
  const review = new Review(connection, turns, vscode.Uri.joinPath(context.extensionUri, "media"));
  const commands = new AgentCommands((thread) => turns.root(thread));
  const files = new WorkspaceFiles();
  const editor = new EditorTracker((state) => pages.broadcast({ t: "editor", editor: state }));
  const failures = new TerminalFailures(
    (t) => commands.owns(t),
    (list) => pages.broadcast({ t: "failures", failures: list }),
  );
  /** A file as the text the page in use gets names it: for the session it shows, relative to
   * that session's project (as the folders spell it) where the file is inside it, else by its
   * path on disk; for a new session, as the workspace names it. Another project's session
   * would read a path relative to this window's folder as a file of its own. */
  const named = (uri: vscode.Uri): string => {
    const project = pages.targetShown()?.project;
    return project ? pathFor(uri.fsPath, local(project)) : vscode.workspace.asRelativePath(uri, false);
  };
  /** A message for the composer of the page in use: shown there, sent when it says so. */
  const prompt = (p: Prompt, focus: boolean) => pages.toPage({ t: "prompt", text: p.text, chips: p.chips, send: p.send }, focus ? "focus" : "quiet");
  const inline = new InlinePrompt(prompt);

  /** Deletes a session once the user agrees, and lets the pages know. */
  const removeSession = async (thread: number, title: string) => {
    const gone = await deleteSession(connection, thread, title);
    if (gone) pages.deleted(thread);
    return gone;
  };
  const host: Host = {
    diff: (thread, path, project) => review.diff(thread, path, project),
    acceptOf: (thread, path) => review.acceptOf(thread, path),
    revertOf: (thread, path) => review.revertOf(thread, path),
    deleteSession: removeSession,
    review: (thread, path) => (path === null ? review.reviewAll(thread) : review.diffOf(thread, path)),
    findFiles: (query, limit) => files.find(query, limit),
    showCommands: () => commands.show(),
    editor: () => editor.current(),
    failures: () => failures.failures(),
  };
  const pages = new Pages(
    context,
    bin,
    host,
    () =>
      connection.get().then(
        () => undefined,
        () => undefined,
      ),
    keys,
  );
  const status = new StatusItem({
    asks: () => asks.waiting(),
    shown: () => pages.shown(),
    activity: (thread) => turns.activityOf(thread),
    review: (thread) => review.filesOf(thread),
  });

  /** An event of the core's, as the extension's own client hears every thread's. */
  const onEvent = (e: P.Event) => {
    turns.event(e);
    review.event(e);
    commands.event(e);
    if (e.event === "turn_completed") void finished(e.thread, e.end);
  };

  /** A turn of a session on show ended while VS Code was not in front: a notice says so. */
  const finished = async (thread: number, end: P.TurnEnd) => {
    if (!vscode.workspace.getConfiguration("ognivo").get("notifications", true)) return;
    if (vscode.window.state.focused || end === "interrupted") return;
    const shown = pages.shownAll().find((s) => s.thread === thread);
    if (!shown) return;
    await review.load(thread);
    const count = review.filesOf(thread).length;
    const choice = await vscode.window.showInformationMessage(finishedNotice(shown.title, end, count), ...(count ? ["Review", "Open"] : ["Open"]));
    if (choice === "Review") await review.reviewAll(thread);
    else if (choice === "Open") await pages.showThread(thread);
  };

  /** What follows the sessions on show: the review, the badge, the terminal, the status. */
  const followPages = () => {
    review.follow(pages.shownAll());
    commands.follow(pages.shown());
    pages.setBadge(review.filesOf(pages.shown()?.thread).length);
    status.changed();
  };

  const command = (id: string, run: (...args: never[]) => unknown) =>
    vscode.commands.registerCommand(id, async (...args: unknown[]) => {
      try {
        return await (run as (...a: unknown[]) => unknown)(...args);
      } catch (e) {
        void vscode.window.showErrorMessage(unlinked(`ognivo: ${message(e)}`));
        return undefined;
      }
    });

  context.subscriptions.push(
    asks,
    review,
    status,
    commands,
    editor,
    failures,
    inline,
    { dispose: () => connection.dispose() },
    { dispose: turns.onActivity(() => status.changed()) },
    vscode.window.registerWebviewViewProvider("ognivo.chat", pages, { webviewOptions: { retainContextWhenHidden: true } }),
    vscode.workspace.registerTextDocumentContentProvider(BASE, review),
    vscode.languages.registerCodeLensProvider({ scheme: "file" }, review),
    vscode.window.registerFileDecorationProvider(review),
    vscode.languages.registerCodeActionsProvider({ scheme: "file" }, new FixWithOgnivo(), { providedCodeActionKinds: FixWithOgnivo.kinds }),
    review.onChanged(() => {
      pages.setBadge(review.filesOf(pages.shown()?.thread).length);
      status.changed();
    }),
    pages.onChange(followPages),
    // the review spells the sessions' projects as the folders do
    vscode.workspace.onDidChangeWorkspaceFolders(followPages),
    pages.onShown((s) => {
      // `ognivo: Delete This Session` in the view's menu while a session shows
      void vscode.commands.executeCommand("setContext", "ognivo.session", s !== undefined);
    }),
    command("ognivo.focus", () => vscode.commands.executeCommand("ognivo.chat.focus")),
    command("ognivo.newSession", () => pages.toPage({ t: "new" })),
    command("ognivo.openSession", async () => {
      const thread = await pickSession(connection, (gone) => pages.deleted(gone));
      if (thread !== undefined) await pages.toPage({ t: "open", thread });
    }),
    command("ognivo.deleteSession", async () => {
      const shown = pages.shown();
      if (shown) await removeSession(shown.thread, shown.title);
    }),
    command("ognivo.openInEditor", () => pages.openTab()),
    command("ognivo.showThread", (thread: number) => (typeof thread === "number" ? pages.showThread(thread) : vscode.commands.executeCommand("ognivo.chat.focus"))),
    command("ognivo.addSelection", async () => {
      const active = vscode.window.activeTextEditor;
      const text = active && quoted(active, named);
      if (text) await pages.toPage({ t: "insert", text });
    }),
    command("ognivo.addFile", async (uri?: vscode.Uri) => {
      const file = uri ?? vscode.window.activeTextEditor?.document.uri;
      if (file?.scheme !== "file") return;
      await pages.toPage({ t: "insert", text: `@${named(file)} ` });
    }),
    command("ognivo.chooseModel", () => chooseModel(connection, keys)),
    command("ognivo.serverKey", () => askServerKey(keys)),
    command("ognivo.settings", () => pages.toPage({ t: "settings" })),
    command("ognivo.mcpServers", () => pages.toPage({ t: "settings", tab: "mcp" })),
    command("ognivo.instructions", () => pages.toPage({ t: "settings", tab: "instructions" })),
    command("ognivo.vscodeSettings", () => vscode.commands.executeCommand("workbench.action.openSettings", "@ext:ognivo.ognivo")),
    command("ognivo.showLog", async () => {
      try {
        await vscode.window.showTextDocument(vscode.Uri.file(logPath()), { preview: true });
      } catch {
        void vscode.window.showInformationMessage(unlinked(`ognivo: no core has written its log yet (${logPath()}).`));
      }
    }),
    // the review in the editor
    command("ognivo.review", (thread?: number) => review.reviewAll(thread)),
    command("ognivo.keepHunk", (arg?: HunkArg) => review.hunk(true, arg)),
    command("ognivo.undoHunk", (arg?: HunkArg) => review.hunk(false, arg)),
    command("ognivo.keepFile", (uri?: vscode.Uri) => review.keepFile(uri)),
    command("ognivo.undoFile", (uri?: vscode.Uri) => review.undoFile(uri)),
    command("ognivo.diffFile", (uri?: vscode.Uri, line?: number) => review.diffFile(uri, line)),
    command("ognivo.nextChange", () => review.go(1)),
    command("ognivo.previousChange", () => review.go(-1)),
    // the ways to ask from the editor
    command("ognivo.editSelection", () => inline.start()),
    command("ognivo.prompt.edit", (reply: vscode.CommentReply) => inline.reply(reply, true)),
    command("ognivo.prompt.ask", (reply: vscode.CommentReply) => inline.reply(reply, false)),
    command("ognivo.fixProblem", async (uri?: vscode.Uri, diagnostics?: vscode.Diagnostic[]) => {
      const p = await fixPrompt(uri, diagnostics);
      if (p) await prompt(p, false);
      else void vscode.window.showInformationMessage("ognivo: no problem at the cursor to fix.");
    }),
    command("ognivo.showCommands", () => commands.show()),
  );

  // the panel moved to the secondary side bar, where nobody looks for it yet: shown once
  if (!context.globalState.get<boolean>(REVEALED, false)) {
    void context.globalState.update(REVEALED, true);
    void vscode.commands.executeCommand("ognivo.chat.focus");
  }
}

export function deactivate(): void {}
