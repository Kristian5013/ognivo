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

/** What a page asks of the extension, and what comes back. */
export interface Requests {
  "settings/set": { params: { key: (typeof SESSION_SETTINGS)[number]; value: unknown }; result: null };
  "instructions/list": { params: { project: string }; result: InstructionFile[] };
  "instructions/save": { params: { path: string; text: string }; result: null };
  /** Opens the file in the editor, made empty first when it is not there. */
  "instructions/open": { params: { path: string }; result: null };
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
  | { t: "hello"; folders: string[]; state: Record<string, string>; place: Place; session: NewSession }
  | { t: "folders"; folders: string[] }
  | { t: "session"; session: NewSession }
  | { t: "core"; data: unknown }
  | { t: "insert"; text: string }
  | { t: "new" }
  | { t: "open"; thread: number }
  /** Shows the settings, at a tab of them. */
  | { t: "settings"; tab?: SettingsTab }
  | { t: "response"; id: number; result?: unknown; error?: string };

export type SettingsTab = "model" | "mcp" | "instructions";
