// A thread as it grows. Events change its rows in place; a row's component re-renders
// when that row changes and the list when rows are added, at most once a frame. A stream
// at the end of a long thread so touches the rows it changes and nothing else (§10:
// 60 fps on a thread of 10k items).
//
// The events mirror the draft protocol of ognivo-protocol (an item starts, gets deltas,
// completes); the core's frozen events will come in through the same `apply`.

import type { Generation, Item, Thread, ToolItem, ToolResult } from "./model.ts";

export type LiveEvent =
  | { type: "turn_started"; index: number; user: string }
  | { type: "item_started"; item: Item }
  /** More text for a reasoning or message item. */
  | { type: "item_delta"; id: string; text: string }
  /** A call's result; other items complete with nothing more. */
  | { type: "item_completed"; id: string; result?: ToolResult }
  | { type: "generation"; generation: Generation }
  | { type: "turn_completed"; index: number; end: string; why?: string };

export type Row =
  | { kind: "user"; key: string; turn: number; text: string }
  /** `live` while it streams, or runs. */
  | { kind: "item"; key: string; item: Item; open: boolean; live: boolean }
  | { kind: "end"; key: string; turn: number; end: string; why?: string };

export type Info = Omit<Thread, "turns" | "generations">;

type Schedule = (flush: () => void) => void;

const frame: Schedule = (flush) =>
  typeof requestAnimationFrame === "function" ? requestAnimationFrame(flush) : queueMicrotask(flush);

export class ThreadStore {
  readonly info: Info;
  readonly generations: Generation[] = [];
  /** Whether a call starts open: changes do, the rest when everything opens. */
  private readonly opens: (item: Item) => boolean;
  private readonly schedule: Schedule;
  private rows: Row[] = [];
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

  constructor(info: Info, opts: { openAll?: boolean; schedule?: Schedule } = {}) {
    this.info = info;
    this.opens = (i) => Boolean(opts.openAll) || (i.kind === "tool" && i.call?.name === "Edit");
    this.schedule = opts.schedule ?? frame;
  }

  /** A recorded thread, whole. */
  static of(thread: Thread, opts: { openAll?: boolean; schedule?: Schedule } = {}): ThreadStore {
    const { turns, generations, ...info } = thread;
    const store = new ThreadStore(info, opts);
    for (const turn of turns) {
      store.apply({ type: "turn_started", index: turn.index, user: turn.user });
      for (const item of turn.items) {
        store.apply({ type: "item_started", item });
        store.apply({ type: "item_completed", id: item.id });
      }
      if (turn.end) {
        store.apply({ type: "turn_completed", index: turn.index, end: turn.end, why: turn.why });
      }
    }
    store.generations.push(...generations);
    return store;
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
        this.add({ kind: "user", key: `u${e.index}`, turn: e.index, text: e.user });
        return;
      case "item_started":
        this.at.set(e.item.id, this.rows.length);
        this.index(e.item);
        this.add({
          kind: "item",
          key: e.item.id,
          item: e.item,
          open: this.opens(e.item),
          live: e.item.kind === "tool" ? !e.item.result : e.item.kind !== "notice" && e.item.kind !== "void",
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
        const result = e.result;
        this.update(
          e.id,
          (item) => (result && item.kind === "tool" ? { ...item, result } : item),
          false,
        );
        if (result) this.index(this.item(e.id));
        return;
      }
      case "generation":
        this.generations.push(e.generation);
        this.touchList();
        return;
      case "turn_completed":
        this.add({ kind: "end", key: `e${e.index}`, turn: e.index, end: e.end, why: e.why });
        return;
    }
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
      const project = this.info.project;
      const path =
        project && call.path.startsWith(`${project}/`) ? call.path.slice(project.length + 1) : call.path;
      this.files.set(path, [...(this.files.get(path) ?? []), item.id]);
    } else {
      return;
    }
    this.indexChanged = true;
    this.later();
  }

  private add(row: Row): void {
    this.rows.push(row);
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
