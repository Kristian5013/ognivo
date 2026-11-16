// The gallery's states of the panel: before a session, from where the user is in the editor
// and the terminals; sessions of the fixtures the tests read; and sessions written here to
// show each part of the panel at once, in the user's language: a finished turn with its
// reads, changes, commands, answer and verdict, and a turn that runs, as the server reads
// its prompt and the model writes, with what the user wrote meanwhile, or the question it
// waits on. Each is made of what the core and the extension say (protocol.ts,
// messages.ts), never of the panel's own parts, so that it shows whatever draws them.

import type { EditorState, NewSession, SettingsTab, TerminalFailure, ToPage } from "../src/shared/messages.ts";
import type * as P from "../src/shared/protocol.ts";

import gemmaFailingBuild from "../fixtures/gemma.failing-build.json";
import gptOssEscaping from "../fixtures/gpt-oss.escaping.json";
import qwenLongFile from "../fixtures/qwen.long-file.json";
import qwenRename from "../fixtures/qwen.rename.json";
import stream from "../fixtures/stream.jsonl";

export interface Scenario {
  folders: string[];
  /** The core's root of a folder the window names otherwise (a link, a junction, an 8.3
   * name): the core lists a folder's sessions by its canonical path. */
  roots?: Record<string, string>;
  session?: Partial<NewSession>;
  threads: P.ThreadSummary[];
  events?: P.Event[];
  trajectories?: Record<number, unknown>;
  /** The thread the page shows, as if it showed it last. */
  show?: number;
  detected?: P.Detected | "failed";
  asks?: { method: string; params: unknown }[];
  settings?: SettingsTab;
  /** The unreviewed changes of the thread on show, which the core lists during a turn
   * too. */
  changes?: P.ChangedFile[];
  /** Events that come after the page opened, one every `every` ms. */
  live?: P.Event[];
  every?: number;
  place?: "sidebar" | "editor";
  /** The file the user works in, and the commands that failed in the user's terminals, as
   * the extension's hello says them. */
  editor?: EditorState | null;
  failures?: TerminalFailure[];
  /** What the extension says after its hello, `at` ms after the page asked the core who
   * it is: the user went to another file, a command failed, a message for the composer. */
  extension?: { at: number; message: ToPage }[];
  /** What the gallery clicks once the page is up, by a CSS selector: a menu, a popover; and
   * when, in ms after the page asked the core who it is (1000 when not said). */
  click?: string;
  clickAt?: number;
  /** The workspace's files, which `@` finds (`files/find`). */
  files?: string[];
  /** The turns the fake plays for the page's messages stop after this many of their steps
   * (waiting, reading the prompt twice, writing, the answer, the generation, the end): a
   * picture of a turn in its middle, whenever it is taken. */
  pause?: number;
}

const NOW = Date.now();
const MIN = 60_000;

const DETECTED: P.Detected = {
  profile: "gpt-oss-20b",
  levels: ["low", "medium", "high"],
  default_level: "medium",
  native: true,
  window: 131072,
  models: [],
  model: "gpt-oss-20b-mxfp4",
  vision: false,
} as P.Detected;

function summary(thread: number, project: string, title: string, over: Partial<P.ThreadSummary> = {}): P.ThreadSummary {
  return {
    thread,
    kind: "chat",
    project,
    profile: "gpt-oss-20b",
    level: "medium",
    server: "http://127.0.0.1:8080",
    model: null,
    title,
    created: NOW - 90 * MIN,
    updated: NOW - 5 * MIN,
    turns: 2,
    unreviewed: 0,
    open: true,
    running: false,
    answer_now: true,
    context: { working: 65536, compacts_at: 52428, window: 131072 },
    mode: "auto",
    vision: false,
    ...over,
  };
}

/** A session's events, written as it would stream them. */
class Writer {
  readonly events: P.Event[] = [];
  private seq: number;
  private id = 0;
  private gen = 0;
  private turnNo = 0;
  private prompt = 4200;

  constructor(
    readonly thread: number,
    info: Partial<P.ThreadInfo>,
    seq = 1000,
  ) {
    this.seq = seq;
    this.push({
      event: "thread_started",
      info: {
        kind: "chat",
        project: "",
        profile: "gpt-oss-20b",
        level: "medium",
        server: "http://127.0.0.1:8080",
        model: null,
        parent: null,
        ...info,
      },
    } as P.EventKind);
  }

  push(e: P.EventKind): this {
    this.events.push({ seq: this.seq++, thread: this.thread, ...e } as P.Event);
    return this;
  }

  mode(mode: P.Mode): this {
    return this.push({ event: "mode_changed", mode });
  }

  turn(user: string): this {
    this.turnNo++;
    return this.push({ event: "turn_started", turn: this.turnNo, user });
  }

  private item(body: object, generation: number | null, done = true): number {
    const id = ++this.id;
    const item = { id, turn: this.turnNo, generation, ...body } as P.Item;
    this.push({ event: "item_started", item });
    if (done) this.push({ event: "item_completed", item });
    return id;
  }

  /** A generation of the model, which the items after it until the next come from. */
  step(outcome = "calls", completion = 60): this {
    this.gen++;
    this.prompt += 600 + completion;
    return this.push({
      event: "generation",
      generation: {
        index: this.gen,
        turn: this.turnNo,
        purpose: "step",
        outcome,
        finish: outcome === "answer" ? "stop" : "tool_calls",
        seed: this.gen,
        prompt_tokens: this.prompt,
        cached_tokens: this.prompt - 500,
        completion_tokens: completion,
        prompt_ms: 180,
        generation_ms: 900 + completion * 18,
        prompt_hash: `sha256:${String(this.gen).padStart(64, "0")}`,
      },
    } as P.EventKind);
  }

