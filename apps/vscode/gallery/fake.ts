// The extension and the core, faked for the gallery: the page of src/webview/ runs as it
// does in VS Code's webview, on a scenario's sessions (scenarios.ts), with the colours VS
// Code puts on a webview (themes/, taken by capture.mjs). `?s=` names the scenario,
// `?theme=light` the light theme (`hc`, `hc-light`: the high-contrast ones), `?place=editor`
// the panel as an editor tab shows it.
//
// The fake speaks as the real ones do (src/shared/protocol.ts, src/shared/messages.ts):
// the hello with the user's file and failed commands, then the scenario's questions, live
// events and messages of the extension. It runs no model: a message the page sends plays
// a short turn of progress and an answer; what the user writes meanwhile reaches it
// (`turn/steer`), or comes back undelivered. What the page changes (sessions, the review
// of changes, the settings of a new session) holds for the page's life.

import type { FromPage, InstructionFile, NewSession, Requests, ToPage } from "../src/shared/messages.ts";
import profiles from "../src/shared/profiles.json";
import * as P from "../src/shared/protocol.ts";
import { samePath } from "../src/shared/text.ts";
import { FILES, SCENARIOS } from "./scenarios.ts";
import dark from "./themes/dark.json";
import hc from "./themes/hc.json";
import hcLight from "./themes/hc-light.json";
import light from "./themes/light.json";

const THEMES: Record<string, typeof dark> = { dark, light, hc, "hc-light": hcLight };
const query = new URLSearchParams(location.search);
const theme = THEMES[query.get("theme") ?? "dark"] ?? dark;
const scenario = SCENARIOS[query.get("s") ?? "welcome"] ?? SCENARIOS.welcome!;
const place = query.get("place") === "editor" ? "editor" : (scenario.place ?? "sidebar");

// the theme, as VS Code puts it on a webview's page
document.documentElement.setAttribute("style", theme.style);
document.body.className = theme.bodyClass;
for (const [k, v] of Object.entries(theme.bodyAttrs)) document.body.setAttribute(k, v);
const defaults = document.createElement("style");
defaults.id = "_defaultStyles";
defaults.textContent = theme.defaultStyles;
document.head.prepend(defaults);

const toPage = (m: ToPage) => window.postMessage(m, "*");
const fromCore = (m: object) => toPage({ t: "core", data: JSON.stringify({ jsonrpc: "2.0", ...m }) });

const project = scenario.folders[0] ?? "";
/** The scenario's events, as the core kept them before the page opened. */
const stored = scenario.events ?? [];
const lastSeq = () => Math.max(0, ...stored.map((e) => e.seq));
/** What the core streamed so far, the stored first: `thread/read` reads it. */
const history: P.Event[] = [...stored];
let seq = Math.max(lastSeq(), ...(scenario.live ?? []).map((e) => e.seq));
/** The sessions, as turns start and end and the page deletes them. */
const threads = scenario.threads.map((t) => ({ ...t }));
/** The unreviewed changes of the thread on show, as the page keeps and undoes them. */
let changes = (scenario.changes ?? []).map((f) => ({ ...f, hunks: [...f.hunks] }));
/** The settings of a new session, as the page changes them. */
const session: NewSession = {
  server: "http://127.0.0.1:8080",
  model: "",
  profile: "",
  reasoning: "",
  context: 0,
  mode: "auto",
  native: true,
  subagents: true,
  ...scenario.session,
};
const PROFILES = profiles as Record<string, { reasoning: { levels: string[]; default: string }; native: boolean }>;

/** What the core keeps of its stream: not what only says how things stand now. */
const kept = (e: P.Event) => e.event !== "progress" && e.event !== "changes_updated";

/** An event of the core's stream, numbered after every other. */
function emit(thread: number, kind: P.EventKind): void {
  // a turn's end gives back what the user wrote that the model never got, as the core's does
  if (kind.event === "turn_completed") {
    const pending = steers.get(thread)?.splice(0) ?? [];
    if (pending.length > 0) kind = { ...kind, undelivered: [...(kind.undelivered ?? []), ...pending] };
  }
  const e = { ...kind, seq: ++seq, thread } as P.Event;
  history.push(e);
  const t = threads.find((s) => s.thread === thread);
  if (t && e.event === "turn_started") Object.assign(t, { running: true, updated: Date.now(), turns: e.turn, title: t.title || firstLine(e.user) });
  if (t && e.event === "turn_completed") Object.assign(t, { running: false, updated: Date.now() });
  fromCore({ method: P.NOTIFY_EVENT, params: e });
}

