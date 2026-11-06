import { Command } from "cmdk";
import { useCallback, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { type Budget, estimateTokens, mentionAt, sessionFiles } from "../context.ts";
import { count } from "../format.ts";
import profiles from "../profiles.json";
import type * as P from "../protocol.ts";
import type { ThreadStore } from "../store.ts";

const BUDGETS: Record<string, { context: Budget } | undefined> = profiles;

/** What a composer on the core does: sends the message (Enter; Shift+Enter is a new
 * line), and stops the running turn. */
export interface Sending {
  send: (text: string) => Promise<void>;
  stop: () => void;
  running: boolean;
  /** On the Native path: ends the model's reasoning, and the answer comes. */
  answerNow?: () => void;
}

/** Where the user writes, with `@` for the files of the session and what the message
 * would cost: its tokens, the files it names, and where the conversation would stand
 * against the thread's context (`context`, as the core opened it) or else the profile's
 * budget. Without a core it sends nothing. */
export function Composer({
  store,
  draft = "",
  sending,
  context,
}: {
  store: ThreadStore;
  draft?: string;
  sending?: Sending;
  context?: P.Context;
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
  const area = useRef<HTMLTextAreaElement>(null);

  const submit = () => {
    if (!sending || busy || sending.running || !text.trim()) return;
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
  const own = estimateTokens(text);
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
    <div className="relative border-t border-border px-6 py-3">
      <div className="mx-auto max-w-3xl">
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
          onKeyDown={(e) => {
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
                disabled={busy || !text.trim()}
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
