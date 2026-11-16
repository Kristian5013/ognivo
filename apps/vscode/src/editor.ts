// The editor as the agent's context (docs/architecture.md §10).
//
// - The file the user works in, and the selection in it, go to the pages as they change
//   (`editor`, 150 ms after the last change): the composer attaches them to the message.
//   While a webview tab (the agent's own) has the focus, the file's editor before it stays.
// - Ctrl+I on a selection asks for what to do with it, right in the editor: VS Code's
//   comment widget, from a comment controller (`ognivo.prompt`) whose only commenting range
//   is the selection. `workbench.action.addComment` opens that widget focused, as a new
//   thread waits for its first comment. Its buttons (package.json's
//   `comments/commentThread/context`) get the text: Edit sends it with the selection to the
//   page, which starts the turn (or queues it while one runs); Ask in the Panel puts it in
//   the composer. When the widget does not open (another extension's comments on the same
//   lines make VS Code ask which one, commenting is off), an input box asks instead: Enter
//   edits, its button asks in the panel.
// - "Fix with ognivo", a Quick Fix on the diagnostics under the cursor, sends them with
//   their lines to the page.

import { isAbsolute, relative, sep } from "node:path";

import * as vscode from "vscode";

import { selectedLines } from "./hunks.ts";
import { chipLabel } from "./shared/context.ts";
import type { ContextChip, EditorState } from "./shared/messages.ts";
import { folders } from "./view.ts";

/** A selection's text that goes to the page, at most. */
const SELECTION_CHARS = 20_000;
const DEBOUNCE_MS = 150;
/** Schemes of editors that take the focus without being the file the user works in: the
 * Output panel's, the commit message's. The file's editor before them stays. */
const BESIDE = new Set(["output", "vscode-scm", "comment", "debug"]);
/** How long the comment widget may take to open before the input box asks instead. */
const WIDGET_MS = 1500;

/** A file as the pages and the model name it: relative to the workspace's first folder (the
 * project a new session works in), with `/`; absolute outside it. */
export function shownPath(uri: vscode.Uri): string {
  const root = folders()[0];
  if (root) {
    const rel = relative(root, uri.fsPath);
    // `..env` at the root is inside; `../x` and `..` are not
    if (rel && rel !== ".." && !rel.startsWith(`..${sep}`) && !isAbsolute(rel)) return rel.split(sep).join("/");
  }
  return uri.fsPath;
}

/** What a page gets as a message for its composer. */
export interface Prompt {
  text: string;
  chips: ContextChip[];
  send: boolean;
}

/** Follows the file the user works in and the selection in it. */
export class EditorTracker {
  private state: EditorState | null;
  private last?: vscode.TextEditor;
  private timer?: ReturnType<typeof setTimeout>;
  private readonly disposables: vscode.Disposable[];

  constructor(private readonly onChange: (state: EditorState | null) => void) {
    this.state = this.compute();
    const later = () => this.schedule();
    this.disposables = [
      vscode.window.onDidChangeActiveTextEditor(later),
      vscode.window.onDidChangeTextEditorSelection((e) => (e.textEditor === vscode.window.activeTextEditor || e.textEditor === this.last) && later()),
      vscode.window.onDidChangeVisibleTextEditors(later),
      vscode.window.tabGroups.onDidChangeTabs(later),
      vscode.workspace.onDidChangeWorkspaceFolders(later),
      vscode.workspace.onDidCloseTextDocument(later),
    ];
  }

  current(): EditorState | null {
    return this.state;
  }

  private schedule(): void {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      const now = this.compute();
      if (JSON.stringify(now) === JSON.stringify(this.state)) return;
      this.state = now;
      this.onChange(now);
    }, DEBOUNCE_MS);
  }

  /** Whether a tab of some group still shows `editor`'s file, in sight or not. */
  private stillOpen(editor: vscode.TextEditor): boolean {
    if (editor.document.isClosed) return false;
    const k = editor.document.uri.toString();
    return vscode.window.tabGroups.all.some((g) =>
      g.tabs.some(
        (t) => (t.input instanceof vscode.TabInputText && t.input.uri.toString() === k) || (t.input instanceof vscode.TabInputTextDiff && t.input.modified.toString() === k),
      ),
    );
  }

  private compute(): EditorState | null {
    const active = vscode.window.activeTextEditor;
    let editor: vscode.TextEditor | undefined;
    if (active?.document.uri.scheme === "file") editor = active;
    else if ((!active || BESIDE.has(active.document.uri.scheme)) && this.last && this.stillOpen(this.last)) editor = this.last;
    this.last = editor;
    if (!editor) return null;
    const doc = editor.document;
    const sel = editor.selection;
    return {
      path: shownPath(doc.uri),
      abs: doc.uri.fsPath,
      language: doc.languageId,
      selection: sel.isEmpty ? null : { ...selectedLines(sel.start, sel.end), text: doc.getText(sel).slice(0, SELECTION_CHARS) },
    };
  }

  dispose(): void {
    clearTimeout(this.timer);
    for (const d of this.disposables) d.dispose();
  }
}