function firstLine(text: string): string {
  return text.split("\n").map((l) => l.trim()).find((l) => l !== "") ?? "";
}

/** The newest of what `pick` finds in the thread's events. */
function newest(thread: number, pick: (e: P.Event) => number | undefined): number {
  return Math.max(0, ...history.filter((e) => e.thread === thread).map((e) => pick(e) ?? 0));
}

function running(thread: number): boolean {
  const ends = history.filter((e) => e.thread === thread && (e.event === "turn_started" || e.event === "turn_completed"));
  return ends[ends.length - 1]?.event === "turn_started";
}

/** The timers of the turn the fake plays in a thread: an interrupt stops them. */
const playing = new Map<number, ReturnType<typeof setTimeout>[]>();
/** Threads whose turn the user stopped: the scenario's live events of them stop too. */
const stopped = new Set<number>();
/** What the user wrote while a thread's turn ran, until the model gets it. */
const steers = new Map<number, string[]>();

/** What the user wrote meanwhile reaches the model, before its next step. */
function deliverSteers(thread: number): void {
  const waiting = steers.get(thread)?.splice(0) ?? [];
  const turn = newest(thread, (e) => (e.event === "turn_started" ? e.turn : undefined));
  for (const text of waiting) {
    const item = { id: newest(thread, (e) => (e.event === "item_started" ? e.item.id : undefined)) + 1, turn, generation: null, kind: "steer", text } as P.Item;
    emit(thread, { event: "item_started", item });
    emit(thread, { event: "item_completed", item });
  }
}

/** A turn of the page's message: the server reads the prompt, the model writes, it
 * answers. `turn` is its number: the next one, or the one a message written anew takes
 * back. */
function play(thread: number, user: string, turn = newest(thread, (e) => (e.event === "turn_started" ? e.turn : undefined)) + 1): void {
  // the scenario's turn the user stopped stays stopped: its events would land in this one
  emit(thread, { event: "turn_started", turn, user });
  const generation = newest(thread, (e) => (e.event === "generation" ? e.generation.index : undefined)) + 1;
  const total = 6200 + Math.round(user.length / 4);
  const cached = 3900;
  const progress = (phase: P.ProgressPhase, over: Partial<P.Progress>): P.EventKind => ({
    event: "progress",
    progress: { generation, phase, prompt_total: total, prompt_cached: cached, prompt_done: null, generated: 0, elapsed_ms: 0, tokens_per_second: null, ...over },
  });
  const text = "The gallery's core runs no model: in VS Code, the model's answer comes here.";
  const steps: (() => void)[] = [
    () => emit(thread, progress("waiting", { elapsed_ms: 40 })),
    () => emit(thread, progress("prompt", { prompt_done: cached, elapsed_ms: 260 })),
    () => emit(thread, progress("prompt", { prompt_done: cached + Math.round((total - cached) * 0.6), elapsed_ms: 900 })),
    // what the user wrote meanwhile reaches the model before it writes; the answer streams
    () => {
      deliverSteers(thread);
      emit(thread, progress("writing", { prompt_done: total, generated: 9, tokens_per_second: 31.8, elapsed_ms: 1500 }));
      const id = newest(thread, (e) => (e.event === "item_started" ? e.item.id : undefined)) + 1;
      emit(thread, { event: "item_started", item: { id, turn, generation, kind: "agent_message", text: "" } as P.Item });
      emit(thread, { event: "item_delta", item: id, delta: { type: "text", text: text.slice(0, 37) } } as P.EventKind);
    },
    () => {
      const id = newest(thread, (e) => (e.event === "item_started" ? e.item.id : undefined));
      emit(thread, { event: "item_delta", item: id, delta: { type: "text", text: text.slice(37) } } as P.EventKind);
      emit(thread, { event: "item_completed", item: { id, turn, generation, kind: "agent_message", text } as P.Item });
    },
    () =>
      emit(thread, {
        event: "generation",
        generation: {
          index: generation,
          turn,
          purpose: "step",
          outcome: "answer",
          finish: "stop",
          seed: generation,
          prompt_tokens: total,
          cached_tokens: cached,
          completion_tokens: 21,
          prompt_ms: 610,
          generation_ms: 660,
          prompt_hash: `sha256:${String(generation).padStart(64, "0")}`,
        },
      }),
    () => end(thread, turn, "answer", text),
  ];
  playing.set(
    thread,
    steps.slice(0, scenario.pause ?? steps.length).map((step, i) => setTimeout(step, 300 * (i + 1))),
  );
}