  instructions(files: string[]): this {
    this.item({ kind: "instructions", files, skipped: [] }, null);
    return this;
  }

  reasoning(text: string, done = true): this {
    const id = this.item({ kind: "reasoning", text: "" }, this.gen + 1, false);
    this.push({ event: "item_delta", item: id, delta: { type: "text", text } } as P.EventKind);
    if (done) this.push({ event: "item_completed", item: { id, turn: this.turnNo, generation: this.gen + 1, kind: "reasoning", text } as P.Item });
    return this;
  }

  message(text: string): this {
    this.item({ kind: "agent_message", text }, this.gen + 1);
    return this;
  }

  notice(text: string): this {
    this.item({ kind: "notice", text }, null);
    return this;
  }

  approval(tool: string, action: string, reason: string, allowed: boolean | null): this {
    this.item({ kind: "approval", tool, action, reason, unreviewed: [], agent: null, allowed }, null);
    return this;
  }

  /** A call and its result; none while it runs. */
  call(
    tool: string,
    call: Record<string, unknown>,
    result?: { text: string; exit?: number | null; ms?: number; outcome?: P.Outcome },
    changes: P.FileChange[] = [],
  ): this {
    const body = {
      kind: "tool_call",
      call_id: `c${this.id + 1}`,
      tool,
      arguments: JSON.stringify(call),
      call,
      result: null,
      changes: [],
    };
    const id = this.item(body, this.gen + 1, false);
    if (result) {
      const done = {
        ...body,
        id,
        turn: this.turnNo,
        generation: this.gen + 1,
        result: { outcome: result.outcome ?? "ok", text: result.text, exit_code: result.exit ?? null, duration_ms: result.ms ?? 40 },
        changes,
      };
      this.push({ event: "item_completed", item: done as P.Item });
    }
    return this;
  }

  draft(tool: string, fields: [string, string][]): this {
    const generation = this.gen + 1;
    this.push({ event: "call_draft", draft: { generation, index: 0, tool, field: null, text: "" } } as P.EventKind);
    for (const [field, text] of fields) this.push({ event: "call_draft", draft: { generation, index: 0, tool, field, text } } as P.EventKind);
    return this;
  }

  /** How far the next generation got: the request went out, the server reads the prompt,
   * the model reasons or writes. */
  progress(phase: P.ProgressPhase, over: Partial<Omit<P.Progress, "generation" | "phase">> = {}): this {
    return this.push({
      event: "progress",
      progress: {
        generation: this.gen + 1,
        phase,
        prompt_total: null,
        prompt_cached: null,
        prompt_done: null,
        generated: 0,
        elapsed_ms: 0,
        tokens_per_second: null,
        ...over,
      },
    });
  }

  /** The reasoning that was still streaming ends, with the text it streamed. */
  reasoned(): this {
    const done = new Set(this.events.flatMap((e) => (e.event === "item_completed" ? [e.item.id] : [])));
    for (const e of [...this.events]) {
      if (e.event !== "item_started" || e.item.kind !== "reasoning" || done.has(e.item.id)) continue;
      const id = e.item.id;
      const text = this.events.flatMap((d) => (d.event === "item_delta" && d.item === id ? [d.delta.text] : [])).join("");
      this.push({ event: "item_completed", item: { ...e.item, text } });
    }
    return this;
  }

  /** What the user wrote while the turn ran, where the model got it: before its next step. */
  steer(text: string): this {
    this.item({ kind: "steer", text }, null);
    return this;
  }

  /** The thread's changes were kept or undone. */
  changesUpdated(): this {
    return this.push({ event: "changes_updated" });
  }

  /** The turn's end; `undelivered`, what the user wrote that the model never got. */
  end(end: P.TurnEnd = "answer", verification: string | null = null, why: string | null = null, undelivered: string[] = []): this {
    return this.push({
      event: "turn_completed",
      turn: this.turnNo,
      end,
      why,
      answer: null,
      verification,
      ...(undelivered.length > 0 ? { undelivered } : {}),
    } as P.EventKind);
  }
}

const PROJECT = "C:/Users/me/proj";

const MAIN_RS = `use std::path::PathBuf;

use clap::Parser;

mod config;

/// Prints the greeting of the config.
#[derive(Parser)]
struct Args {
    /// The config file; the default one when none is given.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config = config::load(args.config.as_deref())?;
    println!("{}", config.greeting);
    Ok(())
}`;

const numbered = (text: string, from = 1) =>
  text
    .split("\n")
    .map((l, i) => `${String(from + i).padStart(6)}\t${l}`)
    .join("\n");

