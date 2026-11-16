// An approval the agent waits on (docs/architecture.md §8): one compact card above the
// composer. Its title says what the agent would do, its code box the command or the path,
// one muted line why it asks; then Run (Enter), the same for the rest of the session, or
// No (Escape), which turns the card into "What instead?" for a word to the agent with the
// no. The keys are the card's: they work where the focus is in it, and Enter on one of its
// buttons is that button's. The card takes the focus as it comes only from nobody: never
// from a composer that has text or the focus, nor from a field the user writes in.

import { type RefObject, useEffect, useRef, useState } from "react";

import type * as P from "../shared/protocol.ts";
import type { Ask, Core } from "../shared/rpc.ts";
import { within } from "../shared/text.ts";
import { Icon } from "./icon.tsx";

export type ApprovalAsk = Extract<Ask, { method: "approval/ask" }>;

/** What a question asks, as its card says it: its icon, its title, the word of its yes,
 * and whether its action is a command line. */
export interface AskKind {
  icon: string;
  title: string;
  verb: string;
  command: boolean;
}

/** A path as the core gives a file outside the project: absolute. */
function absolute(path: string): boolean {
  return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);
}

/** What the question is, by why the core asks and by the tool that asks (the model's
 * names for the tools: Claude Code's, gpt-oss's, an MCP server's). */
export function askKind(p: P.ApprovalAsked, project: string): AskKind {
  const tool = p.tool.toLowerCase();
  const reason = p.reason.trim().toLowerCase();
  const outsideReason = reason === "outside the project";
  if (reason.startsWith("an mcp tool")) return { icon: "plug", title: "Call this MCP tool?", verb: "Call", command: false };
  if (reason.startsWith("a prompt of an mcp server")) return { icon: "plug", title: "Use this MCP prompt?", verb: "Use", command: false };
  if (/shell|bash|exec|command|terminal|^run/.test(tool)) return { icon: "terminal", title: "Run this command?", verb: "Run", command: true };
  if (/edit|write|patch|create|delete|move|rename/.test(tool)) {
    const paths = p.action.split(", ");
    const several = paths.length > 1;
    let title = several ? "Edit these files?" : "Edit this file?";
    if (reason.startsWith("a protected path") && project) {
      const outside = paths.some((path) => absolute(path) && !within(path, project));
      title = outside ? (several ? "Edit files outside the project?" : "Edit a file outside the project?") : several ? "Edit protected files?" : "Edit a protected file?";
    }
    return { icon: "edit", title, verb: "Edit", command: false };
  }
  if (/grep|glob|search|find/.test(tool)) {
    return { icon: "search", title: outsideReason ? "Search outside the project?" : "Search here?", verb: "Search", command: false };
  }
  if (/^ls$|^list/.test(tool)) {
    return { icon: "folder", title: outsideReason ? "List a folder outside the project?" : "List this folder?", verb: "List", command: false };
  }
  if (/read|open|view|cat/.test(tool)) {
    return { icon: "file", title: outsideReason ? "Read a file outside the project?" : "Read this file?", verb: "Read", command: false };
  }
  return { icon: "shield", title: `Allow ${p.tool}?`, verb: "Allow", command: false };
}

/** Why it asks, in one sentence: the core's reason, and the changes nobody reviewed yet
 * that running the project's code would build on. */
export function askWhy(p: P.ApprovalAsked): string {
  const reason = p.reason.trim().replace(/\.$/, "");
  const n = p.unreviewed.length;
  if (/^it runs project code while there are unreviewed changes$/i.test(reason) && n > 0) {
    return `It runs the project's code, and ${n} of ognivo's changes ${n === 1 ? "is" : "are"} not reviewed yet.`;
  }
  const said = reason === "outside the project" ? "The path is outside the project" : reason.charAt(0).toUpperCase() + reason.slice(1);
  return n > 0 ? `${said}. Not reviewed yet: ${p.unreviewed.join(", ")}.` : `${said}.`;
}

