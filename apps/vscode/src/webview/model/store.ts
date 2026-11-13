// A thread as it grows. Events change its rows in place; a row's component re-renders
// when that row changes and the list when rows are added, at most once a frame. A stream
// at the end of a long thread so touches the rows it changes and nothing else (§10:
// 60 fps on a thread of 10k items).
//
// The events mirror ognivo-protocol (an item starts, gets deltas, completes); the core's
// come in through the same `apply` (live.ts). Generations go by their numbers, which
// items name: a recorded thread numbers them from 0, the core from 1.
//
// A call the model is writing is a draft row until its item starts, which takes the
// draft's place: the generation's calls start in the order they were written. A draft
// whose generation ran nothing stays, as not run.

import type { Generation, Item, Picture, Thread, ToolItem, ToolResult } from "./model.ts";
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
 * to run, `void` when it will not run. */
export type DraftState = "writing" | "written" | "void";

export type LiveEvent =
  | { type: "turn_started"; index: number; user: string; images?: Picture[] }
  | { type: "item_started"; item: Item }
  /** More text for a reasoning or message item. */
  | { type: "item_delta"; id: string; text: string }
  /** A call's result, or the whole item as it ended (the core's events carry it). */
  | { type: "item_completed"; id: string; result?: ToolResult; item?: Item }
  | { type: "generation"; generation: Generation }
  | { type: "turn_completed"; index: number; end: string; why?: string; verification?: string }
  /** The turns from `turn` on were taken back: out of the conversation. */
  | { type: "rewound"; turn: number }
  /** More of call `index` of generation `generation` as the model writes it. */
  | { type: "draft"; generation: number; index: number; tool?: string; field?: string; text: string };

export type Row =
  | { kind: "user"; key: string; turn: number; text: string; images: Picture[] }
  /** `live` while it streams, or runs; `open` once the user opened or closed it, else
   * what its kind shows (a failed command opens, a diff shows its first lines). */
  | { kind: "item"; key: string; item: Item; open?: boolean; live: boolean }
  | { kind: "end"; key: string; turn: number; end: string; why?: string; verification?: string }
  /** Where the user took turns `from` to `to` back. */
  | { kind: "rewound"; key: string; from: number; to: number }
  /** A call being written, until its item takes the row. */
  | { kind: "draft"; key: string; draft: Draft; state: DraftState };

export type Info = Omit<Thread, "turns" | "generations">;

type Schedule = (flush: () => void) => void;

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

export class ThreadStore {
  readonly info: Info;
  readonly generations: Generation[] = [];
  private byIndex = new Map<number, Generation>();
  /** Whether every call starts open; else each as its kind shows it. */
  private readonly opens: (item: Item) => boolean | undefined;
  private readonly schedule: Schedule;
  private rows: Row[] = [];
  /** The turn each row belongs to, and the last turn begun. */
  private turnOf: number[] = [];
  private turn = 0;
  /** The turns taken back, as ranges. */
  private dropped: [number, number][] = [];
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
  /** The drafts that may still become calls, by `generation:index`, and how many calls of
   * each generation have started. */
  private drafts = new Map<string, number>();
  private started = new Map<number, number>();

  constructor(info: Info, opts: { openAll?: boolean; schedule?: Schedule } = {}) {
    this.info = info;
    this.opens = () => (opts.openAll ? true : undefined);
    this.schedule = opts.schedule ?? frame;
  }

  /** A recorded thread, whole. */
  static of(thread: Thread, opts: { openAll?: boolean; schedule?: Schedule } = {}): ThreadStore {
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
    return store;
  }

  /** The generation numbered `index`. */
  generation(index: number): Generation | undefined {
    return this.byIndex.get(index);
  }