/** The finished first turn of the showcase. */
function firstTurn(w: Writer): Writer {
  return w
    .mode("auto")
    .turn("Добавь в CLI флаг --verbose: пусть печатает путь к конфигу, когда он задан. И проверь, что тесты проходят.")
    .instructions(["AGENTS.md"])
    .reasoning("The user wants a --verbose flag. Find where the arguments are parsed: clap's Parser, most likely.")
    .call(
      "grep",
      { name: "Grep", pattern: "Parser|clap", path: null, glob: "*.rs" },
      {
        text: "[grep: 3 matches in 2 files for /Parser|clap/ under .]\nsrc/main.rs:3:use clap::Parser;\nsrc/main.rs:8:#[derive(Parser)]\nsrc/config.rs:1:use clap::ValueEnum;",
      },
    )
    .step()
    .call(
      "read",
      { name: "Read", path: "src/main.rs", offset: null, limit: null },
      { text: `[read: src/main.rs — lines 1-21 of 21 lines, 452 B, ~130 tokens]\n${numbered(MAIN_RS)}` },
    )
    .call(
      "read",
      { name: "Read", path: "src/config.rs", offset: 1, limit: 40 },
      {
        text: `[read: src/config.rs — lines 1-12 of 12 lines, 301 B, ~90 tokens]\n${numbered(
          'use std::path::Path;\n\npub struct Config {\n    pub greeting: String,\n}\n\npub fn load(path: Option<&Path>) -> anyhow::Result<Config> {\n    let text = match path {\n        Some(p) => std::fs::read_to_string(p)?,\n        None => String::from("greeting = \\"hello\\""),\n    };\n    Ok(Config { greeting: text })\n}',
        )}`,
      },
    )
    .step()
    .reasoning("Add the flag to Args, then print the config's path in main when it is set. eprintln, so the greeting on stdout stays as it is.")
    .call(
      "edit",
      {
        name: "Edit",
        path: "src/main.rs",
        old: "    config: Option<PathBuf>,\n}",
        new: "    config: Option<PathBuf>,\n    /// Print what the program does.\n    #[arg(short, long)]\n    verbose: bool,\n}",
        replace_all: false,
      },
      { text: "[edit: applied exact] src/main.rs — lines 10-16 now:" },
      [
        {
          path: "src/main.rs",
          created: false,
          hunks: [
            { old_start: 11, new_start: 11, removed: [], added: ["    /// Print what the program does.", "    #[arg(short, long)]", "    verbose: bool,"] },
          ],
        },
      ],
    )
    .call(
      "edit",
      {
        name: "Edit",
        path: "src/main.rs",
        old: "    let config = config::load(args.config.as_deref())?;",
        new: '    if args.verbose {\n        if let Some(path) = &args.config {\n            eprintln!("config: {}", path.display());\n        }\n    }\n    let config = config::load(args.config.as_deref())?;',
        replace_all: false,
      },
      { text: "[edit: applied exact] src/main.rs — lines 19-26 now:" },
      [
        {
          path: "src/main.rs",
          created: false,
          hunks: [
            {
              old_start: 18,
              new_start: 21,
              removed: [],
              added: ["    if args.verbose {", "        if let Some(path) = &args.config {", '            eprintln!("config: {}", path.display());', "        }", "    }"],
            },
          ],
        },
      ],
    )
    .step()
    .call(
      "shell",
      { name: "Bash", command: "cargo build", timeout_ms: null },
      {
        text: "[shell: exit 0, 3.2 s]\n   Compiling proj v0.1.0 (C:\\Users\\me\\proj)\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.18s",
        exit: 0,
        ms: 3180,
      },
    )
    .step()
    .approval("shell", "cargo test", "it runs project code while there are unreviewed changes", true)
    .call(
      "shell",
      { name: "Bash", command: "cargo test", timeout_ms: null },
      {
        text: "[shell: exit 0, 1.9 s]\n   Compiling proj v0.1.0 (C:\\Users\\me\\proj)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.72s\n     Running unittests src\\main.rs (target\\debug\\deps\\proj-3f2a9c.exe)\n\nrunning 4 tests\ntest config::tests::reads_the_default ... ok\ntest config::tests::reads_a_file ... ok\ntest tests::verbose_prints_the_path ... ok\ntest tests::quiet_by_default ... ok\n\ntest result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s",
        exit: 0,
        ms: 1930,
      },
    )
    .step()
    .message(
      "Готово. Флаг `--verbose` (коротко `-v`) добавлен в `Args`; с ним программа печатает путь к конфигу в stderr, так что вывод приветствия не меняется:\n\n```rust\nif args.verbose {\n    if let Some(path) = &args.config {\n        eprintln!(\"config: {}\", path.display());\n    }\n}\n```\n\n- `cargo build` собирается без предупреждений;\n- `cargo test`: **4 passed**, в том числе `verbose_prints_the_path`.\n\nБез флага программа работает как раньше.",
    )
    .step("answer", 180)
    .end("answer", "verified: `cargo test` passed after the last change");
}

const README = "# proj\n\nPrints the greeting of its config.\n\n## Usage\n\n| Flag | What it does |\n| --- | --- |\n| `-c, --config <path>` | The config file |\n";

function showcase(running: "draft" | "ask" | false): { events: P.Event[]; live?: P.Event[] } {
  const w = firstTurn(new Writer(31, { project: PROJECT }));
  if (!running) return { events: w.events };
  w.turn("Опиши флаг в README, в разделе Usage.")
    .reasoning("Read the README and find the Usage section.")
    .call(
      "read",
      { name: "Read", path: "README.md", offset: null, limit: null },
      { text: `[read: README.md — lines 1-9 of 9 lines, 164 B, ~45 tokens]\n${numbered(README)}` },
    )
    .step();
  if (running === "ask") {
    w.reasoning("The table has a row for each flag. Add --verbose after --config, then check the docs build.")
      .call("edit", {
        name: "Edit",
        path: "README.md",
        old: "| `-c, --config <path>` | The config file |",
        new: "| `-c, --config <path>` | The config file |\n| `-v, --verbose` | Prints the config's path to stderr |",
        replace_all: false,
      }, { text: "[edit: applied exact] README.md — lines 8-10 now:" }, [
        { path: "README.md", created: false, hunks: [{ old_start: 9, new_start: 9, removed: [], added: ["| `-v, --verbose` | Prints the config's path to stderr |"] }] },
      ])
      .step()
      .call("shell", { name: "Bash", command: "npm run docs:check", timeout_ms: null });
    return { events: w.events };
  }
  const before = w.events.length;
  w.reasoning("The table has a row for each flag. I'll add one for --verbose right after --config, in the same words as the rest", false).draft(
    "Edit",
    [
      ["path", "README.md"],
      ["old", "| `-c, --config <path>` | The config file |"],
      ["new", "| `-c, --config <path>` | The config file |\n| `-v, --verbose` | Prints the config's"],
    ],
  );
  return { events: w.events.slice(0, before), live: w.events.slice(before) };
}

