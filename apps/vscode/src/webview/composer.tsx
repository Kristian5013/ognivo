// The message being written: Enter sends it, Shift+Enter starts a line, Shift+Tab goes to
// the next mode, Escape stops a running turn. `@` names a file the session read or
// changed; the editor sends selections and files here too (ognivo: Add the Selection…).
// Below: the mode, the model, the context the conversation fills, and send or stop.

import { useEffect, useMemo, useRef, useState } from "react";

import type * as P from "../shared/protocol.ts";
import { onInsert } from "./host.ts";
import { contextSize, mentionAt, sessionFiles } from "./model/context.ts";
import { count } from "./model/format.ts";
import { MODES, modeInfo, nextMode } from "./model/modes.ts";
import type { ThreadStore } from "./model/store.ts";

export interface Sending {
  running: boolean;
  send: (text: string) => Promise<unknown>;
  stop: () => void;
  /** The Native path ends the model's reasoning where it is. */
  answerNow?: () => void;
}

const DRAFTS = "ognivo.draft";

/** Tokens in the units of a context window: K of 1024, as llama-server's `-c` counts. */
function tokens(n: number): string {
  return n < 1024 ? String(n) : `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)}K`;
}

export function Composer({
  store,
  sending,
  mode,
  onMode,
  context,
  model,
  onModel,
  draftKey,
}: {
  store?: ThreadStore;
  sending: Sending;
  mode: P.Mode;
  onMode: (mode: P.Mode) => void;
  context?: P.Context;
  /** The model the session works with, or a new one will. */
  model: string;
  /** Shows where the model is chosen. */
  onModel: () => void;
  /** Where the unsent text waits while another thread shows. */
  draftKey: string;
}) {
  const [text, setText] = useState(() => {
    try {
      return sessionStorage.getItem(`${DRAFTS}.${draftKey}`) ?? "";
    } catch {
      return "";
    }
  });
  const [caret, setCaret] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [picked, setPicked] = useState(0);
  const [closed, setClosed] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const typed = useRef(text);
  typed.current = text;

  useEffect(() => {
    try {
      sessionStorage.setItem(`${DRAFTS}.${draftKey}`, text);
    } catch {
      // the draft lasts as long as the page shows it
    }
  }, [text, draftKey]);

  // the editor's selection and files go in at the caret, on a line of their own
  useEffect(
    () =>
      onInsert((piece) => {
        const cur = typed.current;
        const el = area.current;
        const at = el && document.activeElement === el ? el.selectionStart : cur.length;
        const before = cur.slice(0, at);
        const gap = before && !before.endsWith("\n") ? "\n" : "";
        const next = `${before}${gap}${piece}${cur.slice(at)}`;
        const end = before.length + gap.length + piece.length;
        setText(next);
        setCaret(end);
        requestAnimationFrame(() => {
          el?.focus();
          el?.setSelectionRange(end, end);
        });
      }),
    [],
  );

  // the area grows with the text, to a third of the page
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, Math.max(120, window.innerHeight / 3))}px`;
  }, [text]);

  const files = useMemo(() => (store ? [...sessionFiles(store.items(), store.info.project).keys()] : []), [store, store?.listVersion]);
  const mention = closed ? undefined : mentionAt(text, caret);
  const matches = useMemo(() => {
    if (!mention) return [];
    const q = mention.query.toLowerCase();
    return files.filter((p) => p.toLowerCase().includes(q)).slice(0, 8);
  }, [mention, files]);
  const chosen = matches[Math.min(picked, matches.length - 1)];

  const pick = (path: string) => {
    if (!mention) return;
    const next = `${text.slice(0, mention.start)}@${path} ${text.slice(caret)}`;
    const at = mention.start + path.length + 2;
    setText(next);
    setCaret(at);
    requestAnimationFrame(() => area.current?.setSelectionRange(at, at));
  };

  const submit = () => {
    if (busy || sending.running || !text.trim()) return;
    setBusy(true);
    setError(undefined);
    sending
      .send(text)
      .then(() => {
        setText("");
        setCaret(0);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  const last = store?.generations.at(-1);
  const used = last ? last.prompt + last.completion : 0;
  const info = modeInfo(mode);
  return (
    <div className="composer">
      {mention && matches.length > 0 && (
        <ul className="mentions" role="listbox" aria-label="Files of the session">
          {matches.map((p) => (
            <li key={p}>
              <button
                type="button"
                role="option"
                aria-selected={p === chosen}
                className={p === chosen ? "chosen" : ""}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(p);
                }}
              >
                <span className="codicon codicon-file" /> {p}
              </button>
            </li>
          ))}
        </ul>
      )}
      <textarea
        ref={area}
        value={text}
        rows={2}
        placeholder={store ? "Write to the agent" : "Write to the agent to start a session"}
        aria-label="Message"
        onChange={(e) => {
          setText(e.target.value);
          setCaret(e.target.selectionStart);
          setClosed(false);
          setPicked(0);
        }}
        onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
        onKeyDown={(e) => {
          if (mention && matches.length > 0) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              setPicked((i) => (i + (e.key === "ArrowDown" ? 1 : matches.length - 1)) % matches.length);
              return;
            }
            if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey && chosen) {
              e.preventDefault();
              pick(chosen);
              return;
            }
            if (e.key === "Escape") {
              e.preventDefault();
              setClosed(true);
              return;
            }
          }
          if (e.key === "Tab" && e.shiftKey) {
            e.preventDefault();
            onMode(nextMode(mode));
          } else if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            submit();
          } else if (e.key === "Escape" && sending.running) {
            e.preventDefault();
            sending.stop();
          }
        }}
      />
      {error && <p className="error">{error}</p>}
      <div className="composer-bar">
        <select
          className="mode"
          value={mode}
          title={`${info.says} (Shift+Tab: the next mode)`}
          aria-label="Mode"
          onChange={(e) => onMode(e.target.value as P.Mode)}
        >
          {MODES.map((m) => (
            <option key={m.mode} value={m.mode}>
              {m.glyph} {m.mode === "edit_automatically" ? "Auto-edit" : m.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="link model"
          title={store ? `The session's model: ${model}` : "The server and the model of a new session"}
          onClick={onModel}
        >
          {model || "choose a model"}
        </button>
        {context && (
          <span
            className={`context ${used >= context.compacts_at ? "warn" : ""}`}
            title={`The conversation holds about ${count(used)} tokens of ${contextSize(context.working)} it works with; it compacts near ${contextSize(context.compacts_at)}.`}
          >
            {tokens(used)} / {contextSize(context.working)}
          </span>
        )}
        <span className="spacer" />
        {sending.running && sending.answerNow && (
          <button type="button" className="secondary small" onClick={sending.answerNow} title="End the model's reasoning where it is, and get its answer">
            Answer now
          </button>
        )}
        {sending.running ? (
          <button type="button" className="icon-button stop" title="Stop the turn (Escape)" onClick={sending.stop}>
            <span className="codicon codicon-debug-stop" />
          </button>
        ) : (
          <button type="button" className="icon-button send" title="Send (Enter)" disabled={busy || !text.trim()} onClick={submit}>
            <span className={`codicon codicon-${busy ? "loading codicon-modifier-spin" : "send"}`} />
          </button>
        )}
      </div>
    </div>
  );
}
