// The sessions in VS Code's own pickers: `ognivo: Open a Session…` lists the core's
// sessions, this workspace's first; `ognivo: Choose the Server and the Model…` asks the
// server what it runs and keeps the choice in the settings of a new session.

import * as vscode from "vscode";

import type { Connection } from "./client.ts";
import type * as P from "./shared/protocol.ts";
import { ago, samePath } from "./shared/text.ts";
import { folders } from "./view.ts";

type SessionItem = vscode.QuickPickItem & { thread?: number };

/** The session the user picks: this workspace's first, then the others'. */
export async function pickSession(connection: Connection): Promise<number | undefined> {
  const core = await connection.get();
  const { threads } = await core.call("thread/list", { project: null, limit: 200 });
  const here = new Set(folders().map(samePath));
  const item = (t: P.ThreadSummary): SessionItem => ({
    label: t.title || "New session",
    description: [t.profile, `${t.turns} ${t.turns === 1 ? "turn" : "turns"}`, t.unreviewed ? `${t.unreviewed} unreviewed` : ""]
      .filter(Boolean)
      .join(" · "),
    detail: `${t.running ? "$(loading~spin) running · " : ""}${ago(t.updated)}${here.has(samePath(t.project)) ? "" : ` · ${t.project}`}`,
    thread: t.thread,
  });
  const mine = threads.filter((t) => here.has(samePath(t.project)) && t.kind !== "subagent");
  const others = threads.filter((t) => !here.has(samePath(t.project)) && t.kind !== "subagent");
  const items: SessionItem[] = [
    ...(mine.length ? [{ label: "This workspace", kind: vscode.QuickPickItemKind.Separator }] : []),
    ...mine.map(item),
    ...(others.length ? [{ label: "Other projects", kind: vscode.QuickPickItemKind.Separator }] : []),
    ...others.map(item),
  ];
  if (!items.length) {
    void vscode.window.showInformationMessage("ognivo: no sessions yet. Write to the agent to start one.");
    return undefined;
  }
  const picked = await vscode.window.showQuickPick(items, {
    title: "ognivo: Open a Session",
    placeHolder: "A session to go on with",
    matchOnDescription: true,
    matchOnDetail: true,
  });
  return picked?.thread;
}

/** The server and, behind llama-server's router, the model of a new session, kept in the
 * settings; the profile that fits it and its reasoning levels, as the core finds them. */
export async function chooseModel(connection: Connection): Promise<void> {
  const settings = vscode.workspace.getConfiguration("ognivo");
  const server = await vscode.window.showInputBox({
    title: "ognivo: the llama-server of a new session",
    prompt: "Its address; ognivo never starts a server",
    value: settings.get("server", "http://127.0.0.1:8080"),
    ignoreFocusOut: true,
    validateInput: (v) => (/^https?:\/\/\S+$/.test(v.trim()) ? undefined : "an http:// or https:// address"),
  });
  if (server === undefined) return;
  const url = server.trim();
  const core = await connection.get();
  let detected: P.Detected;
  try {
    detected = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `ognivo: asking ${url} what it runs` },
      () => core.call("server/detect", { url, api_key: null, model: null }),
    );
  } catch (e) {
    void vscode.window.showErrorMessage(`ognivo: ${url} did not answer: ${e instanceof Error ? e.message : String(e)}`);
    return;
  }
  await settings.update("server", url, vscode.ConfigurationTarget.Global);
  let model = "";
  if (detected.models.length) {
    const picked = await vscode.window.showQuickPick(
      detected.models.map((m) => ({
        label: m.id,
        description: m.status === "loaded" ? "loaded" : `${m.status}: the router starts it`,
        detail: m.profile ? `profile ${m.profile}` : "no profile fits it",
      })),
      { title: "ognivo: the model behind llama-server's router", placeHolder: "The model of a new session" },
    );
    if (!picked) return;
    model = picked.label;
  }
  await settings.update("model", model, vscode.ConfigurationTarget.Global);
  const profile = model ? (await core.call("server/detect", { url, api_key: null, model })).profile : detected.profile;
  void vscode.window.showInformationMessage(
    `ognivo: new sessions work with ${model || detected.model || url}${profile ? ` (profile ${profile})` : ": no profile fits it, so the Generic path guesses"}.`,
  );
}