/** A picture of an editor, 160×90: the golden stream's own image is only a PNG's first
 * bytes (the tests need no more), which a page shows as a broken image. */
const PICTURE = {
  mime: "image/png",
  data: "iVBORw0KGgoAAAANSUhEUgAAAKAAAABaCAIAAACwpMoFAAABBUlEQVR42u3ZsQ2CUBSGUSZhAxIGcAgaG0ZgAtewNGECh7C2sqJ8QzCBsTOP0gQi93G+/B3dO81NqJqmtYJXeQLABtgCAc9dl2/x6XHKl3/qn30+LwvYABtgAwzYABtg2yVwraIDDFiABViABViABRiwjgI8prd9BxgwYMCAATuyBFiAAQuwAGu/wK/rxf4+wIABAwZ8RGC5ogVYgAVYgLUW8KSVAgwYMGDAgOXIEmABFmAB1rbAw+1c2AADBgwYMGBHlgALsAADFmDFBvZDPvpvf8CABVhRgeWKFmABFmAB1q/AKd3DDSpgwIABA3ZkCbAAC7AAAxZgARZgARZgARZgwAreB5bfKRtSOZUjAAAAAElFTkSuQmCC",
  width: 160,
  height: 90,
};

const STREAM: P.Event[] = (stream as string)
  .split("\n")
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l) as P.Event)
  .map((e) => (e.event === "turn_started" && e.images?.length ? { ...e, images: e.images.map(() => PICTURE) } : e));

/** A thread from a recorded trajectory: the core kept no events of it. */
function recorded(thread: number, trace: { extra?: { ognivo?: { project?: string; title?: string; profile?: string } } }): Scenario {
  const o = trace.extra?.ognivo ?? {};
  const project = o.project ?? "/home/dev/demo";
  return {
    folders: [project],
    threads: [summary(thread, project, o.title ?? "", { profile: o.profile ?? "", open: false, context: null, mode: null, answer_now: false })],
    trajectories: { [thread]: trace },
    show: thread,
    detected: DETECTED,
  };
}

const welcomeThreads = [
  summary(41, PROJECT, "Добавь в CLI флаг --verbose: пусть печатает путь к конфигу", { updated: NOW - 4 * MIN, unreviewed: 2 }),
  summary(40, PROJECT, "Почему падает тест reads_a_file на Windows?", { updated: NOW - 3 * 60 * MIN, turns: 5 }),
  summary(39, PROJECT, "Перепиши загрузку конфига на serde и toml", { updated: NOW - 26 * 60 * MIN, turns: 9 }),
  summary(38, PROJECT, "Объясни, как устроен main.rs", { updated: NOW - 4 * 24 * 60 * MIN, turns: 1 }),
];

/** A session of another project, with Gemma, that the core does not have open: a message
 * to it picks it up again first. */
const TALLY = summary(37, "C:/Users/me/tally", "Добавь экспорт в CSV", {
  profile: "gemma-4-31b",
  level: "on",
  updated: NOW - 2 * 24 * 60 * MIN,
  open: false,
  answer_now: false,
  context: null,
  mode: null,
  vision: null,
});

const shown = showcase(false);
const drafting = showcase("draft");
const asking = showcase("ask");

const sessionThread = (over: Partial<P.ThreadSummary> = {}) =>
  summary(31, PROJECT, "Добавь в CLI флаг --verbose: пусть печатает путь к конфигу, когда он задан. И проверь, что тесты проходят.", {
    unreviewed: 2,
    ...over,
  });

const CHANGED: P.ChangedFile[] = [
  {
    path: "src/main.rs",
    created: false,
    deleted: false,
    drifted: false,
    hunks: [
      { old_start: 11, new_start: 11, removed: [], added: ["    /// Print what the program does.", "    #[arg(short, long)]", "    verbose: bool,"] },
      { old_start: 18, new_start: 21, removed: [], added: ["    if args.verbose {", "        if let Some(path) = &args.config {", '            eprintln!("config: {}", path.display());', "        }", "    }"] },
    ],
  },
  {
    path: "tests/cli.rs",
    created: true,
    deleted: false,
    drifted: false,
    hunks: [{ old_start: 0, new_start: 1, removed: [], added: ["#[test]", "fn verbose_prints_the_path() {}"] }],
  },
];

// ---------------------------------------------------------------- before a session

/** The workspace's files, which `@` finds, unless a scenario says others. */
export const FILES = [
  "src/main.rs",
  "src/config.rs",
  "tests/cli.rs",
  "Cargo.toml",
  "Cargo.lock",
  "README.md",
  "AGENTS.md",
  "docs/usage.md",
  "docs/config.md",
  "build.rs",
  ".gitignore",
];

const CONFIG_RS =
  'use std::path::Path;\n\npub struct Config {\n    pub greeting: String,\n}\n\npub fn load(path: Option<&Path>) -> anyhow::Result<Config> {\n    let text = match path {\n        Some(p) => std::fs::read_to_string(p)?,\n        None => String::from("greeting = \\"hello\\""),\n    };\n    Ok(Config { greeting: text })\n}';

