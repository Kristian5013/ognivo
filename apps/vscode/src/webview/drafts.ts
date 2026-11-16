// What a composer keeps of the message being written while it does not show (another thread
// on show, the settings) and across a reload of the page ("Connect again"), by its draft's
// key: the words and the chips in the page's session storage, which a reload keeps; the
// images, and the chips the user took away, in the page's memory only, as a message's
// images may be more than that storage holds. A message that did not go when the core went
// away (queued, steered and never read, back from a send that failed) goes into its
// composer's draft, before what the user wrote since, so that "Connect again" gives it back.

import type { ContextChip } from "../shared/messages.ts";
import { chipKey } from "./model/context.ts";
import type { Attachment } from "./model/images.ts";
import type { Message } from "./queue.ts";

const DRAFTS = "ognivo.draft";

/** A message being written, as its composer's draft keeps it. */
export interface Draft {
  text: string;
  chips: ContextChip[];
}

/** What the page's memory keeps of a message being written: what goes with it, and the
 * chips the user took away from it, with the file they were of. */
export interface Held {
  chips: ContextChip[];
  images: Attachment[];
  dismissed: ReadonlySet<string>;
  file: string | undefined;
}

const held = new Map<string, Held>();

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** The page's session storage, where it has one. */
function storage(): Store | undefined {
  try {
    return globalThis.sessionStorage;
  } catch {
    return undefined;
  }
}

function read(key: string): string | null {
  try {
    return storage()?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function write(key: string, value: string | null): void {
  try {
    if (value === null) storage()?.removeItem(key);
    else storage()?.setItem(key, value);
  } catch {
    // the draft lasts as long as the page shows it
  }
}

/** The words of composer `key`'s draft. */
export function loadText(key: string): string {
  return read(`${DRAFTS}.${key}`) ?? "";
}

export function saveText(key: string, text: string): void {
  write(`${DRAFTS}.${key}`, text);
}

/** The chips of composer `key`'s draft: what the page holds, else what the storage kept
 * across a reload. */
export function loadChips(key: string): ContextChip[] {
  const have = held.get(key);
  if (have) return have.chips;
  try {
    const kept: unknown = JSON.parse(read(`${DRAFTS}.${key}.chips`) ?? "[]");
    return Array.isArray(kept) ? kept.filter((c): c is ContextChip => typeof c === "object" && c !== null && typeof c.kind === "string") : [];
  } catch {
    return [];
  }
}

export function saveChips(key: string, chips: readonly ContextChip[]): void {
  write(`${DRAFTS}.${key}.chips`, chips.length ? JSON.stringify(chips) : null);
}

/** What the page holds of composer `key`'s message besides its words. */
export function heldOf(key: string): Held | undefined {
  return held.get(key);
}

export function hold(key: string, h: Held): void {
  if (h.chips.length || h.images.length || h.dismissed.size) held.set(key, h);
  else held.delete(key);
}

/** Composer `key`'s draft goes: sent, or its thread deleted. */
export function forgetDraft(key: string): void {
  held.delete(key);
  write(`${DRAFTS}.${key}`, null);
  write(`${DRAFTS}.${key}.chips`, null);
}

/** `draft` with the messages that did not go before its words, as a message that came back
 * to a composer goes: each message's words on lines of their own, then the draft's; their
 * chips and the draft's, each once. Their images do not keep. */
export function withUnsent(draft: Draft, messages: readonly Message[]): Draft {
  const text = [...messages.map((m) => m.text), draft.text].filter((t) => t.trim() !== "").join("\n\n");
  const seen = new Set<string>();
  const chips = [...messages.flatMap((m) => m.chips), ...draft.chips].filter((c) => {
    const k = chipKey(c);
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
  return { text, chips };
}

/** The messages that did not go, by the composer they go back to, into those composers'
 * drafts. */
export function keepUnsent(unsent: ReadonlyMap<string, readonly Message[]>): void {
  for (const [key, messages] of unsent) {
    if (!messages.length) continue;
    const draft = withUnsent({ text: loadText(key), chips: loadChips(key) }, messages);
    saveText(key, draft.text);
    saveChips(key, draft.chips);
    const have = held.get(key);
    if (have) held.set(key, { ...have, chips: draft.chips });
  }
}
