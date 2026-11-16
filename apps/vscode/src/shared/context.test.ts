import { describe, expect, it } from "vitest";

import { chipFor, chipLabel, fromWindow, pathFor, splitContext, withContext } from "./context.ts";
import type { ContextChip } from "./messages.ts";

const abs = "/home/u/tally/src/parse.rs";
const file: ContextChip = { kind: "file", path: "src/parse.rs", abs };
const selection: ContextChip = {
  kind: "selection",
  path: "src/parse.rs",
  abs,
  start: 4,
  end: 8,
  language: "rust",
  text: "fn amount(field: &str) -> f64 {\n    field.trim().parse().unwrap()\n}\n",
};
const terminal: ContextChip = {
  kind: "terminal",
  command: "cargo run -- expenses.csv",
  exitCode: 101,
  cwd: "/home/u/tally",
  output: "thread 'main' panicked at src/parse.rs:6:35:\ncalled `Result::unwrap()` on an `Err` value: ParseFloatError { kind: Empty }\n",
};
const problem: ContextChip = {
  kind: "problem",
  path: "src/parse.rs",
  abs,
  line: 6,
  message: "mismatched types",
  source: "rustc",
  severity: "error",
};

const lines = (...l: string[]) => l.join("\n");

describe("withContext", () => {
  it("writes the chips below the text, as the model reads them", () => {
    expect(withContext("Fix the panic.", [selection, terminal, problem])).toBe(
      lines(
        "Fix the panic.",
        "",
        "---",
        "Context from VS Code:",
        "- Selection in `src/parse.rs`, lines 4–8:",
        "  ```rust",
        "  fn amount(field: &str) -> f64 {",
        "      field.trim().parse().unwrap()",
        "  }",
        "  ```",
        "- Terminal: `cargo run -- expenses.csv` exited with 101 in `/home/u/tally`:",
        "  ```text",
        "  thread 'main' panicked at src/parse.rs:6:35:",
        "  called `Result::unwrap()` on an `Err` value: ParseFloatError { kind: Empty }",
        "  ```",
        "- Problem in `src/parse.rs` line 6 (error, rustc): mismatched types",
      ),
    );
  });

  it("leaves a message without chips as it is", () => {
    expect(withContext("Fix the panic.", [])).toBe("Fix the panic.");
    expect(withContext("", [])).toBe("");
  });

  it("names a file once: its selection's entry says it", () => {
    expect(withContext("Explain.", [file])).toBe(lines("Explain.", "", "---", "Context from VS Code:", "- Open file: `src/parse.rs`"));
    const both = withContext("Explain.", [file, selection]);
    expect(both).not.toContain("Open file");
    expect(both).toContain("- Selection in `src/parse.rs`, lines 4–8:");
    // another file's chip stays
    const other: ContextChip = { kind: "file", path: "src/main.rs", abs: "/home/u/tally/src/main.rs" };
    expect(withContext("Explain.", [other, selection])).toContain("- Open file: `src/main.rs`\n- Selection in");
    // the same chip twice is one entry
    expect(withContext("Explain.", [problem, problem]).match(/- Problem/g)).toHaveLength(1);
  });

  it("says a line, a command without its directory, a problem without its source", () => {
    const one: ContextChip = { ...selection, start: 6, end: 6, text: "    field.trim().parse().unwrap()" };
    expect(withContext("", [one])).toBe(
      lines("---", "Context from VS Code:", "- Selection in `src/parse.rs`, line 6:", "  ```rust", "      field.trim().parse().unwrap()", "  ```"),
    );
    expect(withContext("", [{ ...terminal, cwd: null, output: "" }])).toBe(
      lines("---", "Context from VS Code:", "- Terminal: `cargo run -- expenses.csv` exited with 101:", "  ```text", "  ```"),
    );
    expect(withContext("", [{ ...problem, source: null, severity: "warning", message: "unused variable: `x`\nconsider `_x`" }])).toBe(
      lines("---", "Context from VS Code:", "- Problem in `src/parse.rs` line 6 (warning): unused variable: `x` consider `_x`"),
    );
  });

  it("fences a body with a backtick run longer than any in it", () => {
    const readme: ContextChip = {
      ...selection,
      path: "README.md",
      abs: "/home/u/tally/README.md",
      language: "markdown",
      text: "Run it:\n\n```sh\ncargo run -- a.csv\n```\n\nor ````` five",
    };
    expect(withContext("", [readme])).toBe(
      lines(
        "---",
        "Context from VS Code:",
        "- Selection in `README.md`, lines 4–8:",
        "  ``````markdown",
        "  Run it:",
        "",
        "  ```sh",
        "  cargo run -- a.csv",
        "  ```",
        "",
        "  or ````` five",
        "  ``````",
      ),
    );
    // and inline code too, with room where it begins or ends with a backtick
    const date: ContextChip = { ...terminal, command: "echo `date`", cwd: "/tmp/a``b" };
    expect(withContext("", [date])).toContain("- Terminal: `` echo `date` `` exited with 101 in ```/tmp/a``b```:");
  });

  it("writes the line ends of a body as LF", () => {
    const crlf: ContextChip = { ...selection, text: "fn a() {}\r\nfn b() {}\r\n" };
    expect(withContext("", [crlf])).toBe(
      lines("---", "Context from VS Code:", "- Selection in `src/parse.rs`, lines 4–8:", "  ```rust", "  fn a() {}", "  fn b() {}", "  ```"),
    );
  });
});

