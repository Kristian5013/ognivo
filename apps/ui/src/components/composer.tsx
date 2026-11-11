import { Command } from "cmdk";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { type Budget, estimateTokens, mentionAt, sessionFiles } from "../context.ts";
import { count } from "../format.ts";
import {
  type Attachment,
  MAX_IMAGES,
  MAX_MESSAGE_BYTES,
  aboutBytes,
  attach,
  imageFiles,
  imageTokens,
} from "../images.ts";
import { MODES, modeInfo, nextMode } from "../modes.ts";
import profiles from "../profiles.json";
import type * as P from "../protocol.ts";
import type { ThreadStore } from "../store.ts";
import { Pictures } from "./pictures.tsx";

const BUDGETS: Record<string, { context: Budget } | undefined> = profiles;

/** What a composer on the core does: sends the message (Enter; Shift+Enter is a new
 * line) with its images, and stops the running turn. */
export interface Sending {
  send: (text: string, images?: Attachment[]) => Promise<void>;
  stop: () => void;
  running: boolean;
  /** On the Native path: ends the model's reasoning, and the answer comes. */
  answerNow?: () => void;
  /** The thread's model takes images: a message may hold them. */
  vision?: boolean;
}

/** Why a message cannot hold images. */
export const NO_VISION =
  "The model of this thread reads no images: its llama-server runs without a projector (--mmproj)";

/** The images of a message as the composer holds them: added by a paste, a drop or the
 * button, each read by the window first; `error` says what could not be added. */
export function useAttachments(vision: boolean | undefined, initial: Attachment[] = []) {
  const [attached, setAttached] = useState<Attachment[]>(initial);
  const [reading, setReading] = useState(0);
  const [error, setError] = useState<string>();
  const add = (files: File[]) => {
    if (files.length === 0) return;
    if (!vision) {
      setError(NO_VISION);
      return;
    }
    const room = MAX_IMAGES - attached.length - reading;
    if (files.length > room) setError(`A message may hold at most ${MAX_IMAGES} images`);
    const taken = files.slice(0, Math.max(0, room));
    if (taken.length === 0) return;
    setError(undefined);
    setReading((n) => n + taken.length);
    for (const f of taken) {
      attach(f, f.name || "a pasted image")
        .then(
          (a) =>
            setAttached((list) => {
              const bytes = list.reduce((n, b) => n + b.bytes, a.bytes);
              if (bytes <= MAX_MESSAGE_BYTES) return [...list, a];
              setError(`${a.name}: the images of a message may have ${aboutBytes(MAX_MESSAGE_BYTES)} together`);
              return list;
            }),
          (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
        )
        .finally(() => setReading((n) => n - 1));
    }
  };
  const remove = (i: number) => setAttached((list) => list.filter((_, j) => j !== i));
  return { attached, setAttached, reading, error, setError, add, remove };
}

/** The thread's mode, and a menu of the others; Shift+Tab in the composer goes to the
 * next. */
function ModePicker({ mode, onMode }: { mode: P.Mode; onMode: (mode: P.Mode) => void }) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const info = modeInfo(mode);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);
  return (
    <div ref={root} className="relative">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        title={`${info.label}: ${info.says} Shift+Tab switches to the next mode.`}
        onClick={() => setOpen((o) => !o)}
        className={`flex items-center gap-1.5 rounded border border-border px-2 py-0.5 hover:bg-raised ${info.tone}`}
      >
        <span aria-hidden className="font-mono">
          {info.glyph}
        </span>
        <span>{info.label}</span>
        <span aria-hidden className="text-muted-foreground">
          ▾
        </span>
      </button>
      {open && (
        <div
          role="menu"
          aria-label="Mode"
          onKeyDown={(e) => {
            if (e.key === "Escape") setOpen(false);
          }}
          className="absolute bottom-full left-0 z-20 mb-1 w-80 overflow-hidden rounded-md border border-border bg-panel py-1 shadow-lg"
        >
          {MODES.map((m) => (
            <button
              key={m.mode}
              type="button"
              role="menuitemradio"
              aria-checked={m.mode === mode}
              onClick={() => {
                onMode(m.mode);
                setOpen(false);
              }}
              className={`flex w-full gap-2 px-3 py-1.5 text-left hover:bg-raised ${m.mode === mode ? "bg-raised" : ""}`}
            >
              <span aria-hidden className={`w-5 shrink-0 font-mono ${m.tone}`}>
                {m.glyph}
              </span>
              <span className="min-w-0">
                <span className={`block font-medium ${m.tone}`}>{m.label}</span>
                <span className="block text-[11px] leading-4 text-muted-foreground">{m.says}</span>
              </span>
            </button>
          ))}
          <p className="border-t border-border px-3 pt-1 text-[11px] text-muted-foreground">
            Shift+Tab switches to the next mode
          </p>
        </div>
      )}
    </div>
  );
}

