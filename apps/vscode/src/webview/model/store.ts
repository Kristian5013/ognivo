// A thread as it grows. Events change its rows in place; a row's component re-renders
// when that row changes and the list when rows are added, at most once a frame. A stream
// at the end of a long thread so touches the rows it changes and nothing else (§10:
// 60 fps on a thread of 10k items).
//
// The events mirror ognivo-protocol (an item starts, gets deltas, completes); the core's
// come in through the same `apply` (live.ts). Generations go by their numbers, which
// items name: a recorded thread numbers them from 0, the core from 1.
//
// Calls that only look (reads, searches, a server's tools, skills) fold into one row when
// they follow each other in a turn, with the thoughts between them: an exploration. A
// thought is a row of its own until the next such call takes it in, so rows only ever go
// at the list's end, where nothing comes after them.
//
// What the turn under way does and is not yet a row (how far the model got, the calls it
// writes, what runs) is the thread's live state, which only the line at the end of the
// conversation listens to: no row re-renders as the model reads, thinks or writes a call.

import type { Progress } from "../../shared/protocol.ts";
import type { Core } from "../../shared/rpc.ts";
import type { Generation, Item, Picture, Thread, ToolItem, ToolResult, Totals } from "./model.ts";
import { relativeTo } from "./paths.ts";

/** A call the model is writing: its tool and each argument as far as it came, in the
 * order they came, by ognivo's names (`path`, `content`, `old`, `new`, `patch`, ...). */
export interface Draft {
  generation: number;
  index: number;
  /** Claude Code's name for the tool, or the model's for one ognivo does not know. */
  tool?: string;
  fields: [string, string][];
}

/** `writing` while the model writes it, `written` once its generation ended and it waits
 * to run. */
export type DraftState = "writing" | "written";

export type LiveEvent =
  | { type: "turn_started"; index: number; user: string; images?: Picture[] }
  | { type: "item_started"; item: Item }
  /** More text for a reasoning or message item. */
  | { type: "item_delta"; id: string; text: string }
  /** A call's result, or the whole item as it ended (the core's events carry it). */
  | { type: "item_completed"; id: string; result?: ToolResult; item?: Item }
  /** A generation ended; `turn` is the turn it was of, where the event says. */
  | { type: "generation"; generation: Generation; turn?: number }
  /** `undelivered`: what the user wrote while it ran that never reached the model. */
  | { type: "turn_completed"; index: number; end: string; why?: string; verification?: string; undelivered?: string[] }
  /** The turns from `turn` on were taken back: out of the conversation. */
  | { type: "rewound"; turn: number }
  /** More of call `index` of generation `generation` as the model writes it. */
  | { type: "draft"; generation: number; index: number; tool?: string; field?: string; text: string }
  /** How far the generation under way got. */
  | { type: "progress"; progress: Progress }
  /** The thread's unreviewed changes changed: some were kept or undone. */
  | { type: "changes" };

/** An item in an exploration, as a row of its own would hold it. */
export interface Step {
  item: Item;
  open?: boolean;
  live: boolean;
}

export type Row =
  | { kind: "user"; key: string; turn: number; text: string; images: Picture[] }
  /** `live` while it streams, or runs; `open` once the user opened or closed it, else
   * what its kind shows (a failed command opens). */
  | { kind: "item"; key: string; item: Item; open?: boolean; live: boolean }
  /** Calls of a turn that only look, one after another, and the thoughts between them:
   * a call first and last. It has the key its first call's row had. */
  | { kind: "steps"; key: string; steps: Step[]; open?: boolean }
  | { kind: "end"; key: string; turn: number; end: string; why?: string; verification?: string; undelivered?: string[] }
  /** Where the user took turns `from` to `to` back. */
  | { kind: "rewound"; key: string; from: number; to: number };

/** How the turn under way stands: what the line at the end of the conversation says. */
export interface Live {
  /** Whether a turn runs. */
  running: boolean;
  /** When the page saw the running turn begin, by its own clock (Unix milliseconds). */
  since?: number;
  /** How far the generation under way got; none between generations. */
  progress?: Progress;
  /** The calls the model writes, or wrote and that wait to run, in the order it wrote
   * them. */
  drafts: readonly { draft: Draft; state: DraftState }[];
  /** The calls that run. */
  calls: readonly ToolItem[];
  /** The model's text that streams: its reasoning, or a message. */
  streaming?: "reasoning" | "message";
  /** A question of the agent's waits for the user (its approval item). */
  asking: boolean;
  /** The conversation is being summed up. */
  compacting: boolean;
}

