import { describe, expect, it } from "vitest";

import { highlight, highlightLines, known, languageOf, splitLines } from "./highlight.ts";

describe("highlighting", () => {
  it("knows a file's language by its name", () => {
    expect(languageOf("src/main.rs")).toBe("rust");
    expect(languageOf("C:\\proj\\app.TSX")).toBe("typescript");
    expect(languageOf("Cargo.toml")).toBe("ini");
    expect(languageOf("build/Makefile")).toBe("makefile");
    expect(languageOf("notes.txt")).toBeUndefined();
    expect(known("ts")).toBe("ts");
    expect(known("rs")).toBe("rs");
    expect(known("toml")).toBe("toml");
    expect(known("klingon")).toBeUndefined();
  });

  it("escapes what it does not colour", () => {
    expect(highlight('x = "<b>"', "python")).not.toContain("<b>");
    expect(highlight("anything", "klingon")).toBeUndefined();
  });

  it("cuts a block into lines, each with the spans it needs", () => {
    const lines = splitLines('<span class="a">one\ntwo</span> three\n<span class="b">four</span>');
    expect(lines).toEqual(['<span class="a">one</span>', '<span class="a">two</span> three', '<span class="b">four</span>']);
  });

  it("keeps a comment over lines in its colour on each", () => {
    const html = highlightLines(["/* one", "two */", "let x = 1;"], "rust")!;
    expect(html).toHaveLength(3);
    expect(html[1]).toContain("hljs-comment");
    expect(html[2]).toContain("hljs-keyword");
  });
});