/** The turn the fake plays ends (`emit` gives back what the model never got). */
function end(thread: number, turn: number, how: P.TurnEnd, answer: string | null): void {
  for (const timer of playing.get(thread) ?? []) clearTimeout(timer);
  playing.delete(thread);
  emit(thread, { event: "turn_completed", turn, end: how, why: null, answer, verification: null });
}

function changesNow(): P.Changes {
  return { files: changes, commands: [], inherited: false };
}

/** The file or every file (`null`) kept or undone: gone from the review. */
function review(thread: number, path: string | null): string[] {
  const gone = changes.filter((f) => path === null || f.path === path).map((f) => f.path);
  changes = changes.filter((f) => !gone.includes(f.path));
  const t = threads.find((s) => s.thread === thread);
  if (t) t.unreviewed = changes.length;
  emit(thread, { event: "changes_updated" });
  return gone;
}

/** A hunk kept or undone, as the core takes one: the hunk the list gave, or none. */
function reviewHunk(params: P.HunkRef): P.Changes {
  const file = changes.find((f) => f.path === params.path);
  const at = file?.hunks.findIndex((h) => JSON.stringify(h) === JSON.stringify(params.hunk)) ?? -1;
  if (!file || at < 0) throw Object.assign(new Error("the change is not as it was: list the changes again"), { code: P.ERRORS.stale });
  file.hunks.splice(at, 1);
  if (file.hunks.length === 0) review(params.thread, file.path);
  else emit(params.thread, { event: "changes_updated" });
  return changesNow();
}

function opened(thread: number, info: P.ThreadInfo, mode: P.Mode): P.Opened {
  return {
    thread,
    info,
    notes: ["not a clean git tree: builds and tests ask until you accept the changes"],
    answer_now: true,
    context: { working: 65536, compacts_at: 52428, window: 131072 },
    mode,
    vision: false,
  };
}

const MCP: P.McpConfig = {
  files: [
    {
      scope: "user",
      path: "C:/Users/me/.ognivo/mcp.json",
      problem: null,
      servers: [
        // a key of nobody's, for the look of a key that is set
        { name: "exa", value: { command: "npx", args: ["-y", "exa-mcp-server"], env: { EXA_API_KEY: "00000000-0000-4000-8000-00000000c0de" } } },
        { name: "github", value: { type: "http", url: "https://api.githubcopilot.com/mcp/" } },
      ],
    },
    { scope: "project", path: `${project}/.mcp.json`, problem: null, servers: [] },
    { scope: "claude", path: "C:/Users/me/.claude.json", problem: null, servers: [] },
  ],
};

const SERVERS: P.McpServers = {
  servers: [
    {
      name: "github",
      source: "C:/Users/me/.ognivo/mcp.json",
      command: "https://api.githubcopilot.com/mcp/",
      remote: true,
      state: "sign_in",
      why: null,
      signed_in: false,
      instructions: null,
      tools: [],
      prompts: [],
    },
  ],
  problems: [],
};

