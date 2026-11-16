// What the extension says outside the panel (src/status.ts, src/asks.ts, src/extension.ts):
// the status bar item of the session on show, and the notices of a turn that ended and of a
// question out of sight. Nothing here knows VS Code, so it is tested on its own
// (wording.test.ts).
//
// Much of what it says came from the model, the project or the core: a command, a file's
// path, a session's title, an error. VS Code reads some of that as its own syntax, so none of
// it goes in as it is: a notice turns `[label](command:…)` into a link that runs the command
// on a click (`unlinked`), the status bar's tooltip is markdown (`code`, `literal`), and its
// text draws `$(name)` as an icon (`iconless`).

import type { ApprovalAsked, Call, Progress, TurnEnd } from "./shared/protocol.ts";
import { code, literal, printable } from "./shared/text.ts";

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** Bidirectional controls, which make the text after them show in another order. */
const BIDI = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

/** `text` as an extension's notice may say it. VS Code reads `[label](command:…)`, and the
 * same with `https:` or `file:`, in such a notice as a link, and a click on it runs the
 * command with no question: a space between `]` and `(` keeps it text. Control characters
 * show as their pictures and bidirectional controls as U+FFFD, so that the notice shows what
 * it holds. Every notice with words of the model, the project, a file's name or the core
 * goes through this. */
export function unlinked(text: string): string {
  return printable(text).replace(BIDI, "\ufffd").replace(/\]\(/g, "] (");
}

/** `text` as the status bar's text says it: `$(name)` is no icon there but what it says. */
export function iconless(text: string): string {
  return text.replace(/(\\?)\$\(([A-Za-z0-9-]+(?:~[A-Za-z]+)?)\)/g, "$1\\$($2)");
}

/** A count of tokens in few characters: `418`, `12.4K`, `132K`. */
export function tokens(n: number): string {
  if (n < 1000) return String(n);
  return `${(n / 1000).toFixed(n < 100_000 ? 1 : 0)}K`;
}

