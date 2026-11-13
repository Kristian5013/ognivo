// ognivo in VS Code (docs/architecture.md §3, §10): the agent's panel in the side bar (and
// in editor tabs), its Changes view with the editor's own diffs, the sessions and the
// server in VS Code's pickers, the core's questions in its dialogs, the editor's selection
// and files sent to the message being written. The agent itself is the Rust core, which
// the extension starts when none runs and which every window of the user's shares.

import * as vscode from "vscode";

import { Asks } from "./asks.ts";
import { BASE, Changes } from "./changes.ts";
import { Connection } from "./client.ts";
import { binary, logPath } from "./core.ts";
import { chooseModel, pickSession } from "./sessions.ts";
import { Pages } from "./view.ts";

/** A fence around `text` that no run of backticks in it closes. */
function fenced(text: string, language: string): string {
  const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((m) => m[0].length));
  const fence = "`".repeat(longest + 1);
  return `${fence}${language}\n${text.replace(/\n$/, "")}\n${fence}`;
}

/** The selection as a message quotes it: where it is, and the code. */
function quoted(editor: vscode.TextEditor): string | undefined {
  const doc = editor.document;
  const parts = editor.selections
    .filter((s) => !s.isEmpty)
    .map((s) => {
      const from = s.start.line + 1;
      // a selection that ends at a line's start ends on the line before
      const to = s.end.character === 0 && s.end.line > s.start.line ? s.end.line : s.end.line + 1;
      const where = vscode.workspace.asRelativePath(doc.uri, false);
      const lines = from === to ? `line ${from}` : `lines ${from}–${to}`;
      return `\`${where}\` ${lines}:\n${fenced(doc.getText(s), doc.languageId)}`;
    });
  return parts.length ? `${parts.join("\n\n")}\n` : undefined;
}

export function activate(context: vscode.ExtensionContext): void {
  const bin = () => binary(vscode.workspace.getConfiguration("ognivo").get("path", ""), context.extensionPath);
  const asks = new Asks(
    (thread) => pages.inSight(thread),
    () => vscode.commands.executeCommand("ognivo.chat.focus"),
  );
  let refreshing: ReturnType<typeof setTimeout> | undefined;
  const connection = new Connection(bin, (core) => {
    asks.follow(core);
    // the Changes view follows the session on show: a call that edited, a turn's end
    core.onEvent((e) => {
      if (e.thread !== pages.shown()?.thread) return;
      if (e.event !== "turn_completed" && e.event !== "item_completed" && e.event !== "thread_rewound") return;
      clearTimeout(refreshing);
      refreshing = setTimeout(() => void changes.refresh(), 250);
    });
  });
  const changes = new Changes(connection);
  const pages = new Pages(context, bin, changes, () =>
    connection.get().then(
      () => undefined,
      () => undefined,
    ),
  );
  const tree = vscode.window.createTreeView("ognivo.changes", { treeDataProvider: changes });
  changes.view = tree;

  const command = (id: string, run: (...args: never[]) => unknown) =>
    vscode.commands.registerCommand(id, run as (...args: unknown[]) => unknown);

  context.subscriptions.push(
    asks,
    tree,
    { dispose: () => connection.dispose() },
    vscode.window.registerWebviewViewProvider("ognivo.chat", pages, { webviewOptions: { retainContextWhenHidden: true } }),
    vscode.workspace.registerTextDocumentContentProvider(BASE, changes),
    vscode.workspace.onDidCloseTextDocument((d) => d.uri.scheme === BASE && changes.closed(d.uri)),
    pages.onShown((s) => changes.show(s && { thread: s.thread, project: s.project })),
    command("ognivo.focus", () => vscode.commands.executeCommand("ognivo.chat.focus")),
    command("ognivo.newSession", () => pages.toPage({ t: "new" })),
    command("ognivo.openSession", async () => {
      try {
        const thread = await pickSession(connection);
        if (thread !== undefined) await pages.toPage({ t: "open", thread });
      } catch (e) {
        void vscode.window.showErrorMessage(`ognivo: ${e instanceof Error ? e.message : String(e)}`);
      }
    }),
    command("ognivo.openInEditor", () => pages.openTab()),
    command("ognivo.addSelection", async () => {
      const editor = vscode.window.activeTextEditor;
      const text = editor && quoted(editor);
      if (text) await pages.toPage({ t: "insert", text });
    }),
    command("ognivo.addFile", async (uri?: vscode.Uri) => {
      const file = uri ?? vscode.window.activeTextEditor?.document.uri;
      if (file?.scheme !== "file") return;
      await pages.toPage({ t: "insert", text: `@${vscode.workspace.asRelativePath(file, false)} ` });
    }),
    command("ognivo.chooseModel", async () => {
      try {
        await chooseModel(connection);
      } catch (e) {
        void vscode.window.showErrorMessage(`ognivo: ${e instanceof Error ? e.message : String(e)}`);
      }
    }),
    command("ognivo.settings", () => pages.toPage({ t: "settings" })),
    command("ognivo.mcpServers", () => pages.toPage({ t: "settings", tab: "mcp" })),
    command("ognivo.instructions", () => pages.toPage({ t: "settings", tab: "instructions" })),
    command("ognivo.vscodeSettings", () => vscode.commands.executeCommand("workbench.action.openSettings", "@ext:ognivo.ognivo")),
    command("ognivo.showLog", async () => {
      try {
        await vscode.window.showTextDocument(vscode.Uri.file(logPath()), { preview: true });
      } catch {
        void vscode.window.showInformationMessage(`ognivo: no core has written its log yet (${logPath()}).`);
      }
    }),
    command("ognivo.changes.open", (node: Parameters<Changes["openFile"]>[0]) =>
      changes.diff(node.thread, node.file.path, node.project),
    ),
    command("ognivo.changes.openFile", (node: Parameters<Changes["openFile"]>[0]) => changes.openFile(node)),
    command("ognivo.changes.accept", (node: Parameters<Changes["openFile"]>[0]) => changes.accept(node)),
    command("ognivo.changes.revert", (node: Parameters<Changes["openFile"]>[0]) => changes.revert(node)),
    command("ognivo.changes.acceptAll", () => changes.accept()),
    command("ognivo.changes.refresh", () => changes.refresh()),
  );
}

export function deactivate(): void {}
