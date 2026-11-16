// The message being written: its chips, its words and its images, and below them the mode,
// the model, the session's notes, how full the context is, and send or stop. Enter sends
// it, or queues it while a turn runs; Ctrl+Enter sends it into the running turn, where the
// model reads it before its next step; Shift+Enter starts a line, Shift+Tab goes to the
// next mode, Escape stops a running turn, and ↑ in an empty composer goes to the question
// the agent asks. `@` names a file: the session's first, then any of the workspace's. The
// file and the selection the user works in go with the message by themselves, as a chip
// the user may take away; images come by paste, drop or the + button, and where the model
// was said to read none, the core asks its server again before they are refused. What is
// sent leaves the composer at once (its draft too); a message that could not go comes back.
// The message being written stays with its composer while another thread or the settings
// show: its words, what the user attached and took away (drafts.ts).

import { type ReactNode, type RefObject, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { chipLabel, fromWindow, splitContext } from "../shared/context.ts";
import type { ContextChip } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";
import { Chip, ImageChip, SuggestChip } from "./chips.tsx";
import { placeByComposer, type Place, useAway } from "./controls.tsx";
import { forgetDraft, heldOf, hold, loadChips, loadText, saveChips, saveText } from "./drafts.ts";
import { findFiles, onInsert } from "./host.ts";
import { Icon, IconButton } from "./icon.tsx";
import { autoChip, chipKey, contextSize, editorChips, failureChip, mentionAt, mentionChoices, sessionFiles } from "./model/context.ts";
import { count } from "./model/format.ts";
import { type Attachment, MAX_IMAGES, MAX_MESSAGE_BYTES, aboutBytes, attach, imageFiles } from "./model/images.ts";
import { MODES, modeInfo, nextMode } from "./model/modes.ts";
import type { ThreadStore } from "./model/store.ts";
import workspace from "./model/workspace.ts";
import type { Message, Queued } from "./queue.ts";

/** Why images do not go, and what to do about it. */
const NO_IMAGES =
  "This model reads no images: its llama-server runs without a projector (--mmproj). Start it with --mmproj, then attach the image again.";

/** The failed commands the user attached or put away: offered once a page. */
const offered = new Set<string>();
/** The editor's selection a message took (its chip's key): not with the next message, nor
 * the next session's first, until the user selects something else. */
let sentSelection: string | undefined;
/** A new session's first message went from a composer that had the focus: the session's
 * own composer, which shows in its place, takes it, so that the user writes on. */
let keepFocus = false;

/** A message for the composer: one that could not go, back as it was (`restore`); or words
 * and chips from elsewhere in VS Code to add to the draft. */
export interface Incoming {
  /** The composer it is for, by its draft's key. */
  key: string;
  text: string;
  chips: ContextChip[];
  images: Attachment[];
  restore: boolean;
  /** Tells one from the next. */
  n: number;
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** The modes, in a menu above their button. */
function ModePicker({ mode, onMode }: { mode: P.Mode; onMode: (mode: P.Mode) => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useAway(open, [box], () => setOpen(false), false);
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
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              setOpen(false);
            }
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

/** The session's notes as it opened (a tree that is not clean, a small context, the
 * instruction files), and what went wrong on the page: a count by the ring, the list in a
 * popover, put away for the session with Dismiss. */
export function NotesButton({ notes, errors, onDismiss }: { notes: readonly string[]; errors: readonly string[]; onDismiss: () => void }) {
  const [place, setPlace] = useState<Place>();
  const button = useRef<HTMLButtonElement>(null);
  const pop = useRef<HTMLDivElement>(null);
  const open = place !== undefined;
  const close = (focus = false) => {
    setPlace(undefined);
    if (focus) button.current?.focus();
  };
  useAway(open, [pop, button], () => close(), false);
  useEffect(() => {
    if (open) pop.current?.querySelector<HTMLElement>("button")?.focus();
  }, [open]);
  const n = notes.length + errors.length;
  if (n === 0) return null;
  const what = errors.length ? `${errors.length === 1 ? "A problem" : `${errors.length} problems`}${notes.length ? ` and ${notes.length} note${notes.length === 1 ? "" : "s"}` : ""}` : `${notes.length} note${notes.length === 1 ? "" : "s"} of the session`;
  return (
    <>
      <button
        ref={button}
        type="button"
        className={`pill notes${errors.length ? " err" : ""}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={what}
        title={what}
        onClick={() => (open ? close() : setPlace(placeByComposer(button.current!, 260, 420)))}
      >
        <Icon name={errors.length ? "error" : "info"} />
        <span className="notes-count">{n}</span>
      </button>
      {place && (
        <div
          ref={pop}
          className="notes-pop"
          role="dialog"
          aria-label="Notes"
          style={{ left: place.left, width: place.width, top: place.top, bottom: place.bottom, maxHeight: place.maxHeight }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              close(true);
            }
          }}
        >
          <ul className="notes-list">
            {errors.map((t) => (
              <li key={`e:${t}`} className="notes-item err">
                <Icon name="error" />
                <span>{t}</span>
              </li>
            ))}
            {notes.map((t) => (
              <li key={`n:${t}`} className="notes-item">
                <Icon name="info" />
                <span>{t}</span>
              </li>
            ))}
          </ul>
          <div className="notes-foot">
            <button
              type="button"
              className="btn ghost small"
              onClick={() => {
                close(true);
                onDismiss();
              }}
            >
              Dismiss
            </button>
          </div>
        </div>
      )}
    </>
  );
}

/** The words a queued message shows: its own, or what goes with it. */
function shown(q: Queued): string {
  const { text, chips } = splitContext(q.text);
  const words = text.trim();
  if (words) return words.split("\n")[0]!;
  const labels = [...chips.map((c) => c.label), ...q.chips.map(chipLabel)];
  if (labels.length) return labels.join(", ");
  return q.images.length ? `${q.images.length} image${q.images.length === 1 ? "" : "s"}` : "";
}

/** The messages that wait for the turn to end, above the composer: each one sent into the
 * turn now, or dropped. */
export function QueueList({
  items,
  running,
  onSendNow,
  onRemove,
}: {
  items: readonly Queued[];
  running: boolean;
  onSendNow: (q: Queued) => void;
  onRemove: (q: Queued) => void;
}) {
  if (!items.length) return null;
  return (
    <ul className="queue" aria-label="Queued messages">
      {items.map((q) => {
        const words = shown(q);
        // a turn takes no images from the side: they go with the next turn
        const later = running && q.images.length > 0;
        return (
          <li key={q.id} className="queued">
            <Icon name="clock" className="queued-icon" />
            <span className="queued-label">Queued</span>
            <span className="queued-text" title={q.text}>
              {words}
            </span>
            <button
              type="button"
              className="btn secondary small"
              disabled={later}
              title={later ? "A message with images goes as the next turn" : running ? "Send it into the turn: the model reads it before its next step" : "Send it now"}
              onClick={() => onSendNow(q)}
            >
              Send now
            </button>
            <IconButton icon="close" title="Remove from the queue" onClick={() => onRemove(q)} />
          </li>
        );
      })}
    </ul>
  );
}

export function Composer({
  draftKey,
  store,
  running,
  pending,
  asking,
  onAsk,
  mode,
  onMode,
  context,
  model,
  notes,
  vision,
  onVision,
  home,
  root,
  onSubmit,
  stop,
  answerNow,
  incoming,
  onIncoming,
  error,
  onError,
  areaRef,
}: {
  /** Where the unsent text waits while another thread shows. */
  draftKey: string;
  store?: ThreadStore;
  /** A turn runs: Enter queues, Ctrl+Enter steers. */
  running: boolean;
  /** A message of this composer's is on its way. */
  pending: boolean;
  /** A question of the agent's waits above. */
  asking: boolean;
  /** Sends the focus to that question. */
  onAsk: () => void;
  mode: P.Mode;
  onMode: (mode: P.Mode) => void;
  context?: P.Context;
  /** The model's pill (modelpop.tsx). */
  model: ReactNode;
  /** The notes' indicator. */
  notes?: ReactNode;
  /** Whether the model takes images; `null` or `undefined` where nobody said. */
  vision: boolean | null | undefined;
  /** Whether the model takes images now, asked again: its server may have been started with
   * its projector since it said `false`. Images are refused only on this answer. */
  onVision: () => Promise<boolean>;
  /** The window's first folder, which the extension names the workspace's files from. */
  home: string;
  /** Where the session works, as the window names it: `@` names a file for its agent. */
  root: string;
  /** Takes the message: as a turn, into the queue, or into the running turn (`now`).
   * Whether it took it. */
  onSubmit: (m: Message, now: boolean) => boolean;
  stop: () => void;
  /** The Native path ends the model's reasoning where it is. */
  answerNow?: () => void;
  incoming?: Incoming;
  /** The composer took message `n`. */
  onIncoming: (n: number) => void;
  /** What went wrong with the last message sent. */
  error?: string;
  onError: (error: string | undefined) => void;
  /** The text area, for the page to know whether the user writes. */
  areaRef: RefObject<HTMLTextAreaElement | null>;
}) {
  // the message as this composer last showed it: after the settings, another thread, a reload
  const [text, setText] = useState(() => loadText(draftKey));
  const [caret, setCaret] = useState(0);
  const [picked, setPicked] = useState(0);
  const [closed, setClosed] = useState(false);
  const [found, setFound] = useState<string[]>([]);
  const [attached, setAttached] = useState<ContextChip[]>(() => loadChips(draftKey));
  // what the user took away, while the file it was of is still the editor's
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => {
    const kept = heldOf(draftKey);
    return kept && kept.file === workspace.editor?.abs ? kept.dismissed : new Set();
  });
  const [images, setImages] = useState<Attachment[]>(() => heldOf(draftKey)?.images ?? []);
  const [local, setLocal] = useState<string>();
  const [dropping, setDropping] = useState(false);
  const [, setOffers] = useState(0);
  const area = useRef<HTMLTextAreaElement | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const typed = useRef(text);
  typed.current = text;
  const imagesNow = useRef(images);
  imagesNow.current = images;
  /** Where the caret goes once the text that changed is in the area, with the focus. */
  const caretTo = useRef<number | undefined>(undefined);
  // before the next key: a key typed at once after a change goes where the caret is meant
  // to be, not where the browser left it (the end), as a frame later would have it
  useLayoutEffect(() => {
    const at = caretTo.current;
    const el = area.current;
    if (at === undefined || !el) return;
    caretTo.current = undefined;
    el.setSelectionRange(at, at);
    // the user works elsewhere in VS Code: the caret waits for the page to get the focus,
    // or the keys meant for the editor would go into the message, and its Enter send it
    if (!document.hasFocus()) {
      window.addEventListener("focus", () => document.activeElement === document.body && el.focus(), { once: true });
      return;
    }
    el.focus();
    el.setSelectionRange(at, at);
  });

  const editor = useSyncExternalStore(workspace.subscribe, () => workspace.editor);
  const failures = useSyncExternalStore(workspace.subscribe, () => workspace.failures);
  // the ring and the files of the session follow the thread as it grows
  const listVersion = useSyncExternalStore(
    useCallback((l: () => void) => store?.subscribeList(l) ?? (() => {}), [store]),
    () => store?.listVersion ?? 0,
  );

  useEffect(() => saveText(draftKey, text), [text, draftKey]);
  useEffect(() => saveChips(draftKey, attached), [attached, draftKey]);
  const file = editor?.abs;
  useEffect(() => hold(draftKey, { chips: attached, images, dismissed, file }), [draftKey, attached, images, dismissed, file]);

  useEffect(() => {
    const active = document.activeElement;
    if (keepFocus && document.hasFocus() && (active === null || active === document.body)) area.current?.focus();
    keepFocus = false;
  }, []);

  // a chip taken away comes back with the next file
  const fileSeen = useRef(file);
  useEffect(() => {
    if (fileSeen.current !== file) setDismissed(new Set());
    fileSeen.current = file;
  }, [file]);
  // a selection that went with a message comes back once the user selected something else
  const selectionNow = editor?.selection ? `${editor.abs}:${editor.selection.start}-${editor.selection.end}` : undefined;
  const selectionSeen = useRef(selectionNow);
  useEffect(() => {
    if (selectionSeen.current !== selectionNow) sentSelection = undefined;
    selectionSeen.current = selectionNow;
  }, [selectionNow]);

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
        caretTo.current = end;
      }),
    [],
  );

  // a message from elsewhere: Ctrl+I's "Ask in the Panel", or one that could not go
  useEffect(() => {
    if (!incoming) return;
    const merged = (a: ContextChip[], b: ContextChip[]) => {
      const have = new Set(a.map(chipKey));
      return [...a, ...b.filter((c) => !have.has(chipKey(c)))];
    };
    const now = typed.current;
    // one that could not go before what the user wrote since; a blank part adds no line (a
    // prompt of chips only, a message of images only)
    const parts = incoming.restore ? [incoming.text, now] : [now.trimEnd(), incoming.text];
    const next = parts.filter((t) => t.trim() !== "").join("\n");
    setText(next);
    setCaret(next.length);
    caretTo.current = next.length;
    if (incoming.restore) {
      setAttached((a) => merged(incoming.chips, a));
      setImages((list) => [...incoming.images, ...list].slice(0, MAX_IMAGES));
    } else {
      setAttached((a) => merged(a, incoming.chips));
    }
    onIncoming(incoming.n);
    // each message once, by its number
  }, [incoming?.n]);

  // the area grows with the text, to a third of the page
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, Math.max(120, window.innerHeight / 3))}px`;
  }, [text]);

  const mention = closed ? undefined : mentionAt(text, caret);
  const query = mention?.query;
  // the workspace's files, a moment after the user stops typing
  useEffect(() => {
    if (query === undefined) {
      setFound([]);
      return;
    }
    let live = true;
    const t = setTimeout(() => {
      findFiles(query, 20).then(
        (files) => live && setFound(files),
        () => live && setFound([]),
      );
    }, 120);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [query]);
  const sessionList = useMemo(
    () => (mention && store ? [...sessionFiles(store.items(), store.info.project).keys()] : []),
    [mention !== undefined, store, listVersion],
  );
  // the extension names them from the window's first folder: for a session of another folder
  // or project, from its own root, or by their paths on disk
  const workspaceList = useMemo(() => found.map((p) => fromWindow(p, home, root)), [found, home, root]);
  const choices = mention ? mentionChoices(mention.query, sessionList, workspaceList) : [];
  const chosen = choices[Math.min(picked, choices.length - 1)];

  const pick = (path: string) => {
    if (!mention) return;
    const next = `${text.slice(0, mention.start)}@${path} ${text.slice(caret)}`;
    const at = mention.start + path.length + 2;
    setText(next);
    setCaret(at);
    caretTo.current = at;
  };

  const selected = editorChips(editor).selection;
  const selectedKey = selected && chipKey(selected);
  const auto = autoChip(editor, sentSelection !== undefined && sentSelection === selectedKey ? new Set([...dismissed, sentSelection]) : dismissed, attached);
  const newest = failures[0];
  const suggestion =
    newest && !offered.has(newest.id) && !attached.some((c) => chipKey(c) === chipKey(failureChip(newest))) ? newest : undefined;

  const addImages = async (files: File[]) => {
    if (!files.length) return;
    // what the server said may be old: asked again, it may run its projector now
    if (vision === false && !(await onVision())) {
      setLocal(NO_IMAGES);
      return;
    }
    // a refusal of before does not hold any more
    setLocal((l) => (l === NO_IMAGES ? undefined : l));
    const full = () => {
      if (imagesNow.current.length < MAX_IMAGES) return false;
      setLocal(`A message takes at most ${MAX_IMAGES} images.`);
      return true;
    };
    for (const f of files) {
      if (full()) return;
      let a: Attachment;
      try {
        a = await attach(f, f.name || "a pasted image");
      } catch (e) {
        setLocal(message(e));
        continue;
      }
      // the images as they are now: another paste or drop may have added its own meanwhile
      if (full()) return;
      if (imagesNow.current.reduce((n, i) => n + i.bytes, a.bytes) > MAX_MESSAGE_BYTES) {
        setLocal(`A message's images take at most ${aboutBytes(MAX_MESSAGE_BYTES)} together.`);
        continue;
      }
      const list = [...imagesNow.current, a];
      imagesNow.current = list;
      setImages(list);
    }
  };

  /** The composer empty again; the file the user works in goes with the next message too,
   * but a selection that went with this one not again until it changes. */
  const clear = (sent: ContextChip | undefined) => {
    sentSelection = sent?.kind === "selection" ? chipKey(sent) : undefined;
    setText("");
    setCaret(0);
    setAttached([]);
    setImages([]);
    setDismissed(new Set());
    setLocal(undefined);
    // now, not with the next render: sending a new session's first message shows the
    // session's own composer before this one renders again
    forgetDraft(draftKey);
  };

  const submit = (now: boolean) => {
    const words = text.trimEnd();
    if (!words.trim() && !attached.length && !images.length) return;
    const m: Message = { text: words, chips: [...(auto ? [auto] : []), ...attached], images };
    onError(undefined);
    if (onSubmit(m, now)) {
      keepFocus = draftKey === "new" && document.activeElement === area.current;
      clear(auto);
    }
  };

  const last = store?.generations.at(-1);
  const used = last ? last.prompt + last.completion : 0;
  const ready = !pending && (text.trim().length > 0 || attached.length > 0 || images.length > 0);
  const shownError = error ?? local;
  const placeholder = asking
    ? "Answer above (↑), or tell ognivo something else"
    : running
      ? "Ask a follow-up · Enter queues it · Esc stops the turn"
      : store
        ? "Ask a follow-up · @ for files"
        : "What should ognivo do? · @ for files";
  const attachTitle =
    vision === false
      ? "Attach an image (when last asked, its llama-server ran without --mmproj: ognivo asks it again)"
      : "Attach an image (or paste one, or drop it here)";

  return (
    <div
      className={`composer${running ? " running" : ""}${dropping ? " dropping" : ""}`}
      onClick={(e) => e.target === e.currentTarget && area.current?.focus()}
      onDragOver={(e) => {
        if (![...e.dataTransfer.types].includes("Files")) return;
        e.preventDefault();
        setDropping(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropping(false);
      }}
      onDrop={(e) => {
        if (![...e.dataTransfer.types].includes("Files")) return;
        e.preventDefault();
        setDropping(false);
        void addImages(imageFiles(e.dataTransfer.files));
      }}
    >
      {mention && choices.length > 0 && (
        <ul className="mentions" role="listbox" aria-label="Files">
          {choices.map((p) => (
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
      {(auto || attached.length > 0 || images.length > 0 || suggestion) && (
        <div className="ctx-chips">
          {auto && (
            <Chip
              chip={auto}
              hint={auto.kind === "file" ? "current file" : undefined}
              onRemove={() => setDismissed((d) => new Set(d).add(chipKey(auto)))}
            />
          )}
          {attached.map((c) => (
            <Chip key={chipKey(c)} chip={c} onRemove={() => setAttached((a) => a.filter((x) => x !== c))} />
          ))}
          {images.map((img, i) => (
            <ImageChip
              key={`${i}:${img.data.length}`}
              image={img}
              index={i}
              onRemove={() => {
                // at once, for an image that is still being read to go after the others
                const list = imagesNow.current.filter((x) => x !== img);
                imagesNow.current = list;
                setImages(list);
              }}
            />
          ))}
          {suggestion && (
            <SuggestChip
              chip={failureChip(suggestion)}
              onAttach={() => {
                offered.add(suggestion.id);
                setAttached((a) => [...a, failureChip(suggestion)]);
              }}
              onDismiss={() => {
                offered.add(suggestion.id);
                setOffers((n) => n + 1);
              }}
            />
          )}
        </div>
      )}
      <textarea
        ref={(el) => {
          area.current = el;
          areaRef.current = el;
        }}
        value={text}
        rows={2}
        placeholder={placeholder}
        aria-label="Message to ognivo"
        onChange={(e) => {
          setText(e.target.value);
          setCaret(e.target.selectionStart);
          setClosed(false);
          setPicked(0);
        }}
        onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
        onPaste={(e) => {
          const files = imageFiles(e.clipboardData?.files);
          if (!files.length) return;
          // a paste of words and an image at once (a spreadsheet's cells come as both): the
          // words go in as the area takes any, and the image goes with the message
          if (!e.clipboardData.getData("text/plain")) e.preventDefault();
          void addImages(files);
        }}
        onKeyDown={(e) => {
          if (mention && choices.length > 0) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              setPicked((i) => (i + (e.key === "ArrowDown" ? 1 : choices.length - 1)) % choices.length);
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
          } else if (e.key === "ArrowUp" && asking && text === "") {
            e.preventDefault();
            onAsk();
          } else if (e.key === "Enter" && !e.shiftKey && !e.altKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            submit(e.ctrlKey || e.metaKey);
          } else if (e.key === "Escape" && running) {
            e.preventDefault();
            stop();
          }
        }}
      />
      {shownError && (
        <p className="composer-error" role="alert">
          <span>{shownError}</span>
          <IconButton
            icon="close"
            title="Hide"
            onClick={() => {
              setLocal(undefined);
              onError(undefined);
            }}
          />
        </p>
      )}
      <div className="composer-bar">
        <input
          ref={picker}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={(e) => {
            void addImages(imageFiles(e.target.files));
            e.target.value = "";
          }}
        />
        <IconButton icon="add" className="attach" title={attachTitle} onClick={() => picker.current?.click()} />
        <ModePicker mode={mode} onMode={onMode} />
        {model}
        <span className="spacer" />
        {notes}
        {context && used > 0 && <ContextRing used={used} context={context} />}
        {running && answerNow && (
          // its words go where the composer is narrow (dock.css): its name stays, and its icon is
          // not an arrow, which the send button beside it is
          <button
            type="button"
            className="pill answer-now"
            aria-label="Answer now"
            onClick={answerNow}
            title="Answer now: end the model's reasoning where it is, and get its answer"
          >
            <Icon name="comment" />
            <span>Answer now</span>
          </button>
        )}
        {running && ready && (
          <button type="button" className="round send ready" title="Queue it (Enter); Ctrl+Enter sends it into the turn now" aria-label="Queue" onClick={() => submit(false)}>
            <Icon name="arrow-up" />
          </button>
        )}
        {running ? (
          <button type="button" className="round stop" title="Stop the turn (Esc)" aria-label="Stop" onClick={stop}>
            <span className="stop-square" />
          </button>
        ) : (
          <button type="button" className={`round send${ready ? " ready" : ""}`} title="Send (Enter)" aria-label="Send" disabled={!ready} onClick={() => submit(false)}>
            <Icon name={pending ? "loading" : "arrow-up"} spin={pending} />
          </button>
        )}
      </div>
    </div>
  );
}
