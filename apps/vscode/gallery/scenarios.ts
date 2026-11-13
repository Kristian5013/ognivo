// The gallery's states of the panel: before a session, sessions of the fixtures the tests
// read, and sessions written here to show each part of the panel at once, in the user's
// language: a finished turn with its reads, changes, commands and answer, and a turn that
// runs, with the call the model writes, or the question it waits on.

import type { NewSession, SettingsTab } from "../src/shared/messages.ts";
import type * as P from "../src/shared/protocol.ts";

import gemmaFailingBuild from "../fixtures/gemma.failing-build.json";
import gptOssEscaping from "../fixtures/gpt-oss.escaping.json";
import qwenLongFile from "../fixtures/qwen.long-file.json";
import qwenRename from "../fixtures/qwen.rename.json";
import stream from "../fixtures/stream.jsonl";

export interface Scenario {
  folders: string[];
  session?: Partial<NewSession>;
  threads: P.ThreadSummary[];
  events?: P.Event[];
  trajectories?: Record<number, unknown>;
  /** The thread the page shows, as if it showed it last. */
  show?: number;
  detected?: P.Detected | "failed";
  asks?: { method: string; params: unknown }[];
  settings?: SettingsTab;
  /** The unreviewed changes of the thread on show; none while a turn runs. */
  changes?: P.ChangedFile[];
  /** Events that come after the page opened, one every `every` ms. */
  live?: P.Event[];
  every?: number;
  place?: "sidebar" | "editor";
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

  end(end: P.TurnEnd = "answer", verification: string | null = null, why: string | null = null): this {
    return this.push({ event: "turn_completed", turn: this.turnNo, end, why, answer: null, verification } as P.EventKind);
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

const STREAM: P.Event[] = (stream as string)
  .split("\n")
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l) as P.Event);

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

export const SCENARIOS: Record<string, Scenario> = {
  welcome: { folders: [PROJECT], threads: welcomeThreads, detected: DETECTED },
  "welcome-down": { folders: [PROJECT], threads: [], detected: "failed" },
  "no-folder": { folders: [], threads: [], detected: DETECTED },
  session: { folders: [PROJECT], threads: [sessionThread()], events: shown.events, show: 31, detected: DETECTED, changes: CHANGED },
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
