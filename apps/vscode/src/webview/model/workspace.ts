// What the page knows of the user's VS Code besides the core (src/shared/messages.ts): the
// file the user works in with its selection, and the commands that failed in the user's
// own terminals, as the extension last said them (host.ts feeds them); and the messages
// that other places of VS Code hand the composer (Ctrl+I, a Quick Fix, a failed command's
// suggestion), which the welcome page hands it too. One store for the page
// (`workspace`); its parts are new objects when they change, so that a component may take
// them as React's snapshots.

import type { ContextChip, EditorState, TerminalFailure } from "../../shared/messages.ts";

/** A message for the composer: shown there, and sent when `send`, queued while a turn
 * runs. */
export interface Prompt {
  text: string;
  chips: ContextChip[];
  send: boolean;
}

export class Workspace {
  private editorNow: EditorState | null = null;
  private failuresNow: TerminalFailure[] = [];
  private readonly listeners = new Set<() => void>();
  private readonly prompts = new Set<(prompt: Prompt) => void>();
  /** Prompts that came while nothing took them: the first that does gets them. */
  private readonly waiting: Prompt[] = [];

  /** The file the user works in, `null` for none. */
  get editor(): EditorState | null {
    return this.editorNow;
  }

  /** The commands that failed in the user's terminals, the newest first. */
  get failures(): TerminalFailure[] {
    return this.failuresNow;
  }

  setEditor(editor: EditorState | null): void {
    this.editorNow = editor;
    this.changed();
  }

  setFailures(failures: TerminalFailure[]): void {
    this.failuresNow = failures;
    this.changed();
  }

  /** `listener` hears each change of `editor` and `failures`. */
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** `listener` takes the composer's prompts, those that waited for it first. */
  onPrompt = (listener: (prompt: Prompt) => void): (() => void) => {
    this.prompts.add(listener);
    for (const prompt of this.waiting.splice(0)) listener(prompt);
    return () => this.prompts.delete(listener);
  };

  /** A message for the composer, from the extension or from the page itself. */
  emitPrompt = (prompt: Prompt): void => {
    if (this.prompts.size === 0) this.waiting.push(prompt);
    for (const l of this.prompts) l(prompt);
  };

  private changed(): void {
    for (const l of this.listeners) l();
  }
}

/** The page's own. */
const workspace = new Workspace();
export default workspace;
