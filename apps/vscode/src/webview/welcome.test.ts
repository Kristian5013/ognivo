import { describe, expect, it, vi } from "vitest";

import type { EditorState, NewSession, TerminalFailure } from "../shared/messages.ts";
import type * as P from "../shared/protocol.ts";

// the page's host takes VS Code's webview API as it loads
vi.hoisted(() => {
  (globalThis as unknown as { acquireVsCodeApi: () => unknown }).acquireVsCodeApi = () => ({ postMessage() {} });
});

const { placeIn, recentSessions, serverLine, suggestions, workspaceSessions } = await import("./welcome.tsx");

const NOW = Date.UTC(2026, 10, 16, 12, 0, 0);

const failure = (over: Partial<TerminalFailure> = {}): TerminalFailure => ({
  id: "t1",
  command: "cargo run -- expenses.csv",
  exitCode: 101,
  cwd: "/home/u/tally",
  output: "thread 'main' panicked at src/parse.rs:6:35:\ncalled `Result::unwrap()` on an `Err` value",
  at: NOW - 2 * 60_000,
  ...over,
});

const editor = (selection: EditorState["selection"] = null): EditorState => ({
  path: "src/parse.rs",
  abs: "/home/u/tally/src/parse.rs",
  language: "rust",
  selection,
});

describe("the empty state's suggestions", () => {
  it("are none where the user is nowhere", () => {
    expect(suggestions(null, [], NOW)).toEqual([]);
  });

  it("fix the newest failure, change the selection, explain and test the file", () => {
    const older = failure({ id: "t0", command: "cargo build", exitCode: 1 });
    const found = suggestions(editor({ start: 4, end: 8, text: "\n  pub fn parse_amount(cell: &str) -> i64 {\n  }\n" }), [failure(), older], NOW);
    expect(found.map((s) => s.kind)).toEqual(["failure", "selection", "explain", "tests"]);
    const [fix, change, explain, tests] = found;

    expect(fix).toMatchObject({ title: "Fix", code: "cargo run -- expenses.csv", detail: "exit 101 · src/parse.rs:6" });
    expect(fix!.prompt).toEqual({
      text: "Fix `cargo run -- expenses.csv` (exit 101).",
      chips: [{ kind: "terminal", command: "cargo run -- expenses.csv", exitCode: 101, cwd: "/home/u/tally", output: failure().output }],
      send: true,
    });

    // what to change is the user's to say: the composer gets the selection, not a send
    expect(change).toMatchObject({ title: "Change the selection in parse.rs", detail: "lines 4–8 · pub fn parse_amount(cell: &str) -> i64 {" });
    expect(change!.prompt.send).toBe(false);
    expect(change!.prompt.chips).toEqual([
      { kind: "selection", path: "src/parse.rs", abs: "/home/u/tally/src/parse.rs", start: 4, end: 8, language: "rust", text: "\n  pub fn parse_amount(cell: &str) -> i64 {\n  }\n" },
    ]);

    const file = { kind: "file", path: "src/parse.rs", abs: "/home/u/tally/src/parse.rs" };
    expect(explain).toMatchObject({ title: "Explain parse.rs", prompt: { text: "Explain src/parse.rs: what it does and how it works.", chips: [file], send: true } });
    expect(tests).toMatchObject({ title: "Write tests for parse.rs", prompt: { text: "Write tests for src/parse.rs, and run them.", chips: [file], send: true } });
  });

  it("offer tests for code only: not for a README, a config or data", () => {
    const of = (path: string, language: string) => suggestions({ path, abs: `/home/u/tally/${path}`, language, selection: null }, [], NOW).map((s) => s.kind);
    expect(of("README.md", "markdown")).toEqual(["explain"]);
    expect(of("Cargo.toml", "toml")).toEqual(["explain"]);
    expect(of("package.json", "json")).toEqual(["explain"]);
    expect(of("src/app.ts", "typescript")).toEqual(["explain", "tests"]);
  });

  it("say a line of one, and nothing of a blank selection", () => {
    expect(suggestions(editor({ start: 6, end: 6, text: "let x = 1;" }), [], NOW)[0]).toMatchObject({ kind: "selection", detail: "line 6 · let x = 1;" });
    expect(suggestions(editor({ start: 6, end: 7, text: "  \n " }), [], NOW).map((s) => s.kind)).toEqual(["explain", "tests"]);
  });

  it("name a failure by when it was, without a place in its output", () => {
    const [fix] = suggestions(null, [failure({ output: "error: could not compile `tally`" })], NOW);
    expect(fix!.detail).toBe("exit 101 · 2 min ago");
  });

  it("keep a command's backticks out of its code, and do without a command the shell did not say", () => {
    const [ticked] = suggestions(null, [failure({ command: "echo `date`" })], NOW);
    // markdown takes the spaces inside the ticks away again
    expect(ticked!.prompt.text).toBe("Fix `` echo `date` `` (exit 101).");
    const [unsaid] = suggestions(null, [failure({ command: "  " })], NOW);
    expect(unsaid).toMatchObject({ title: "Fix the command that failed", code: undefined, prompt: { text: "Fix the command that failed (exit 101)." } });
  });
});