/** What Ctrl+I works on: a file's lines, and the chip they go to the page as. */
interface Target {
  uri: vscode.Uri;
  range: vscode.Range;
  chip: Extract<ContextChip, { kind: "selection" }>;
}

/** The selection's lines as a chip, and the range of whole lines they are; the cursor's
 * line when nothing is selected. */
function selectionTarget(editor: vscode.TextEditor): Target {
  const doc = editor.document;
  const sel = editor.selection;
  const lines = sel.isEmpty ? { start: sel.active.line + 1, end: sel.active.line + 1 } : selectedLines(sel.start, sel.end);
  const range = new vscode.Range(lines.start - 1, 0, lines.end - 1, doc.lineAt(lines.end - 1).text.length);
  return {
    uri: doc.uri,
    range,
    chip: {
      kind: "selection",
      path: shownPath(doc.uri),
      abs: doc.uri.fsPath,
      start: lines.start,
      end: lines.end,
      language: doc.languageId,
      text: doc.getText(sel.isEmpty ? range : sel).slice(0, SELECTION_CHARS),
    },
  };
}

/** Ctrl+I: what to do with the selection, asked right where it is. */
export class InlinePrompt {
  private controller?: vscode.CommentController;
  private target?: Target;
  /** Hears VS Code ask for the commenting ranges of a document. */
  private provided?: (uri: vscode.Uri) => void;
  /** The newest Ctrl+I: an older one whose widget gave up does not ask in an input box,
   * which would close the newer one's widget. */
  private attempt = 0;

  constructor(
    /** Hands a message to the page; `focus` gives the page the focus. */
    private readonly send: (prompt: Prompt, focus: boolean) => Promise<void>,
  ) {}

  async start(): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.scheme !== "file") {
      void vscode.window.showInformationMessage("ognivo: select lines of a file first, then Ctrl+I asks what to do with them.");
      return;
    }
    const target = selectionTarget(editor);
    const attempt = ++this.attempt;
    if (!(await this.widget(target)) && attempt === this.attempt) await this.inputBox(target);
  }

  /** Opens VS Code's comment widget on the target's lines; whether it opened. */
  private async widget(target: Target): Promise<boolean> {
    this.finish();
    this.target = target;
    // VS Code asks for the ranges of the new controller's documents at once: once it has
    // the target's, the cursor is in a commenting range
    const asked = new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => resolve(false), WIDGET_MS);
      this.provided = (uri) => {
        if (uri.toString() !== target.uri.toString()) return;
        clearTimeout(timer);
        resolve(true);
      };
    });
    const controller = vscode.comments.createCommentController("ognivo.prompt", "ognivo");
    controller.options = { prompt: "Ask ognivo…", placeHolder: "What should ognivo do with these lines? Ctrl+Enter: Edit" };
    controller.commentingRangeProvider = {
      provideCommentingRanges: (doc) => {
        const t = this.target;
        if (!t || doc.uri.toString() !== t.uri.toString()) return [];
        this.provided?.(doc.uri);
        return [t.range];
      },
    };
    this.controller = controller;
    if (!(await asked) || this.target !== target) return false;
    // the editor takes the ranges a moment after it asked for them
    await new Promise((resolve) => setTimeout(resolve, 100));
    // the widget's text box is a document of the `comment` scheme, named after the controller
    const opened = new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => {
        watch.dispose();
        resolve(false);
      }, WIDGET_MS);
      const watch = vscode.workspace.onDidOpenTextDocument((d) => {
        if (d.uri.scheme !== "comment" || (d.uri.authority !== "ognivo.prompt" && !d.uri.path.includes("ognivo.ognivo"))) return;
        clearTimeout(timer);
        watch.dispose();
        resolve(true);
      });
    });
    await vscode.commands.executeCommand("workbench.action.addComment");
    return opened;
  }

  /** The comment widget's buttons: Edit (`send`) and Ask in the Panel. */
  async reply(reply: vscode.CommentReply, send: boolean): Promise<void> {
    const t = this.target;
    const chip = t && reply.thread.uri.toString() === t.uri.toString() ? t.chip : undefined;
    const text = reply.text.trim();
    this.finish();
    if (!chip) return;
    // nothing written: the composer gets the lines, and the user writes there
    await this.send({ text, chips: [chip], send: send && text !== "" }, !send || text === "");
  }

  /** When the widget does not open: an input box with the same two ways out. */
  private inputBox(target: Target): Promise<void> {
    this.finish();
    const box = vscode.window.createInputBox();
    box.title = `ognivo · ${chipLabel(target.chip)}`;
    box.placeholder = "What should ognivo do with these lines?";
    box.prompt = "Enter: Edit · the button: Ask in the Panel";
    box.buttons = [{ iconPath: new vscode.ThemeIcon("comment-discussion"), tooltip: "Ask in the Panel" }];
    return new Promise((resolve) => {
      box.onDidAccept(() => {
        const text = box.value.trim();
        if (!text) return;
        box.hide();
        void this.send({ text, chips: [target.chip], send: true }, false);
      });
      box.onDidTriggerButton(() => {
        const text = box.value.trim();
        box.hide();
        void this.send({ text, chips: [target.chip], send: false }, true);
      });
      box.onDidHide(() => {
        box.dispose();
        resolve();
      });
      box.show();
    });
  }

  /** The widget goes, with its thread and its commenting range. */
  private finish(): void {
    this.provided = undefined;
    this.target = undefined;
    this.controller?.dispose();
    this.controller = undefined;
  }

  dispose(): void {
    this.finish();
  }
}

