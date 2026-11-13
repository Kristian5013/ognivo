// The core's questions in VS Code's own dialogs (docs/architecture.md §8, §10). Trust in a
// project's rules, MCP servers and hooks, and an MCP server's sign-in, ask in a modal
// dialog of the window whose workspace holds the project. An approval asks in the page that
// shows its session; while none of this window's pages is in sight, a notice says so too,
// and the status bar counts what waits.

import * as vscode from "vscode";

import type * as P from "./shared/protocol.ts";
import type { Ask, Core } from "./shared/rpc.ts";
import { within } from "./shared/text.ts";
import { folders } from "./view.ts";

/** Whether `project` is this window's: one of its folders, or inside one. */
function ours(project: string): boolean {
  return folders().some((f) => within(project, f));
}

function list(lines: string[], most = 12): string {
  const shown = lines.slice(0, most).map((l) => `• ${l}`);
  if (lines.length > most) shown.push(`… and ${lines.length - most} more`);
  return shown.join("\n");
}

async function trust(core: Core, ask: Ask): Promise<void> {
  switch (ask.method) {
    case "rules/trust": {
      const p = ask.params as P.TrustAsked;
      const yes = await vscode.window.showWarningMessage(
        "ognivo: trust the project's rules?",
        {
          modal: true,
          detail: `The agent's session in ${p.project} would follow these rules of the project's files:\n\n${list(p.rules.map((r) => `${r.text} (${r.source})`))}\n\nThe answer is kept until the files change.`,
        },
        "Trust",
        "Not Now",
      );
      core.answer(ask.id, { trust: yes === "Trust" });
      break;
    }
    case "mcp/trust": {
      const p = ask.params as P.McpTrustAsked;
      const yes = await vscode.window.showWarningMessage(
        "ognivo: start the project's MCP servers?",
        {
          modal: true,
          detail: `${p.project}/.mcp.json starts these servers. Each runs on your machine as you:\n\n${list(p.servers.map((s) => `${s.name}: ${s.command}`))}\n\nThe answer is kept until .mcp.json changes.`,
        },
        "Trust",
        "Not Now",
      );
      core.answer(ask.id, { trust: yes === "Trust" });
      break;
    }
    case "hooks/trust": {
      const p = ask.params as P.HooksTrustAsked;
      const yes = await vscode.window.showWarningMessage(
        "ognivo: run the project's hooks?",
        {
          modal: true,
          detail: `The project's settings run these commands on the agent's events, on your machine as you:\n\n${list(p.hooks.map((h) => `${h.hook} (${h.source})`))}\n\nThe answer is kept until the settings change.`,
        },
        "Trust",
        "Not Now",
      );
      core.answer(ask.id, { trust: yes === "Trust" });
      break;
    }
    case "mcp/sign_in": {
      const p = ask.params as P.McpSignInAsked;
      const yes = await vscode.window.showInformationMessage(
        `ognivo: sign in to the MCP server ${p.server.name}?`,
        { modal: true, detail: `${p.server.command} wants a sign-in. Your browser opens the server's page.` },
        "Sign In",
        "Not Now",
      );
      core.answer(ask.id, { sign_in: yes === "Sign In" });
      break;
    }
    default:
      break;
  }
}

/** Answers the core's trust questions natively, and tells of approvals out of sight. */
export class Asks {
  private readonly status: vscode.StatusBarItem;
  private readonly shown = new Set<number>();
  private core?: Core;

  constructor(
    /** Whether a page of this window shows `thread`, in sight. */
    private readonly inSight: (thread: number) => boolean,
    /** Brings the agent's page into sight. */
    private readonly focus: () => Thenable<unknown>,
  ) {
    this.status = vscode.window.createStatusBarItem("ognivo.asks", vscode.StatusBarAlignment.Right, 100);
    this.status.name = "ognivo: questions";
    this.status.command = "ognivo.focus";
  }

  /** Follows the questions of a new connection's client. */
  follow(core: Core): void {
    this.core = core;
    this.shown.clear();
    core.subscribe(() => this.changed());
  }

  private changed(): void {
    const core = this.core;
    if (!core) return;
    const asks = [...core.asks.values()];
    for (const ask of asks) {
      if (this.shown.has(ask.id)) continue;
      if (ask.method === "approval/ask") {
        this.shown.add(ask.id);
        this.notice(core, ask);
      } else if (ours((ask.params as { project: string }).project)) {
        this.shown.add(ask.id);
        void trust(core, ask);
      }
    }
    // the ones answered or withdrawn
    for (const id of this.shown) if (!core.asks.has(id)) this.shown.delete(id);
    const waiting = asks.filter((a) => a.method === "approval/ask").length;
    if (waiting) {
      this.status.text = `$(bell-dot) ognivo: ${waiting} ${waiting === 1 ? "question" : "questions"}`;
      this.status.tooltip = "The agent waits for your answer";
      this.status.backgroundColor = new vscode.ThemeColor("statusBarItem.warningBackground");
      this.status.show();
    } else {
      this.status.hide();
    }
  }

  /** An approval out of sight: a notice with its answers, and the way to the page. */
  private notice(core: Core, ask: Ask): void {
    const p = ask.params as P.ApprovalAsked;
    if (this.inSight(p.thread)) return;
    const who = p.agent ? `A subagent (${p.agent})` : "The agent";
    void vscode.window
      .showInformationMessage(`ognivo: ${who} asks: ${p.tool} ${p.action}`, { detail: p.reason }, "Allow", "Deny", "Open")
      .then(async (choice) => {
        if (choice === "Allow") core.answer(ask.id, { decision: "allow", instead: null });
        else if (choice === "Deny") core.answer(ask.id, { decision: "deny", instead: null });
        else if (choice === "Open") await this.focus();
      });
  }

  dispose(): void {
    this.status.dispose();
  }
}
