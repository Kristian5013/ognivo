// The Changes view (docs/architecture.md §8): what the agent of the session on show changed
// and nobody reviewed yet, from the core's edit journal. A file opens in the editor's own
// diff, against what it held before the session (`changes/base`); it is accepted or put
// back from the view, one file or all.

import { basename, dirname, isAbsolute, join } from "node:path";

import * as vscode from "vscode";

import type { Connection } from "./client.ts";
import { ERRORS } from "./shared/protocol.ts";
import type * as P from "./shared/protocol.ts";
import { CoreError } from "./shared/rpc.ts";

/** The scheme of a file as it was before the session: `ognivo-base:/<path>?<thread>`. */
export const BASE = "ognivo-base";

/** A file of the view. */
interface FileNode {
  file: P.ChangedFile;
  thread: number;
  project: string;
}

function absolute(path: string, project: string): string {
  return isAbsolute(path) ? path : join(project, path);
}

function counts(f: P.ChangedFile): { added: number; removed: number } {
  return {
    added: f.hunks.reduce((n, h) => n + h.added.length, 0),
    removed: f.hunks.reduce((n, h) => n + h.removed.length, 0),
  };
}

export class Changes implements vscode.TreeDataProvider<FileNode>, vscode.TextDocumentContentProvider {
  private session?: { thread: number; project: string };
  private files: FileNode[] = [];
  private readonly treeEmitter = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.treeEmitter.event;
  private readonly docEmitter = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this.docEmitter.event;
  /** The base documents open now, to tell when their session's journal changed. */
  private readonly bases = new Set<string>();
  view?: vscode.TreeView<FileNode>;

  constructor(private readonly connection: Connection) {}

  /** The session on show, whose changes the view lists. */
  show(session: { thread: number; project: string } | undefined): void {
    if (session?.thread === this.session?.thread && session?.project === this.session?.project) return;
    this.session = session;
    void this.refresh();
  }

  /** Reads the journal of the session on show again. */
  async refresh(): Promise<void> {
    const s = this.session;
    if (!s) {
      this.set([], undefined);
      return;
    }
    try {
      const core = await this.connection.get();
      const c = await core.call("changes/list", { thread: s.thread });
      if (this.session !== s) return;
      this.set(
        c.files.map((file) => ({ file, thread: s.thread, project: s.project })),
        c.commands.length ? `and ${c.commands.length} command${c.commands.length === 1 ? "" : "s"} with side effects, which a revert does not undo` : undefined,
      );
    } catch (e) {
      if (this.session !== s) return;
      // a thread the core does not have open has no journal there; a running turn holds it
      if (e instanceof CoreError && e.code === ERRORS.busy) this.set(this.files, "A turn runs: its changes show when it ends.");
      else this.set([], undefined);
    }
  }

  private set(files: FileNode[], message: string | undefined): void {
    this.files = files;
    if (this.view) {
      this.view.message = message;
      this.view.badge = files.length ? { value: files.length, tooltip: `${files.length} unreviewed` } : undefined;
    }
    void vscode.commands.executeCommand("setContext", "ognivo.unreviewed", files.length > 0);
    this.treeEmitter.fire();
    for (const uri of this.bases) this.docEmitter.fire(vscode.Uri.parse(uri));
  }

  getChildren(node?: FileNode): FileNode[] {
    return node ? [] : this.files;
  }

  getTreeItem(node: FileNode): vscode.TreeItem {
    const f = node.file;
    const path = absolute(f.path, node.project);
    const item = new vscode.TreeItem(basename(f.path), vscode.TreeItemCollapsibleState.None);
    const { added, removed } = counts(f);
    const where = dirname(f.path);
    const status = f.deleted ? "deleted" : f.created ? "new" : "";
    item.description = [where !== "." ? where : "", status, `+${added} −${removed}`].filter(Boolean).join(" · ");
    item.resourceUri = vscode.Uri.file(path);
    item.tooltip = new vscode.MarkdownString(
      `**${f.path}**  \n+${added} −${removed} against what it held before the session` +
        (f.drifted ? "  \n$(warning) Something else changed it too since the agent's edit: a revert loses that." : ""),
      true,
    );
    if (f.drifted) item.iconPath = new vscode.ThemeIcon("warning");
    item.contextValue = f.deleted ? "file.deleted" : "file";
    item.command = { command: "ognivo.changes.open", title: "Open the Change", arguments: [node] };
    return item;
  }