/** How a turn ended, as the core said. */
export interface TurnEnd {
  turn: number;
  end: string;
  why?: string;
  verification?: string;
  /** What the user wrote while it ran that never reached the model: the next turn's. */
  undelivered: string[];
}

/** What a turn's generations cost, and the last of them. */
export interface TurnCost extends Totals {
  generations: number;
  last: Generation;
}

/** Where a thread lives: the core, and its number there. What the thread's view asks the
 * core (its unreviewed changes, its questions) goes there. */
export interface Source {
  core: Core;
  thread: number;
}

export type Info = Omit<Thread, "turns" | "generations">;

type Schedule = (flush: () => void) => void;

interface Options {
  /** Every call and exploration opens; else each as its kind shows it. */
  openAll?: boolean;
  schedule?: Schedule;
  source?: Source;
}

const frame: Schedule = (flush) =>
  typeof requestAnimationFrame === "function" ? requestAnimationFrame(flush) : queueMicrotask(flush);

/** Whether an item that starts is still going: text that streams, a call that runs, a
 * question that waits, a summary being written. */
function liveOf(item: Item): boolean {
  switch (item.kind) {
    case "tool":
      return !item.result;
    case "subagent":
      return !item.end;
    case "approval":
      return item.allowed === undefined;
    case "compaction":
      return item.replaced === undefined;
    case "reasoning":
    case "message":
      return true;
    default:
      return false;
  }
}

/** Whether a call only looks: it reads, searches, lists, asks a server's tool or loads a
 * skill. Such calls in a row make an exploration. A server's tool is taken to look: the
 * thread does not say which change things, and the line shows each when opened. */
export function explores(item: Item): item is ToolItem {
  if (item.kind !== "tool") return false;
  switch (item.call?.name) {
    case "Read":
    case "Grep":
    case "Glob":
    case "ToolSearch":
    case "Mcp":
    case "Skill":
      return true;
    default:
      return false;
  }
}

const NO_DRAFTS: Live["drafts"] = [];
const NO_CALLS: Live["calls"] = [];

export class ThreadStore {
  readonly info: Info;
  readonly generations: Generation[] = [];
  /** Where the thread lives, for a store of the core's; none for a recorded one. */
  readonly source?: Source;
  private byIndex = new Map<number, Generation>();
  /** The turn of each generation: as its event says, else the turn of its first item. */
  private turnOfGeneration = new Map<number, number>();
  private readonly openAll: boolean;
  private readonly schedule: Schedule;
  private rows: Row[] = [];
  /** The turn each row belongs to, and the last turn begun. */
  private turnOf: number[] = [];
  private turn = 0;
  /** The turns taken back, as ranges. */
  private dropped: [number, number][] = [];
  /** The row of each item, and of each exploration by its key. */
  private at = new Map<string, number>();
  private rowListeners = new Map<number, Set<() => void>>();
  private listListeners = new Set<() => void>();
  private dirty = new Set<number>();
  private changed = false;
  private scheduled = false;
  private version = 0;
  // the files the session changed, with the calls that did it, and the calls that waited
  // for an approval: kept as calls complete, so the side panels never walk the thread
  private files = new Map<string, string[]>();
  private refusals: string[] = [];
  private indexListeners = new Set<() => void>();
  private indexChanged = false;
  private indexVersion = 0;
  // the live state: the turn that runs, the items that go on, the calls being written by
  // `generation:index`, how many calls of each generation started, the newest progress
  private running = false;
  private since?: number;
  private going = new Map<string, Item>();
  private writing = new Map<string, { draft: Draft; state: DraftState }>();
  private started = new Map<number, number>();
  private progress?: Progress;
  private liveSnapshot: Live = {
    running: false,
    drafts: NO_DRAFTS,
    calls: NO_CALLS,
    asking: false,
    compacting: false,
  };
  private liveStale = false;
  private liveChanged = false;
  private liveListeners = new Set<() => void>();
  private ends: TurnEnd[] = [];
  private endListeners = new Set<(end: TurnEnd) => void>();

  constructor(info: Info, opts: Options = {}) {
    this.info = info;
    this.openAll = opts.openAll ?? false;
    this.schedule = opts.schedule ?? frame;
    this.source = opts.source;
  }