export function ApprovalCard({
  ask,
  core,
  project,
  of,
  mayFocus,
  onLeave,
  rootRef,
}: {
  ask: ApprovalAsk;
  core: Core;
  /** The session's project: whether a path is outside it. */
  project: string;
  /** How many questions wait, this one first. */
  of: number;
  /** Whether the card may take the focus as it comes. */
  mayFocus: () => boolean;
  /** The card goes with the focus in it: where the focus goes next. */
  onLeave: () => void;
  /** The card, for the composer to send the focus to. */
  rootRef?: RefObject<HTMLDivElement | null>;
}) {
  const p = ask.params;
  const kind = askKind(p, project);
  const why = askWhy(p);
  const [instead, setInstead] = useState<string>();
  const root = useRef<HTMLDivElement | null>(null);
  /** When the card showed: an Enter sooner (a double or held one) was meant for what was
   * there before, such as the card this one follows. */
  const shownAt = useRef(performance.now());

  // the focus only from nobody, as the card first shows, and not again
  useEffect(() => {
    if (mayFocus()) root.current?.focus({ preventScroll: true });
  }, []);

  const answer = (decision: P.Decision, said: string | null = null) => {
    if (root.current?.contains(document.activeElement)) onLeave();
    core.answer(ask.id, { decision, instead: said });
  };
  const no = () => setInstead("");

  return (
    <div
      ref={(el) => {
        root.current = el;
        if (rootRef) rootRef.current = el;
      }}
      className={`ask${instead === undefined ? " asking" : " denying"}`}
      role="group"
      aria-label={kind.title}
      tabIndex={-1}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          if (instead === undefined) {
            no();
          } else {
            // back to the question, its keys with it
            setInstead(undefined);
            root.current?.focus({ preventScroll: true });
          }
          return;
        }
        // Enter on the card itself is the yes; on a button, that button's own
        if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && e.target === e.currentTarget && instead === undefined) {
          e.preventDefault();
          if (e.repeat || performance.now() - shownAt.current < 500) return;
          answer("allow");
        }
      }}
    >
      {instead === undefined ? (
        <>
          <div className="ask-head" title={p.tool}>
            <Icon name={kind.icon} className="ask-icon" />
            <span className="ask-title">{kind.title}</span>
            <span className="spacer" />
            {p.agent && (
              <span className="tag ask-agent" title={`The subagent of the task «${p.agent}» asks`}>
                subagent · {p.agent}
              </span>
            )}
            {of > 1 && <span className="tag">1 of {of}</span>}
          </div>
          <pre className="ask-action">
            {kind.command && (
              <span className="ask-prompt" aria-hidden>
                ${" "}
              </span>
            )}
            {p.action}
          </pre>
          <p className="ask-why" title={p.unreviewed.length ? `${why}\n\n${p.unreviewed.join("\n")}` : why}>
            {why}
          </p>
          <div className="ask-buttons">
            <button type="button" className="btn primary" onClick={() => answer("allow")}>
              {kind.verb}
              <kbd aria-hidden>Enter</kbd>
            </button>
            <button
              type="button"
              className="btn secondary"
              title={`Allow exactly this ${kind.command ? "command" : "action"} for the rest of the session`}
              onClick={() => answer("allow_session")}
            >
              Allow for this session
            </button>
            <button type="button" className="btn secondary ask-no" onClick={no}>
              No
              <kbd aria-hidden>Esc</kbd>
            </button>
          </div>
        </>
      ) : (
        <form
          className="ask-instead"
          onSubmit={(e) => {
            e.preventDefault();
            answer("deny", instead.trim() || null);
          }}
        >
          <div className="ask-head">
            <span className="ask-title">Not</span>
            <span className="ask-not">
              <code className="ask-what">{p.action}</code>.
            </span>
            <span className="ask-title">What instead?</span>
          </div>
          <input
            autoFocus
            value={instead}
            onChange={(e) => setInstead(e.target.value)}
            aria-label="What ognivo should do instead"
            placeholder="What ognivo should do instead, or nothing"
            spellCheck={false}
          />
          <div className="ask-buttons">
            <button type="button" className="btn secondary" onClick={() => answer("deny", null)}>
              Just no
            </button>
            <span className="spacer" />
            <button type="submit" className="btn primary">
              Send
              <kbd aria-hidden>Enter</kbd>
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