function seconds(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${s % 60} s`;
}

/** `text` cut to `room` characters, with an ellipsis where it was cut. */
function cut(text: string, room: number): string {
  const one = text.replace(/\s+/g, " ").trim();
  return one.length > room ? `${one.slice(0, room - 1)}…` : one;
}

function base(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

/** What a generation does now, as the status bar says it: `Reading the conversation ·
 * 63%`, `Thinking · 34 tok/s`, `Writing · 34 tok/s`; `long` adds the counts. */
export function progressText(p: Progress, long = false): string {
  const speed = p.tokens_per_second !== null && p.tokens_per_second > 0 ? `${Math.round(p.tokens_per_second)} tok/s` : undefined;
  const made = p.generated > 0 ? `${tokens(p.generated)} ${p.generated === 1 ? "token" : "tokens"}` : undefined;
  switch (p.phase) {
    case "waiting":
      return "Waiting for the model";
    case "prompt": {
      if (p.prompt_total === null || p.prompt_done === null) return "Reading the conversation";
      const cached = Math.min(p.prompt_cached ?? 0, p.prompt_total);
      const toRead = p.prompt_total - cached;
      if (toRead <= 0) return "Reading the conversation";
      const share = Math.min(100, Math.max(0, Math.round(((p.prompt_done - cached) / toRead) * 100)));
      return `Reading the conversation · ${share}%${long ? ` of ${tokens(toRead)}` : ""}`;
    }
    case "reasoning":
    case "writing": {
      const parts = long ? [made, speed] : [speed ?? made];
      return [p.phase === "reasoning" ? "Thinking" : "Writing", ...parts.filter((x) => x !== undefined)].join(" · ");
    }
  }
}

/** A call that runs, as the status bar says it: `Running cargo test`, `Editing parse.rs`. */
export function callText(tool: string, call: Call | null): string {
  switch (call?.name) {
    case "Bash":
      return `Running ${cut(call.command.split("\n")[0] ?? "", 40)}`;
    case "Edit":
    case "Write":
      return `Editing ${base(call.path)}`;
    case "ApplyPatch":
      return "Editing files";
    case "Read":
      return `Reading ${base(call.path)}`;
    case "Grep":
      return "Searching";
    case "Glob":
      return "Listing files";
    case "Task":
      return call.tasks.length === 1 ? "A subagent works" : "Subagents work";
    case "Mcp":
      return `Using ${call.server}`;
    case "Skill":
      return `Using the skill ${call.skill}`;
    case "ToolSearch":
      return "Looking for a tool";
    default:
      return `Running ${cut(tool, 30)}`;
  }
}

/** What the agent of the session on show is doing. */
export interface Activity {
  running: boolean;
  /** The newest progress of its current generation. */
  progress: Progress | null;
  /** The call that runs now, as `callText` says it. */
  call: string | null;
}

/** A file to review, as the status bar's tooltip lists it. */
export interface ReviewFile {
  path: string;
  added: number;
  removed: number;
}

export interface StatusInput {
  /** The approvals that wait, every session's. */
  asks: readonly ApprovalAsked[];
  activity: Activity | undefined;
  /** The unreviewed files of the session on show. */
  review: readonly ReviewFile[];
  /** The session on show, `null` for none. */
  title: string | null;
}

export interface Status {
  /** With VS Code's icons. */
  text: string;
  /** The text without its icons, for a screen reader. */
  label: string;
  /** Markdown, without icons. */
  tooltip: string;
  /** A question waits: the item stands out. */
  warning: boolean;
  /** What a click does: the page of the question, the review of all files, the panel. */
  click: "asks" | "review" | "focus";
}

/** How a question is put, by its tool: `Run this command?`. */
export function approvalTitle(tool: string, reason: string): string {
  const t = tool.toLowerCase();
  const outside = /outside/i.test(reason);
  if (/bash|shell|exec|command|terminal/.test(t)) return "Run this command?";
  if (/edit|write|patch/.test(t)) return outside ? "Edit a file outside the project?" : "Make this edit?";
  if (/read|grep|glob|ls|list|search_files/.test(t)) return outside ? "Read outside the project?" : "Read this?";
  if (/webfetch|fetch/.test(t)) return "Fetch this page?";
  if (/websearch/.test(t)) return "Search the web?";
  if (t.startsWith("mcp")) return "Use this tool?";
  return `Allow ${cut(tool, 30)}?`;
}

/** What VS Code shows of an extension's notice at most: it cuts the rest to "...". */
const NOTICE_CHARS = 1000;
/** The end of a question's notice that cannot be allowed from the notice. */
const IN_THE_PANEL = " · open it to allow it";

/** A question as a notice says it, in one line, and whether the notice shows its whole
 * action as it is: only then may the user allow it from the notice. An action of more than
 * one line (each break shows as ` ⏎ `), one too long for a notice, and one with characters
 * a notice does not show as they are, it shows as far as it can: the panel's card (Open)
 * allows it, and the notice can only deny it. */
export function approvalNotice(a: Pick<ApprovalAsked, "tool" | "action" | "reason" | "agent">): { text: string; whole: boolean } {
  const who = a.agent ? ` (a subagent: ${cut(a.agent, 40)})` : "";
  const why = a.reason ? ` — ${cut(a.reason, 120)}` : "";
  const head = `ognivo asks${who}: ${approvalTitle(a.tool, a.reason)} `;
  const raw = a.action.trim();
  const lines = raw.split(/\r\n|\r|\n/).map((l) => l.replace(/\s+/g, " ").trim());
  const action = lines.join(" ⏎ ");
  const text = unlinked(`${head}${action}${why}`);
  // one line, with no space but spaces and tabs, and nothing the notice shows otherwise
  const asItIs = lines.length === 1 && !/[^\S \t]/.test(raw) && unlinked(raw) === raw;
  if (asItIs && text.length <= NOTICE_CHARS) return { text, whole: true };
  const room = NOTICE_CHARS - unlinked(`${head}${why}${IN_THE_PANEL}`).length;
  return { text: unlinked(`${head}${cut(unlinked(action), room)}${why}${IN_THE_PANEL}`), whole: false };
}

function filesToReview(n: number): string {
  return `${plural(n, "file")} to review`;
}

/** A path as the tooltip lists it: its end, where it is long. */
function shortPath(path: string, room = 120): string {
  return path.length > room ? `…${path.slice(-(room - 1))}` : path;
}

/** The status bar item: a question first, then what the agent does, then what waits for a
 * review, else the way to the panel. In the tooltip, a path and an action are code, and the
 * rest that is not the extension's own is escaped: none of it is read as markdown. */
export function statusLine(s: StatusInput): Status {
  const item = (icon: string, words: string) => ({ text: `$(${icon}) ${iconless(words)}`, label: words });
  const review = s.review.length;
  const files = s.review
    .slice(0, 8)
    .map((f) => `- ${code(shortPath(f.path))} +${f.added} −${f.removed}`)
    .concat(review > 8 ? [`- … and ${review - 8} more`] : []);
  // a title of spaces only is none: `****` would draw a rule
  const session = s.title !== null ? `**${literal(cut(s.title, 100) || "New session")}**\n\n` : "";
  if (s.asks.length) {
    const lines = s.asks.slice(0, 5).map((a) => `- ${literal(approvalTitle(a.tool, a.reason))} ${code(cut(a.action, 80))}`);
    if (s.asks.length > 5) lines.push(`- … and ${s.asks.length - 5} more`);
    const words = s.asks.length === 1 ? "ognivo asks" : `ognivo asks ${s.asks.length}`;
    return {
      ...item("bell-dot", words),
      tooltip: `ognivo waits for your answer:\n\n${lines.join("\n")}\n\nClick to answer in the panel.`,
      warning: true,
      click: "asks",
    };
  }
  const a = s.activity;
  if (a?.running) {
    const doing = a.call ?? (a.progress ? progressText(a.progress) : "Working");
    const detail = a.call ?? (a.progress ? progressText(a.progress, true) : "Working");
    const elapsed = a.progress && !a.call && a.progress.elapsed_ms >= 1000 ? ` · ${seconds(a.progress.elapsed_ms)}` : "";
    const more = review ? ` · ${filesToReview(review)}` : "";
    return {
      ...item("sparkle", `${doing}${more}`),
      tooltip: `${session}${literal(cut(detail, 120))}${elapsed}${review ? `\n\n${filesToReview(review)}:\n\n${files.join("\n")}` : ""}\n\nClick to show the panel.`,
      warning: false,
      click: "focus",
    };
  }
  if (review) {
    return {
      ...item("sparkle", filesToReview(review)),
      tooltip: `${session}${filesToReview(review)}:\n\n${files.join("\n")}\n\nClick to review them all.`,
      warning: false,
      click: "review",
    };
  }
  return {
    ...item("sparkle", "ognivo"),
    tooltip: `${session}Nothing to review.\n\nClick to show the agent (Ctrl+Alt+O).`,
    warning: false,
    click: "focus",
  };
}

/** The notice of a turn that ended while VS Code was not in front: `ognivo finished "Fix
 * the panic" · 3 files to review`. */
export function finishedNotice(title: string, end: TurnEnd, files: number): string {
  const t = `"${cut(title || "New session", 60)}"`;
  const what = {
    answer: `finished ${t}`,
    interrupted: `stopped ${t}`,
    step_limit: `stopped ${t} at the step limit`,
    time_limit: `stopped ${t} at the time limit`,
    stuck: `stopped ${t}: the model was stuck`,
    error: `failed in ${t}`,
  }[end];
  return unlinked(`ognivo ${what}${files ? ` · ${filesToReview(files)}` : ""}`);
}