/** `load` selected in src/config.rs. */
const LOAD_SELECTED = { start: 7, end: 13, text: CONFIG_RS.split("\n").slice(6, 13).join("\n") };

const CONFIG_EDITOR: EditorState = {
  path: "src/config.rs",
  abs: `${PROJECT}/src/config.rs`,
  language: "rust",
  selection: LOAD_SELECTED,
};

const MAIN_EDITOR: EditorState = { path: "src/main.rs", abs: `${PROJECT}/src/main.rs`, language: "rust", selection: null };

/** `cargo test` failed in the user's own terminal, a few minutes ago. */
const TEST_FAILED: TerminalFailure = {
  id: "gallery-1",
  command: "cargo test",
  exitCode: 101,
  cwd: "C:\\Users\\me\\proj",
  output: [
    "   Compiling proj v0.1.0 (C:\\Users\\me\\proj)",
    "    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.84s",
    "     Running unittests src\\main.rs (target\\debug\\deps\\proj-3f2a9c.exe)",
    "",
    "running 4 tests",
    "test config::tests::reads_the_default ... ok",
    "test config::tests::reads_a_file ... FAILED",
    "test tests::quiet_by_default ... ok",
    "test tests::verbose_prints_the_path ... ok",
    "",
    "failures:",
    "",
    "---- config::tests::reads_a_file stdout ----",
    "thread 'config::tests::reads_a_file' panicked at src\\config.rs:31:9:",
    "assertion `left == right` failed",
    '  left: "greeting = \\"hi\\"\\r\\n"',
    ' right: "greeting = \\"hi\\"\\n"',
    "",
    "test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s",
    "",
    "error: test failed, to rerun pass `--bin proj`",
  ].join("\n"),
  at: NOW - 3 * MIN,
};

/** The docs' linter failed, just now, after the page opened. */
const DOCS_FAILED: TerminalFailure = {
  id: "gallery-2",
  command: "npm run docs:check",
  exitCode: 1,
  cwd: "C:\\Users\\me\\proj",
  output: [
    "> proj-docs@0.1.0 docs:check",
    "> markdownlint README.md docs",
    "",
    "README.md:9 MD013/line-length Line length [Expected: 80; Actual: 96]",
  ].join("\n"),
  at: NOW - 20_000,
};

/** llama-server's router with three models, one of them loaded. */
const ROUTER: P.Detected = {
  profile: "qwen3.8-27b",
  levels: ["xhigh", "medium", "low", "off"],
  default_level: "medium",
  native: true,
  window: 131072,
  models: [
    { id: "Qwen3.8-27B-Q4_K_M", status: "loaded", profile: "qwen3.8-27b" },
    { id: "gpt-oss-20b-mxfp4", status: "unloaded", profile: "gpt-oss-20b" },
    { id: "gemma-4-31b-it-Q4_K_M", status: "unloaded", profile: "gemma-4-31b" },
  ],
  model: "Qwen3.8-27B-Q4_K_M",
  vision: false,
};

// ---------------------------------------------------------------- a turn that runs

const SECOND = "Опиши флаг в README, в разделе Usage.";

/** The model's read of the README, at the second turn's first step. */
function readme(w: Writer): Writer {
  return w
    .reasoning("Read the README and find the Usage section.")
    .call(
      "read",
      { name: "Read", path: "README.md", offset: null, limit: null },
      { text: `[read: README.md — lines 1-9 of 9 lines, 164 B, ~45 tokens]\n${numbered(README)}` },
    );
}

/** The request went out, and the server reads the prompt: all of it when its cache of the
 * conversation is gone, the rest of it after `cached`. */
function reading(w: Writer, total: number, cached: number): Writer {
  const prompt = { prompt_total: total, prompt_cached: cached };
  const done = (part: number) => cached + Math.round((total - cached) * part);
  return w
    .progress("waiting", { elapsed_ms: 30 })
    .progress("prompt", { ...prompt, prompt_done: cached, elapsed_ms: 240 })
    .progress("prompt", { ...prompt, prompt_done: done(0.21), elapsed_ms: 1100 })
    .progress("prompt", { ...prompt, prompt_done: done(0.43), elapsed_ms: 2000 })
    .progress("prompt", { ...prompt, prompt_done: done(0.63), elapsed_ms: 2900 });
}

/** The server read the prompt; the model reasons, then writes the README's edit. */
function writing(w: Writer, total: number, cached: number): Writer {
  const read = { prompt_total: total, prompt_cached: cached, prompt_done: total };
  return w
    .progress("reasoning", { ...read, generated: 1, elapsed_ms: 4500 })
    .reasoning("The table has a row for each flag. I'll add one for --verbose right after --config, in the same words as the rest", false)
    .progress("reasoning", { ...read, generated: 58, tokens_per_second: 33.4, elapsed_ms: 6200 })
    .draft("Edit", [
      ["path", "README.md"],
      ["old", "| `-c, --config <path>` | The config file |"],
      ["new", "| `-c, --config <path>` | The config file |\n| `-v, --verbose` | Prints the config's"],
    ])
    .progress("writing", { ...read, generated: 418, tokens_per_second: 34.2, elapsed_ms: 16800 });
}

/** The second turn as the page finds it, and what comes live: the server reads a prompt
 * whose cache is gone (`prompt`), or reads it and the model writes (`writing`). */
