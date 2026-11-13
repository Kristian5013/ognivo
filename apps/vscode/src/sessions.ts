// The sessions in VS Code's own pickers: `ognivo: Open a Session…` lists the core's
// sessions, this workspace's first, each with a button that deletes it (as does `ognivo:
// Delete This Session`); `ognivo: Choose the Server and the Model…` asks the server what it
// runs and keeps the choice in the settings of a new session.

import * as vscode from "vscode";

import type { Connection } from "./client.ts";
import { ERRORS } from "./shared/protocol.ts";
import type * as P from "./shared/protocol.ts";
import { CoreError } from "./shared/rpc.ts";
import { ago, samePath } from "./shared/text.ts";
import { folders } from "./view.ts";

type SessionItem = vscode.QuickPickItem & { thread?: number; title?: string };

const DELETE: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon("trash"), tooltip: "Delete the session" };

/** Deletes a session once the user agrees: its conversation and its subagents' threads go;
 * the files it changed stay as they are. Whether it did. */
export async function deleteSession(connection: Connection, thread: number, title: string): Promise<boolean> {
  const sure = await vscode.window.showWarningMessage(
    `Delete the session «${title || "New session"}»?`,
    {
      modal: true,
      detail: "Its conversation goes, with its subagents' threads. The files it changed stay as they are: what it left unreviewed counts as accepted.",
    },
    "Delete",
  );
  if (sure !== "Delete") return false;
  const core = await connection.get();
  try {
    await core.call("thread/delete", { thread });
  } catch (e) {
    const busy = e instanceof CoreError && e.code === ERRORS.busy;
    void vscode.window.showErrorMessage(busy ? "ognivo: a turn of the session runs: stop it first." : `ognivo: ${e instanceof Error ? e.message : String(e)}`);
    return false;
  }
  return true;
}

/** The session the user picks: this workspace's first, then the others'. A session's
 * button deletes it. */
export async function pickSession(connection: Connection, onDeleted: (thread: number) => void): Promise<number | undefined> {
  const core = await connection.get();
  const here = new Set(folders().map(samePath));
  const item = (t: P.ThreadSummary): SessionItem => ({
    label: t.title || "New session",
    description: [t.profile, `${t.turns} ${t.turns === 1 ? "turn" : "turns"}`, t.unreviewed ? `${t.unreviewed} unreviewed` : ""]
      .filter(Boolean)
      .join(" · "),
    detail: `${t.running ? "$(loading~spin) running · " : ""}${ago(t.updated)}${here.has(samePath(t.project)) ? "" : ` · ${t.project}`}`,
    thread: t.thread,
    title: t.title,
    buttons: [DELETE],
  });
  const list = async (): Promise<SessionItem[]> => {
    const { threads } = await core.call("thread/list", { project: null, limit: 200 });
    const mine = threads.filter((t) => here.has(samePath(t.project)) && t.kind !== "subagent");
    const others = threads.filter((t) => !here.has(samePath(t.project)) && t.kind !== "subagent");
    return [
      ...(mine.length ? [{ label: "This workspace", kind: vscode.QuickPickItemKind.Separator }] : []),
      ...mine.map(item),
      ...(others.length ? [{ label: "Other projects", kind: vscode.QuickPickItemKind.Separator }] : []),
      ...others.map(item),
    ];
  };
  const items = await list();
  if (!items.length) {
    void vscode.window.showInformationMessage("ognivo: no sessions yet. Write to the agent to start one.");
    return undefined;
  }
  const pick = vscode.window.createQuickPick<SessionItem>();
  pick.title = "ognivo: Open a Session";
  pick.placeholder = "A session to go on with";
  pick.matchOnDescription = true;
  pick.matchOnDetail = true;
  pick.items = items;
  return new Promise((resolve) => {
    let chosen: number | undefined;
    pick.onDidAccept(() => {
      chosen = pick.selectedItems[0]?.thread;
      pick.hide();
    });
    pick.onDidTriggerItemButton(async ({ item: it }) => {
      if (it.thread === undefined) return;
      // the dialog takes the focus: the picker would close without this
      pick.ignoreFocusOut = true;
      const gone = await deleteSession(connection, it.thread, it.title ?? "");
      pick.ignoreFocusOut = false;
      if (!gone) return;
      onDeleted(it.thread);
      pick.items = await list();
      if (!pick.items.some((i) => i.thread !== undefined)) pick.hide();
    });
    pick.onDidHide(() => {
      pick.dispose();
      resolve(chosen);
    });
    pick.show();
  });
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