  /** A recorded thread, whole. */
  static of(thread: Thread, opts: Options = {}): ThreadStore {
    const { turns, generations, ...info } = thread;
    const store = new ThreadStore(info, opts);
    for (const turn of turns) {
      store.apply({ type: "turn_started", index: turn.index, user: turn.user, images: turn.images });
      for (const item of turn.items) {
        store.apply({ type: "item_started", item });
        store.apply({ type: "item_completed", id: item.id });
      }
      if (turn.end) {
        store.apply({ type: "turn_completed", index: turn.index, end: turn.end, why: turn.why, verification: turn.verification });
      }
    }
    for (const g of generations) store.addGeneration(g);
    // a recording runs nothing, even where its last turn has no end
    store.stop();
    return store;
  }

  /** The generation numbered `index`. */
  generation(index: number): Generation | undefined {
    return this.byIndex.get(index);
  }

  private addGeneration(g: Generation, turn?: number): void {
    this.generations.push(g);
    this.byIndex.set(g.index, g);
    if (turn !== undefined) this.turnOfGeneration.set(g.index, turn);
  }

  /** What turn `turn`'s generations cost; none when no generation of it is known. */
  turnCost(turn: number): TurnCost | undefined {
    let cost: TurnCost | undefined;
    for (const g of this.generations) {
      if (this.turnOfGeneration.get(g.index) !== turn) continue;
      cost ??= { prompt: 0, cached: 0, completion: 0, generationMs: 0, generations: 0, last: g };
      cost.prompt += g.prompt;
      cost.cached += g.cached;
      cost.completion += g.completion;
      cost.generationMs += g.generationMs;
      cost.generations++;
      if (g.index >= cost.last.index) cost.last = g;
    }
    return cost;
  }

  get length(): number {
    return this.rows.length;
  }

  /** Changes when rows are added or a generation ends: what the list and the HUD show. */
  get listVersion(): number {
    return this.version;
  }

  row(index: number): Row | undefined {
    return this.rows[index];
  }

  key(index: number): string {
    return this.rows[index]?.key ?? String(index);
  }

  apply(e: LiveEvent): void {
    switch (e.type) {
      case "turn_started":
        this.turn = e.index;
        this.running = true;
        this.since = Date.now();
        this.progress = undefined;
        this.writing.clear();
        this.going.clear();
        this.touchLive();
        this.add({ kind: "user", key: `u${e.index}`, turn: e.index, text: e.user, images: e.images ?? [] });
        return;
      case "item_started": {
        const item = e.item;
        if (item.generation !== undefined && !this.turnOfGeneration.has(item.generation)) {
          this.turnOfGeneration.set(item.generation, this.turn);
        }
        this.begun(item);
        const live = liveOf(item);
        if (!(explores(item) && this.fold(item, live))) {
          this.at.set(item.id, this.rows.length);
          this.index(item);
          this.add({ kind: "item", key: item.id, item, open: this.openAll || undefined, live });
        }
        this.track(item, live);
        return;
      }
      case "item_delta":
        this.update(e.id, (item) =>
          item.kind === "reasoning" || item.kind === "message" ? { ...item, text: item.text + e.text } : item,
        );
        return;
      case "item_completed": {
        const { result, item: whole } = e;
        this.update(e.id, (item) => whole ?? (result && item.kind === "tool" ? { ...item, result } : item), false);
        if (result || (whole?.kind === "tool" && whole.result)) this.index(this.item(e.id));
        return;
      }
      case "generation":
        this.addGeneration(e.generation, e.turn ?? this.turn);
        this.endDrafts(e.generation.index, e.generation.outcome === "calls");
        if (this.progress && this.progress.generation <= e.generation.index) {
          this.progress = undefined;
          this.touchLive();
        }
        this.touchList();
        return;
      case "turn_completed": {
        this.stop();
        const end: TurnEnd = {
          turn: e.index,
          end: e.end,
          why: e.why,
          verification: e.verification,
          undelivered: e.undelivered ?? [],
        };
        this.ends.push(end);
        this.add({
          kind: "end",
          key: `e${e.index}`,
          turn: e.index,
          end: e.end,
          why: e.why,
          verification: e.verification,
          ...(end.undelivered.length ? { undelivered: end.undelivered } : {}),
        });
        // the turn's changes are all in: what waits for a review may differ
        this.changesTouched();
        for (const l of this.endListeners) l(end);
        return;
      }
      case "draft":
        this.draft(e);
        return;
      case "progress":
        // a word that comes after its turn or its generation ended says nothing of either
        if (!this.running || this.byIndex.has(e.progress.generation)) return;
        this.progress = e.progress;
        this.touchLive();
        return;
      case "changes":
        this.changesTouched();
        return;
      case "rewound": {
        const to = this.turn;
        if (e.turn > to) return;
        this.dropped.push([e.turn, to]);
        this.turnOf.forEach((t, i) => {
          if (t >= e.turn && t <= to) this.touch(i);
        });
        this.add({ kind: "rewound", key: `r${e.turn}-${to}`, from: e.turn, to });
        this.changesTouched();
        return;
      }
    }
  }