function working(upTo: "prompt" | "writing"): { events: P.Event[]; live: P.Event[] } {
  const w = firstTurn(new Writer(31, { project: PROJECT })).turn(SECOND);
  const before = w.events.length;
  reading(w, 12400, 0);
  if (upTo === "writing") writing(w, 12400, 0);
  return { events: w.events.slice(0, before), live: w.events.slice(before) };
}

/** What the user wrote while the second turn ran: the first reached the model before its
 * second step; the next waits in the queue while the turn runs on. */
const STEERED = "И добавь пример с -v под таблицей.";
const QUEUED = "Потом проверь, что ссылки в README живые.";

function steered(): { events: P.Event[]; live: P.Event[] } {
  const w = readme(firstTurn(new Writer(31, { project: PROJECT })).turn(SECOND))
    .step()
    .steer(STEERED);
  const before = w.events.length;
  writing(reading(w, 12900, 12300), 12900, 12300);
  return { events: w.events.slice(0, before), live: w.events.slice(before) };
}

const README_CHANGE: P.FileChange = {
  path: "README.md",
  created: false,
  hunks: [{ old_start: 9, new_start: 9, removed: [], added: ["| `-v, --verbose` | Prints the config's path to stderr |"] }],
};

/** The second turn writes its edit and answers, and ends before its next step: what the
 * user sent into it meanwhile (the scenario's "Send now") never reached the model, and the
 * core gives it back (`undelivered`), for the page to send as the next turn. */
function undelivered(): { events: P.Event[]; live: P.Event[] } {
  const w = writing(readme(firstTurn(new Writer(31, { project: PROJECT })).turn(SECOND)).step(), 12900, 12300);
  const before = w.events.length;
  const read = { prompt_total: 12900, prompt_cached: 12300, prompt_done: 12900 };
  w.progress("writing", { ...read, generated: 520, tokens_per_second: 34.4, elapsed_ms: 19800 })
    .progress("writing", { ...read, generated: 596, tokens_per_second: 34.5, elapsed_ms: 22000 })
    .reasoned()
    .call(
    "edit",
    {
      name: "Edit",
      path: "README.md",
      old: "| `-c, --config <path>` | The config file |",
      new: "| `-c, --config <path>` | The config file |\n| `-v, --verbose` | Prints the config's path to stderr |",
      replace_all: false,
    },
    { text: "[edit: applied exact] README.md — lines 8-10 now:" },
    [README_CHANGE],
  )
    .step()
    .message("Готово: в таблице Usage появилась строка для `-v, --verbose`, теми же словами, что и остальные.")
    .step("answer", 90)
    .end("answer", "NOT verified: README.md changed after the last passing build or test; no build or test ran after it");
  return { events: w.events.slice(0, before), live: w.events.slice(before) };
}

/** The second turn waits: the model would edit Cargo's config of the user, outside the
 * project. */
function editAsks(): P.Event[] {
  return firstTurn(new Writer(31, { project: PROJECT }))
    .turn("Собирай проект с -C target-cpu=native.")
    .reasoning("A rustflags entry in Cargo's config does that; the user's own is in ~/.cargo, outside the project.")
    .call("edit", {
      name: "Edit",
      path: "C:/Users/me/.cargo/config.toml",
      old: "[build]\n",
      new: '[build]\nrustflags = ["-C", "target-cpu=native"]\n',
      replace_all: false,
    }).events;
}

// ---------------------------------------------------------------- a turn that ended

const TILDE = "Пусть --config понимает ~ в начале пути.";

const EXPAND_HOME = [
  "/// The path with a leading `~` as the user's home directory.",
  "fn expand_home(path: PathBuf) -> PathBuf {",
  '    match path.strip_prefix("~") {',
  "        Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),",
  "        Err(_) => path,",
  "    }",
  "}",
  "",
];

const HOME_TEST = [
  "use std::process::Command;",
  "",
  "#[test]",
  "fn a_tilde_is_the_home_directory() {",
  '    let home = dirs::home_dir().expect("a home directory");',
  '    std::fs::write(home.join(".proj-test.toml"), "greeting = \\"hi\\"").unwrap();',
  '    let out = Command::new(env!("CARGO_BIN_EXE_proj"))',
  '        .args(["--config", "~/.proj-test.toml"])',
  "        .output()",
  "        .unwrap();",
  "    assert!(out.status.success());",
  "}",
];

const TILDE_CHANGED: P.ChangedFile[] = [
  {
    path: "src/main.rs",
    created: false,
    deleted: false,
    drifted: false,
    hunks: [
      { old_start: 15, new_start: 15, removed: [], added: EXPAND_HOME },
      {
        old_start: 17,
        new_start: 25,
        removed: ["    let config = config::load(args.config.as_deref())?;"],
        added: ["    let path = args.config.map(expand_home);", "    let config = config::load(path.as_deref())?;"],
      },
    ],
  },
  { path: "tests/home.rs", created: true, deleted: false, drifted: false, hunks: [{ old_start: 0, new_start: 1, removed: [], added: HOME_TEST }] },
];

/** A session of one turn that changed two files, and ended as `end` says: its answer with
 * the summary under it, verified or not, or a turn the step limit stopped. */