/** What the core answers. */
function answer(method: string, params: Record<string, unknown>): unknown {
  switch (method) {
    case "initialize":
      return { protocol: P.VERSION, core: "0.0.1", exe: "", seq: lastSeq() };
    case "thread/list": {
      // a folder by its canonical path, as the core takes it
      const given = params.project as string | null;
      const only = given ? (scenario.roots?.[given] ?? given) : given;
      const listed = threads.filter((t) => !only || samePath(t.project) === samePath(only));
      return { threads: listed.slice(0, (params.limit as number | null) ?? 200) };
    }
    case "thread/read": {
      const thread = params.thread as number;
      // a thread and its subagents' threads
      const subagents = new Set(
        history.flatMap((e) => (e.event === "thread_started" && e.info.parent?.thread === thread ? [e.thread] : [])),
      );
      return { events: history.filter((e) => kept(e) && (e.thread === thread || subagents.has(e.thread))) };
    }
    case "thread/trajectory": {
      const trace = scenario.trajectories?.[params.thread as number];
      if (trace) return { trajectory: trace };
      // a session the core kept no events of: its trajectory says where it worked, as the
      // core's does
      const t = threads.find((s) => s.thread === params.thread);
      const ognivo = t ? { project: t.project, profile: t.profile, server: t.server, title: t.title } : undefined;
      return {
        trajectory: {
          schema_version: "ATIF-v1.4",
          session_id: "gallery",
          steps: [{ step_id: 1, source: "system", message: "You are ognivo, a coding agent.", extra: { conversation: 1, reason: "start" } }],
          ...(ognivo ? { extra: { ognivo } } : {}),
        },
      };
    }
    case "thread/start": {
      const p = params as unknown as P.StartThread;
      const thread = Math.max(100, ...threads.map((t) => t.thread)) + 1;
      const detected = scenario.detected && scenario.detected !== "failed" ? scenario.detected : undefined;
      // behind a router, the profile of the model the session asks for
      const routed = detected?.models.find((m) => m.id === p.server.model);
      const profile = p.profile ?? (routed ? routed.profile : detected?.profile) ?? "";
      // as the core's Native driver, which reads some formats only
      if (p.native && PROFILES[profile]?.native === false) {
        throw Object.assign(new Error(`${profile}: the Native path reads gpt-oss's and Qwen's formats so far, not Gemma 4's`), { code: P.ERRORS.notStarted });
      }
      const info: P.ThreadInfo = {
        kind: "chat",
        project: p.project,
        profile,
        level: p.level ?? detected?.default_level ?? null,
        server: p.server.url,
        model: p.server.model,
        parent: null,
      };
      const mode = p.mode ?? "auto";
      threads.unshift({
        thread,
        kind: "chat",
        project: p.project,
        profile: info.profile,
        level: info.level,
        server: info.server,
        model: info.model,
        title: "",
        created: Date.now(),
        updated: Date.now(),
        turns: 0,
        unreviewed: 0,
        open: true,
        running: false,
        answer_now: true,
        context: { working: 65536, compacts_at: 52428, window: 131072 },
        mode,
        vision: false,
      });
      emit(thread, { event: "thread_started", info });
      return opened(thread, info, mode);
    }
    case "thread/resume": {
      const thread = params.thread as number;
      const t = threads.find((s) => s.thread === thread);
      const started = history.find((e) => e.thread === thread && e.event === "thread_started");
      if (!t) throw Object.assign(new Error("no such thread"), { code: P.ERRORS.noThread });
      const info: P.ThreadInfo =
        started?.event === "thread_started"
          ? started.info
          : { kind: t.kind, project: t.project, profile: t.profile, level: t.level, server: t.server, model: t.model, parent: null };
      // as the core's Native driver, which reads some formats only
      if (params.native === true && PROFILES[info.profile]?.native === false) {
        throw Object.assign(new Error(`${info.profile}: the Native path reads gpt-oss's and Qwen's formats so far, not this one's`), { code: P.ERRORS.notStarted });
      }
      t.open = true;
      return opened(thread, info, (params.mode as P.Mode | null) ?? t.mode ?? "auto");
    }
    case "turn/start":
    case "turn/edit": {
      const thread = params.thread as number;
      // a session the core does not have open is picked up first (`thread/resume`)
      if (threads.find((s) => s.thread === thread)?.open === false) {
        throw Object.assign(new Error(`thread ${thread} is not open`), { code: P.ERRORS.noThread });
      }
      if (running(thread)) throw Object.assign(new Error("a turn is running"), { code: P.ERRORS.busy });
      if (method === "turn/edit") {
        const turn = params.turn as number;
        emit(thread, { event: "thread_rewound", turn });
        play(thread, params.text as string, turn);
      } else {
        play(thread, params.text as string);
      }
      return {};
    }
    case "turn/steer": {
      const thread = params.thread as number;
      if (!running(thread)) throw Object.assign(new Error("no turn runs: start one"), { code: P.ERRORS.idle });
      steers.set(thread, [...(steers.get(thread) ?? []), params.text as string]);
      // the model gets it at its next step; a turn of the scenario's has steps of its own
      if (!playing.has(thread)) setTimeout(() => running(thread) && deliverSteers(thread), 700);
      return {};
    }
    case "turn/interrupt": {
      const thread = params.thread as number;
      if (!running(thread)) throw Object.assign(new Error("no turn runs"), { code: P.ERRORS.noTurn });
      stopped.add(thread);
      end(thread, newest(thread, (e) => (e.event === "turn_started" ? e.turn : undefined)), "interrupted", null);
      return {};
    }
    case "thread/mode":
      emit(params.thread as number, { event: "mode_changed", mode: params.mode as P.Mode });
      return {};
    case "thread/vision": {
      // the scenario's server says what it said as the thread opened
      const t = threads.find((s) => s.thread === params.thread);
      if (!t?.open) throw Object.assign(new Error(`thread ${String(params.thread)} is not open`), { code: P.ERRORS.noThread });
      const vision: P.Vision = { vision: t.vision ?? false };
      return vision;
    }
    case "server/detect": {
      const detected = scenario.detected;
      if (detected === "failed" || !detected) throw new Error("the server does not answer: connection refused");
      // behind a router: what the model the params name runs
      const routed = detected.models.find((m) => m.id === params.model);
      if (!routed) return detected;
      const profile = routed.profile ? PROFILES[routed.profile] : undefined;
      // a model the router has not loaded has no slot yet: no window, no word of images
      const loaded = routed.status === "loaded";
      return {
        ...detected,
        model: routed.id,
        profile: routed.profile,
        levels: profile?.reasoning.levels ?? [],
        default_level: profile?.reasoning.default ?? null,
        native: profile?.native ?? false,
        window: loaded ? detected.window : null,
        vision: loaded ? detected.vision : null,
      };
    }
    case "mcp/config":
      return MCP;
    case "mcp/servers":
      return SERVERS;
    // the core lists a thread's changes while a turn runs too, as last saved
    case "changes/list":
      return changesNow();
    case "changes/accept": {
      const accepted = review(params.thread as number, params.path as string | null);
      return { accepted, unreviewed: changes.length > 0 };
    }
    case "changes/revert": {
      const created = changes.find((f) => f.path === params.path)?.created ?? false;
      review(params.thread as number, params.path as string);
      return { removed: created };
    }
    case "changes/accept_hunk":
    case "changes/revert_hunk":
      return reviewHunk(params as unknown as P.HunkRef);
    case "changes/base":
      return { text: null };
    default:
      // what changes the core's state answers as if it did
      return {};
  }
}