  /** What a file held before the session, for the left side of its diff. */
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    this.bases.add(uri.toString());
    const thread = Number(new URLSearchParams(uri.query).get("thread"));
    const path = new URLSearchParams(uri.query).get("path") ?? "";
    if (!path) return "";
    try {
      const core = await this.connection.get();
      return (await core.call("changes/base", { thread, path })).text ?? "";
    } catch {
      return "";
    }
  }

  /** The change of `path` in the editor's diff: before the session on the left, now on the
   * right; a deleted file against nothing. */
  async diff(thread: number, path: string, project: string): Promise<void> {
    const file = absolute(path, project);
    const query = new URLSearchParams({ thread: String(thread), path }).toString();
    const left = vscode.Uri.from({ scheme: BASE, path: vscode.Uri.file(file).path, query });
    const node = this.files.find((n) => n.thread === thread && n.file.path === path);
    const right = node?.file.deleted
      ? vscode.Uri.from({ scheme: BASE, path: vscode.Uri.file(file).path, query: "" })
      : vscode.Uri.file(file);
    const title = `${basename(path)} (before ognivo ↔ now)`;
    await vscode.commands.executeCommand("vscode.diff", left, right, title, { preview: true });
  }

  async accept(node?: FileNode): Promise<void> {
    const s = node ? { thread: node.thread } : this.session;
    if (!s) return;
    const path = node ? node.file.path : null;
    if (!node) {
      const all = await vscode.window.showInformationMessage(
        "Accept every unreviewed change of the session?",
        { modal: true, detail: "Builds and tests then run without asking. Nothing on disk changes." },
        "Accept All",
      );
      if (all !== "Accept All") return;
    }
    try {
      const core = await this.connection.get();
      await core.call("changes/accept", { thread: s.thread, path });
    } catch (e) {
      void vscode.window.showErrorMessage(`ognivo: ${e instanceof Error ? e.message : String(e)}`);
    }
    await this.refresh();
  }

  async revert(node: FileNode): Promise<void> {
    try {
      await this.revertOf(node.thread, node.file.path);
    } catch (e) {
      void vscode.window.showErrorMessage(`ognivo: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /** From a page: accepts one file as it is, or every file once the user agrees. */
  async acceptOf(thread: number, path: string | null): Promise<boolean> {
    if (path === null) {
      const all = await vscode.window.showInformationMessage(
        "Accept every unreviewed change of the session?",
        { modal: true, detail: "Builds and tests then run without asking. Nothing on disk changes." },
        "Accept All",
      );
      if (all !== "Accept All") return false;
    }
    const core = await this.connection.get();
    await core.call("changes/accept", { thread, path });
    await this.refresh();
    return true;
  }

  /** Puts back one file, or every file, as it was before the session, once the user agrees:
   * a file the agent created goes, and a file something else changed too loses that. */
  async revertOf(thread: number, path: string | null): Promise<boolean> {
    const core = await this.connection.get();
    const { files } = await core.call("changes/list", { thread });
    const chosen = path === null ? files : files.filter((f) => f.path === path);
    if (!chosen.length) return false;
    const drifted = chosen.filter((f) => f.drifted);
    let title: string;
    let detail: string;
    if (path !== null) {
      const f = chosen[0]!;
      const what = f.created ? "delete it: the agent created it" : "put it back as it was before the session";
      title = `Revert ${f.path}?`;
      detail = f.drifted
        ? `Something else changed it too since the agent's edit, and a revert loses that. A revert will ${what}.`
        : `A revert will ${what}.`;
    } else {
      title = `Revert every change of the session (${chosen.length} ${chosen.length === 1 ? "file" : "files"})?`;
      detail =
        "Each file goes back to what it held before the session; the files the agent created are deleted." +
        (drifted.length ? ` Something else changed ${drifted.map((f) => f.path).join(", ")} too, and a revert loses that.` : "");
    }
    const sure = await vscode.window.showWarningMessage(title, { modal: true, detail }, drifted.length ? "Revert Anyway" : "Revert");
    if (!sure) return false;
    try {
      for (const f of chosen) await core.call("changes/revert", { thread, path: f.path, force: f.drifted });
    } finally {
      await this.refresh();
    }
    return true;
  }

  /** The file itself, in the editor. */
  async openFile(node: FileNode): Promise<void> {
    await vscode.window.showTextDocument(vscode.Uri.file(absolute(node.file.path, node.project)), { preview: true });
  }

  closed(uri: vscode.Uri): void {
    this.bases.delete(uri.toString());
  }
}