function tilde(end: "verified" | "unverified" | "stopped"): P.Event[] {
  const w = new Writer(32, { project: PROJECT })
    .mode("auto")
    .turn(TILDE)
    .reasoning("Expand a leading ~ in --config before the file is read: main takes the path from Args.")
    .call(
      "read",
      { name: "Read", path: "src/main.rs", offset: null, limit: null },
      { text: `[read: src/main.rs — lines 1-21 of 21 lines, 452 B, ~130 tokens]\n${numbered(MAIN_RS)}` },
    )
    .step()
    .call(
      "edit",
      { name: "Edit", path: "src/main.rs", old: "fn main() -> anyhow::Result<()> {", new: `${EXPAND_HOME.join("\n")}\nfn main() -> anyhow::Result<()> {`, replace_all: false },
      { text: "[edit: applied exact] src/main.rs — lines 13-24 now:" },
      [{ path: "src/main.rs", created: false, hunks: [{ old_start: 15, new_start: 15, removed: [], added: EXPAND_HOME }] }],
    )
    .call(
      "edit",
      {
        name: "Edit",
        path: "src/main.rs",
        old: "    let config = config::load(args.config.as_deref())?;",
        new: "    let path = args.config.map(expand_home);\n    let config = config::load(path.as_deref())?;",
        replace_all: false,
      },
      { text: "[edit: applied exact] src/main.rs — lines 23-28 now:" },
      [{ path: "src/main.rs", created: false, hunks: TILDE_CHANGED[0]!.hunks.slice(1) }],
    )
    .call(
      "write",
      { name: "Write", path: "tests/home.rs", content: HOME_TEST.join("\n") },
      { text: "[write: tests/home.rs — a new file, 12 lines, 431 B]" },
      [{ path: "tests/home.rs", created: true, hunks: TILDE_CHANGED[1]!.hunks }],
    )
    .step();
  if (end === "stopped") {
    return w.end(
      "step_limit",
      "NOT verified: src/main.rs, tests/home.rs changed after the last passing build or test; no build or test ran after it",
      "the step limit (40) came first",
    ).events;
  }
  const passed = end === "verified";
  return w
    .call(
      "shell",
      { name: "Bash", command: "cargo test", timeout_ms: null },
      passed
        ? {
            text: "[shell: exit 0, 2.4 s]\n   Compiling proj v0.1.0 (C:\\Users\\me\\proj)\n    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.21s\n     Running tests\\home.rs (target\\debug\\deps\\home-9b41c2.exe)\n\nrunning 1 test\ntest a_tilde_is_the_home_directory ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s",
            exit: 0,
            ms: 2400,
          }
        : {
            text: "[shell: exit 101, 2.6 s]\n   Compiling proj v0.1.0 (C:\\Users\\me\\proj)\nerror[E0433]: failed to resolve: use of undeclared crate or module `dirs`\n  --> src\\main.rs:18:19\n   |\n18 |         Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),\n   |                     ^^^^ use of undeclared crate or module `dirs`\n\nerror: could not compile `proj` (bin \"proj\") due to 1 previous error",
            exit: 101,
            ms: 2600,
          },
    )
    .step()
    .message(
      passed
        ? "Готово: `--config ~/app.toml` теперь читает файл из домашнего каталога. `expand_home` в `main.rs` заменяет `~` в начале пути, остальные пути не меняются; тест `tests/home.rs` это проверяет."
        : "Сделал `expand_home` и тест, но сборка не проходит: в проекте нет крейта `dirs`. Добавить его в `Cargo.toml`, или взять домашний каталог из `HOME` и `USERPROFILE` без зависимости?",
    )
    .step("answer", 150)
    .end(
      "answer",
      passed
        ? "verified: `cargo test` passed after the last change"
        : "NOT verified: src/main.rs, tests/home.rs changed after the last passing build or test; `cargo test` exited 101",
    ).events;
}

const tildeThread = (over: Partial<P.ThreadSummary> = {}) => summary(32, PROJECT, TILDE, { turns: 1, unreviewed: 2, ...over });

/** What the gallery clicks for the model's popover: the composer's model pill. */
const MODEL_PILL = 'button.pill.model, button[aria-haspopup][aria-label^="Model"]';

const prompting = working("prompt");
const writes = working("writing");
const steering = steered();
const ending = undelivered();

