// Before a session: what a new one works with (the server, its model and profile, as the
// core finds them), and this workspace's last sessions to go on with.

import { useEffect, useState } from "react";

import type { NewSession, SettingsTab } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";
import { ago } from "../shared/text.ts";
import { useDetected } from "./detect.ts";
import { run } from "./host.ts";

export function Welcome({
  core,
  session,
  project,
  onOpen,
  onSettings,
}: {
  core: Core;
  session: NewSession;
  /** The workspace's folder a new session works in; none without a folder. */
  project?: string;
  onOpen: (t: P.ThreadSummary) => void;
  onSettings: (tab: SettingsTab) => void;
}) {
  const detected = useDetected(core, session.server, session.model);
  const [recent, setRecent] = useState<P.ThreadSummary[]>([]);
  useEffect(() => {
    if (!project) return;
    let live = true;
    core.call("thread/list", { project, limit: 6 }).then(
      (r) => live && setRecent(r.threads.filter((t) => t.kind !== "subagent")),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [core, project]);

  const d = detected && detected !== "failed" ? detected : undefined;
  const profile = session.profile || d?.profile;
  const model = session.model || d?.model;
  return (
    <div className="welcome">
      <div className="welcome-head">
        <svg width="28" height="28" viewBox="0 0 24 24" aria-hidden className="spark">
          <path d="M12 1.5 14.1 9.9 22.5 12 14.1 14.1 12 22.5 9.9 14.1 1.5 12 9.9 9.9Z" />
          <circle cx="19.4" cy="4.6" r="1.35" />
          <circle cx="4.9" cy="19.1" r="0.85" />
        </svg>
        <div>
          <h1>ognivo</h1>
          <p className="muted">A coding agent on your own llama-server</p>
        </div>
      </div>
      {!project && (
        <p className="notice warn">
          <span className="codicon codicon-folder" /> Open a folder: a session works in a folder of the workspace.
        </p>
      )}
      <div className="facts">
        <div>
          <span className="muted">Server</span>
          <span className="mono">{session.server}</span>
          {detected === undefined && <span className="codicon codicon-loading codicon-modifier-spin" />}
          {detected === "failed" && <span className="err">does not answer</span>}
        </div>
        {d && (
          <div>
            <span className="muted">Model</span>
            <span>
              {model ?? "—"}
              {profile ? <span className="muted"> · profile {profile}</span> : <span className="warn"> · no profile fits it</span>}
            </span>
          </div>
        )}
        {d && (
          <div>
            <span className="muted">Reasoning</span>
            <span>{session.reasoning || (d.default_level ? `${d.default_level} (the model's default)` : "the model's default")}</span>
          </div>
        )}
        {d?.window && (
          <div>
            <span className="muted">Window</span>
            <span>{Math.round(d.window / 1024)}K tokens</span>
          </div>
        )}
        <div className="buttons">
          <button type="button" className="secondary small" onClick={() => onSettings("model")}>
            Change the server or the model
          </button>
          <button type="button" className="link" onClick={() => onSettings("mcp")}>
            MCP servers
          </button>
          <button type="button" className="link" onClick={() => onSettings("instructions")}>
            Instructions
          </button>
        </div>
      </div>
      {recent.length > 0 && (
        <div className="recent">
          <h2>
            Recent sessions
            <button type="button" className="link" onClick={() => run("openSession")}>
              All…
            </button>
          </h2>
          <ul>
            {recent.map((t) => (
              <li key={t.thread}>
                <button type="button" onClick={() => onOpen(t)}>
                  <span className="recent-title">{t.title || "New session"}</span>
                  <span className="muted">
                    {t.running ? "running · " : ""}
                    {ago(t.updated)} · {t.turns} {t.turns === 1 ? "turn" : "turns"}
                    {t.unreviewed ? ` · ${t.unreviewed} unreviewed` : ""}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
