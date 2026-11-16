// What terminals show, as text (src/terminals.ts): a command's output read from one of the
// user's terminals (shell integration gives it raw, with the terminal's escape sequences),
// and the agent's commands as the "ognivo" terminal prints them. Nothing here knows VS Code,
// so it is tested on its own (output.test.ts).
//
// What the "ognivo" terminal prints of the model's commands and of their output shows its
// control characters instead of obeying them (`printable`): an escape sequence in a file the
// agent read would set the user's clipboard (OSC 52), make a link (OSC 8), fake the shell's
// marks (OSC 633) or the title. Only ognivo's own colours are escape sequences there.

import type { ToolResult } from "./shared/protocol.ts";
import { printable } from "./shared/text.ts";

/** An escape sequence: OSC (titles, shell integration's marks, links) up to BEL or ST; CSI
 * (colours, cursor moves); a two-character escape. */
const ESCAPES = /\x1b\][\s\S]*?(?:\x07|\x1b\\|$)|\x1b\[[0-?]*[ -/]*[@-~]|\x1b[ -/]*[0-~]?|\x9b[0-?]*[ -/]*[@-~]/g;
/** Control characters but the tab and the line feed. */
const CONTROLS = /[\x00-\x08\x0b-\x1f\x7f]/g;

/** `raw` as the screen would show it, in text: no escape sequences or control characters,
 * CRLF as LF, and what a carriage return went back over replaced by what came after it. */
export function plain(raw: string): string {
  const text = raw.replace(ESCAPES, "").replace(/\r+\n/g, "\n");
  return text
    .split("\n")
    .map((line) => {
      // a progress bar writes over itself: the last version of the line stays
      const back = line.lastIndexOf("\r");
      return (back >= 0 ? line.slice(back + 1) : line).replace(CONTROLS, "");
    })
    .join("\n");
}

/** The end of `text`: its last `lines` lines, of at most `chars` characters, from the start
 * of a line where it can; no blank lines at either end. */
export function tail(text: string, lines = 80, chars = 8000): string {
  let rows = text.replace(/\s+$/, "").split("\n");
  if (rows.length > lines) rows = rows.slice(-lines);
  let out = rows.join("\n");
  if (out.length > chars) {
    out = out.slice(-chars);
    const cut = out.indexOf("\n");
    if (cut >= 0 && cut < out.length - 1) out = out.slice(cut + 1);
  }
  return out.replace(/^\s*\n/, "");
}

/** A shell result's text without ognivo's own lines before the output: `[shell: exit 0,
 * 1.2 s]` and `[cwd: …]`. */
export function shellOutput(text: string): string {
  const lines = text.split("\n");
  let i = 0;
  if (/^\[shell: .*\]$/.test(lines[0] ?? "")) i++;
  if (/^\[cwd: .*\]$/.test(lines[i] ?? "")) i++;
  return lines.slice(i).join("\n");
}

const DIM = "\x1b[2m";
const BOLD = "\x1b[1m";
const RED = "\x1b[31m";
const GREEN = "\x1b[32m";
const YELLOW = "\x1b[33m";
const RESET = "\x1b[0m";

function seconds(ms: number): string {
  return ms < 10_000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms / 1000)} s`;
}

/** How a command of the agent ended, in a few words. */
export function ending(result: Pick<ToolResult, "outcome" | "text" | "exit_code" | "duration_ms">): { ok: boolean; words: string } {
  const time = result.duration_ms >= 100 ? ` · ${seconds(result.duration_ms)}` : "";
  if (result.outcome === "refused") return { ok: false, words: "not run: it was not allowed" };
  if (result.outcome === "skipped") return { ok: false, words: "not run" };
  const killed = /^\[shell: (timed out|killed)/.exec(result.text);
  if (killed) return { ok: false, words: `${killed[1] === "timed out" ? "timed out" : "killed"}${time}` };
  if (result.exit_code !== null && result.exit_code !== 0) return { ok: false, words: `exit ${result.exit_code}${time}` };
  if (result.outcome === "error") return { ok: false, words: `failed${time}` };
  return { ok: true, words: `exit ${result.exit_code ?? 0}${time}` };
}

/** A completed command of the agent as the "ognivo" terminal prints it: `$ command` (a
 * command of many lines goes on with `> `), its output, and how it ended; CRLF line ends,
 * as a terminal wants them, and a blank line after. `who` says whose it was when not the
 * agent's own (a subagent's). The command and its output show their control characters. */
export function commandBlock(command: string, result: ToolResult, who?: string): string {
  const out: string[] = [];
  if (who) out.push(`${DIM}${printable(who.replace(/\s+/g, " "))}${RESET}`);
  command
    .replace(/\r\n?/g, "\n")
    .replace(/\n+$/, "")
    .split("\n")
    .forEach((line, i) => out.push(`${BOLD}${i === 0 ? "$" : ">"} ${printable(line)}${RESET}`));
  // a lone carriage return stays: on the screen, a progress bar writes over itself
  const body = result.outcome === "refused" || result.outcome === "skipped" ? "" : shellOutput(result.text).replace(/\r\n/g, "\n").replace(/\n+$/, "");
  if (body) out.push(...body.split("\n").map((line) => printable(line, true)));
  const end = ending(result);
  const colour = end.ok ? GREEN : result.outcome === "refused" || result.outcome === "skipped" ? YELLOW : RED;
  out.push(`${colour}${end.ok ? "✓" : "✗"} ${end.words}${RESET}`, "");
  return `${out.join("\r\n")}\r\n`;
}

/** A line the "ognivo" terminal prints when it shows another session's commands: its title,
 * on one line, with its control characters shown. */
export function sessionLine(title: string): string {
  return `${DIM}── ${printable(title.replace(/\s+/g, " ").trim() || "New session")} ──${RESET}\r\n\r\n`;
}

/** What clears a terminal's screen and its scrollback, and puts the cursor at the top. */
export const CLEAR = "\x1b[2J\x1b[3J\x1b[H";
