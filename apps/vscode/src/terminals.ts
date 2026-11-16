// Terminals (docs/architecture.md §10). The agent's commands: a terminal of VS Code's,
// "ognivo", that prints each command the agent of the session on show ran, with its output
// and how it ended, their control characters shown instead of obeyed (output.ts). It only
// shows (nothing typed in it goes anywhere); it is made the first time it is asked for
// (`ognivo: Show the Agent's Commands`, the page's `terminal/show`) and shows what ran since
// the extension started.
//
// The user's own terminals: with shell integration, VS Code says when a command starts and
// ends, and gives what it writes. A command that ends with an exit code other than 0 is a
// failure the page offers as context ("cargo run · exit 101"): its command line, where it
// ran, and the end of its output. The newest five are kept. This reads the output of the
// user's commands, so `ognivo.terminalSuggestions` turns it off.

import * as vscode from "vscode";

import { CLEAR, commandBlock, plain, sessionLine, tail } from "./output.ts";
import type { TerminalFailure } from "./shared/messages.ts";
import type * as P from "./shared/protocol.ts";

/** Commands kept of each session, and their characters at most. */
const KEEP_COMMANDS = 100;
const KEEP_CHARS = 400_000;

/** The read-only terminal of the agent's commands. */
export class AgentCommands implements vscode.Pseudoterminal {
  private terminal?: vscode.Terminal;
  private opened = false;
  private readonly writer = new vscode.EventEmitter<string>();
  readonly onDidWrite = this.writer.event;
  /** What each session ran, as the terminal prints it. */
  private readonly ran = new Map<number, string[]>();
  private session?: { thread: number; title: string };

  constructor(
    /** The session whose subagent a thread is, else the thread itself. */
    private readonly root: (thread: number) => number,
  ) {}

  /** Whether `t` is this terminal, which the user terminals' watch leaves alone. */
  owns(t: vscode.Terminal): boolean {
    return t === this.terminal;
  }

  /** The session on show: the terminal shows its commands. */
  follow(session: { thread: number; title: string } | undefined): void {
    if (session?.thread === this.session?.thread) {
      this.session = session;
      return;
    }
    this.session = session;
    if (this.opened) this.replay();
  }

  /** An event of the core's: a command of the agent's that completed. */
  event(e: P.Event): void {
    if (e.event !== "item_completed" || e.item.kind !== "tool_call" || e.item.call?.name !== "Bash" || !e.item.result) return;
    const thread = this.root(e.thread);
    const block = commandBlock(e.item.call.command, e.item.result, thread !== e.thread ? "a subagent's command" : undefined);
    const list = this.ran.get(thread) ?? [];
    list.push(block);
    let chars = list.reduce((n, b) => n + b.length, 0);
    while (list.length > KEEP_COMMANDS || (chars > KEEP_CHARS && list.length > 1)) chars -= list.shift()!.length;
    this.ran.set(thread, list);
    if (this.opened && this.session?.thread === thread) this.writer.fire(block);
  }

  /** Shows the terminal, made the first time. */
  show(): void {
    if (!this.terminal) {
      this.terminal = vscode.window.createTerminal({ name: "ognivo", pty: this, iconPath: new vscode.ThemeIcon("sparkle"), isTransient: true });
    }
    this.terminal.show(false);
  }

  open(): void {
    this.opened = true;
    this.replay();
  }

  close(): void {
    this.opened = false;
    this.terminal = undefined;
  }

  /** What is typed in it goes nowhere: it only shows. */
  handleInput(): void {}

  /** The screen again: the session on show and what it ran. */
  private replay(): void {
    const s = this.session;
    let text = CLEAR;
    if (!s) text += "\x1b[2mNo session shows in the panel: its commands show here as the agent runs them.\x1b[0m\r\n";
    else {
      text += sessionLine(s.title);
      const ran = this.ran.get(s.thread) ?? [];
      text += ran.length ? ran.join("") : "\x1b[2mThe agent's commands show here as it runs them.\x1b[0m\r\n\r\n";
    }
    this.writer.fire(text);
  }

