import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from "react";

import { ThreadView } from "./components/thread.tsx";
import { count, ending, percent } from "./format.ts";
import type { Thread } from "./model.ts";
import { startProbe } from "./perf.ts";
import { play, timeline } from "./replay.ts";
import { type Session, bench, sessions, shifted } from "./sessions.ts";
import { ThreadStore } from "./store.ts";

// `?session=qwen.long-file` opens that one and `?open=all` opens everything (screenshots);
// `?bench=10000&stream=qwen.probing&speed=1` measures the perf budgets of §10 on a thread
// of that many items with that session streaming at its end.
const params = new URLSearchParams(location.search);
const OPEN_ALL = params.get("open") === "all";
const BENCH = Number(params.get("bench")) || undefined;
const SPEEDS = [1, 4, 16];

/** Endpoint, model and what the session cost so far: the start of the Runtime HUD (§10). */
function Hud({
  store,
  replaying,
  speed,
  onReplay,
  onSpeed,
}: {
  store: ThreadStore;
  replaying: boolean;
  speed: number;
  onReplay: () => void;
  onSpeed: (speed: number) => void;
}) {
  const subscribe = useCallback((l: () => void) => store.subscribeList(l), [store]);
  useSyncExternalStore(subscribe, () => store.listVersion);
  const t = store.generations.reduce(
    (a, g) => ({
      prompt: a.prompt + g.prompt,
      cached: a.cached + g.cached,
      completion: a.completion + g.completion,
      ms: a.ms + g.generationMs,
    }),
    { prompt: 0, cached: 0, completion: 0, ms: 0 },
  );
  const rate = t.ms ? Math.round(t.completion / (t.ms / 1000)) : undefined;
  return (
    <header className="flex items-center gap-4 border-b border-border px-6 py-2 text-[12px]">
      <span className="font-medium">{store.info.model}</span>
      <span className="truncate text-muted-foreground">{store.info.server}</span>
      <span className="ml-auto flex shrink-0 items-center gap-4 text-muted-foreground">
        <span>{store.generations.length} generations</span>
        <span>
          prompt {count(t.prompt)} · cached {percent(t.cached, t.prompt)}
        </span>
        <span>
          {count(t.completion)} generated{rate ? ` · ${rate} tok/s` : ""}
        </span>
        <select
          aria-label="Replay speed"
          value={speed}
          onChange={(e) => onSpeed(Number(e.target.value))}
          className="rounded border border-border bg-panel px-1 py-0.5"
        >
          {SPEEDS.map((s) => (
            <option key={s} value={s}>
              {s}×
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={onReplay}
          className="rounded border border-border px-2 py-0.5 text-foreground hover:bg-raised"
        >
          {replaying ? "Stop" : "Replay"}
        </button>
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
                  {end !== "answer" && <span className="text-[11px] text-warn">{ending(end)}</span>}
                </button>
              );
            })}
          </section>
        ))}
      </div>
    </nav>
  );
}

/** Where the user writes; it sends nothing until there is a core to send to. */
function Composer() {
  const [text, setText] = useState("");
  return (
    <div className="border-t border-border px-6 py-3">
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={2}
        placeholder="Ask ognivo (no core connected yet)"
        className="mx-auto block w-full max-w-3xl resize-none rounded-lg border border-border bg-panel px-3 py-2 leading-5 outline-none placeholder:text-muted-foreground focus:border-spark/60"
      />
    </div>
  );
}

function useBench(): { session: Session; store: ThreadStore } | undefined {
  return useMemo(() => {
    if (!BENCH) return undefined;
    const thread = bench(BENCH);
    return { session: { key: "bench", thread }, store: ThreadStore.of(thread, { openAll: OPEN_ALL }) };
  }, []);
}

export function App() {
  const big = useBench();
  const [selected, setSelected] = useState(() => params.get("session") ?? sessions[0]?.key);
  const session = big?.session ?? sessions.find((s) => s.key === selected);
  const whole = useMemo(
    () => big?.store ?? (session ? ThreadStore.of(session.thread, { openAll: OPEN_ALL }) : undefined),
    [big, session],
  );
  const [replay, setReplay] = useState<{ store: ThreadStore; stop: () => void }>();
  const [speed, setSpeed] = useState(() => Number(params.get("speed")) || 1);

  // the bench streams a recorded session at the end of its long thread
  useEffect(() => {
    const stream = sessions.find((s) => s.key === params.get("stream"));
    if (!big || !stream) return;
    const probe = startProbe();
    const thread: Thread = shifted(stream.thread, {
      turns: big.session.thread.turns.length,
      generations: big.session.thread.generations.length,
      prefix: "live.",
    });
    probe.streaming = true;
    return play(big.store, timeline(thread), speed, () => (probe.streaming = false));
  }, [big, speed]);

  const stop = () => {
    replay?.stop();
    setReplay(undefined);
  };
  const toggleReplay = () => {
    if (replay || !session || !whole) return stop();
    const store = new ThreadStore(whole.info, { openAll: OPEN_ALL });
    const done = () => setReplay((r) => (r?.store === store ? { ...r, stop: () => {} } : r));
    setReplay({ store, stop: play(store, timeline(session.thread), speed, done) });
  };

  const store = replay?.store ?? whole;
  return (
    <div className="flex h-full">
      <Sidebar
        selected={selected}
        onSelect={(key) => {
          stop();
          setSelected(key);
        }}
      />
      <main className="flex min-w-0 flex-1 flex-col">
        {store ? (
          <>
            <Hud
              store={store}
              replaying={Boolean(replay)}
              speed={speed}
              onReplay={toggleReplay}
              onSpeed={setSpeed}
            />
            <ThreadView
              key={store.info.id + (replay ? ".replay" : "")}
              store={store}
              atEnd={Boolean(big)}
            />
            <Composer />
          </>
        ) : (
          <p className="m-auto text-muted-foreground">No sessions.</p>
        )}
      </main>
    </div>
  );
}
