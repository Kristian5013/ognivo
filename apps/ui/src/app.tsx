import { useState } from "react";

import { ThreadView } from "./components/thread.tsx";
import { count, ending, percent } from "./format.ts";
import { type Thread, totals } from "./model.ts";
import { type Session, sessions } from "./sessions.ts";

/** Endpoint, model and what the session cost: the start of the Runtime HUD (§10). */
function Hud({ thread }: { thread: Thread }) {
  const t = totals(thread);
  const rate = t.generationMs ? Math.round(t.completion / (t.generationMs / 1000)) : undefined;
  return (
    <header className="flex items-center gap-4 border-b border-border px-6 py-2 text-[12px]">
      <span className="font-medium">{thread.model}</span>
      <span className="truncate text-muted-foreground">{thread.server}</span>
      <span className="ml-auto flex shrink-0 gap-4 text-muted-foreground">
        <span>{thread.generations.length} generations</span>
        <span>
          prompt {count(t.prompt)} · cached {percent(t.cached, t.prompt)}
        </span>
        <span>
          {count(t.completion)} generated{rate ? ` · ${rate} tok/s` : ""}
        </span>
      </span>
    </header>
  );
}

function Sidebar({ selected, onSelect }: { selected?: string; onSelect: (key: string) => void }) {
  const groups = new Map<string, Session[]>();
  for (const s of sessions) {
    const list = groups.get(s.thread.profile) ?? [];
    list.push(s);
    groups.set(s.thread.profile, list);
  }
  return (
    <nav className="flex w-72 shrink-0 flex-col border-r border-border bg-panel">
      <div className="px-4 py-3 text-[15px] font-semibold tracking-tight text-spark">ognivo</div>
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        {[...groups].map(([profile, list]) => (
          <section key={profile}>
            <h2 className="px-4 pt-3 pb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
              {profile}
            </h2>
            {list.map((s) => {
              const end = s.thread.turns.at(-1)?.end;
              return (
                <button
                  key={s.key}
                  type="button"
                  onClick={() => onSelect(s.key)}
                  className={`block w-full px-4 py-1.5 text-left hover:bg-raised ${s.key === selected ? "bg-raised" : ""}`}
                >
                  <span className="line-clamp-2 leading-5">{s.thread.title}</span>
                  {end !== "answer" && (
                    <span className="text-[11px] text-warn">{ending(end)}</span>
                  )}
                </button>
              );
            })}
          </section>
        ))}
      </div>
    </nav>
  );
}

export function App() {
  // `?session=qwen.long-file` opens that one: screenshots and perf runs pick their session
  const [selected, setSelected] = useState(
    () => new URLSearchParams(location.search).get("session") ?? sessions[0]?.key,
  );
  const thread = sessions.find((s) => s.key === selected)?.thread;
  return (
    <div className="flex h-full">
      <Sidebar selected={selected} onSelect={setSelected} />
      <main className="flex min-w-0 flex-1 flex-col">
        {thread ? (
          <>
            <Hud thread={thread} />
            <ThreadView key={thread.id} thread={thread} />
          </>
        ) : (
          <p className="m-auto text-muted-foreground">No sessions.</p>
        )}
      </main>
    </div>
  );
}
