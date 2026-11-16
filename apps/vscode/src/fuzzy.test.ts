import { describe, expect, it } from "vitest";

import { excludeGlob, gitignoreGlobs, joinGlobs, rank, score } from "./fuzzy.ts";

const FILES = [
  "README.md",
  "src/parse.rs",
  "src/parser/mod.rs",
  "src/main.rs",
  "src/error.rs",
  "tests/compare_files.rs",
  "tests/parse.rs",
  "crates/ognivo-core/src/agent/parse_args.rs",
  "apps/vscode/src/webview/ModelPopover.tsx",
];

describe("ranking the workspace's files", () => {
  it("matches the characters in order, case aside", () => {
    expect(score("src/parse.rs", "PRS")).toBeDefined();
    expect(score("src/parse.rs", "zz")).toBeUndefined();
    expect(score("src/parse.rs", "sr")).toBeDefined();
    // the order counts
    expect(score("src/main.rs", "nm")).toBeUndefined();
  });

  it("puts a match in the file's name first, the shorter path before the longer", () => {
    expect(rank(FILES, "parse", 4)).toEqual(["src/parse.rs", "tests/parse.rs", "crates/ognivo-core/src/agent/parse_args.rs", "src/parser/mod.rs"]);
  });

  it("finds a file by the starts of its words", () => {
    expect(rank(FILES, "mp", 1)).toEqual(["apps/vscode/src/webview/ModelPopover.tsx"]);
    expect(rank(FILES, "cf", 1)).toEqual(["tests/compare_files.rs"]);
  });

  it("takes the whole path when the name alone does not match", () => {
    expect(rank(FILES, "testsparse", 2)).toEqual(["tests/parse.rs"]);
    expect(rank(FILES, "src/err", 1)).toEqual(["src/error.rs"]);
  });

  it("lets spaces match anything", () => {
    expect(rank(FILES, "main rs", 1)).toEqual(["src/main.rs"]);
  });

  it("gives the shortest paths for an empty query, and no more than the limit", () => {
    expect(rank(FILES, "", 2)).toEqual(["README.md", "src/main.rs"]);
    expect(rank(FILES, "s", 0)).toEqual([]);
  });
});

describe("what findFiles leaves out", () => {
  it("joins the settings' globs that are on, and the folders' contents", () => {
    expect(
      excludeGlob(
        { "**/.git": true, "**/node_modules": true, "**/*.js": { when: "$(basename).ts" }, "**/off": false },
        { "**/node_modules": true, "**/dist/**": true },
      ),
    ).toBe("{**/.git,**/.git/**,**/node_modules,**/node_modules/**,**/dist/**}");
    expect(excludeGlob(undefined, {})).toBeUndefined();
    expect(excludeGlob({ "**/x": true })).toBe("{**/x,**/x/**}");
  });

  it("spells out one group in braces, and leaves out what it cannot", () => {
    expect(joinGlobs(["**/{target,out}"])).toBe("{**/target,**/target/**,**/out,**/out/**}");
    expect(joinGlobs(["**/{a,{b,c}}"])).toBeUndefined();
    expect(joinGlobs(["**/dist/**"])).toBe("**/dist/**");
  });

  it("reads the plain lines of a .gitignore", () => {
    const text = "# build\n/target\nnode_modules/\n*.log\n!keep.log\ndocs/build/\n\n\\#odd\n[Bb]in\n*\n";
    expect(gitignoreGlobs(text)).toEqual(["target", "**/node_modules", "**/*.log", "docs/build"]);
  });
});