export const SCENARIOS: Record<string, Scenario> = {
  // the user has load() of config.rs selected, and cargo test just failed in a terminal
  welcome: {
    folders: [PROJECT],
    threads: [...welcomeThreads, TALLY],
    detected: DETECTED,
    editor: CONFIG_EDITOR,
    failures: [TEST_FAILED],
  },
  // the user opens main.rs once the page is up, and the docs' check fails in a terminal
  "welcome-file": {
    folders: [PROJECT],
    threads: welcomeThreads.slice(1, 3),
    detected: DETECTED,
    extension: [
      { at: 200, message: { t: "editor", editor: MAIN_EDITOR } },
      { at: 350, message: { t: "failures", failures: [DOCS_FAILED, TEST_FAILED] } },
    ],
  },
  // the first session of all: no file open, no failure, nothing before
  "welcome-empty": { folders: [PROJECT], threads: [], detected: DETECTED },
  // a suggestion of the empty state taken: a new session starts with the file as a chip,
  // the model reads and writes, and the session's notes wait by the ring
  "first-message": {
    folders: [PROJECT],
    threads: welcomeThreads,
    detected: DETECTED,
    editor: MAIN_EDITOR,
    pause: 4,
    extension: [
      {
        at: 300,
        message: {
          t: "prompt",
          text: "Explain src/config.rs: what it does and how it works.",
          chips: [{ kind: "file", path: "src/config.rs", abs: `${PROJECT}/src/config.rs` }],
          send: true,
        },
      },
    ],
  },
  "welcome-down": { folders: [PROJECT], threads: [], detected: "failed" },
  "no-folder": { folders: [], threads: [], detected: DETECTED },
  // the model's popover in the composer, over the empty state, on llama-server's router
  "model-popover": {
    folders: [PROJECT],
    threads: welcomeThreads,
    detected: ROUTER,
    session: { model: "Qwen3.8-27B-Q4_K_M" },
    editor: CONFIG_EDITOR,
    failures: [TEST_FAILED],
    click: MODEL_PILL,
  },
  // the server reads a prompt whose cache is gone: 63% of 12.4K when the picture is taken
  "progress-prompt": {
    folders: [PROJECT],
    threads: [sessionThread({ running: true })],
    events: prompting.events,
    live: prompting.live,
    every: 250,
    show: 31,
    detected: DETECTED,
    changes: CHANGED,
  },
  // the server reads the prompt, the model reasons, then writes the README's edit
  "progress-writing": {
    folders: [PROJECT],
    threads: [sessionThread({ running: true })],
    events: writes.events,
    live: writes.live,
    every: 90,
    show: 31,
    detected: DETECTED,
    changes: CHANGED,
  },
  // what the user wrote at the first step reached the model; a message from elsewhere in
  // VS Code comes while the turn runs on, and waits in the queue
  queued: {
    folders: [PROJECT],
    threads: [sessionThread({ running: true })],
    events: steering.events,
    live: steering.live,
    every: 90,
    show: 31,
    detected: DETECTED,
    changes: CHANGED,
    extension: [{ at: 900, message: { t: "prompt", text: QUEUED, chips: [], send: true } }],
  },
  // a message queued while the turn runs is sent into it ("Send now"), and the turn ends
  // before the model got it: the core gives it back, and it goes as the next turn
  undelivered: {
    folders: [PROJECT],
    threads: [sessionThread({ running: true })],
    events: ending.events,
    live: ending.live,
    every: 90,
    show: 31,
    detected: DETECTED,
    changes: [...CHANGED, { ...README_CHANGE, deleted: false, drifted: false }],
    extension: [{ at: 300, message: { t: "prompt", text: STEERED, chips: [], send: true } }],
    click: ".queued button.btn",
    clickAt: 650,
  },
  // an edit outside the project asks, in the card above the composer
  "approval-edit": {
    folders: [PROJECT],
    threads: [sessionThread({ running: true, turns: 2 })],
    events: editAsks(),
    show: 31,
    detected: DETECTED,
    changes: CHANGED,
    asks: [
      {
        method: "approval/ask",
        params: {
          thread: 31,
          item: null,
          tool: "edit",
          action: "C:/Users/me/.cargo/config.toml",
          reason: "a protected path or outside the project",
          unreviewed: [],
          agent: null,
        },
      },
    ],
  },
  // a turn's end: its answer, the verdict of the facts, the files to review
  "summary-verified": { folders: [PROJECT], threads: [tildeThread()], events: tilde("verified"), show: 32, detected: DETECTED, changes: TILDE_CHANGED },
  "summary-unverified": { folders: [PROJECT], threads: [tildeThread()], events: tilde("unverified"), show: 32, detected: DETECTED, changes: TILDE_CHANGED },
  stopped: { folders: [PROJECT], threads: [tildeThread()], events: tilde("stopped"), show: 32, detected: DETECTED, changes: TILDE_CHANGED },
  session: { folders: [PROJECT], threads: [sessionThread()], events: shown.events, show: 31, detected: DETECTED, changes: CHANGED },
  // the window has the project open by its 8.3 name, as a Windows runner's %TEMP% is: the
  // core keeps the sessions by the long one, and the page finds them all the same
  linked: {
    folders: ["C:/Users/ME~1/proj"],
    roots: { "C:/Users/ME~1/proj": PROJECT },
    threads: [sessionThread(), TALLY],
    events: shown.events,
    show: 31,
    detected: DETECTED,
    changes: CHANGED,
    editor: { ...CONFIG_EDITOR, abs: "C:/Users/ME~1/proj/src/config.rs" },
  },
  running: {
    folders: [PROJECT],
    threads: [sessionThread({ running: true, turns: 2 })],
    events: drafting.events,
    live: drafting.live,
    every: 120,
    show: 31,
    detected: DETECTED,
  },
  approval: {
    folders: [PROJECT],
    threads: [sessionThread({ running: true, turns: 2 })],
    events: asking.events,
    show: 31,
    detected: DETECTED,
    asks: [
      {
        method: "approval/ask",
        params: {
          thread: 31,
          item: null,
          tool: "shell",
          action: "npm run docs:check",
          reason: "it runs project code while there are unreviewed changes",
          unreviewed: ["src/main.rs", "README.md"],
          agent: null,
        },
      },
    ],
  },
  stream: {
    folders: ["/home/user/proj"],
    threads: [summary(11, "/home/user/proj", "Say hello instead of hi, and check the README says the same.", { mode: "edit_automatically" })],
    events: STREAM,
    show: 11,
    detected: DETECTED,
  },
  "qwen-rename": recorded(21, qwenRename),
  "gemma-build": recorded(22, gemmaFailingBuild),
  "gpt-oss-escaping": recorded(23, gptOssEscaping),
  "qwen-long-file": recorded(24, qwenLongFile),
  "settings-model": { folders: [PROJECT], threads: [], detected: DETECTED, settings: "model" },
  "settings-mcp": { folders: [PROJECT], threads: [sessionThread()], events: shown.events, show: 31, detected: DETECTED, settings: "mcp" },
  "settings-instructions": {
    folders: [PROJECT],
    threads: [sessionThread()],
    events: shown.events,
    show: 31,
    detected: DETECTED,
    settings: "instructions",
  },
};
