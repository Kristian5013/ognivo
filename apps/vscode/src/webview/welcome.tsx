// Before a session: what a new one works with (the server, its model and profile, as the
// core finds them), where to set it up, and this workspace's last sessions to go on with.

import { useEffect, useState } from "react";

import type { NewSession, SettingsTab } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";
import type { Core } from "../shared/rpc.ts";
import { ago, samePath } from "../shared/text.ts";
import { useDetected } from "./detect.ts";
import { request, run } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";

/** ognivo's spark. */
export function Spark({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden className="spark">
      <path d="M12 1.5 14.1 9.9 22.5 12 14.1 14.1 12 22.5 9.9 14.1 1.5 12 9.9 9.9Z" />
      <circle cx="19.4" cy="4.6" r="1.35" />
      <circle cx="4.9" cy="19.1" r="0.85" />
    </svg>
  );
}

export function Welcome({
  core,
  session,
  project,
  version,
  onOpen,
  onSettings,
}: {
  core: Core;
  session: NewSession;
  /** The workspace's folder a new session works in; none without a folder. */
  project?: string;
  /** Changes as sessions start, turn, end and go. */
  version: number;
  onOpen: (t: P.ThreadSummary) => void;
  onSettings: (tab: SettingsTab) => void;
}) {
  const detected = useDetected(core, session.server, session.model);
  const [recent, setRecent] = useState<P.ThreadSummary[]>([]);
  useEffect(() => {
    if (!project) return;
    let live = true;
    // every project's, then this one's by the page's own spelling of paths: VS Code names
    // `C:\` as `c:\`, a core of before 2026-11-13 found nothing by it
    const here = samePath(project);
    core.call("thread/list", { project: null, limit: 200 }).then(
      (r) => live && setRecent(r.threads.filter((t) => t.kind !== "subagent" && samePath(t.project) === here).slice(0, 5)),
      () => {},
    );
    return () => {
      live = false;
    };
  }, [core, project, version]);

  const d = detected && detected !== "failed" ? detected : undefined;
  const profile = session.profile || d?.profile;
  const model = session.model || d?.model || profile;
  const reasoning = session.reasoning || d?.default_level;
  const state = detected === undefined ? "pending" : detected === "failed" ? "failed" : "ok";
  return (
    <div className="welcome">
      <div className="hero">
        <Spark size={34} />
        <h1>ognivo</h1>
        <p>A coding agent on your own llama-server</p>
      </div>
      {!project && (
        <div className="callout warn">
          <Icon name="folder" />
          <span>Open a folder: a session works in a folder of the workspace.</span>
        </div>
      )}
      <div className={`conn is-${state}`}>
        <div className="conn-head">
          {state === "pending" ? <Icon name="loading" spin className="conn-dot" /> : <span className={`dot is-${state}`} />}
          <span className="conn-model">{state === "failed" ? "The server does not answer" : (model ?? "Looking at the server…")}</span>
          <span className="spacer" />
          <button type="button" className="btn ghost small" onClick={() => onSettings("model")}>
            Change
          </button>
        </div>
        <div className="conn-facts">
          <span className="mono">{session.server}</span>
          {d && (profile ? <span>profile {profile}</span> : <span className="warn">no profile fits it</span>)}
          {d && reasoning && <span>reasoning {reasoning}</span>}
          {d?.window ? <span>{Math.round(d.window / 1024)}K context</span> : null}
        </div>
      </div>
      <div className="quick">
        <button type="button" className="chip" onClick={() => onSettings("mcp")}>
          <Icon name="plug" /> MCP servers
        </button>
        <button type="button" className="chip" onClick={() => onSettings("instructions")}>
          <Icon name="book" /> Instructions
        </button>
        <button type="button" className="chip" onClick={() => run("openSession")}>
          <Icon name="history" /> All sessions
        </button>
      </div>
      {recent.length > 0 && (
        <div className="recent">
          <h2>Recent sessions</h2>
          <ul>
            {recent.map((t) => (
              <li key={t.thread}>
                <button type="button" className="recent-open" onClick={() => onOpen(t)}>
                  <Icon name={t.running ? "loading" : "comment-discussion"} spin={t.running} className="recent-icon" />
                  <span className="recent-title">{t.title || "New session"}</span>
                  <span className="recent-meta">
                    {t.unreviewed ? (
                      <span className="tag" title={`${t.unreviewed} files with unreviewed changes`}>
                        {t.unreviewed} changed
                      </span>
                    ) : null}
                    {ago(t.updated)}
                  </span>
                </button>
                <IconButton
                  icon="trash"
                  title={`Delete «${t.title || "New session"}»…`}
                  className="recent-delete"
                  disabled={t.running}
                  onClick={() => void request("sessions/delete", { thread: t.thread, title: t.title }).catch(() => {})}
                />
              </li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