/** Where the user writes, with `@` for the files of the session and what the message
 * would cost: its tokens, the files it names, and where the conversation would stand
 * against the thread's context (`context`, as the core opened it) or else the profile's
 * budget. Without a core it sends nothing. With `onMode` it has the thread's mode. */
export function Composer({
  store,
  draft = "",
  sending,
  context,
  mode,
  onMode,
}: {
  store: ThreadStore;
  draft?: string;
  sending?: Sending;
  context?: P.Context;
  mode?: P.Mode;
  onMode?: (mode: P.Mode) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  // the list version stands for the items: they are read again when rows come
  const version = useSyncExternalStore(subscribe, () => store.listVersion);
  const files = useMemo(() => sessionFiles(store.items(), store.info.project), [store, version]);
  const [text, setText] = useState(draft);
  const [caret, setCaret] = useState(draft.length);
  const [active, setActive] = useState("");
  const [closed, setClosed] = useState(false);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [dragging, setDragging] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const images = useAttachments(sending?.vision);
  const empty = !text.trim() && images.attached.length === 0;

  const submit = () => {
    if (!sending || busy || sending.running || empty || images.reading > 0) return;
    setBusy(true);
    setError(undefined);
    sending
      .send(text, images.attached)
      .then(() => {
        setText("");
        setCaret(0);
        images.setAttached([]);
        images.setError(undefined);
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  const mention = closed ? undefined : mentionAt(text, caret);
  const matches = useMemo(() => {
    if (!mention) return [];
    const q = mention.query.toLowerCase();
    return [...files.keys()].filter((p) => p.toLowerCase().includes(q)).slice(0, 8);
  }, [mention, files]);
  const selected = matches.includes(active) ? active : matches[0];

  const pick = (path: string) => {
    if (!mention) return;
    const next = `${text.slice(0, mention.start)}@${path} ${text.slice(caret)}`;
    const at = mention.start + path.length + 2;
    setText(next);
    setCaret(at);
    requestAnimationFrame(() => area.current?.setSelectionRange(at, at));
  };

  const named = [...new Set([...text.matchAll(/@([\w./-]+)/g)].map((m) => m[1]!))].filter((p) => files.has(p));
  const own =
    estimateTokens(text) + images.attached.reduce((n, a) => n + imageTokens(a.width, a.height), 0);
  const known = named.reduce((n, p) => n + (files.get(p) ?? 0), 0);
  const unread = named.filter((p) => files.get(p) === undefined);
  const last = store.generations.at(-1);
  const now = last ? last.prompt + last.completion : 0;
  const after = now + own + known;
  const profile = BUDGETS[store.info.profile]?.context;
  const budget = context
    ? { working: context.working, compactsAt: context.compacts_at }
    : profile && { working: profile.working_max, compactsAt: Math.floor(profile.working_max * profile.compact_at) };
  const share = budget ? after / budget.working : 0;
  const over = budget !== undefined && after > budget.compactsAt;

  return (
    <div
      className={`relative border-t border-border px-6 py-3 ${dragging ? "bg-raised/60" : ""}`}
      onDragOver={(e) => {
        if (!sending || ![...e.dataTransfer.types].includes("Files")) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDragging(false);
      }}
      onDrop={(e) => {
        if (!sending) return;
        e.preventDefault();
        setDragging(false);
        images.add(imageFiles(e.dataTransfer.files));
      }}
    >
      <div className="mx-auto max-w-3xl">
        {(images.attached.length > 0 || images.reading > 0) && (
          <div className="mb-2 flex flex-wrap items-center gap-2" aria-label="Images of the message">
            <Pictures pictures={images.attached} label="of the message" size="h-16" onRemove={images.remove} />
            {images.reading > 0 && (
              <span role="status" className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                <span className="spinner" aria-hidden /> reading {images.reading === 1 ? "an image" : `${images.reading} images`}
              </span>
            )}
          </div>
        )}
        {mention && matches.length > 0 && (
          <Command
            shouldFilter={false}
            value={selected}
            onValueChange={setActive}
            label="Files of the session"
            className="absolute bottom-full mb-1 w-80 overflow-hidden rounded-md border border-border bg-panel shadow-lg"
          >
            <Command.List className="max-h-56 overflow-y-auto py-1">
              {matches.map((p) => (
                <Command.Item
                  key={p}
                  value={p}
                  onSelect={pick}
                  className="flex cursor-pointer items-center gap-2 px-3 py-1 font-mono text-[12px] data-[selected=true]:bg-raised"
                >
                  <span className="min-w-0 flex-1 truncate">{p}</span>
                  <span className="text-[11px] text-muted-foreground">
                    {files.get(p) === undefined ? "" : `~${count(files.get(p)!)}`}
                  </span>
                </Command.Item>
              ))}
            </Command.List>
          </Command>
        )}
        <textarea
          ref={area}
          value={text}
          rows={2}
          placeholder={
            sending
              ? "Ask ognivo · @ names a file of the session · Enter sends, Shift+Enter is a new line"
              : "Ask ognivo (no core connected) · @ names a file of the session"
          }
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart);
            setClosed(false);
          }}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
          onPaste={(e) => {
            if (!sending) return;
            const pasted = imageFiles(e.clipboardData.files);
            if (pasted.length === 0) return;
            // an image copied from a page may come with its text: that is pasted as usual
            if (!e.clipboardData.types.includes("text/plain")) e.preventDefault();
            images.add(pasted);
          }}
          onKeyDown={(e) => {
            if (e.key === "Tab" && e.shiftKey && mode && onMode && !(mention && matches.length > 0)) {
              e.preventDefault();
              onMode(nextMode(mode));
              return;
            }
            if (!mention || matches.length === 0) {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing && sending) {
                e.preventDefault();
                submit();
              }
              return;
            }
            const i = Math.max(0, matches.indexOf(selected ?? ""));
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault();
              const next = (i + (e.key === "ArrowDown" ? 1 : matches.length - 1)) % matches.length;
              setActive(matches[next]!);
            } else if ((e.key === "Enter" || e.key === "Tab") && selected) {
              e.preventDefault();
              pick(selected);
            } else if (e.key === "Escape") {
              setClosed(true);
            }
          }}
          className="composer block w-full resize-none rounded-lg border border-border bg-panel px-3 py-2 leading-5 outline-none placeholder:text-muted-foreground"
        />
        <div className="mt-1 flex flex-wrap items-center gap-x-3 text-[11px] text-muted-foreground">
          {mode && onMode && <ModePicker mode={mode} onMode={onMode} />}
          {sending && (
            <>
              <button
                type="button"
                disabled={!sending.vision}
                onClick={() => picker.current?.click()}
                title={
                  sending.vision
                    ? "Add images the model sees with the message (or paste or drop them here)"
                    : NO_VISION
                }
                className="rounded border border-border px-2 py-0.5 hover:bg-raised disabled:cursor-not-allowed disabled:opacity-50"
              >
                + Image
              </button>
              <input
                ref={picker}
                type="file"
                accept="image/*"
                multiple
                hidden
                aria-label="Images to add"
                onChange={(e) => {
                  images.add(imageFiles(e.currentTarget.files));
                  e.currentTarget.value = "";
                }}
              />
            </>
          )}
          <span>
            ≈ {count(own)} tokens{known ? ` + ${count(known)} in files` : ""}
            {unread.length ? ` + ${unread.length === 1 ? "a file" : `${unread.length} files`} not read yet` : ""}
          </span>
          {budget && (
            <span className={over ? "text-warn" : undefined}>
              after it ≈ {count(after)} of {count(budget.working)} ({Math.round(share * 100)}%)
              {over ? `: past ${count(budget.compactsAt)}, the conversation compacts` : ` · compacts near ${count(budget.compactsAt)}`}
            </span>
          )}
          {error && <span className="text-err">{error}</span>}
          {images.error && <span className="text-err">{images.error}</span>}
          <span className="ml-auto">counts are estimates: 4 bytes a token</span>
          {sending &&
            (sending.running ? (
              <>
                {sending.answerNow && (
                  <button
                    type="button"
                    onClick={sending.answerNow}
                    title="End the model's reasoning where it stands and ask for the answer"
                    className="rounded border border-spark/60 px-2 py-0.5 text-foreground hover:bg-raised"
                  >
                    Answer now
                  </button>
                )}
                <button
                  type="button"
                  onClick={sending.stop}
                  className="rounded border border-warn/60 px-2 py-0.5 text-foreground hover:bg-raised"
                >
                  Stop
                </button>
              </>
            ) : (
              <button
                type="button"
                disabled={busy || empty || images.reading > 0}
                onClick={submit}
                className="rounded-md bg-spark px-3 py-0.5 font-medium text-background hover:opacity-90 disabled:bg-raised disabled:text-muted-foreground"
              >
                Send
              </button>
            ))}
        </div>
      </div>
    </div>
  );
}
