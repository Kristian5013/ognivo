import { Command } from "cmdk";
import { useCallback, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { estimateTokens, mentionAt, sessionFiles } from "../context.ts";
import { count } from "../format.ts";
import profiles from "../profiles.json";
import type { ThreadStore } from "../store.ts";

const BUDGETS = profiles as Record<string, { context: { working_max: number; compact_at: number } }>;

/** Where the user writes, with `@` for the files of the session and what the message
 * would cost: its tokens, the files it names, and where the conversation would stand
 * against the profile's budget. It sends nothing until there is a core to send to. */
export function Composer({ store, draft = "" }: { store: ThreadStore; draft?: string }) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  // the list version stands for the items: they are read again when rows come
  const version = useSyncExternalStore(subscribe, () => store.listVersion);
  const files = useMemo(() => sessionFiles(store.items(), store.info.project), [store, version]);
  const [text, setText] = useState(draft);
  const [caret, setCaret] = useState(draft.length);
  const [active, setActive] = useState("");
  const [closed, setClosed] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);

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
  const budget = BUDGETS[store.info.profile]?.context;
  const share = budget ? after / budget.working_max : 0;

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
          placeholder="Ask ognivo (no core connected yet) · @ names a file of the session"
          onChange={(e) => {
            setText(e.target.value);
            setCaret(e.target.selectionStart);
            setClosed(false);
          }}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
          onKeyDown={(e) => {
            if (!mention || matches.length === 0) return;
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
          className="block w-full resize-none rounded-lg border border-border bg-panel px-3 py-2 leading-5 outline-none placeholder:text-muted-foreground focus:border-spark/60"
        />
        <div className="mt-1 flex flex-wrap items-center gap-x-3 text-[11px] text-muted-foreground">
          <span>
            ≈ {count(own)} tokens{known ? ` + ${count(known)} in files` : ""}
            {unread.length ? ` + ${unread.length === 1 ? "a file" : `${unread.length} files`} not read yet` : ""}
          </span>
          {budget && (
            <span className={share > budget.compact_at ? "text-warn" : undefined}>
              after it ≈ {count(after)} of {count(budget.working_max)} ({Math.round(share * 100)}%)
              {share > budget.compact_at ? `: over ${Math.round(budget.compact_at * 100)}%, the conversation compacts` : ""}
            </span>
          )}
          <span className="ml-auto">counts are estimates until a core asks the server</span>
        </div>
      </div>
    </div>
  );
}
