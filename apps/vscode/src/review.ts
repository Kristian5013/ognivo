// The review of the agent's changes in the editor (docs/architecture.md §8, §10): what the
// agents of the sessions on show changed and nobody reviewed yet, from the core's edit
// journal (`changes/list`, which answers while a turn runs too), shown where the user works.
//
// - In the editor: the lines a change added are tinted, and a marker on the line after
//   lines it removed shows them on hover; above each change a lens says
//   `✦ ognivo | Keep | Undo | Diff` (`changes/accept_hunk`, `changes/revert_hunk`). A file
//   with unsaved edits shows none of it: the hunks are of the file on disk.
// - In the explorer and on tabs: a ✦ and a colour on each unreviewed file.
// - In one tab: every file against what it held before the session (`vscode.changes`), and
//   each file in the editor's own diff (`vscode.diff`), the left side from `changes/base`
//   through the `ognivo-base:` scheme.
// - Keep keeps a change as it is (nothing on disk changes); Undo puts the lines back, a whole
//   file once the user agrees. An undo writes the file on disk, so a file with unsaved edits
//   is saved first, once the user agrees, or not undone.
//
// Each session on show has a snapshot of its journal, listed again (250 ms after the last
// word) when a call changed files, a turn ended or rewound, the core says the journal
// changed (`changes_updated`), the pages show other sessions, a file of it is saved or
// changes on disk otherwise (a git checkout, Discard Changes, a formatter), and VS Code gets
// the focus back. Its files are named as this window's folders spell them (view.ts `local`):
// the core names a project by its real path, which may be another spelling (a link, an 8.3
// name), and only the folder's is the file the editors have open.

import { basename, isAbsolute, join } from "node:path";

import * as vscode from "vscode";

import type { Connection } from "./client.ts";
import { addedSpan, counts, fence, hunkAt, hunkLine, isReviewTitle, nextHunk, removedMark, removedText, reviewTitle } from "./hunks.ts";
import { ERRORS } from "./shared/protocol.ts";
import type * as P from "./shared/protocol.ts";
import { CoreError } from "./shared/rpc.ts";
import type { Turns } from "./turns.ts";
import { local } from "./view.ts";
import { type ReviewFile, unlinked } from "./wording.ts";

/** The scheme of a file as it was before the session: `ognivo-base:/<path>?thread=…&path=…`. */
export const BASE = "ognivo-base";
const REFRESH_MS = 250;
/** JSON-RPC's own code for a method the core does not have: a core older than the hunks. */
const NO_METHOD = -32601;
const EMPTY: P.Changes = { files: [], commands: [], inherited: false };

/** A session on show: the review follows its journal. */
export interface Session {
  thread: number;
  project: string;
  title: string;
}

interface Snapshot {
  session: Session;
  files: P.ChangedFile[];
  /** Its files by their URIs' keys. */
  byUri: Map<string, P.ChangedFile>;
}

/** A file of a snapshot. */
interface Found {
  snapshot: Snapshot;
  file: P.ChangedFile;
  uri: vscode.Uri;
}

/** What a lens's Keep and Undo carry: the hunk as the list gave it. */
export interface HunkArg {
  thread: number;
  path: string;
  hunk: P.Hunk;
}

/** A file the journal names (relative to `project` inside it, absolute outside), as this
 * window's folders spell it (`local`): the core's spelling may be another. */
function absolute(path: string, project: string): string {
  return local(isAbsolute(path) ? path : join(project, path));
}

