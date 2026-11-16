// The core's questions in VS Code's own dialogs (docs/architecture.md §8, §10). Trust in a
// project's rules, MCP servers and hooks, and an MCP server's sign-in, ask in a modal
// dialog of the window whose workspace holds the project. An approval asks in the page that
// shows its session; a notice asks too while no page of this window shows it in sight, and
// while VS Code is not in front of a window whose page shows it (`ognivo.notifications`).
// The notice allows only an action it shows whole, as it is, on one line; else it offers the
// page's card (Open) and Deny. The status bar's item (src/status.ts) says what waits.

import * as vscode from "vscode";

import type * as P from "./shared/protocol.ts";
import type { Ask, Core } from "./shared/rpc.ts";
import { within } from "./shared/text.ts";
import { folders, local } from "./view.ts";
import { approvalNotice } from "./wording.ts";

/** Whether `project` is this window's: one of its folders, or inside one. The core names it
 * by its real path, which may be another spelling of the folder (a link, an 8.3 name). */
function ours(project: string): boolean {
  const here = local(project);
  return folders().some((f) => within(here, f));
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
  private readonly shown = new Set<number>();
  private core?: Core;
  private unsubscribe?: () => void;

  constructor(
    /** Whether a page of this window shows `thread`, in sight. */
    private readonly inSight: (thread: number) => boolean,
    /** Whether a page of this window shows `thread`, in sight or not. */
    private readonly shownHere: (thread: number) => boolean,
    /** Brings the page that shows `thread` into sight, or opens it in one. */
    private readonly open: (thread: number) => Thenable<unknown>,
    /** The questions that wait changed. */
    private readonly onChange: () => void,
  ) {}

  /** The questions that wait now. */
  waiting(): Ask[] {
    return this.core ? [...this.core.asks.values()] : [];
  }

  /** Follows the questions of a new connection's client. */
  follow(core: Core): void {
    this.unsubscribe?.();
    this.core = core;
    this.shown.clear();
    this.unsubscribe = core.subscribe(() => this.changed());
    // the questions that waited when it connected
    this.changed();
  }

  private changed(): void {
    const core = this.core;
    if (!core) return;
    for (const ask of core.asks.values()) {
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
    this.onChange();
  }

  /** An approval out of sight, or while the user is in another program: a notice with its
   * answers, and the way to its session's page. A notice that cannot show the whole action
   * as it is (more lines, too long, characters it does not show) does not allow it: the
   * panel's card does, and the notice only denies. */
  private notice(core: Core, ask: Ask): void {
    const p = ask.params as P.ApprovalAsked;
    const away = vscode.workspace.getConfiguration("ognivo").get("notifications", true) && !vscode.window.state.focused && this.shownHere(p.thread);
    if (this.inSight(p.thread) && !away) return;
    const { text, whole } = approvalNotice(p);
    const answers = whole ? ["Allow", "Allow for This Session", "Deny", "Open"] : ["Open", "Deny"];
    void vscode.window
      .showInformationMessage(text, ...answers)
      .then(async (choice) => {
        if (choice === "Allow") core.answer(ask.id, { decision: "allow", instead: null });
        else if (choice === "Allow for This Session") core.answer(ask.id, { decision: "allow_session", instead: null });
        else if (choice === "Deny") core.answer(ask.id, { decision: "deny", instead: null });
        else if (choice === "Open") await this.open(p.thread);
      });
  }

  dispose(): void {
    this.unsubscribe?.();
  }
}