  private addGeneration(g: Generation): void {
    this.generations.push(g);
    this.byIndex.set(g.index, g);
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
        this.add({ kind: "user", key: `u${e.index}`, turn: e.index, text: e.user, images: e.images ?? [] });
        return;
      case "item_started":
        if (this.takeDraft(e.item)) return;
        if (e.item.kind === "void" && e.item.generation !== undefined) this.endDrafts(e.item.generation, false);
        this.at.set(e.item.id, this.rows.length);
        this.index(e.item);
        this.add({
          kind: "item",
          key: e.item.id,
          item: e.item,
          open: this.opens(e.item),
          live: liveOf(e.item),
        });
        return;
      case "item_delta":
        this.update(e.id, (item) =>
          item.kind === "reasoning" || item.kind === "message"
            ? { ...item, text: item.text + e.text }
            : item,
        );
        return;
      case "item_completed": {
        const { result, item: whole } = e;
        this.update(
          e.id,
          (item) => whole ?? (result && item.kind === "tool" ? { ...item, result } : item),
          false,
        );
        if (result || (whole?.kind === "tool" && whole.result)) this.index(this.item(e.id));
        return;
      }
      case "generation":
        this.addGeneration(e.generation);
        this.endDrafts(e.generation.index, e.generation.outcome === "calls");
        this.touchList();
        return;
      case "turn_completed":
        for (const g of new Set([...this.drafts.keys()].map((k) => Number(k.split(":")[0])))) this.endDrafts(g, false);
        this.add({ kind: "end", key: `e${e.index}`, turn: e.index, end: e.end, why: e.why, verification: e.verification });
        return;
      case "draft":
        this.draft(e);
        return;
      case "rewound": {
        const to = this.turn;
        if (e.turn > to) return;
        this.dropped.push([e.turn, to]);
        this.turnOf.forEach((t, i) => {
          if (t >= e.turn && t <= to) this.touch(i);
        });
        this.add({ kind: "rewound", key: `r${e.turn}-${to}`, from: e.turn, to });
        return;
      }
    }
  }

  /** More of a call being written: a draft row of its own, the first time. */
  private draft(e: Extract<LiveEvent, { type: "draft" }>): void {
    const key = `${e.generation}:${e.index}`;
    const at = this.drafts.get(key);
    const row = at === undefined ? undefined : this.rows[at];
    if (at === undefined || row?.kind !== "draft") {
      this.settle(e.generation);
      this.drafts.set(key, this.rows.length);
      const fields: [string, string][] = e.field ? [[e.field, e.text]] : [];
      this.add({
        kind: "draft",
        key: `d${this.rows.length}`,
        draft: { generation: e.generation, index: e.index, tool: e.tool, fields },
        state: "writing",
      });
      return;
    }
    const fields = row.draft.fields;
    const next = [...fields];
    if (e.field && e.text) {
      const i = fields.findIndex(([f]) => f === e.field);
      if (i < 0) next.push([e.field, e.text]);
      else next[i] = [e.field, fields[i]![1] + e.text];
    }
    this.rows[at] = { ...row, draft: { ...row.draft, tool: row.draft.tool ?? e.tool, fields: next } };
    this.touch(at);
  }

  /** A call's item into the row of its draft, if it has one. */
  private takeDraft(item: Item): boolean {
    if (item.kind !== "tool" || item.generation === undefined) return false;
    const n = this.started.get(item.generation) ?? 0;
    this.started.set(item.generation, n + 1);
    const key = `${item.generation}:${n}`;
    const at = this.drafts.get(key);
    const row = at === undefined ? undefined : this.rows[at];
    if (at === undefined || row?.kind !== "draft") return false;
    this.drafts.delete(key);
    this.at.set(item.id, at);
    this.index(item);
    this.rows[at] = { kind: "item", key: row.key, item, open: this.opens(item), live: !item.result };
    this.touch(at);
    return true;
  }

  /** Generation `generation` ended: its drafts wait to run, or will not run. */
  private endDrafts(generation: number, run: boolean): void {
    for (const [key, at] of this.drafts) {
      if (!key.startsWith(`${generation}:`)) continue;
      const row = this.rows[at];
      if (row?.kind !== "draft") continue;
      if (!run) this.drafts.delete(key);
      this.rows[at] = { ...row, state: run ? "written" : "void" };
      this.touch(at);
    }
  }

  /** What streamed of a generation before its first call ends where the call begins. */
  private settle(generation: number): void {
    for (let i = this.rows.length - 1; i >= 0; i--) {
      const row = this.rows[i]!;
      if (row.kind !== "item" || row.item.generation !== generation) return;
      if (row.live && (row.item.kind === "reasoning" || row.item.kind === "message")) {
        this.rows[i] = { ...row, live: false };
        this.touch(i);
      }
    }
  }

  /** Whether row `index` belongs to a turn the user took back. */
  isDropped(index: number): boolean {
    const t = this.turnOf[index];
    return this.rows[index]?.kind !== "rewound" && t !== undefined && this.dropped.some(([a, b]) => t >= a && t <= b);
  }

  setOpen(id: string, open: boolean): void {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    if (i === undefined || row?.kind !== "item" || row.open === open) return;
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
    return this.rows.flatMap((r) => (r.kind === "item" ? [r.item] : []));
  }

  /** The items generation `generation` made, in order (the Inspector's, on demand). */
  itemsOf(generation: number): Item[] {
    return this.rows.flatMap((r) => (r.kind === "item" && r.item.generation === generation ? [r.item] : []));
  }

  /** Changes when a file changes or a call waits for an approval. */
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

  private item(id: string): Item | undefined {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    return row?.kind === "item" ? row.item : undefined;
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
    this.indexChanged = true;
    this.later();
  }

  private add(row: Row): void {
    this.rows.push(row);
    this.turnOf.push(this.turn);
    this.touchList();
  }

  private update(id: string, change: (item: Item) => Item, live?: boolean): void {
    const i = this.at.get(id);
    const row = i === undefined ? undefined : this.rows[i];
    if (i === undefined || row?.kind !== "item") return;
    const item = change(row.item);
    if (item === row.item && (live === undefined || live === row.live)) return;
    this.rows[i] = { ...row, item, live: live ?? row.live };
    this.touch(i);
  }

  private touch(i: number): void {
    this.dirty.add(i);
    this.later();
  }

  private touchList(): void {
    this.changed = true;
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
    if (this.indexChanged) {
      this.indexChanged = false;
      this.indexVersion++;
      for (const l of this.indexListeners) l();
    }
  }
}
