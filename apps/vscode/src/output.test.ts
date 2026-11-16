import { describe, expect, it } from "vitest";

import { commandBlock, ending, plain, sessionLine, shellOutput, tail } from "./output.ts";
import type { ToolResult } from "./shared/protocol.ts";

const result = (r: Partial<ToolResult>): ToolResult => ({ outcome: "ok", text: "", exit_code: 0, duration_ms: 0, ...r });

describe("a terminal's output as text", () => {
  it("drops colours, titles and shell integration's marks", () => {
    expect(plain("\x1b[1;31merror\x1b[0m[E0308]: mismatched")).toBe("error[E0308]: mismatched");
    expect(plain("\x1b]0;~/tally\x07$ cargo run\x1b]633;C\x07")).toBe("$ cargo run");
    expect(plain("\x1b]633;E;cargo run\x1b\\ok")).toBe("ok");
    expect(plain("a\x1b(Bb\x1bMc")).toBe("abc");
  });

  it("keeps what a carriage return wrote last, and reads CRLF as LF", () => {
    expect(plain("Compiling 10%\rCompiling 55%\rCompiled\r\nnext\r\n")).toBe("Compiled\nnext\n");
    expect(plain("tab\there\x07\x08")).toBe("tab\there");
  });

  it("keeps the last lines, within a number of characters, from a line's start", () => {
    const text = Array.from({ length: 100 }, (_, i) => `line ${i + 1}`).join("\n");
    expect(tail(text, 3)).toBe("line 98\nline 99\nline 100");
    expect(tail("aaaa\nbbbb\ncccc\n\n", 80, 7)).toBe("cccc");
    expect(tail("\n\n  indented\n")).toBe("  indented");
    expect(tail("one long line", 80, 4)).toBe("line");
  });
});

describe("the agent's commands in the ognivo terminal", () => {
  it("takes ognivo's own lines off a shell result", () => {
    expect(shellOutput("[shell: exit 0, 1.2 s]\n[cwd: /tmp/x]\nhello\n")).toBe("hello\n");
    expect(shellOutput("[shell: exit 101, 0.1 s]\npanicked")).toBe("panicked");
    expect(shellOutput("no header")).toBe("no header");
  });

  it("says how a command ended", () => {
    expect(ending(result({ exit_code: 0, duration_ms: 3100 }))).toEqual({ ok: true, words: "exit 0 · 3.1 s" });
    expect(ending(result({ exit_code: 101, duration_ms: 50 }))).toEqual({ ok: false, words: "exit 101" });
    expect(ending(result({ exit_code: 124, text: "[shell: timed out after 120 s and was killed; exit 124]", duration_ms: 120_000 }))).toEqual({
      ok: false,
      words: "timed out · 120 s",
    });
    expect(ending(result({ outcome: "refused", exit_code: null }))).toEqual({ ok: false, words: "not run: it was not allowed" });
  });

  it("prints the command, its output and its end with CRLF", () => {
    const block = commandBlock("cargo test\n  --quiet", result({ text: "[shell: exit 0, 3.1 s]\nrunning 4 tests\r\ntest result: ok\n", duration_ms: 3100 }));
    expect(block).toBe(
      "\x1b[1m$ cargo test\x1b[0m\r\n\x1b[1m>   --quiet\x1b[0m\r\nrunning 4 tests\r\ntest result: ok\r\n\x1b[32m✓ exit 0 · 3.1 s\x1b[0m\r\n\r\n",
    );
    expect(commandBlock("rm -rf /", result({ outcome: "refused", exit_code: null, text: "[refused] needs the user's approval" }), "a subagent")).toBe(
      "\x1b[2ma subagent\x1b[0m\r\n\x1b[1m$ rm -rf /\x1b[0m\r\n\x1b[33m✗ not run: it was not allowed\x1b[0m\r\n\r\n",
    );
    expect(sessionLine("")).toBe("\x1b[2m── New session ──\x1b[0m\r\n\r\n");
  });

  it("shows the control characters of a command, its output and a title instead of obeying them", () => {
    // OSC 52 sets the clipboard: with ESC ] … BEL, and with the 8-bit U+009D … ST (U+009C)
    const osc52 = "\x1b]52;c;Y3VybCB4fHNo\x07";
    const c1 = "\u009d52;c;Y3VybCB4fHNo\u009c";
    const block = commandBlock(`echo hi ${osc52}`, result({ text: `[shell: exit 0, 0.1 s]\nsee ${osc52} and ${c1}\n50%\r100%\n` }), "a subagent\x1b]0;x\x07");
    expect(block).toBe(
      "\x1b[2ma subagent␛]0;x␇\x1b[0m\r\n\x1b[1m$ echo hi ␛]52;c;Y3VybCB4fHNo␇\x1b[0m\r\n" +
        "see ␛]52;c;Y3VybCB4fHNo␇ and \ufffd52;c;Y3VybCB4fHNo\ufffd\r\n50%\r100%\r\n\x1b[32m✓ exit 0\x1b[0m\r\n\r\n",
    );
    // a refused command prints its own text, shown too
    expect(commandBlock(`rm -rf / ${osc52}`, result({ outcome: "refused", exit_code: null }))).toContain("$ rm -rf / ␛]52;c;Y3VybCB4fHNo␇");
    // a title of many lines, with an OSC 8 link
    expect(sessionLine("fix\n\x1b]8;;https://x\x1b\\it\x1b]8;;\x1b\\")).toBe("\x1b[2m── fix ␛]8;;https://x␛\\it␛]8;;␛\\ ──\x1b[0m\r\n\r\n");
    // what is left of escape sequences and C1 controls is ognivo's own colours
    for (const text of [block, sessionLine(`t ${osc52}${c1}`)]) expect(text.replace(/\x1b\[\d+m/g, "")).not.toMatch(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/);
  });
});