/** A URI as one spelling for comparing: on Windows paths are one case. */
function key(uri: vscode.Uri): string {
  const s = uri.toString();
  return process.platform === "win32" ? s.toLowerCase() : s;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function status(text: string, ms = 5000): void {
  vscode.window.setStatusBarMessage(text, ms);
}

export class Review implements vscode.TextDocumentContentProvider, vscode.CodeLensProvider, vscode.FileDecorationProvider {
  private sessions: Session[] = [];
  private readonly snapshots = new Map<number, Snapshot>();
  private readonly timers = new Map<number, ReturnType<typeof setTimeout>>();
  /** Each thread's newest listing: an older answer that comes after it is not taken. */
  private readonly tickets = new Map<number, number>();
  /** Whether the core keeps and undoes single hunks; an older one does whole files only. */
  private hunks = true;
  /** The base documents open now, and the last text each one had. */
  private readonly bases = new Map<string, string>();
  private readonly docEmitter = new vscode.EventEmitter<vscode.Uri>();
  readonly onDidChange = this.docEmitter.event;
  private readonly lensEmitter = new vscode.EventEmitter<void>();
  readonly onDidChangeCodeLenses = this.lensEmitter.event;
  private readonly decorationEmitter = new vscode.EventEmitter<vscode.Uri[] | undefined>();
  readonly onDidChangeFileDecorations = this.decorationEmitter.event;
  private readonly changedEmitter = new vscode.EventEmitter<void>();
  /** A snapshot changed: what waits for a review is something else now. */
  readonly onChanged = this.changedEmitter.event;
  private readonly added: vscode.TextEditorDecorationType;
  private readonly removedAbove: vscode.TextEditorDecorationType;
  private readonly removedBelow: vscode.TextEditorDecorationType;
  /** What each editor shows of the review now, to set it again only when it changes. */
  private readonly drawn = new WeakMap<vscode.TextEditor, string>();
  private readonly context = new Map<string, unknown>();
  /** The files of the review with unsaved edits, by their URIs' keys. */
  private readonly dirty = new Set<string>();
  private cursorTimer?: ReturnType<typeof setTimeout>;
  /** The workspace's files as they change on disk. VS Code watches its folders anyway: this
   * only hears it, and a file the review does not have costs a lookup. */
  private readonly watcher = vscode.workspace.createFileSystemWatcher("**/*");
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    private readonly connection: Connection,
    private readonly turns: Turns,
    media: vscode.Uri,
  ) {
    const ruler = vscode.OverviewRulerLane.Left;
    this.added = vscode.window.createTextEditorDecorationType({
      isWholeLine: true,
      backgroundColor: new vscode.ThemeColor("ognivo.addedLineBackground"),
      overviewRulerColor: new vscode.ThemeColor("editorOverviewRuler.addedForeground"),
      overviewRulerLane: ruler,
    });
    const removed = (where: "top" | "bottom"): vscode.DecorationRenderOptions => ({
      isWholeLine: true,
      borderColor: new vscode.ThemeColor("ognivo.removedLineMarker"),
      borderStyle: "solid",
      borderWidth: where === "top" ? "2px 0 0 0" : "0 0 2px 0",
      overviewRulerColor: new vscode.ThemeColor("ognivo.removedLineMarker"),
      overviewRulerLane: ruler,
      gutterIconPath: vscode.Uri.joinPath(media, "removed.svg"),
      gutterIconSize: "contain",
    });
    this.removedAbove = vscode.window.createTextEditorDecorationType(removed("top"));
    this.removedBelow = vscode.window.createTextEditorDecorationType(removed("bottom"));
    this.disposables.push(
      this.added,
      this.removedAbove,
      this.removedBelow,
      this.docEmitter,
      this.lensEmitter,
      this.decorationEmitter,
      this.changedEmitter,
      vscode.window.onDidChangeVisibleTextEditors((editors) => this.draw(editors)),
      vscode.window.tabGroups.onDidChangeTabs(() => this.draw()),
      vscode.window.onDidChangeActiveTextEditor(() => this.cursorChanged()),
      vscode.window.onDidChangeTextEditorSelection((e) => e.textEditor === vscode.window.activeTextEditor && this.cursorChanged()),
      vscode.workspace.onDidChangeTextDocument((e) => this.edited(e.document, e.contentChanges.length > 0)),
      vscode.workspace.onDidSaveTextDocument((d) => {
        const found = this.find(d.uri);
        if (found) this.schedule(found.snapshot.session.thread);
      }),
      // a file of the review that is not open, changed on disk by something else
      this.watcher,
      this.watcher.onDidChange((uri) => this.changedOnDisk(uri)),
      this.watcher.onDidCreate((uri) => this.changedOnDisk(uri, true)),
      this.watcher.onDidDelete((uri) => this.changedOnDisk(uri, true)),
      vscode.workspace.onDidCloseTextDocument((d) => d.uri.scheme === BASE && this.bases.delete(d.uri.toString())),
      vscode.window.onDidChangeWindowState((s) => s.focused && this.refreshAll()),
    );
    this.disposables.push({ dispose: turns.onRunning((thread) => this.runningChanged(thread)) });
    this.updateContext();
  }

  dispose(): void {
    for (const t of this.timers.values()) clearTimeout(t);
    clearTimeout(this.cursorTimer);
    for (const d of this.disposables) d.dispose();
  }

  // ---- which sessions, and their snapshots

  /** The sessions the pages show, the page used last's first. A page names a session's
   * project as the core does, by its real path: the review spells it as this window's
   * folders do, so that its files are the ones the editors have open. */
  follow(shown: Session[]): void {
    const sessions = shown.map((s) => ({ ...s, project: local(s.project) }));
    const before = new Map(this.sessions.map((s) => [s.thread, s]));
    this.sessions = sessions;
    let changed = sessions.length !== before.size || sessions.some((s, i) => s.thread !== [...before.keys()][i]);
    for (const thread of [...this.snapshots.keys()]) {
      if (sessions.some((s) => s.thread === thread)) continue;
      const gone = this.snapshots.get(thread)!;
      this.snapshots.delete(thread);
      this.decorationEmitter.fire(gone.files.map((f) => this.uriOf(f, gone.session)));
      changed = true;
    }
    for (const s of sessions) {
      const was = before.get(s.thread);
      const snapshot = this.snapshots.get(s.thread);
      if (snapshot) snapshot.session = s;
      if (!was || was.project !== s.project) this.schedule(s.thread, 0);
    }
    if (changed) this.changed();
  }

  /** The session on show of `thread`, or else the core's word on which project it is of. */
  private async session(thread: number): Promise<Session | undefined> {
    const shown = this.sessions.find((s) => s.thread === thread);
    if (shown) return shown;
    const core = await this.connection.get();
    const { threads } = await core.call("thread/list", { project: null, limit: 200 });
    const t = threads.find((x) => x.thread === thread);
    return t && { thread, project: local(t.project), title: t.title };
  }

  /** An event of the core's: the snapshot of its session is listed again where it says
   * the journal changed. */
  event(e: P.Event): void {
    if (!this.sessions.some((s) => s.thread === e.thread)) return;
    switch (e.event) {
      case "turn_completed":
      case "thread_rewound":
      case "changes_updated":
        this.schedule(e.thread);
        break;
      case "item_completed":
        if ((e.item.kind === "tool_call" && e.item.changes.length > 0) || (e.item.kind === "subagent" && e.item.changed.length > 0)) this.schedule(e.thread);
        break;
      default:
        break;
    }
  }

  /** The client connected (again): every snapshot is listed anew. */
  refreshAll(): void {
    for (const s of this.sessions) this.schedule(s.thread);
  }

  private schedule(thread: number, ms = REFRESH_MS): void {
    clearTimeout(this.timers.get(thread));
    this.timers.set(
      thread,
      setTimeout(() => {
        this.timers.delete(thread);
        void this.load(thread);
      }, ms),
    );
  }

  /** Lists the journal of `thread` again, now. */
  async load(thread: number): Promise<void> {
    const session = this.sessions.find((s) => s.thread === thread);
    if (!session) return;
    const ticket = (this.tickets.get(thread) ?? 0) + 1;
    this.tickets.set(thread, ticket);
    try {
      const core = await this.connection.get();
      const c = await core.call("changes/list", { thread });
      if (this.tickets.get(thread) === ticket) this.set(session, c);
    } catch (e) {
      if (this.tickets.get(thread) !== ticket) return;
      // a thread the core does not have open has no journal there
      if (e instanceof CoreError && e.code === ERRORS.noThread) this.set(session, EMPTY);
      // else (an older core holds the journal while a turn runs, or the core is away)
      // what was listed stays
    }
  }

  /** A list the core gave with an operation, or just now: newer than what a listing that
   * started before it will bring. */
  private take(session: Session, c: P.Changes): void {
    this.tickets.set(session.thread, (this.tickets.get(session.thread) ?? 0) + 1);
    this.set(session, c);
  }

  /** The snapshot of `session` is now `c`. */
  private set(session: Session, c: P.Changes): void {
    if (!this.sessions.some((s) => s.thread === session.thread)) return;
    const before = this.snapshots.get(session.thread);
    const byUri = new Map<string, P.ChangedFile>();
    for (const f of c.files) byUri.set(key(this.uriOf(f, session)), f);
    const now: Snapshot = { session, files: c.files, byUri };
    this.snapshots.set(session.thread, now);
    const touched: vscode.Uri[] = [];
    const paths = new Set<string>();
    for (const f of c.files) {
      const was = before?.byUri.get(key(this.uriOf(f, session)));
      if (!was || JSON.stringify(was) !== JSON.stringify(f)) {
        touched.push(this.uriOf(f, session));
        paths.add(f.path);
      }
    }
    for (const f of before?.files ?? []) {
      if (!byUri.has(key(this.uriOf(f, session)))) {
        touched.push(this.uriOf(f, session));
        paths.add(f.path);
      }
    }
    if (before && touched.length === 0) return;
    if (touched.length) this.decorationEmitter.fire(touched);
    // the left side of an open diff of a file whose change is something else now
    for (const uri of this.bases.keys()) {
      const q = new URLSearchParams(vscode.Uri.parse(uri).query);
      if (Number(q.get("thread")) === session.thread && paths.has(q.get("path") ?? "")) this.docEmitter.fire(vscode.Uri.parse(uri));
    }
    this.changed();
  }

  private changed(): void {
    this.lensEmitter.fire();
    this.draw();
    this.updateContext();
    this.changedEmitter.fire();
  }

  private runningChanged(thread: number): void {
    if (!this.sessions.some((s) => s.thread === thread)) return;
    this.lensEmitter.fire();
    this.updateContext();
  }

  private uriOf(f: P.ChangedFile, session: Session): vscode.Uri {
    return vscode.Uri.file(absolute(f.path, session.project));
  }

  /** The unreviewed file at `uri`, of the first session on show that has it. */
  private find(uri: vscode.Uri): Found | undefined {
    const k = key(uri);
    for (const s of this.sessions) {
      const snapshot = this.snapshots.get(s.thread);
      const file = snapshot?.byUri.get(k);
      if (snapshot && file) return { snapshot, file, uri: this.uriOf(file, snapshot.session) };
    }
    return undefined;
  }

  /** The unreviewed files of `thread`, as the status bar lists them. */
  filesOf(thread: number | undefined): ReviewFile[] {
    const snapshot = thread === undefined ? undefined : this.snapshots.get(thread);
    return (snapshot?.files ?? []).map((f) => ({ path: f.path, ...counts(f) }));
  }

  // ---- the editor: decorations, lenses, the explorer's marks

  /** Whether `editor` is a file's own editor, not a side of a diff: its group shows that
   * file's tab. Decorations there would cover the diff's own colours. */
  private plainEditor(editor: vscode.TextEditor): boolean {
    if (editor.document.uri.scheme !== "file" || editor.viewColumn === undefined) return false;
    const group = vscode.window.tabGroups.all.find((g) => g.viewColumn === editor.viewColumn);
    const input = group?.activeTab?.input;
    return input instanceof vscode.TabInputText && key(input.uri) === key(editor.document.uri);
  }

  private draw(editors: readonly vscode.TextEditor[] = vscode.window.visibleTextEditors): void {
    for (const editor of editors) this.decorate(editor);
  }

  private decorate(editor: vscode.TextEditor): void {
    const doc = editor.document;
    const found = !doc.isDirty && this.plainEditor(editor) ? this.find(doc.uri) : undefined;
    const added: vscode.DecorationOptions[] = [];
    const above: vscode.DecorationOptions[] = [];
    const below: vscode.DecorationOptions[] = [];
    if (found && !found.file.deleted) {
      const n = doc.lineCount;
      const whole = (line: number) => new vscode.Range(line, 0, line, doc.lineAt(line).text.length);
      for (const h of found.file.hunks) {
        const hover = h.removed.length ? this.hover(h, doc.languageId) : undefined;
        const span = addedSpan(h);
        if (span && span.start < n) {
          const end = Math.min(span.end, n - 1);
          added.push({ range: new vscode.Range(span.start, 0, end, doc.lineAt(end).text.length), hoverMessage: hover });
        }
        const mark = removedMark(h, n);
        // the lines that replaced removed ones say them on hover themselves
        if (mark) (mark.where === "above" ? above : below).push({ range: whole(mark.line), hoverMessage: span ? undefined : hover });
      }
    }
    const drawn = JSON.stringify([added, above, below].map((list) => list.map((o) => [o.range.start.line, o.range.end.line, o.range.end.character])));
    if (this.drawn.get(editor) === drawn && !found) return;
    this.drawn.set(editor, drawn);
    editor.setDecorations(this.added, added);
    editor.setDecorations(this.removedAbove, above);
    editor.setDecorations(this.removedBelow, below);
  }

  /** What a change removed, for a hover. */
  private hover(h: P.Hunk, language: string): vscode.MarkdownString {
    const { text, more } = removedText(h.removed);
    const ticks = fence(text);
    const md = new vscode.MarkdownString();
    md.appendMarkdown(`**ognivo** removed ${h.removed.length === 1 ? "a line" : `${h.removed.length} lines`} here:\n\n`);
    md.appendMarkdown(`${ticks}${language.replace(/[^\w#+.-]/g, "")}\n${text}\n${ticks}\n`);
    if (more) md.appendMarkdown(`\n… and ${more} more`);
    return md;
  }

  /** A file of a session on show changed on disk, maybe by something else than the core (a
   * git checkout, Discard Changes, a formatter in a terminal), which can leave nothing to
   * review in it: its sessions are listed again. A folder that came or went (`folder`) counts
   * for the files in it: VS Code says that a folder was deleted, and not that each of its
   * files was. */
  private changedOnDisk(uri: vscode.Uri, folder = false): void {
    if (uri.scheme !== "file") return;
    let k: string | undefined;
    for (const s of this.sessions) {
      const snapshot = this.snapshots.get(s.thread);
      if (!snapshot?.files.length) continue;
      k ??= key(uri);
      const under = `${k}/`;
      if (snapshot.byUri.has(k) || (folder && [...snapshot.byUri.keys()].some((f) => f.startsWith(under)))) this.schedule(s.thread);
    }
  }

  /** A document of a file of the review changed: on disk (the agent wrote it, an undo put
   * lines back, something else did) or by the user, whose unsaved edits hide the review until
   * they are saved. `content` says its text changed: in a document without unsaved edits, VS
   * Code read the file again from disk. */
  private edited(doc: vscode.TextDocument, content: boolean): void {
    if (doc.uri.scheme !== "file" || !this.find(doc.uri)) return;
    if (content && !doc.isDirty) this.changedOnDisk(doc.uri);
    this.draw(vscode.window.visibleTextEditors.filter((e) => e.document === doc));
    const k = key(doc.uri);
    // the lenses go with the first unsaved edit and come back with the save (or a reload)
    if (doc.isDirty !== this.dirty.has(k)) {
      if (doc.isDirty) this.dirty.add(k);
      else this.dirty.delete(k);
      this.lensEmitter.fire();
    } else if (!doc.isDirty) {
      this.lensEmitter.fire();
    }
    this.cursorChanged();
  }

  provideCodeLenses(doc: vscode.TextDocument): vscode.CodeLens[] {
    if (doc.uri.scheme !== "file" || doc.isDirty) return [];
    const found = this.find(doc.uri);
    if (!found || found.file.hunks.length === 0) return [];
    const { snapshot, file } = found;
    const thread = snapshot.session.thread;
    const n = doc.lineCount;
    const at = (line: number) => new vscode.Range(line, 0, line, 0);
    const label = (line: number, title: string, tooltip: string) => new vscode.CodeLens(at(line), { title, command: "", tooltip });
    const lens = (line: number, title: string, command: string, tooltip: string, args: unknown[]) =>
      new vscode.CodeLens(at(line), { title, command, tooltip, arguments: args });
    if (this.turns.isRunning(thread)) {
      return file.hunks.map((h) => label(hunkLine(h, n), "✦ ognivo · a turn runs", "Keep or undo the change once the turn ends"));
    }
    const diff = (line: number) => lens(line, "Diff", "ognivo.diffFile", "The file against what it held before the session", [doc.uri, line]);
    // a file the agent created or deleted is kept or undone whole, and so is every file
    // with a core that cannot take a single hunk
    if (file.created || file.deleted || !this.hunks) {
      const line = Math.min(...file.hunks.map((h) => hunkLine(h, n)));
      const what = file.created ? "✦ ognivo · new file" : file.deleted ? "✦ ognivo · deleted" : "✦ ognivo";
      return [
        label(line, what, "Changed by ognivo, not reviewed"),
        lens(line, this.hunks ? "Keep" : "Keep File", "ognivo.keepFile", "Keep the file's changes as they are", [doc.uri]),
        lens(line, this.hunks ? "Undo" : "Undo File", "ognivo.undoFile", file.created ? "Delete the file: the agent created it" : "Put the file back as it was before the session", [doc.uri]),
        diff(line),
      ];
    }
    return file.hunks.flatMap((hunk) => {
      const line = hunkLine(hunk, n);
      const arg: HunkArg = { thread, path: file.path, hunk };
      return [
        label(line, "✦ ognivo", "Changed by ognivo, not reviewed"),
        lens(line, "Keep", "ognivo.keepHunk", "Keep this change (Ctrl+Alt+Enter)", [arg]),
        lens(line, "Undo", "ognivo.undoHunk", "Put these lines back as they were (Ctrl+Alt+Backspace)", [arg]),
        diff(line),
      ];
    });
  }

  provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined {
    if (uri.scheme !== "file" || !this.find(uri)) return undefined;
    return new vscode.FileDecoration("✦", "Changed by ognivo, not reviewed", new vscode.ThemeColor("ognivo.unreviewedForeground"));
  }

  private cursorChanged(): void {
    clearTimeout(this.cursorTimer);
    this.cursorTimer = setTimeout(() => this.updateContext(), 50);
  }

  private setContext(name: string, value: unknown): void {
    const was = this.context.get(name);
    if (was !== undefined && JSON.stringify(was) === JSON.stringify(value)) return;
    this.context.set(name, value);
    void vscode.commands.executeCommand("setContext", name, value);
  }

  /** The context keys of the menus and keys: the files to review (`resource in
   * ognivo.unreviewedUris`), the change at the cursor, the turn of the session on show. */
  private updateContext(): void {
    const uris: string[] = [];
    for (const s of this.sessions) for (const f of this.snapshots.get(s.thread)?.files ?? []) uris.push(this.uriOf(f, s).toString());
    this.setContext("ognivo.unreviewedUris", uris);
    const first = this.sessions[0];
    this.setContext("ognivo.unreviewed", first !== undefined && (this.snapshots.get(first.thread)?.files.length ?? 0) > 0);
    this.setContext("ognivo.turnRunning", first !== undefined && this.turns.isRunning(first.thread));
    const here = this.atCursor();
    this.setContext("ognivo.hunkAtCursor", here !== undefined && !this.turns.isRunning(here.found.snapshot.session.thread));
  }

  /** The change at the cursor of the active editor, in a file saved as it is on disk. */
  private atCursor(): { found: Found; hunk: P.Hunk } | undefined {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.scheme !== "file" || editor.document.isDirty) return undefined;
    const found = this.find(editor.document.uri);
    if (!found) return undefined;
    const i = hunkAt(found.file.hunks, editor.selection.active.line, editor.document.lineCount);
    const hunk = found.file.hunks[i];
    return hunk ? { found, hunk } : undefined;
  }

  // ---- what the user does: keep, undo, look

  /** Keeps (`keep`) or undoes one change: the lens's, else the one at the cursor. */
  async hunk(keep: boolean, arg?: HunkArg): Promise<void> {
    let target: { found: Found; hunk: P.Hunk } | undefined;
    if (arg && typeof arg === "object" && "hunk" in arg) {
      const snapshot = this.snapshots.get(arg.thread);
      const file = snapshot?.files.find((f) => f.path === arg.path);
      if (snapshot && file) target = { found: { snapshot, file, uri: this.uriOf(file, snapshot.session) }, hunk: arg.hunk };
    } else {
      target = this.atCursor();
    }
    if (!target) {
      status("$(info) ognivo: no unreviewed change of the agent's here");
      return;
    }
    const { found, hunk } = target;
    const thread = found.snapshot.session.thread;
    const doc = vscode.workspace.textDocuments.find((d) => key(d.uri) === key(found.uri));
    if (doc?.isDirty) {
      void vscode.window.showWarningMessage(unlinked(`ognivo: save ${basename(found.file.path)} first: the agent's changes are kept and undone on disk.`));
      return;
    }
    if (this.turns.isRunning(thread)) {
      status("$(sparkle) ognivo: a turn runs: keep or undo once it ends");
      return;
    }
    if (found.file.created || found.file.deleted || !this.hunks) {
      await (keep ? this.keepFile(found.uri) : this.undoFile(found.uri));
      return;
    }
    try {
      const core = await this.connection.get();
      this.take(found.snapshot.session, await core.call(keep ? "changes/accept_hunk" : "changes/revert_hunk", { thread, path: found.file.path, hunk }));
    } catch (e) {
      if (e instanceof CoreError && e.code === ERRORS.stale) {
        status("$(warning) ognivo: the file changed since the changes were listed: they are listed again");
        this.schedule(thread, 0);
      } else if (e instanceof CoreError && e.code === ERRORS.busy) {
        status("$(sparkle) ognivo: a turn runs: keep or undo once it ends");
      } else if (e instanceof CoreError && e.code === NO_METHOD) {
        // a core from before the hunks: the file, whole, from now on
        this.hunks = false;
        this.lensEmitter.fire();
        void vscode.window.showInformationMessage("ognivo: this core keeps and undoes whole files only. Keep or undo the file instead.");
      } else if (e instanceof CoreError && e.code === ERRORS.journal && /keep or undo the whole file$/.test(e.message)) {
        // a change that goes whole only (a file that is not UTF-8 text): the file's own
        // action, once the user picks it (Undo asks again in its modal)
        const whole = keep ? "Keep the File" : "Undo the File";
        this.schedule(thread, 0);
        void vscode.window.showWarningMessage(unlinked(`ognivo: ${message(e)}.`), whole).then((choice) => {
          if (choice === whole) void (keep ? this.keepFile(found.uri) : this.undoFile(found.uri));
        });
      } else {
        void vscode.window.showErrorMessage(unlinked(`ognivo: ${message(e)}`));
        this.schedule(thread, 0);
      }
    }
  }

  /** The unreviewed file of a command's argument (a menu's URI), else the active editor's. */
  private fileOf(uri?: unknown): Found | undefined {
    const u = uri instanceof vscode.Uri ? uri : vscode.window.activeTextEditor?.document.uri;
    return u ? this.find(u) : undefined;
  }

  /** Keeps the file's changes as they are: nothing on disk changes. */
  async keepFile(uri?: unknown): Promise<void> {
    const found = this.fileOf(uri);
    if (!found) {
      status("$(info) ognivo: this file has no unreviewed change of the agent's");
      return;
    }
    await this.acceptOf(found.snapshot.session.thread, found.file.path).catch((e: unknown) => {
      void vscode.window.showErrorMessage(unlinked(`ognivo: ${busy(e) ? "a turn runs: keep the changes once it ends." : message(e)}`));
    });
  }

  /** Puts the file back as it was before the session, once the user agrees. */
  async undoFile(uri?: unknown): Promise<void> {
    const found = this.fileOf(uri);
    if (!found) {
      status("$(info) ognivo: this file has no unreviewed change of the agent's");
      return;
    }
    await this.revertOf(found.snapshot.session.thread, found.file.path).catch((e: unknown) => {
      void vscode.window.showErrorMessage(unlinked(`ognivo: ${busy(e) ? "a turn runs: undo the changes once it ends." : message(e)}`));
    });
  }

  /** Keeps one file's changes, or every file's (`null`), as they are: without a question,
   * since nothing on disk changes. Whether it did. */
  async acceptOf(thread: number, path: string | null): Promise<boolean> {
    const core = await this.connection.get();
    try {
      await core.call("changes/accept", { thread, path });
    } finally {
      await this.load(thread);
    }
    return true;
  }

  /** Puts back one file, or every file (`null`), as it was before the session, once the
   * user agrees: a file the agent created goes, and a file something else changed too loses
   * that. A file with unsaved edits in an editor is saved first, once the user agrees: the
   * undo writes the file on disk, and VS Code would keep showing the edits over it, for a save
   * to write back. Whether it did. */
  async revertOf(thread: number, path: string | null): Promise<boolean> {
    const core = await this.connection.get();
    const shown = this.sessions.find((s) => s.thread === thread);
    const list = async () => {
      const c = await core.call("changes/list", { thread });
      if (shown) this.take(shown, c);
      return { c, chosen: path === null ? c.files : c.files.filter((f) => f.path === path) };
    };
    let { c, chosen } = await list();
    const session = shown ?? (await this.session(thread));
    const unsaved = session ? this.unsaved(chosen, session) : [];
    if (unsaved.length) {
      if (!(await this.saveFirst(unsaved))) return false;
      // saved, the files hold the user's edits too: the journal says so (drifted)
      ({ c, chosen } = await list());
    }
    if (!chosen.length) {
      status(`$(info) ognivo: nothing to undo: ${path === null ? "every change of the session is kept or undone" : "the file has no unreviewed change of the agent's"}`);
      return false;
    }
    const { commands } = c;
    const drifted = chosen.filter((f) => f.drifted);
    // what the agent's commands did (a build, an install) is not in the journal
    const ran = commands.length
      ? ` ${commands.length === 1 ? "A command" : `${commands.length} commands`} it ran had effects of their own, which undoing does not reverse: ${commands.slice(0, 5).join("; ")}${commands.length > 5 ? "; …" : ""}.`
      : "";
    let title: string;
    let detail: string;
    if (path !== null) {
      const f = chosen[0]!;
      title = `Undo ognivo's changes to ${f.path}?`;
      const what = f.created ? "The agent created it: undoing deletes it." : "It goes back to what it held before the session.";
      detail = f.drifted ? `${what} Something else changed it too since the agent's edit, and undoing loses that.` : what;
    } else {
      title = `Undo every change of the session (${chosen.length} ${chosen.length === 1 ? "file" : "files"})?`;
      detail =
        "Each file goes back to what it held before the session; the files the agent created are deleted." +
        (drifted.length ? ` Something else changed ${drifted.map((f) => f.path).join(", ")} too, and undoing loses that.` : "") +
        ran;
    }
    const sure = await vscode.window.showWarningMessage(title, { modal: true, detail }, drifted.length ? "Undo Anyway" : "Undo");
    if (!sure) return false;
    try {
      // the core undoes one file at a time
      for (const f of chosen) await core.call("changes/revert", { thread, path: f.path, force: f.drifted });
    } finally {
      await this.load(thread);
    }
    return true;
  }

  /** The open documents of `files` with unsaved edits: text documents and notebooks. */
  private unsaved(files: readonly P.ChangedFile[], session: Session): (vscode.TextDocument | vscode.NotebookDocument)[] {
    const keys = new Set(files.map((f) => key(this.uriOf(f, session))));
    const docs: (vscode.TextDocument | vscode.NotebookDocument)[] = [...vscode.workspace.textDocuments, ...(vscode.workspace.notebookDocuments ?? [])];
    return docs.filter((d) => d.isDirty && keys.has(key(d.uri)));
  }

  /** Saves `docs` once the user agrees, before an undo puts their files back on disk. Whether
   * they are saved. */
  private async saveFirst(docs: readonly (vscode.TextDocument | vscode.NotebookDocument)[]): Promise<boolean> {
    const names = docs.map((d) => basename(d.uri.fsPath));
    const one = names.length === 1;
    const shown = names.length > 5 ? [...names.slice(0, 5), `${names.length - 5} more`] : names;
    const which = shown.length === 1 ? shown[0]! : `${shown.slice(0, -1).join(", ")} and ${shown[shown.length - 1]!}`;
    const detail = one
      ? `${which} has unsaved edits. The undo writes the file on disk, and the editor would keep your edits over it, ready to be saved back. Once saved, your edits are in the file too, and undoing the whole file loses them with the agent's changes. The lenses' Undo puts back the agent's changes one by one.`
      : `${which} have unsaved edits. The undo writes the files on disk, and the editors would keep your edits over them, ready to be saved back. Once saved, your edits are in the files too, and undoing whole files loses them with the agent's changes. The lenses' Undo puts back the agent's changes one by one.`;
    const sure = await vscode.window.showWarningMessage(`Save ${one ? which : `${names.length} files`} before undoing?`, { modal: true, detail }, "Save and Undo");
    if (sure !== "Save and Undo") return false;
    for (const d of docs) {
      if (await d.save()) continue;
      void vscode.window.showWarningMessage(unlinked(`ognivo: ${basename(d.uri.fsPath)} was not saved, so nothing was undone.`));
      return false;
    }
    return true;
  }

  /** The URI of what `path` held before the session, for the left side of its diff. */
  private baseUri(thread: number, path: string, file: string): vscode.Uri {
    const query = new URLSearchParams({ thread: String(thread), path }).toString();
    return vscode.Uri.from({ scheme: BASE, path: vscode.Uri.file(file).path, query });
  }

  /** What a file held before the session, for the left side of its diff. An error keeps
   * the text the side had (it does not go blank while a turn runs on an older core); a file
   * the journal no longer has (kept, undone) is as it is on disk: no change. */
  async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
    const q = new URLSearchParams(uri.query);
    const thread = Number(q.get("thread"));
    const path = q.get("path") ?? "";
    // the right side of a deleted file's diff: nothing
    if (!path) return "";
    const id = uri.toString();
    try {
      const core = await this.connection.get();
      const text = (await core.call("changes/base", { thread, path })).text ?? "";
      this.bases.set(id, text);
      return text;
    } catch (e) {
      if (e instanceof CoreError && e.code === ERRORS.journal) {
        const session = this.sessions.find((s) => s.thread === thread);
        const text = await disk(session ? absolute(path, session.project) : vscode.Uri.parse(id).with({ scheme: "file", query: "" }).fsPath);
        if (text !== undefined) {
          this.bases.set(id, text);
          return text;
        }
      }
      const kept = this.bases.get(id) ?? "";
      this.bases.set(id, kept);
      return kept;
    }
  }

  /** The change of `path` in the editor's diff: before the session on the left, now on the
   * right; a deleted file against nothing. At `line` (from 0) when it is given. */
  async diff(thread: number, path: string, project: string, line?: number): Promise<void> {
    const file = absolute(path, project);
    const left = this.baseUri(thread, path, file);
    const deleted = this.snapshots.get(thread)?.files.find((f) => f.path === path)?.deleted ?? false;
    const right = deleted ? vscode.Uri.from({ scheme: BASE, path: vscode.Uri.file(file).path, query: "" }) : vscode.Uri.file(file);
    const title = `${basename(path)} (before ognivo ↔ now)`;
    const at = line !== undefined && line >= 0 ? new vscode.Range(line, 0, line, 0) : undefined;
    await vscode.commands.executeCommand("vscode.diff", left, right, title, { preview: true, selection: at });
  }

  /** A file's diff, by its session and its path as the journal names it. */
  async diffOf(thread: number, path: string): Promise<void> {
    const session = await this.session(thread);
    if (!session) throw new Error("the core does not have that session");
    await this.diff(thread, path, session.project);
  }

  /** The diff of a file of the review: a menu's or a lens's, else the active editor's. */
  async diffFile(uri?: unknown, line?: unknown): Promise<void> {
    const found = this.fileOf(uri);
    if (!found) {
      status("$(info) ognivo: this file has no unreviewed change of the agent's");
      return;
    }
    const at = typeof line === "number" ? line : uri === undefined ? vscode.window.activeTextEditor?.selection.active.line : undefined;
    await this.diff(found.snapshot.session.thread, found.file.path, found.snapshot.session.project, at);
  }

  /** Every unreviewed file of a session in one tab, each against what it held before the
   * session; the review tab opened before goes. The session on show when none is given. */
  async reviewAll(thread?: number): Promise<void> {
    const t = typeof thread === "number" ? thread : this.sessions[0]?.thread;
    if (t === undefined) {
      void vscode.window.showInformationMessage("ognivo: no session shows in the panel, so nothing is to review.");
      return;
    }
    const session = await this.session(t);
    if (!session) {
      void vscode.window.showErrorMessage("ognivo: the core does not have that session.");
      return;
    }
    let files: P.ChangedFile[];
    try {
      const core = await this.connection.get();
      ({ files } = await core.call("changes/list", { thread: t }));
    } catch (e) {
      if (!(e instanceof CoreError && e.code === ERRORS.busy)) throw e;
      // an older core while a turn runs: what was listed last
      files = this.snapshots.get(t)?.files ?? [];
    }
    if (this.sessions.some((s) => s.thread === t)) this.take(session, { files, commands: [], inherited: false });
    if (!files.length) {
      void vscode.window.showInformationMessage("ognivo: nothing to review: every change of the session is kept or undone.");
      return;
    }
    const resources = files.map((f): [vscode.Uri, vscode.Uri | undefined, vscode.Uri | undefined] => {
      const abs = absolute(f.path, session.project);
      const file = vscode.Uri.file(abs);
      const base = this.baseUri(t, f.path, abs);
      if (f.created) return [file, undefined, file];
      if (f.deleted) return [file, base, undefined];
      return [file, base, file];
    });
    const old = vscode.window.tabGroups.all.flatMap((g) => g.tabs).filter((tab) => isReviewTitle(tab.label));
    if (old.length) await vscode.window.tabGroups.close(old, true);
    await vscode.commands.executeCommand("vscode.changes", reviewTitle(files.length), resources);
  }

  /** The files of the review in the order the next change goes through them: the sessions
   * on show in their order, each one's files by path. Deleted files have nothing to show. */
  private ordered(): Found[] {
    const out: Found[] = [];
    for (const s of this.sessions) {
      const snapshot = this.snapshots.get(s.thread);
      if (!snapshot) continue;
      const files = snapshot.files.filter((f) => !f.deleted && f.hunks.length).sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
      for (const file of files) out.push({ snapshot, file, uri: this.uriOf(file, s) });
    }
    return out;
  }

  /** Goes to the next change of the agent's (`direction` 1) or the previous one (-1): in
   * this file, then in the next file of the review, round to the first. */
  async go(direction: 1 | -1): Promise<void> {
    const files = this.ordered();
    if (!files.length) {
      status("$(info) ognivo: nothing to review");
      return;
    }
    const editor = vscode.window.activeTextEditor;
    const here = editor && !editor.document.isDirty ? this.find(editor.document.uri) : undefined;
    if (editor && here) {
      const i = nextHunk(here.file.hunks, editor.selection.active.line, editor.document.lineCount, direction);
      if (i >= 0) {
        reveal(editor, hunkLine(here.file.hunks[i]!, editor.document.lineCount));
        return;
      }
    }
    const at = editor ? files.findIndex((f) => key(f.uri) === key(editor.document.uri)) : -1;
    const next = at < 0 ? files[direction === 1 ? 0 : files.length - 1]! : files[(at + direction + files.length) % files.length]!;
    const opened = await vscode.window.showTextDocument(next.uri, { preview: true });
    const lines = next.file.hunks.map((h) => hunkLine(h, opened.document.lineCount));
    reveal(opened, direction === 1 ? Math.min(...lines) : Math.max(...lines));
  }
}

function busy(e: unknown): boolean {
  return e instanceof CoreError && e.code === ERRORS.busy;
}

/** Puts the cursor at the start of `line` and shows it. */
function reveal(editor: vscode.TextEditor, line: number): void {
  const at = new vscode.Position(line, 0);
  editor.selection = new vscode.Selection(at, at);
  editor.revealRange(new vscode.Range(at, at), vscode.TextEditorRevealType.InCenterIfOutsideViewport);
}

/** A file's text on disk; `undefined` when it cannot be read, "" when it is not there. */
async function disk(path: string): Promise<string | undefined> {
  try {
    return new TextDecoder().decode(await vscode.workspace.fs.readFile(vscode.Uri.file(path)));
  } catch (e) {
    return e instanceof vscode.FileSystemError && e.code === "FileNotFound" ? "" : undefined;
  }
}