describe("a place in a command's output", () => {
  it("is the first file and line it names", () => {
    expect(placeIn("thread 'main' panicked at src/parse.rs:6:35:")).toBe("src/parse.rs:6");
    expect(placeIn("error[E0308]: mismatched types\n --> src/main.rs:4:5\n  |")).toBe("src/main.rs:4");
    expect(placeIn("src/app.ts(12,5): error TS2322: Type 'string' is not assignable")).toBe("src/app.ts:12");
    expect(placeIn("main.cpp\nC:\\Users\\me\\proj\\main.cpp(12): error C2065: 'x': undeclared identifier", "C:\\Users\\me\\proj")).toBe("main.cpp:12");
    expect(placeIn("./main.go:12:5: undefined: x")).toBe("main.go:12");
    expect(placeIn("tests/test_cli.py:30: AssertionError")).toBe("tests/test_cli.py:30");
    expect(placeIn('Traceback (most recent call last):\n  File "/home/u/tally/app.py", line 12, in <module>', "/home/u/tally")).toBe("app.py:12");
    expect(placeIn("    at Object.<anonymous> (/home/u/tally/index.js:12:5)", "/home/u/tally")).toBe("index.js:12");
  });

  it("is relative to where the command ran, and a name alone elsewhere", () => {
    expect(placeIn("  --> C:\\Users\\me\\proj\\src\\main.rs:12:5", "c:\\Users\\me\\proj")).toBe("src/main.rs:12");
    expect(placeIn("panicked at /home/u/.cargo/registry/src/serde-1.0/src/de.rs:120:9", "/home/u/tally")).toBe("de.rs:120");
  });

  it("is none in an address, a version or a log without lines", () => {
    expect(placeIn("Error: connect ECONNREFUSED 127.0.0.1:8080")).toBeUndefined();
    expect(placeIn("   Compiling tally v0.1.0 (/home/u/tally)\nerror: could not compile `tally`")).toBeUndefined();
    expect(placeIn("make: *** [Makefile:12: all] Error 1")).toBeUndefined();
  });
});

const summary = (thread: number, project: string, over: Partial<P.ThreadSummary> = {}): P.ThreadSummary => ({
  thread,
  kind: "chat",
  project,
  profile: "gpt-oss-20b",
  level: null,
  server: "http://127.0.0.1:8080",
  model: null,
  title: `session ${thread}`,
  created: NOW,
  updated: NOW,
  turns: 1,
  unreviewed: 0,
  open: false,
  running: false,
  answer_now: false,
  context: null,
  mode: null,
  vision: null,
  ...over,
});

describe("the recent sessions", () => {
  it("are this workspace's first, by the page's spelling of paths, then the others', five at most", () => {
    const threads = [
      summary(9, "/home/u/other"),
      summary(8, "C:\\Users\\me\\proj"),
      summary(7, "/home/u/other", { kind: "subagent" }),
      summary(6, "/home/u/second"),
      summary(5, "/home/u/third"),
      summary(4, "/home/u/other"),
      summary(3, "/home/u/other"),
      summary(2, "/home/u/other"),
    ];
    const recent = recentSessions(threads, ["c:/Users/me/proj/", "/home/u/second"]);
    expect(recent.map((r) => [r.thread.thread, r.here])).toEqual([
      [8, true],
      [6, true],
      [9, false],
      [5, false],
      [4, false],
    ]);
  });

  it("are this workspace's where the core lists them for its folders, whatever the folder's spelling", () => {
    // the window has C:\Users\RUNNER~1\proj open; the core keeps the session by the long name
    const threads = [summary(9, "/home/u/other"), summary(8, "C:\\Users\\runneradmin\\proj"), summary(5, "/home/u/linked-target")];
    const recent = recentSessions(threads, ["C:\\Users\\RUNNER~1\\proj", "/home/u/link"], new Set([8, 5]));
    expect(recent.map((r) => [r.thread.thread, r.here])).toEqual([
      [8, true],
      [5, true],
      [9, false],
    ]);
  });
});