describe("splitContext", () => {
  it("gives back the text and each chip, called as before it was sent", () => {
    const labels = { file: "parse.rs", selection: "parse.rs:4–8", terminal: "cargo run · exit 101", problem: "parse.rs:6" };
    for (const chip of [file, selection, terminal, problem]) {
      const message = withContext("Fix the panic.", [chip]);
      const { text, chips } = splitContext(message);
      expect(text).toBe("Fix the panic.");
      expect(chips).toHaveLength(1);
      expect(chips[0]).toMatchObject({ kind: chip.kind, label: labels[chip.kind] });
      expect(chipLabel(chip)).toBe(labels[chip.kind]);
      // the entry's markdown, as the message holds it
      expect(message.endsWith(`Context from VS Code:\n${chips[0]!.detail}`)).toBe(true);
    }
    const all = splitContext(withContext("Fix the panic.\n", [file, terminal, selection, problem]));
    expect(all.text).toBe("Fix the panic.\n");
    expect(all.chips.map((c) => c.label)).toEqual(["cargo run · exit 101", "parse.rs:4–8", "parse.rs:6"]);
    expect(all.chips[0]!.detail).toBe(
      lines(
        "- Terminal: `cargo run -- expenses.csv` exited with 101 in `/home/u/tally`:",
        "  ```text",
        "  thread 'main' panicked at src/parse.rs:6:35:",
        "  called `Result::unwrap()` on an `Err` value: ParseFloatError { kind: Empty }",
        "  ```",
      ),
    );
  });

  it("reads bodies with fences inside, and a block that is all the message", () => {
    const readme: ContextChip = { ...selection, start: 2, end: 2, text: "```\n````\n```" };
    const message = withContext("", [readme, { ...terminal, command: "echo `date`" }]);
    const { text, chips } = splitContext(message);
    expect(text).toBe("");
    expect(chips.map((c) => [c.kind, c.label])).toEqual([
      ["selection", "parse.rs:2"],
      ["terminal", "echo `date` · exit 101"],
    ]);
    expect(chips[0]!.detail).toBe(lines("- Selection in `src/parse.rs`, line 2:", "  `````rust", "  ```", "  ````", "  ```", "  `````"));
  });

  it("leaves a message without a block as it is", () => {
    for (const message of [
      "",
      "Why does it panic?",
      "Notes:\n\n---\nContext from VS Code: is what the extension adds",
      // a head the user wrote, with no entries after it, or with what no entry is
      "a\n\n---\nContext from VS Code:\n",
      "a\n\n---\nContext from VS Code:\n- Open file: `x`\nand more",
      "a\n\n---\nContext from VS Code:\n- Selection in `x`, lines 1–2:\n  ```rust\n  no end",
    ]) {
      expect(splitContext(message)).toEqual({ text: message, chips: [] });
    }
  });

  it("reads a message with CRLF line ends, and bodies that had them", () => {
    const crlf: ContextChip = { ...terminal, output: "error: a\r\nerror: b\r\n" };
    const message = withContext("Fix it.\nPlease.", [crlf, problem]).replace(/\n/g, "\r\n");
    const { text, chips } = splitContext(message);
    expect(text).toBe("Fix it.\r\nPlease.");
    expect(chips.map((c) => c.label)).toEqual(["cargo run · exit 101", "parse.rs:6"]);
    expect(chips[0]!.detail).toContain("\n  error: a\n  error: b\n  ```");
  });

  it("takes the last block: one in the text the user wrote stays text", () => {
    const quoted = withContext("Earlier I sent:", [file]);
    const message = withContext(quoted, [problem]);
    const { text, chips } = splitContext(message);
    expect(text).toBe(quoted);
    expect(chips.map((c) => c.kind)).toEqual(["problem"]);
  });

  it("finds the block after a text that ends in its head, and a command the shell did not say", () => {
    for (const said of ["a\n\n---\nContext from VS Code:", "---\nContext from VS Code:"]) {
      const { text, chips } = splitContext(withContext(said, [file]));
      expect([text, chips.map((c) => c.label)]).toEqual([said, ["parse.rs"]]);
    }
    const unsaid = withContext("t", [{ ...terminal, command: " " }]);
    expect(unsaid).toContain("- Terminal: ` ` exited with 101 in `/home/u/tally`:");
    expect(splitContext(unsaid).chips.map((c) => c.label)).toEqual(["exit 101"]);
    expect(chipLabel({ ...terminal, command: "" })).toBe("exit 101");
  });
});

