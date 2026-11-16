// What the extension (src/view.ts) and its pages (src/webview/host.ts) say to each other.

/** Where a page is: VS Code's side bar, or a tab among the editors. */
export type Place = "sidebar" | "editor";

/** What a new session starts with: the extension's settings (package.json). */
export interface NewSession {
  server: string;
  model: string;
  profile: string;
  reasoning: string;
  context: number;
  mode: string;
  native: boolean;
  subagents: boolean;
}

/** The settings of a new session a page may change, by their names under `ognivo.`. */
export const SESSION_SETTINGS = ["server", "model", "profile", "reasoning", "context", "mode", "native", "subagents"] as const;

/** The extension's commands a page runs. */
export type Command = "openSession" | "chooseModel" | "vscodeSettings" | "newSession" | "showLog";

/** A file of instructions the agent gets (crates/ognivo-core/src/instructions.rs). */
export interface InstructionFile {
  /** `user`: ognivo's own, for every project; `claude`: Claude Code's user file, which
   * ognivo reads too; `project`: the project's root. */
  scope: "user" | "claude" | "project";
  path: string;
  /** How the page names it: `~/.ognivo/AGENTS.md`, `AGENTS.md`. */
  shown: string;
  exists: boolean;
  /** Its text, for the file the page edits itself (the user's own). */
  text?: string;
  bytes: number;
}

/** The file the user works in, and the selection in it. Lines from 1, inclusive. */
export interface EditorState {
  /** Relative to the workspace, `/` separated. */
  path: string;
  /** Its path on disk. */
  abs: string;
  /** VS Code's language id: `rust`, `typescript`, `plaintext`. */
  language: string;
  /** `null` when nothing is selected. */
  selection: EditorSelection | null;
}

export interface EditorSelection {
  start: number;
  end: number;
  /** What is selected, at most 20 000 characters. */
  text: string;
}

/** A command that failed in one of the user's own terminals (shell integration). */
export interface TerminalFailure {
  /** Tells one failure from another, as the extension saw them end. */
  id: string;
  command: string;
  exitCode: number;
  /** Where it ran, where the shell says. */
  cwd: string | null;
  /** The last of what it wrote: at most 80 lines and 8 000 characters, ANSI stripped. */
  output: string;
  /** When it ended, in Unix milliseconds. */
  at: number;
}

/** What a message carries besides its text; turned into text when sent (context.ts). */
export type ContextChip =
  /** A file the user works in. */
  | { kind: "file"; path: string; abs: string }
  /** Lines `start` to `end` of a file, with their text. */
  | { kind: "selection"; path: string; abs: string; start: number; end: number; language: string; text: string }
  /** A command that failed in the user's terminal, with what it wrote. */
  | { kind: "terminal"; command: string; exitCode: number; cwd: string | null; output: string }
  /** A diagnostic of a file, at `line`. */
  | { kind: "problem"; path: string; abs: string; line: number; message: string; source: string | null; severity: "error" | "warning" | "info" | "hint" };

/** What a page asks of the extension, and what comes back. */
export interface Requests {
  "settings/set": { params: { key: (typeof SESSION_SETTINGS)[number]; value: unknown }; result: null };
  "instructions/list": { params: { project: string }; result: InstructionFile[] };
  "instructions/save": { params: { path: string; text: string }; result: null };
  /** Opens the file in the editor, made empty first when it is not there. */
  "instructions/open": { params: { path: string }; result: null };
  /** Accepts a file's unreviewed change, or every file's (`null`) once the user agrees;
   * whether it did. */
  "changes/accept": { params: { thread: number; path: string | null }; result: boolean };
  /** Puts back a file, or every file (`null`), once the user agrees; whether it did. */
  "changes/revert": { params: { thread: number; path: string | null }; result: boolean };
  /** Deletes a session once the user agrees in VS Code's dialog; whether it did. */
  "sessions/delete": { params: { thread: number; title: string }; result: boolean };
  /** Whether Exa takes the key: one search of a result, as its MCP server would make. */
  "exa/check": { params: { key: string }; result: { ok: boolean; says: string } };
  /** The workspace's files that match `query`, fuzzily, the best first: at most `limit`
   * (20 when not given), relative to the workspace with `/`. */
  "files/find": { params: { query: string; limit?: number }; result: string[] };
  /** Opens the review of a thread's unreviewed changes: every file's in one tab (`path`
   * `null`), or a file's diff. */
  "review/open": { params: { thread: number; path: string | null }; result: null };
  /** Shows the "ognivo" terminal, which shows the agent's commands as they ran. */
  "terminal/show": { params: Record<string, never>; result: null };
}

/** What a page sends the extension. */
export type FromPage =
  | { t: "ready" }
  | { t: "connect" }
  | { t: "core"; data: string }
  | { t: "store"; key: string; value: string | null }
  | { t: "open"; path: string; project: string; line?: number }
  | { t: "diff"; thread: number; path: string; project: string }
  | { t: "link"; href: string }
  /** The thread the page shows now, `null` for none (a new session). */
  | { t: "shown"; thread: number | null; title: string; project: string }
  | { t: "command"; command: Command }
  | { t: "request"; id: number; method: keyof Requests; params: unknown };

/** What the extension sends a page. */
export type ToPage =
  /** The first message: with the file the user works in and the commands that failed in
   * the user's terminals. */
  | {
      t: "hello";
      folders: string[];
      state: Record<string, string>;
      place: Place;
      session: NewSession;
      editor: EditorState | null;
      failures: TerminalFailure[];
    }
  /** The user went to another file, or selected something else: debounced, and while a
   * page has the focus the last file's editor stays. */
  | { t: "editor"; editor: EditorState | null }
  /** The commands that failed in the user's terminals, the newest first, at most 5. */
  | { t: "failures"; failures: TerminalFailure[] }
  /** A message for the composer from elsewhere in VS Code (Ctrl+I, a Quick Fix, a failed
   * command's suggestion): shown there, and sent when `send`, queued while a turn runs. */
  | { t: "prompt"; text: string; chips: ContextChip[]; send: boolean }
  | { t: "folders"; folders: string[] }
  | { t: "session"; session: NewSession }
  | { t: "core"; data: unknown }
  | { t: "insert"; text: string }
  | { t: "new" }
  | { t: "open"; thread: number }
  /** Shows the settings, at a tab of them. */
  | { t: "settings"; tab?: SettingsTab }
  /** A session was deleted: a page that shows it goes to a new one. */
  | { t: "deleted"; thread: number }
  | { t: "response"; id: number; result?: unknown; error?: string };

export type SettingsTab = "model" | "mcp" | "instructions";
