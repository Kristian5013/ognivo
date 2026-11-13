// The lines of a change as the panel shows them, file by file: an edit's own lines with
// what it matched around them, a written file whole, a patch of gpt-oss's `apply_patch`
// (V4A), or the core's hunks of what a call changed (`changes` of its item).

import type { FileChange, ToolItem } from "./model.ts";

export interface DiffLine {
  kind: "add" | "del" | "ctx" | "gap";
  text: string;
}

export interface FileDiff {
  path: string;
  created: boolean;
  deleted: boolean;
  lines: DiffLine[];
  added: number;
  removed: number;
  /** The first line it changed, in the file as it is now; unknown for a recorded call. */
  at?: number;
}

function lines(text: string): string[] {
  if (!text) return [];
  return text.replace(/\n$/, "").split("\n");
}

function counted(path: string, list: DiffLine[], over: Partial<FileDiff> = {}): FileDiff {
  return {
    path,
    created: false,
    deleted: false,
    lines: list,
    added: list.filter((l) => l.kind === "add").length,
    removed: list.filter((l) => l.kind === "del").length,
    ...over,
  };
}

/** What changed between two texts, line by line: the lines they share stay as context, by
 * a longest common subsequence; texts too long for one are all taken out, then all put in. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = lines(before);
  const b = lines(after);
  if (a.length * b.length > 250_000) {
    return [...a.map((text) => ({ kind: "del" as const, text })), ...b.map((text) => ({ kind: "add" as const, text }))];
  }
  // lcs[i][j]: the common lines of a[i..] and b[j..]
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i]![j] = a[i] === b[j] ? lcs[i + 1]![j + 1]! + 1 : Math.max(lcs[i + 1]![j]!, lcs[i]![j + 1]!);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      out.push({ kind: "ctx", text: a[i]! });
      i++;
      j++;
    } else if (j < b.length && (i === a.length || lcs[i]![j + 1]! >= lcs[i + 1]![j]!)) {
      out.push({ kind: "add", text: b[j]! });
      j++;
    } else {
      out.push({ kind: "del", text: a[i]! });
      i++;
    }
  }
  // the lines a deletion took out come before those an addition put in their place
  for (let k = 1; k < out.length; k++) {
    if (out[k]!.kind === "del" && out[k - 1]!.kind === "add") {
      let s = k - 1;
      while (s > 0 && out[s - 1]!.kind === "add") s--;
      const [del] = out.splice(k, 1);
      out.splice(s, 0, del!);
    }
  }
  return out;
}

/** The core's hunks of a file: what each took out and put in, a gap between them. */
export function fromHunks(change: FileChange): FileDiff {
  const out: DiffLine[] = [];
  change.hunks.forEach((h, n) => {
    if (n > 0) out.push({ kind: "gap", text: "" });
    for (const text of h.removed) out.push({ kind: "del", text });
    for (const text of h.added) out.push({ kind: "add", text });
  });
  return counted(change.path, out, { created: change.created, at: change.hunks[0]?.newStart || undefined });
}

/** gpt-oss's patch: each file it adds, updates or deletes, with the lines it gives. */
export function fromPatch(patch: string): FileDiff[] {
  const files: FileDiff[] = [];
  let file: { path: string; created: boolean; deleted: boolean; lines: DiffLine[] } | undefined;
  const close = () => {
    if (file) files.push(counted(file.path, file.lines, { created: file.created, deleted: file.deleted }));
    file = undefined;
  };
  for (const line of patch.split("\n")) {
    const head = /^\*\*\* (Add|Update|Delete) File: (.+)$/.exec(line);
    if (head) {
      close();
      file = { path: head[2]!.trim(), created: head[1] === "Add", deleted: head[1] === "Delete", lines: [] };
      continue;
    }
    if (!file || line.startsWith("*** ")) continue;
    if (line.startsWith("@@")) {
      if (file.lines.length) file.lines.push({ kind: "gap", text: "" });
    } else if (line.startsWith("+")) {
      file.lines.push({ kind: "add", text: line.slice(1) });
    } else if (line.startsWith("-")) {
      file.lines.push({ kind: "del", text: line.slice(1) });
    } else if (line.startsWith(" ")) {
      file.lines.push({ kind: "ctx", text: line.slice(1) });
    }
  }
  close();
  return files;
}

/** The files a call changes, or would: from its arguments while it runs and for a recorded
 * session, with the core's hunks once they are there. */
export function diffsOf(item: ToolItem): FileDiff[] {
  const call = item.call;
  const changes = item.changes ?? [];
  const at = (path: string) => changes.find((c) => c.path === path || path.endsWith(`/${c.path}`))?.hunks[0]?.newStart;
  switch (call?.name) {
    case "Edit": {
      // its own lines show what it matched around the change; a replacement everywhere
      // counts as many times as the core found it
      const own = counted(call.path, lineDiff(call.old, call.new), { at: at(call.path) });
      if (call.replaceAll && changes.length) {
        const all = fromHunks(changes[0]!);
        return [{ ...own, added: all.added, removed: all.removed }];
      }
      return [own];
    }
    case "Write": {
      const change = changes[0];
      if (change && !change.created) return [{ ...fromHunks(change), path: call.path }];
      return [counted(call.path, lines(call.content).map((text) => ({ kind: "add", text })), { created: true })];
    }
    case "ApplyPatch":
      return fromPatch(call.patch);
    default:
      return changes.map(fromHunks);
  }
}

/** A path's file name, and the directory it is in. */
export function splitPath(path: string): { name: string; dir: string } {
  const p = path.replace(/\\/g, "/");
  const cut = p.lastIndexOf("/");
  return cut < 0 ? { name: p, dir: "" } : { name: p.slice(cut + 1), dir: p.slice(0, cut) };
}