/** The workspace's files that match `query` as `files/find` ranks them: each character in
 * order, a run of them and the name itself first. */
function findFiles(query: string, limit = 20): string[] {
  const q = query.toLowerCase();
  const score = (file: string): number | undefined => {
    const f = file.toLowerCase();
    let at = 0;
    let last = -2;
    let sum = 0;
    for (const ch of q) {
      const i = f.indexOf(ch, at);
      if (i < 0) return undefined;
      sum += i === last + 1 ? 2 : 1;
      last = i;
      at = i + 1;
    }
    return f.slice(f.lastIndexOf("/") + 1).includes(q) ? sum + 5 : sum;
  };
  return (scenario.files ?? FILES)
    .flatMap((f) => {
      const s = score(f);
      return s === undefined ? [] : [{ f, s }];
    })
    .sort((a, b) => b.s - a.s || a.f.length - b.f.length)
    .slice(0, limit)
    .map((x) => x.f);
}

/** What the extension answers. */
function request(method: keyof Requests, params: Record<string, unknown>): unknown {
  switch (method) {
    case "exa/check":
      return { ok: true, says: "Exa takes the key: searches work" };
    case "files/find":
      return findFiles(params.query as string, params.limit as number | undefined);
    // the review tab and the agent's terminal are VS Code's: nothing to show here
    case "review/open":
    case "terminal/show":
      console.log(`the extension would show ${method === "review/open" ? "the review" : "the agent's commands"}:`, params);
      return null;
    // as if the user agreed in VS Code's dialog
    case "changes/accept":
    case "changes/revert":
      review(params.thread as number, params.path as string | null);
      return true;
    case "settings/set":
      Object.assign(session, { [params.key as string]: params.value });
      setTimeout(() => toPage({ t: "session", session: { ...session } }), 30);
      return null;
    // as the extension keeps it: the page learns only whether a key is kept
    case "server/key":
      session.key = Boolean(params.key);
      setTimeout(() => {
        toPage({ t: "session", session: { ...session } });
        toPage({ t: "keys" });
      }, 30);
      return null;
    case "sessions/delete": {
      const thread = params.thread as number;
      // a session deleted already is not there to delete again
      const at = threads.findIndex((t) => t.thread === thread);
      if (at < 0) return false;
      threads.splice(at, 1);
      setTimeout(() => toPage({ t: "deleted", thread }), 50);
      return true;
    }
    case "instructions/list": {
      const files: InstructionFile[] = [
        { scope: "user", path: "C:/Users/me/.ognivo/AGENTS.md", shown: "~/.ognivo/AGENTS.md", exists: true, bytes: 61, text: "- Отвечай по-русски.\n- C и C++ собирай MSVC: `cl -nologo -W4 -EHsc main.cpp`.\n" },
        { scope: "claude", path: "C:/Users/me/.claude/CLAUDE.md", shown: "~/.claude/CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/AGENTS.md`, shown: "AGENTS.md", exists: true, bytes: 1843 },
        { scope: "project", path: `${project}/CLAUDE.md`, shown: "CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/.claude/CLAUDE.md`, shown: ".claude/CLAUDE.md", exists: false, bytes: 0 },
        { scope: "project", path: `${project}/CLAUDE.local.md`, shown: "CLAUDE.local.md", exists: false, bytes: 0 },
      ];
      return files;
    }
    default:
      return null;
  }
}

/** A click as a pointer makes one, for what opens on a press as well as on a click. */
function press(selector: string): void {
  const target = document.querySelector<HTMLElement>(selector);
  if (!target) {
    console.warn(`the gallery found nothing to click: ${selector}`);
    return;
  }
  for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup"]) {
    const init = { bubbles: true, cancelable: true, view: window, button: 0 };
    target.dispatchEvent(type.startsWith("pointer") ? new PointerEvent(type, { ...init, pointerType: "mouse", isPrimary: true }) : new MouseEvent(type, init));
  }
  target.click();
}

/** What follows the page's hello: the scenario's tab, questions, live events, the
 * extension's messages, and its click. */
function afterInitialize(): void {
  if (scenario.settings) setTimeout(() => toPage({ t: "settings", tab: scenario.settings }), 50);
  scenario.asks?.forEach((a, i) => setTimeout(() => fromCore({ id: 9000 + i, method: a.method, params: a.params }), 250));
  // numbered as they come, after what the fake's turns streamed meanwhile
  (scenario.live ?? []).forEach((e, i) =>
    setTimeout(
      () => {
        if (!stopped.has(e.thread)) emit(e.thread, e);
      },
      400 + i * (scenario.every ?? 150),
    ),
  );
  for (const { at, message } of scenario.extension ?? []) setTimeout(() => toPage(message), at);
  const click = scenario.click;
  if (click) setTimeout(() => press(click), scenario.clickAt ?? 1000);
}

(window as unknown as { acquireVsCodeApi: () => unknown }).acquireVsCodeApi = () => ({
  postMessage(m: FromPage) {
    switch (m.t) {
      case "ready": {
        const state: Record<string, string> = {};
        if (scenario.show !== undefined) state[`ognivo.last.${samePath(project)}`] = String(scenario.show);
        toPage({
          t: "hello",
          folders: scenario.folders,
          state,
          place,
          session: { ...session },
          editor: scenario.editor ?? null,
          failures: scenario.failures ?? [],
        });
        break;
      }
      case "core": {
        const r = JSON.parse(m.data) as { id?: number; method?: string; params?: Record<string, unknown>; result?: unknown };
        if (r.method === undefined || r.id === undefined) {
          console.log("answer of the page:", r);
          break;
        }
        try {
          fromCore({ id: r.id, result: answer(r.method, r.params ?? {}) });
        } catch (e) {
          const code = (e as { code?: number }).code ?? -32000;
          fromCore({ id: r.id, error: { code, message: e instanceof Error ? e.message : String(e) } });
        }
        if (r.method === "initialize") afterInitialize();
        break;
      }
      case "request":
        toPage({ t: "response", id: m.id, result: request(m.method, (m.params ?? {}) as Record<string, unknown>) });
        break;
      default:
        console.log("the page says:", m);
    }
  },
});
