import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ContextChip } from "../shared/messages.ts";
import { forgetDraft, heldOf, hold, keepUnsent, loadChips, loadText, saveChips, saveText, withUnsent } from "./drafts.ts";
import type { Message } from "./queue.ts";

const file: ContextChip = { kind: "file", path: "src/parse.rs", abs: "/home/u/tally/src/parse.rs" };
const terminal: ContextChip = { kind: "terminal", command: "cargo test", exitCode: 101, cwd: "/home/u/tally", output: "FAILED" };
const words = (text: string, chips: ContextChip[] = []): Message => ({ text, chips, images: [] });

/** The page's session storage, as a reload keeps it. */
class Session {
  readonly kept = new Map<string, string>();
  getItem(k: string): string | null {
    return this.kept.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.kept.set(k, v);
  }
  removeItem(k: string): void {
    this.kept.delete(k);
  }
}

let session: Session;
beforeEach(() => {
  session = new Session();
  vi.stubGlobal("sessionStorage", session);
});
afterEach(() => {
  vi.unstubAllGlobals();
});

describe("a composer's draft", () => {
  it("takes the messages that did not go before what the user wrote since, their chips each once", () => {
    const draft = withUnsent({ text: "and keep the old behaviour", chips: [file] }, [words("Fix the parser", [terminal, file]), words("", [file]), words("Then run the tests")]);
    expect(draft.text).toBe("Fix the parser\n\nThen run the tests\n\nand keep the old behaviour");
    expect(draft.chips).toEqual([terminal, file]);
    expect(withUnsent({ text: "", chips: [] }, [])).toEqual({ text: "", chips: [] });
  });

  it("keeps the words and the chips across a reload, the images only in the page", () => {
    saveText("31", "Fix it");
    saveChips("31", [terminal]);
    // a reload: what the page held is gone, the storage is not
    expect(loadText("31")).toBe("Fix it");
    expect(loadChips("31")).toEqual([terminal]);
    // what the page holds is newer than what the storage kept
    hold("31", { chips: [file], images: [], dismissed: new Set(), file: undefined });
    expect(loadChips("31")).toEqual([file]);
    saveChips("31", []);
    expect(session.kept.has("ognivo.draft.31.chips")).toBe(false);
    // a broken entry is no chips
    session.setItem("ognivo.draft.40.chips", "{not json");
    expect(loadChips("40")).toEqual([]);
    session.setItem("ognivo.draft.40.chips", JSON.stringify([terminal, 3, null]));
    expect(loadChips("40")).toEqual([terminal]);
    forgetDraft("31");
    forgetDraft("40");
  });

  it("holds nothing for a message with nothing besides its words, and lets a deleted thread's go", () => {
    hold("7", { chips: [], images: [], dismissed: new Set(), file: undefined });
    expect(heldOf("7")).toBeUndefined();
    hold("7", { chips: [], images: [], dismissed: new Set(["file:/a"]), file: "/a" });
    expect(heldOf("7")?.dismissed.has("file:/a")).toBe(true);
    saveText("7", "words");
    saveChips("7", [file]);
    forgetDraft("7");
    expect(heldOf("7")).toBeUndefined();
    expect(loadText("7")).toBe("");
    expect(loadChips("7")).toEqual([]);
  });

  it("gets the messages that did not go when the core went away, for the reload to give back", () => {
    saveText("31", "what I write now");
    keepUnsent(
      new Map([
        ["31", [words("steered", [terminal]), words("queued")]],
        ["new", [words("a first message that could not go", [file])]],
        ["32", []],
      ]),
    );
    expect(loadText("31")).toBe("steered\n\nqueued\n\nwhat I write now");
    expect(loadChips("31")).toEqual([terminal]);
    expect(loadText("new")).toBe("a first message that could not go");
    expect(loadChips("new")).toEqual([file]);
    expect(session.kept.has("ognivo.draft.32")).toBe(false);
    forgetDraft("31");
    forgetDraft("new");
  });

  it("lasts as long as the page shows it where the page has no storage", () => {
    vi.stubGlobal("sessionStorage", undefined);
    saveText("9", "words");
    expect(loadText("9")).toBe("");
    expect(loadChips("9")).toEqual([]);
  });
});
