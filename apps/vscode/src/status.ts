// The status bar's `ognivo` item (docs/architecture.md §10): what waits for the user and
// what the agent of the session on show does, by priority: a question ("ognivo asks", in
// the warning colours; a click opens its session's page), a running turn (`Writing · 34
// tok/s`, from the core's progress), the files to review (a click reviews them all), and
// else just the way to the panel. The words are wording.ts's; its tooltip reads no path,
// command or title as markdown.

import * as vscode from "vscode";

import type { Ask } from "./shared/rpc.ts";
import type * as P from "./shared/protocol.ts";
import { type Activity, type ReviewFile, statusLine } from "./wording.ts";

export interface StatusSource {
  /** The approvals that wait. */
  asks(): Ask[];
  /** The session on show. */
  shown(): { thread: number; title: string } | undefined;
  activity(thread: number): Activity | undefined;
  review(thread: number): ReviewFile[];
}

export class StatusItem {
  private readonly item: vscode.StatusBarItem;
  private timer?: ReturnType<typeof setTimeout>;
  private last = "";

  constructor(private readonly source: StatusSource) {
    this.item = vscode.window.createStatusBarItem("ognivo.status", vscode.StatusBarAlignment.Right, 100);
    this.item.name = "ognivo";
    this.update();
    this.item.show();
  }

  /** Something it shows changed; progress comes often, so it is drawn once in a while. */
  changed(): void {
    if (this.timer) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.update();
    }, 100);
  }

  private update(): void {
    const shown = this.source.shown();
    const approvals = this.source.asks().filter((a): a is Extract<Ask, { method: "approval/ask" }> => a.method === "approval/ask");
    const s = statusLine({
      asks: approvals.map((a) => a.params as P.ApprovalAsked),
      activity: shown ? this.source.activity(shown.thread) : undefined,
      review: shown ? this.source.review(shown.thread) : [],
      title: shown ? shown.title : null,
    });
    const asking = approvals[0]?.params.thread;
    const command: vscode.Command =
      s.click === "asks" && asking !== undefined
        ? { command: "ognivo.showThread", title: "Answer", arguments: [asking] }
        : s.click === "review"
          ? { command: "ognivo.review", title: "Review the Changes" }
          : { command: "ognivo.focus", title: "Focus the Agent" };
    const drawn = JSON.stringify([s, command]);
    if (drawn === this.last) return;
    this.last = drawn;
    this.item.text = s.text;
    // without icons: `$(name)` in a path or a command is what it says
    this.item.tooltip = new vscode.MarkdownString(s.tooltip, false);
    this.item.backgroundColor = s.warning ? new vscode.ThemeColor("statusBarItem.warningBackground") : undefined;
    this.item.command = command;
    this.item.accessibilityInformation = { label: s.label, role: "button" };
  }

  dispose(): void {
    clearTimeout(this.timer);
    this.item.dispose();
  }
}