  /** The live state of a turn that ends, or of a recording: nothing runs. */
  private stop(): void {
    this.running = false;
    this.since = undefined;
    this.progress = undefined;
    this.writing.clear();
    this.going.clear();
    this.touchLive();
  }

  /** An item began: a call takes its draft's place among those being written, and a
   * generation that ran nothing ends its drafts. */
  private begun(item: Item): void {
    if (item.kind === "void" && item.generation !== undefined) this.endDrafts(item.generation, false);
    if (item.kind !== "tool" || item.generation === undefined) return;
    const n = this.started.get(item.generation) ?? 0;
    this.started.set(item.generation, n + 1);
    if (this.writing.delete(`${item.generation}:${n}`)) this.touchLive();
  }

  /** A call that only looks into the exploration at the turn's end, with the thoughts
   * since its last call; false when the turn's last rows are not one. */
  private fold(item: ToolItem, live: boolean): boolean {
    let j = this.rows.length - 1;
    while (j >= 0 && this.turnOf[j] === this.turn && isThought(this.rows[j]!)) j--;
    const head = this.rows[j];
    if (!head || this.turnOf[j] !== this.turn) return false;
    let steps: Step[];
    if (head.kind === "steps") steps = head.steps;
    else if (head.kind === "item" && explores(head.item)) steps = [{ item: head.item, open: head.open, live: head.live }];
    else return false;
    const thoughts = this.rows.slice(j + 1).map((r) => {
      const t = r as Extract<Row, { kind: "item" }>;
      return { item: t.item, open: t.open, live: t.live };
    });
    const added: Step[] = [...thoughts, { item, open: this.openAll || undefined, live }];
    this.rows[j] = {
      kind: "steps",
      key: head.key,
      steps: [...steps, ...added],
      open: head.kind === "steps" ? head.open : this.openAll || undefined,
    };
    for (const s of added) this.at.set(s.item.id, j);
    this.at.set(head.key, j);
    // the thoughts' rows go: nothing came after them
    this.cut(j + 1);
    this.index(item);
    this.touch(j);
    return true;
  }

  /** Rows from `from` on out of the list: only ever its last ones. */
  private cut(from: number): void {
    if (from >= this.rows.length) return;
    this.rows.length = from;
    this.turnOf.length = from;
    for (const i of this.dirty) if (i >= from) this.dirty.delete(i);
    this.touchList();
  }

  /** More of a call being written: the live state's, until its item starts. */
  private draft(e: Extract<LiveEvent, { type: "draft" }>): void {
    const key = `${e.generation}:${e.index}`;
    const have = this.writing.get(key);
    if (!have) {
      // what streamed of the generation before its call ends where the call begins
      this.settle(e.generation);
      const fields: [string, string][] = e.field ? [[e.field, e.text]] : [];
      this.writing.set(key, { draft: { generation: e.generation, index: e.index, tool: e.tool, fields }, state: "writing" });
    } else {
      const fields = have.draft.fields;
      const next = [...fields];
      if (e.field && e.text) {
        const i = fields.findIndex(([f]) => f === e.field);
        if (i < 0) next.push([e.field, e.text]);
        else next[i] = [e.field, fields[i]![1] + e.text];
      }
      this.writing.set(key, { ...have, draft: { ...have.draft, tool: have.draft.tool ?? e.tool, fields: next } });
    }
    this.touchLive();
  }

  /** Generation `generation` ended: its drafts wait to run, or will not run. */
  private endDrafts(generation: number, run: boolean): void {
    let any = false;
    for (const [key, d] of this.writing) {
      if (!key.startsWith(`${generation}:`)) continue;
      if (run) this.writing.set(key, { ...d, state: "written" });
      else this.writing.delete(key);
      any = true;
    }
    if (any) this.touchLive();
  }

