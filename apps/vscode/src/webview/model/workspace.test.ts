import { describe, expect, it } from "vitest";

import type { EditorState, TerminalFailure } from "../../shared/messages.ts";
import workspace, { type Prompt, Workspace } from "./workspace.ts";

const editor: EditorState = {
  path: "src/parse.rs",
  abs: "/home/u/tally/src/parse.rs",
  language: "rust",
  selection: { start: 4, end: 8, text: "fn amount() {}" },
};
const failure: TerminalFailure = {
  id: "t1",
  command: "cargo run -- expenses.csv",
  exitCode: 101,
  cwd: "/home/u/tally",
  output: "thread 'main' panicked",
  at: 1_763_300_000_000,
};

describe("Workspace", () => {
  it("holds the editor and the failures, and says when they change", () => {
    const w = new Workspace();
    expect([w.editor, w.failures]).toEqual([null, []]);
    let heard = 0;
    const stop = w.subscribe(() => heard++);
    w.setEditor(editor);
    w.setFailures([failure]);
    expect([w.editor, w.failures, heard]).toEqual([editor, [failure], 2]);
    // a snapshot stays itself until the next change
    const failures = w.failures;
    expect(w.failures).toBe(failures);
    w.setEditor(null);
    expect([w.editor, heard]).toEqual([null, 3]);
    stop();
    w.setFailures([]);
    expect(heard).toBe(3);
  });

  it("hands prompts to those who listen, and keeps them until someone does", () => {
    const w = new Workspace();
    const early: Prompt = { text: "Fix this problem.", chips: [], send: true };
    w.emitPrompt(early);
    const got: Prompt[] = [];
    const stop = w.onPrompt((p) => got.push(p));
    expect(got).toEqual([early]);
    const later: Prompt = { text: "Explain.", chips: [{ kind: "file", path: editor.path, abs: editor.abs }], send: false };
    w.emitPrompt(later);
    expect(got).toEqual([early, later]);
    // a listener that comes later gets nothing that was taken already
    const second: Prompt[] = [];
    w.onPrompt((p) => second.push(p));
    expect(second).toEqual([]);
    stop();
    w.emitPrompt(early);
    expect([got.length, second.length]).toEqual([2, 1]);
  });

  it("is one store for the page, whose methods work unbound", () => {
    const { subscribe, onPrompt, emitPrompt } = workspace;
    let heard = 0;
    const stop = subscribe(() => heard++);
    workspace.setEditor(editor);
    stop();
    expect(heard).toBe(1);
    const got: string[] = [];
    const off = onPrompt((p) => got.push(p.text));
    emitPrompt({ text: "Go on.", chips: [], send: true });
    off();
    expect(got).toEqual(["Go on."]);
    workspace.setEditor(null);
  });
});
