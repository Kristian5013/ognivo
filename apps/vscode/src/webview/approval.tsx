// An approval the agent waits on (docs/architecture.md §8): what would happen and why it
// asks; allowed once, for the rest of the session, or denied with what to do instead.

import { useState } from "react";

import type { Ask, Core } from "../shared/rpc.ts";
import type * as P from "../shared/protocol.ts";

export type ApprovalAsk = Extract<Ask, { method: "approval/ask" }>;

export function ApprovalCard({ ask, core, first }: { ask: ApprovalAsk; core: Core; first: boolean }) {
  const p: P.ApprovalAsked = ask.params;
  const [instead, setInstead] = useState<string>();
  const answer = (decision: P.Decision, why: string | null = null) => core.answer(ask.id, { decision, instead: why });
  return (
    <div
      className="approval"
      role="group"
      aria-label="The agent asks"
      onKeyDown={(e) => {
        if (instead !== undefined) return;
        if (e.key === "Enter" && !e.shiftKey) {
          e.preventDefault();
          answer("allow");
        }
      }}
    >
      <div className="approval-head">
        <span className="codicon codicon-shield" />
        <span>{p.agent ? `A subagent (${p.agent}) asks` : "The agent asks"}</span>
        <code className="approval-tool">{p.tool}</code>
      </div>
      <pre className="approval-action">{p.action}</pre>
      {p.reason && <p className="muted">{p.reason}</p>}
      {p.unreviewed.length > 0 && (
        <p className="muted">
          It runs the project's code, and these changes are not reviewed yet: {p.unreviewed.join(", ")}
        </p>
      )}
      {instead === undefined ? (
        <div className="buttons">
          <button type="button" className="primary" autoFocus={first} onClick={() => answer("allow")}>
            Allow
          </button>
          <button type="button" className="secondary" onClick={() => answer("allow_session")} title="This exact action again, for the rest of the session">
            Allow for the session
          </button>
          <button type="button" className="secondary" onClick={() => setInstead("")}>
            Deny…
          </button>
        </div>
      ) : (
        <form
          className="instead"
          onSubmit={(e) => {
            e.preventDefault();
            answer("deny", instead.trim() || null);
          }}
        >
          <input
            autoFocus
            value={instead}
            onChange={(e) => setInstead(e.target.value)}
            placeholder="What the agent should do instead (optional)"
            onKeyDown={(e) => e.key === "Escape" && setInstead(undefined)}
          />
          <button type="submit" className="primary">
            Deny
          </button>
        </form>
      )}
    </div>
  );
}