  /** What streamed of a generation before its first call ends where the call begins. */
  private settle(generation: number): void {
    for (let i = this.rows.length - 1; i >= 0; i--) {
      const row = this.rows[i]!;
      if (row.kind !== "item" || row.item.generation !== generation) return;
      if (row.live && (row.item.kind === "reasoning" || row.item.kind === "message")) {
        this.rows[i] = { ...row, live: false };
        this.track(row.item, false);
        this.touch(i);
      }
    }
  }

  /** Whether row `index` belongs to a turn the user took back. */
  isDropped(index: number): boolean {
    const t = this.turnOf[index];
    return this.rows[index]?.kind !== "rewound" && t !== undefined && this.dropped.some(([a, b]) => t >= a && t <= b);
  }

  /** Opens or closes item `id`'s row, or its line in an exploration. */
  setOpen(id: string, open: boolean): void {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    if (i === undefined || !row) return;
    if (row.kind === "item") {
      if (row.item.id !== id || row.open === open) return;
      this.rows[i] = { ...row, open };
    } else if (row.kind === "steps") {
      const k = row.steps.findIndex((s) => s.item.id === id);
      if (k < 0 || row.steps[k]!.open === open) return;
      const steps = [...row.steps];
      steps[k] = { ...steps[k]!, open };
      this.rows[i] = { ...row, steps };
    } else {
      return;
    }
    this.touch(i);
  }

  /** Opens or closes the exploration keyed `key`. */
  openSteps(key: string, open: boolean): void {
    const i = this.at.get(key);
    const row = i === undefined ? undefined : this.rows[i];
    if (i === undefined || row?.kind !== "steps" || row.key !== key || row.open === open) return;
    this.rows[i] = { ...row, open };
    this.touch(i);
  }

  subscribeRow(index: number, listener: () => void): () => void {
    let set = this.rowListeners.get(index);
    if (!set) this.rowListeners.set(index, (set = new Set()));
    set.add(listener);
    return () => {
      set.delete(listener);
      if (!set.size) this.rowListeners.delete(index);
    };
  }

  /** Every item, in order (the composer's file list, on demand). */
  items(): Item[] {
    return this.rows.flatMap((r) => (r.kind === "item" ? [r.item] : r.kind === "steps" ? r.steps.map((s) => s.item) : []));
  }

  /** The items generation `generation` made, in order (the Inspector's, on demand). */
  itemsOf(generation: number): Item[] {
    return this.items().filter((i) => i.generation === generation);
  }

  /** Changes when a file changes or a call waits for an approval, and when what waits for
   * a review may have changed: a turn ended or was taken back, changes were kept or
   * undone. */
  get changesVersion(): number {
    return this.indexVersion;
  }

  /** The files the session changed, in the order it first changed them, each with the
   * calls that did. Paths inside the project are relative to it. */
  changedFiles(): { path: string; calls: ToolItem[] }[] {
    return [...this.files].map(([path, ids]) => ({
      path,
      calls: ids.map((id) => this.item(id)).filter((i): i is ToolItem => i?.kind === "tool"),
    }));
  }

  /** The calls that needed an approval and did not get it. */
  refusedCalls(): ToolItem[] {
    return this.refusals.map((id) => this.item(id)).filter((i): i is ToolItem => i?.kind === "tool");
  }

  subscribeChanges(listener: () => void): () => void {
    this.indexListeners.add(listener);
    return () => this.indexListeners.delete(listener);
  }

  subscribeList(listener: () => void): () => void {
    this.listListeners.add(listener);
    return () => this.listListeners.delete(listener);
  }

  /** How the turn under way stands. The same object until something of it changes. */
  get live(): Live {
    if (this.liveStale) {
      this.liveStale = false;
      this.liveSnapshot = this.snapshot();
    }
    return this.liveSnapshot;
  }

  /** Tells, once a frame, that the live state changed: the model's progress, the calls it
   * writes, what runs. No row's listener hears of it. */
  subscribeLive(listener: () => void): () => void {
    this.liveListeners.add(listener);
    return () => this.liveListeners.delete(listener);
  }

  /** How the last turn ended; none before a turn ended. */
  get lastEnd(): TurnEnd | undefined {
    return this.ends.at(-1);
  }

