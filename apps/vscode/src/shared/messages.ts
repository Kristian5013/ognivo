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

/** The extension's commands a page runs. */
export type Command = "openSession" | "chooseModel" | "settings" | "newSession";

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
  | { t: "command"; command: Command };

/** What the extension sends a page. */
export type ToPage =
  | { t: "hello"; folders: string[]; state: Record<string, string>; place: Place; session: NewSession }
  | { t: "folders"; folders: string[] }
  | { t: "session"; session: NewSession }
  | { t: "core"; data: unknown }
  | { t: "insert"; text: string }
  | { t: "new" }
  | { t: "open"; thread: number };