  dispose(): void {
    this.terminal?.dispose();
    this.writer.dispose();
  }
}

/** Raw output of a command kept while it runs: the end of it, before the escapes go. */
const RAW = 64_000;
/** How long a command's end waits for the rest of its output. */
const DRAIN_MS = 1000;
const KEEP_FAILURES = 5;

/** The commands that failed in the user's own terminals. */
export class TerminalFailures {
  private list: TerminalFailure[] = [];
  /** What each command that runs wrote so far, and when it is all there; with its terminal,
   * which may close before the command ends (and then no end comes). */
  private readonly reading = new Map<vscode.TerminalShellExecution, { terminal: vscode.Terminal; raw: string; done: Promise<void> }>();
  private next = 1;
  private readonly disposables: vscode.Disposable[] = [];

  constructor(
    /** A terminal of the extension's own, whose commands are not the user's. */
    private readonly ours: (t: vscode.Terminal) => boolean,
    /** The list changed: the newest first. */
    private readonly onChange: (failures: TerminalFailure[]) => void,
  ) {
    this.disposables.push(
      vscode.window.onDidStartTerminalShellExecution((e) => this.started(e)),
      vscode.window.onDidEndTerminalShellExecution((e) => void this.ended(e)),
      vscode.window.onDidCloseTerminal((t) => {
        for (const [execution, r] of this.reading) if (r.terminal === t) this.reading.delete(execution);
      }),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("ognivo.terminalSuggestions") && !this.enabled() && this.list.length) {
          this.list = [];
          this.onChange([]);
        }
      }),
    );
  }

  failures(): TerminalFailure[] {
    return this.list;
  }

  private enabled(): boolean {
    return vscode.workspace.getConfiguration("ognivo").get("terminalSuggestions", true);
  }

  private started(e: vscode.TerminalShellExecutionStartEvent): void {
    if (!this.enabled() || this.ours(e.terminal)) return;
    // the stream gives only what comes after it is first read: at once, here
    const stream = e.execution.read();
    const reading = { terminal: e.terminal, raw: "", done: Promise.resolve() };
    reading.done = (async () => {
      try {
        for await (const chunk of stream) {
          reading.raw += chunk;
          if (reading.raw.length > RAW * 2) reading.raw = reading.raw.slice(-RAW);
        }
      } catch {
        // the terminal went away while it ran
      }
    })();
    this.reading.set(e.execution, reading);
  }

  private async ended(e: vscode.TerminalShellExecutionEndEvent): Promise<void> {
    const reading = this.reading.get(e.execution);
    this.reading.delete(e.execution);
    if (!this.enabled() || this.ours(e.terminal)) return;
    const command = e.execution.commandLine.value.trim();
    const cwd = e.execution.cwd ? (e.execution.cwd.scheme === "file" ? e.execution.cwd.fsPath : e.execution.cwd.toString()) : null;
    // the same command, where it ran: a shell says where only once its first prompt is up
    const same = (f: TerminalFailure) => f.command === command && (f.cwd === null || cwd === null || f.cwd === cwd);
    // Ctrl+C, or a shell that did not say: not a failure to offer
    if (e.exitCode === undefined) return;
    if (e.exitCode === 0) {
      // the same command works now: its failure is no news any more
      const kept = this.list.filter((f) => !same(f));
      if (kept.length !== this.list.length) {
        this.list = kept;
        this.onChange(this.list);
      }
      return;
    }
    if (!command) return;
    // the end may come before the last of the output
    if (reading) await Promise.race([reading.done, new Promise<void>((resolve) => setTimeout(resolve, DRAIN_MS))]);
    const raw = reading ? reading.raw.slice(-RAW) : "";
    const failure: TerminalFailure = {
      id: `${Date.now().toString(36)}-${this.next++}`,
      command,
      exitCode: e.exitCode,
      cwd,
      output: tail(plain(raw)),
      at: Date.now(),
    };
    this.list = [failure, ...this.list.filter((f) => !same(f))].slice(0, KEEP_FAILURES);
    this.onChange(this.list);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}