describe("this workspace's sessions", () => {
  it("are each folder's as the core lists them, each once, the last updated first, with its folder", () => {
    const folders = ["C:\\Users\\RUNNER~1\\proj", "/home/u/second", "/home/u/second-again"];
    const proj = [summary(8, "C:\\Users\\runneradmin\\proj", { updated: NOW - 3000 }), summary(4, "C:\\Users\\runneradmin\\proj", { updated: NOW - 9000 })];
    const second = [summary(6, "/home/u/second", { updated: NOW - 5000 })];
    // the same folder twice, by two spellings, lists its sessions twice
    const { threads, folder } = workspaceSessions(folders, [proj, second, second]);
    expect(threads.map((t) => t.thread)).toEqual([8, 6, 4]);
    expect([...folder]).toEqual([
      [8, folders[0]],
      [4, folders[0]],
      [6, folders[1]],
    ]);
    expect(workspaceSessions([], [])).toEqual({ threads: [], folder: new Map() });
  });
});

const session: NewSession = {
  server: "http://127.0.0.1:8080/",
  model: "",
  profile: "",
  reasoning: "",
  context: 0,
  mode: "auto",
  native: true,
  subagents: true,
};

const detected = (over: Partial<P.Detected> = {}): P.Detected => ({
  profile: "gpt-oss-20b",
  levels: ["low", "medium", "high"],
  default_level: "medium",
  native: true,
  window: 131072,
  models: [],
  model: "gpt-oss-20b-mxfp4",
  vision: false,
  ...over,
});

describe("the server's status line", () => {
  /** The line as a wide side bar shows it, and its state. */
  const said = (detection: Parameters<typeof serverLine>[0], s = session) => {
    const line = serverLine(detection, s);
    return [line.state, `${line.name}${line.where}${line.says}`];
  };

  it("says the model on the server, and what the core found of it on hover", () => {
    expect(serverLine(detected(), session)).toEqual({
      state: "ok",
      name: "gpt-oss-20b-mxfp4",
      where: " on 127.0.0.1:8080",
      says: " · ready",
      hint: "http://127.0.0.1:8080/ · profile gpt-oss-20b · reasoning medium · 128K context",
    });
  });

  it("says while it looks, when the server does not answer, and what is missing", () => {
    expect(said(undefined)).toEqual(["pending", "Looking at 127.0.0.1:8080…"]);
    expect(said("failed")).toEqual(["failed", "127.0.0.1:8080 does not answer"]);
    expect(said(detected({ profile: null }))).toEqual(["warn", "gpt-oss-20b-mxfp4 on 127.0.0.1:8080 · no profile fits it"]);
    const several = [
      { id: "qwen3-coder", status: "loaded", profile: "qwen3-coder" },
      { id: "gemma-4", status: "loaded", profile: null },
    ];
    expect(said(detected({ profile: null, model: null, models: several }))).toEqual(["warn", "127.0.0.1:8080 runs several models · choose one"]);
    // a router's model is named by the user, not by a profile the user chose
    expect(said(detected({ profile: null, model: null, models: several }), { ...session, profile: "qwen3-coder" })[1]).toBe("127.0.0.1:8080 runs several models · choose one");
  });

  it("names a server of one model by its profile, as the core names no model of it", () => {
    expect(said(detected({ model: null }))).toEqual(["ok", "gpt-oss-20b on 127.0.0.1:8080 · ready"]);
    expect(said(detected({ model: null }), { ...session, profile: "qwen3-coder" })).toEqual(["ok", "qwen3-coder on 127.0.0.1:8080 · ready"]);
    expect(said(detected({ model: null, profile: null }))).toEqual(["warn", "127.0.0.1:8080 · no profile fits its model"]);
  });

  it("says on hover the level a new session gets: not a level its model's profile has not", () => {
    const hint = (s: Partial<NewSession>) => serverLine(detected(), { ...session, ...s }).hint;
    expect(hint({ reasoning: "high" })).toContain("· reasoning high ·");
    // chosen for Qwen, then gpt-oss picked: gpt-oss's default
    expect(hint({ reasoning: "xhigh" })).toContain("· reasoning medium ·");
    expect(hint({ reasoning: "xhigh", profile: "qwen3.8-27b" })).toContain("· reasoning xhigh ·");
  });

  it("says a router's model that is not loaded starts with the session", () => {
    const routed = detected({ model: null, models: [{ id: "qwen3-coder", status: "unloaded", profile: "qwen3-coder" }] });
    expect(said(routed, { ...session, model: "qwen3-coder", profile: "qwen3-coder" })).toEqual(["ok", "qwen3-coder on 127.0.0.1:8080 · starts with the session"]);
  });
});
