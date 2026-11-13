// The message being written: Enter sends it, Shift+Enter starts a line, Shift+Tab goes to
// the next mode, Escape stops a running turn. `@` names a file the session read or
// changed; the editor sends selections and files here too (ognivo: Add the Selection…).
// Below: the mode, the model, how full the context is, and send or stop.

import { useEffect, useMemo, useRef, useState } from "react";

import type * as P from "../shared/protocol.ts";
import { onInsert } from "./host.ts";
import { Icon } from "./icon.tsx";
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

/** The modes, in a menu above their button. */
function ModePicker({ mode, onMode }: { mode: P.Mode; onMode: (mode: P.Mode) => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", away);
    return () => window.removeEventListener("mousedown", away);
  }, [open]);
  const info = modeInfo(mode);
  return (
    <div className="picker" ref={box}>
      <button
        type="button"
        className={`pill mode-${mode}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Mode: ${info.label}`}
        title={`${info.says} (Shift+Tab: the next mode)`}
        onClick={() => setOpen(!open)}
      >
        <Icon name={info.icon} />
        <span>{mode === "edit_automatically" ? "Auto-edit" : info.label}</span>
        <Icon name="chevron-down" className="pill-chev" />
      </button>
      {open && (
        <div
          className="menu"
          role="menu"
          onKeyDown={(e) => {
            if (e.key === "Escape") setOpen(false);
          }}
        >
          {MODES.map((m) => (
            <button
              key={m.mode}
              type="button"
              role="menuitemradio"
              aria-checked={m.mode === mode}
              className={`menu-item${m.mode === mode ? " current" : ""}`}
              autoFocus={m.mode === mode}
              onClick={() => {
                onMode(m.mode);
                setOpen(false);
              }}
            >
              <Icon name={m.icon} className={`menu-icon mode-${m.mode}`} />
              <span className="menu-text">
                <span className="menu-label">{m.label}</span>
                <span className="menu-says">{m.says}</span>
              </span>
              {m.mode === mode && <Icon name="check" className="menu-check" />}
            </button>
          ))}
          <div className="menu-foot">Shift+Tab: the next mode</div>
        </div>
      )}
    </div>
  );
}

/** How much of the working context the conversation holds, as a ring. */
function ContextRing({ used, context }: { used: number; context: P.Context }) {
  const share = Math.min(1, used / context.working);
  const near = used >= context.compacts_at * 0.9;
  const r = 6;
  const c = 2 * Math.PI * r;
  return (
    <span
      className={`ring${near ? " warn" : ""}`}
      title={`The conversation holds about ${count(used)} tokens of ${contextSize(context.working)}; it is compacted near ${contextSize(context.compacts_at)}.`}
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden>
        <circle cx="8" cy="8" r={r} className="ring-track" />
        <circle cx="8" cy="8" r={r} className="ring-fill" strokeDasharray={`${share * c} ${c}`} transform="rotate(-90 8 8)" />
      </svg>
      <span className="ring-text">{Math.round(share * 100)}%</span>
    </span>
  );
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
  const ready = !busy && text.trim().length > 0;
  return (
    <div className={`composer${sending.running ? " running" : ""}`} onClick={(e) => e.target === e.currentTarget && area.current?.focus()}>
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
                <Icon name="file" /> {p}
              </button>
            </li>
          ))}
        </ul>
      )}
      <textarea
        ref={area}
        value={text}
        rows={2}
        placeholder={
          sending.running
            ? "The agent is working — Esc stops it"
            : store
              ? "Write to the agent — @ names a file of the session"
              : "What should the agent do? Enter starts a session"
        }
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
      {error && <p className="composer-error">{error}</p>}
      <div className="composer-bar">
        <ModePicker mode={mode} onMode={onMode} />
        <button
          type="button"
          className="pill model"
          title={store ? `The session's model: ${model}` : "The server and the model of a new session"}
          onClick={onModel}
        >
          <Icon name="server-environment" />
          <span className="pill-text">{model || "Choose a model"}</span>
        </button>
        <span className="spacer" />
        {context && used > 0 && <ContextRing used={used} context={context} />}
        {sending.running && sending.answerNow && (
          <button type="button" className="pill answer-now" onClick={sending.answerNow} title="End the model's reasoning where it is, and get its answer">
            <Icon name="debug-step-out" />
            <span>Answer now</span>
          </button>
        )}
        {sending.running ? (
          <button type="button" className="round stop" title="Stop the turn (Escape)" aria-label="Stop" onClick={sending.stop}>
            <span className="stop-square" />
          </button>
        ) : (
          <button type="button" className={`round send${ready ? " ready" : ""}`} title="Send (Enter)" aria-label="Send" disabled={!ready} onClick={submit}>
            <Icon name={busy ? "loading" : "arrow-up"} spin={busy} />
          </button>
        )}
      </div>
    </div>
  );
}