  /** Hears each turn that ends from now on, as it ends: what the user wrote while it ran
   * and the model never got is in `undelivered`. */
  onTurnEnd(listener: (end: TurnEnd) => void): () => void {
    this.endListeners.add(listener);
    return () => this.endListeners.delete(listener);
  }

  private snapshot(): Live {
    const calls: ToolItem[] = [];
    let streaming: Live["streaming"];
    let asking = false;
    let compacting = false;
    for (const item of this.going.values()) {
      switch (item.kind) {
        case "tool":
          calls.push(item);
          break;
        case "reasoning":
        case "message":
          streaming = item.kind;
          break;
        case "approval":
          asking = true;
          break;
        case "compaction":
          compacting = true;
          break;
      }
    }
    return {
      running: this.running,
      since: this.since,
      progress: this.progress,
      drafts: this.writing.size ? [...this.writing.values()] : NO_DRAFTS,
      calls: calls.length ? calls : NO_CALLS,
      streaming,
      asking,
      compacting,
    };
  }

  private item(id: string): Item | undefined {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    if (row?.kind === "item") return row.item.id === id ? row.item : undefined;
    if (row?.kind === "steps") return row.steps.find((s) => s.item.id === id)?.item;
    return undefined;
  }

  /** A finished call into the side panels' indexes: a change that applied, or a refusal. */
  private index(item: Item | undefined): void {
    if (item?.kind !== "tool" || !item.result) return;
    const { call, result } = item;
    if (result.outcome === "refused") {
      this.refusals.push(item.id);
    } else if (
      (call?.name === "Edit" && result.text.startsWith("[edit: applied")) ||
      (call?.name === "Write" && result.text.startsWith("[write: "))
    ) {
      const path = relativeTo(this.info.project, call.path);
      this.files.set(path, [...(this.files.get(path) ?? []), item.id]);
    } else {
      return;
    }
    this.changesTouched();
  }

  private add(row: Row): void {
    this.rows.push(row);
    this.turnOf.push(this.turn);
    this.touchList();
  }

  private update(id: string, change: (item: Item) => Item, live?: boolean): void {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    if (i === undefined || !row) return;
    if (row.kind === "item") {
      if (row.item.id !== id) return;
      const item = change(row.item);
      if (item === row.item && (live === undefined || live === row.live)) return;
      this.rows[i] = { ...row, item, live: live ?? row.live };
      this.track(item, live ?? row.live);
    } else if (row.kind === "steps") {
      const k = row.steps.findIndex((s) => s.item.id === id);
      const step = row.steps[k];
      if (!step) return;
      const item = change(step.item);
      if (item === step.item && (live === undefined || live === step.live)) return;
      const steps = [...row.steps];
      steps[k] = { ...step, item, live: live ?? step.live };
      this.rows[i] = { ...row, steps };
      this.track(item, live ?? step.live);
    } else {
      return;
    }
    this.touch(i);
  }

  /** Keeps the items that go on, for the live state: it changes when one begins or ends. */
  private track(item: Item, live: boolean): void {
    if (live) {
      if (!this.going.has(item.id)) this.touchLive();
      this.going.set(item.id, item);
    } else if (this.going.delete(item.id)) {
      this.touchLive();
    }
  }

  private touch(i: number): void {
    this.dirty.add(i);
    this.later();
  }

  private touchList(): void {
    this.changed = true;
    this.later();
  }

  private touchLive(): void {
    this.liveStale = true;
    this.liveChanged = true;
    this.later();
  }

  private changesTouched(): void {
    this.indexChanged = true;
    this.later();
  }

  private later(): void {
    if (this.scheduled) return;
    this.scheduled = true;
    this.schedule(() => this.flush());
  }

  /** Tells what changed since the last frame. */
  flush(): void {
    this.scheduled = false;
    const dirty = [...this.dirty];
    this.dirty.clear();
    for (const i of dirty) for (const l of this.rowListeners.get(i) ?? []) l();
    if (this.changed) {
      this.changed = false;
      this.version++;
      for (const l of this.listListeners) l();
    }
    if (this.liveChanged) {
      this.liveChanged = false;
      for (const l of this.liveListeners) l();
    }
    if (this.indexChanged) {
      this.indexChanged = false;
      this.indexVersion++;
      for (const l of this.indexListeners) l();
    }
  }
}

/** A row of the model's reasoning alone: a thought an exploration takes in. */
function isThought(row: Row): boolean {
  return row.kind === "item" && row.item.kind === "reasoning";
}
