import { useCallback, useEffect, useMemo, useState, useSyncExternalStore } from "react";

import { Composer } from "./components/composer.tsx";
import { type Panel, SidePanel } from "./components/panels.tsx";
import { ThreadView } from "./components/thread.tsx";
import { count, ending, percent } from "./format.ts";
import type { Thread } from "./model.ts";
import { startProbe } from "./perf.ts";
import { play, timeline } from "./replay.ts";
import { type Session, bench, sessions, shifted } from "./sessions.ts";
import { ThreadStore } from "./store.ts";

// `?session=qwen.long-file` opens that one, `?open=all` opens everything,
// `?panel=changes`, `approvals` or `inspector` the side panel and `?draft=…` starts the
// composer with that text (screenshots);
// `?bench=10000&stream=qwen.probing&speed=1` measures the perf budgets of §10 on a thread
// of that many items with that session streaming at its end.
const params = new URLSearchParams(location.search);
const OPEN_ALL = params.get("open") === "all";
const BENCH = Number(params.get("bench")) || undefined;
const PANEL = ["changes", "approvals", "inspector"].includes(params.get("panel") ?? "")
  ? (params.get("panel") as Panel)
  : undefined;
const SPEEDS = [1, 4, 16];
const DRAFT = params.get("draft") ?? "";

/** Endpoint, model and what the session cost so far: the start of the Runtime HUD (§10). */
function Hud({
  store,
  replaying,
  speed,
  onReplay,
  onSpeed,
  panel,
  onPanel,
}: {
  store: ThreadStore;
  replaying: boolean;
  speed: number;
  onReplay: () => void;
  onSpeed: (speed: number) => void;
  panel?: Panel;
  onPanel: (panel?: Panel) => void;
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
    // the side panel narrows it: what it costs gives way first, the model and the controls stay
    <header className="@container border-b border-border">
      <div className="flex items-center gap-4 px-6 py-2 text-[12px]">
        <span className="shrink-0 font-medium whitespace-nowrap">{store.info.model}</span>
        <span className="min-w-0 truncate text-muted-foreground">{store.info.server}</span>
        <span className="ml-auto hidden shrink-0 gap-4 text-muted-foreground @4xl:flex">
          <span>{store.generations.length} generations</span>
          <span>
            prompt {count(t.prompt)} · cached {percent(t.cached, t.prompt)}
          </span>
          <span>
            {count(t.completion)} generated{rate ? ` · ${rate} tok/s` : ""}
          </span>
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-2 text-muted-foreground @4xl:ml-0">
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
          {(["changes", "approvals", "inspector"] as const).map((p) => (
            <button
              key={p}
              type="button"
              aria-pressed={panel === p}
              onClick={() => onPanel(panel === p ? undefined : p)}
              className={`rounded border px-2 py-0.5 hover:bg-raised ${panel === p ? "border-spark/60 text-foreground" : "border-border"}`}
            >
              {p === "changes" ? "Changes" : p === "approvals" ? "Approvals" : "Inspector"}
            </button>
          ))}
        </span>
      </div>
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
  const [panel, setPanel] = useState<Panel | undefined>(PANEL);
  // `?generation=3` opens the Inspector at that generation (screenshots)
  const [generation, setGeneration] = useState<number | undefined>(() => {
    const g = params.get("generation");
    return g === null ? undefined : Number(g);
  });
  const inspect = useCallback((g: number) => {
    setGeneration(g);
    setPanel("inspector");
  }, []);

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
          setGeneration(undefined);
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
              panel={panel}
              onPanel={setPanel}
            />
            <ThreadView
              key={store.info.id + (replay ? ".replay" : "")}
              store={store}
              atEnd={Boolean(big)}
              onInspect={inspect}
            />
            <Composer key={store.info.id} store={store} draft={DRAFT} />
          </>
        ) : (
          <p className="m-auto text-muted-foreground">No sessions.</p>
        )}
      </main>
      {store && panel && (
        <SidePanel
          store={store}
          panel={panel}
          onPanel={setPanel}
          generation={generation}
          onGeneration={setGeneration}
        />
      )}
    </div>
  );
}