describe("a chip for the session it goes to", () => {
  it("names a file inside the session's root relative to it, with /", () => {
    expect(pathFor("/home/u/tally/src/parse.rs", "/home/u/tally")).toBe("src/parse.rs");
    expect(pathFor("/home/u/tally/src/parse.rs", "/home/u/tally/")).toBe("src/parse.rs");
    expect(pathFor("C:\\Users\\me\\proj\\src\\config.rs", "c:\\users\\me\\proj")).toBe("src/config.rs");
    expect(pathFor("c:/Users/me/proj/src/config.rs", "C:\\Users\\me\\proj")).toBe("src/config.rs");
    // a project that is a folder of the window's first folder
    expect(pathFor("/home/u/mono/sub/a.rs", "/home/u/mono/sub")).toBe("a.rs");
  });

  it("names a file outside the session's root by its path on disk, as the editor spelled it", () => {
    expect(pathFor("/home/u/tally/src/parse.rs", "/home/u/other")).toBe("/home/u/tally/src/parse.rs");
    // a folder whose name only begins as the root's is not inside it
    expect(pathFor("/home/u/tally2/src/parse.rs", "/home/u/tally")).toBe("/home/u/tally2/src/parse.rs");
    expect(pathFor("C:\\Users\\me\\proj\\src\\config.rs", "C:/Users/me/tally")).toBe("C:\\Users\\me\\proj\\src\\config.rs");
    // the root itself, and a root nobody knows
    expect(pathFor("/home/u/tally", "/home/u/tally")).toBe("/home/u/tally");
    expect(pathFor("/home/u/tally/a.rs", "")).toBe("/home/u/tally/a.rs");
    // Linux tells cases apart
    expect(pathFor("/home/u/Tally/a.rs", "/home/u/tally")).toBe("/home/u/Tally/a.rs");
  });

  it("rebases the chips that name a file, from their path on disk, and leaves a command's", () => {
    const other = "/home/u/other";
    expect(chipFor(selection, "/home/u/tally")).toBe(selection);
    expect(chipFor(selection, other)).toEqual({ ...selection, path: abs });
    expect(chipFor(file, other)).toEqual({ ...file, path: abs });
    expect(chipFor(problem, other)).toEqual({ ...problem, path: abs });
    expect(chipFor(terminal, other)).toBe(terminal);
    // the window's second folder: its files went as paths on disk, and are its own
    const second: ContextChip = { kind: "file", path: "/home/u/second/src/lib.rs", abs: "/home/u/second/src/lib.rs" };
    expect(chipFor(second, "/home/u/second")).toEqual({ ...second, path: "src/lib.rs" });
    // the same again is the same
    expect(chipFor(chipFor(selection, other), "/home/u/tally")).toEqual(selection);
    // a session whose root the page does not know yet (its history is on its way) gets the
    // file by its path on disk: right whatever the session's project
    expect(chipFor(selection, "")).toEqual({ ...selection, path: abs });
    expect(chipFor(file, "")).toEqual({ ...file, path: abs });
    expect(chipFor(terminal, "")).toBe(terminal);
    // the block says the path the session reads
    expect(withContext("Why?", [chipFor(file, other)])).toContain("- Open file: `/home/u/tally/src/parse.rs`");
  });

  it("names the workspace's files that @ offers for a session of another root", () => {
    expect(fromWindow("src/parse.rs", "/home/u/tally", "/home/u/tally")).toBe("src/parse.rs");
    expect(fromWindow("src/parse.rs", "/home/u/tally", "/home/u/other")).toBe("/home/u/tally/src/parse.rs");
    expect(fromWindow("sub/a.rs", "/home/u/mono", "/home/u/mono/sub")).toBe("a.rs");
    expect(fromWindow("src/config.rs", "C:\\Users\\me\\proj", "C:\\Users\\me\\tally")).toBe("C:\\Users\\me\\proj\\src\\config.rs");
    expect(fromWindow("src/config.rs", "C:/Users/me/proj", "C:/Users/me/tally")).toBe("C:/Users/me/proj/src/config.rs");
    // a file outside the first folder came by its path on disk already
    expect(fromWindow("/home/u/second/b.rs", "/home/u/tally", "/home/u/second")).toBe("b.rs");
    expect(fromWindow("D:\\x\\b.rs", "C:\\Users\\me\\proj", "C:\\Users\\me\\tally")).toBe("D:\\x\\b.rs");
    // a window without a folder: as it is
    expect(fromWindow("src/parse.rs", "", "/home/u/other")).toBe("src/parse.rs");
    // a session whose root the page does not know yet: by its path on disk
    expect(fromWindow("src/parse.rs", "/home/u/tally", "")).toBe("/home/u/tally/src/parse.rs");
    expect(fromWindow("/home/u/second/b.rs", "/home/u/tally", "")).toBe("/home/u/second/b.rs");
  });
});

describe("chipLabel", () => {
  it("names a command by its first words, up to an option and about 24 characters", () => {
    const label = (command: string) => chipLabel({ ...terminal, command, exitCode: 1 });
    expect(label("cargo run -- expenses.csv")).toBe("cargo run · exit 1");
    expect(label("  npm test\n")).toBe("npm test · exit 1");
    expect(label("cargo clippy --workspace --all-targets")).toBe("cargo clippy · exit 1");
    expect(label("python3 manage.py migrate accounts")).toBe("python3 manage.py · exit 1");
    expect(label("./target/release/a-very-long-program-name x")).toBe("./target/release/a-very… · exit 1");
  });
});