function severity(s: vscode.DiagnosticSeverity): Extract<ContextChip, { kind: "problem" }>["severity"] {
  switch (s) {
    case vscode.DiagnosticSeverity.Error:
      return "error";
    case vscode.DiagnosticSeverity.Warning:
      return "warning";
    case vscode.DiagnosticSeverity.Information:
      return "info";
    default:
      return "hint";
  }
}

/** Lines around a problem the model gets with it, before and after. */
const AROUND = 3;

/** "Fix with ognivo" on the diagnostics under the cursor. */
export class FixWithOgnivo implements vscode.CodeActionProvider {
  static readonly kinds = [vscode.CodeActionKind.QuickFix];

  provideCodeActions(doc: vscode.TextDocument, _range: vscode.Range, context: vscode.CodeActionContext): vscode.CodeAction[] {
    if (doc.uri.scheme !== "file" || context.diagnostics.length === 0) return [];
    if (context.only && !context.only.contains(vscode.CodeActionKind.QuickFix) && !vscode.CodeActionKind.QuickFix.contains(context.only)) return [];
    const diagnostics = [...context.diagnostics];
    const action = new vscode.CodeAction("Fix with ognivo", vscode.CodeActionKind.QuickFix);
    action.diagnostics = diagnostics;
    action.command = { command: "ognivo.fixProblem", title: "Fix with ognivo", arguments: [doc.uri, diagnostics] };
    return [action];
  }
}

/** The message that asks to fix `diagnostics` of `uri`: each problem, and their lines with
 * three more on each side. Without arguments, the problems at the active editor's cursor. */
export async function fixPrompt(uri?: unknown, diagnostics?: unknown): Promise<Prompt | undefined> {
  let file = uri instanceof vscode.Uri ? uri : vscode.window.activeTextEditor?.document.uri;
  if (!file) return undefined;
  let problems = Array.isArray(diagnostics) ? diagnostics.filter((d): d is vscode.Diagnostic => d instanceof vscode.Diagnostic) : [];
  if (!problems.length) {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.uri.toString() !== file.toString()) return undefined;
    const at = editor.selection.active;
    problems = vscode.languages.getDiagnostics(file).filter((d) => d.range.contains(at) || d.range.start.line === at.line);
    file = editor.document.uri;
  }
  if (!problems.length) return undefined;
  const doc = await vscode.workspace.openTextDocument(file);
  const path = shownPath(file);
  const abs = file.fsPath;
  const chips: ContextChip[] = problems.map((d) => ({
    kind: "problem",
    path,
    abs,
    line: d.range.start.line + 1,
    message: d.message,
    source: d.source ?? null,
    severity: severity(d.severity),
  }));
  const first = Math.max(0, Math.min(...problems.map((d) => d.range.start.line)) - AROUND);
  const last = Math.min(doc.lineCount - 1, Math.max(...problems.map((d) => d.range.end.line)) + AROUND);
  const range = new vscode.Range(first, 0, last, doc.lineAt(last).text.length);
  chips.push({ kind: "selection", path, abs, start: first + 1, end: last + 1, language: doc.languageId, text: doc.getText(range).slice(0, SELECTION_CHARS) });
  return { text: problems.length === 1 ? "Fix this problem." : "Fix these problems.", chips, send: true };
}
