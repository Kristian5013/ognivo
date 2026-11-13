// An approval the agent waits on (docs/architecture.md §8): what would happen and why it
// asks; allowed once (Enter), for the rest of the session, or denied with what to do
// instead.

import { useState } from "react";

import type * as P from "../shared/protocol.ts";
import type { Ask, Core } from "../shared/rpc.ts";
import { Icon } from "./icon.tsx";

export type ApprovalAsk = Extract<Ask, { method: "approval/ask" }>;

/** What the question is, by the tool that asks (the model's names for the tools). */
function question(tool: string): { icon: string; title: string } {
  const t = tool.toLowerCase();
  if (/shell|bash|exec|command|run/.test(t)) return { icon: "terminal", title: "Run this command?" };
  if (/edit|write|patch|create|delete/.test(t)) return { icon: "edit", title: "Allow this change?" };
  if (/read|open|grep|glob|search|find/.test(t)) return { icon: "file", title: "Allow reading this?" };
  if (t.startsWith("mcp")) return { icon: "plug", title: "Allow this tool call?" };
  return { icon: "shield", title: `Allow ${tool}?` };
}

export function ApprovalCard({ ask, core, first }: { ask: ApprovalAsk; core: Core; first: boolean }) {
  const p: P.ApprovalAsked = ask.params;
  const [instead, setInstead] = useState<string>();
  const answer = (decision: P.Decision, why: string | null = null) => core.answer(ask.id, { decision, instead: why });
  const q = question(p.tool);
  return (
    <div
      className="ask"
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
      <div className="ask-head">
        <Icon name={q.icon} className="ask-icon" />
        <span className="ask-title">{q.title}</span>
        <span className="spacer" />
        {p.agent && <span className="tag">subagent · {p.agent}</span>}
        <span className="tag mono">{p.tool}</span>
      </div>
      <pre className="ask-action">{p.action}</pre>
      {p.reason && <p className="ask-why">It asks because {p.reason}.</p>}
      {p.unreviewed.length > 0 && (
        <p className="ask-why">
          Not reviewed yet:{" "}
          {p.unreviewed.map((f) => (
            <code key={f}>{f}</code>
          ))}
        </p>
      )}
      {instead === undefined ? (
        <div className="ask-buttons">
          <button type="button" className="btn ghost" onClick={() => setInstead("")}>
            Deny…
          </button>
          <span className="spacer" />
          <button type="button" className="btn secondary" onClick={() => answer("allow_session")} title="This exact action again, for the rest of the session">
            Allow for the session
          </button>
          <button type="button" className="btn primary" title="Allow it once (Enter)" autoFocus={first} onClick={() => answer("allow")}>
            Allow
          </button>
        </div>
      ) : (
        <form
          className="ask-instead"
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
          <button type="submit" className="btn danger">
            Deny
          </button>
        </form>
      )}
    </div>
  );
}
