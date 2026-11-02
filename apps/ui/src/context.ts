// What the composer counts: the files a session touched and what reading them cost, the
// mention being written, and a message's tokens.

import type { Item } from "./model.ts";

/** A rough count, about four bytes a token, until a core can ask the server's tokenizer. */
export function estimateTokens(text: string): number {
  return Math.ceil(new TextEncoder().encode(text).length / 4);
}

const READ = /^\[read: (.+?) — lines (\d+)-(\d+) of (\d+) lines, .*?~(\d+) tokens\]/;

/** The files a session touched, relative to its project, with what reading each whole
 * cost when it did: the read tool says `~N tokens` of the lines it returned. */
export function sessionFiles(items: Item[], project: string): Map<string, number | undefined> {
  const rel = (p: string) => (project && p.startsWith(`${project}/`) ? p.slice(project.length + 1) : p);
  const files = new Map<string, number | undefined>();
  const see = (p: string) => {
    const path = rel(p.trim());
    if (path && !files.has(path)) files.set(path, undefined);
  };
  for (const i of items) {
    if (i.kind !== "tool" || !i.call || i.result?.outcome !== "ok") continue;
    const c = i.call;
    if (c.name === "Read" || c.name === "Edit" || c.name === "Write") see(c.path);
    if (c.name === "Glob") i.result.text.split("\n").slice(1).forEach(see);
    const whole = c.name === "Read" ? READ.exec(i.result.text) : null;
    if (whole && whole[2] === "1" && whole[3] === whole[4]) files.set(rel(whole[1]!), Number(whole[5]));
  }
  return files;
}

/** `@query` right before the caret, the start of a mention being written. */
export function mentionAt(text: string, caret: number): { start: number; query: string } | undefined {
  const m = /(^|\s)@([\w./-]*)$/.exec(text.slice(0, caret));
  return m ? { start: caret - m[2]!.length - 1, query: m[2]! } : undefined;
}
