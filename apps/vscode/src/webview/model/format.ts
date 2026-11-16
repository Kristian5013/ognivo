import type { Progress } from "../../shared/protocol.ts";
import type { Call, ToolItem } from "./model.ts";
import { relativeTo } from "./paths.ts";
import type { Draft, Live } from "./store.ts";

export function seconds(ms: number): string {
  return ms < 10_000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.round(ms / 1000)} s`;
}

/** Tokens in few figures: `418`, `1.2K`, `12.4K`, `31K`, `128K`, `1.2M`. */
export function short(n: number): string {
  if (n < 1000) return String(Math.round(n));
  const [value, unit] = n < 1_000_000 ? [n / 1000, "K"] : [n / 1_000_000, "M"];
  return `${value < 100 ? value.toFixed(1).replace(/\.0$/, "") : Math.round(value)}${unit}`;
}

/** Time gone by as a clock shows it: `0:07`, `2:14`, `1:02:03`. */
export function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const mm = Math.floor(s / 60) % 60;
  const ss = String(s % 60).padStart(2, "0");
  return s < 3600 ? `${Math.floor(s / 60)}:${ss}` : `${Math.floor(s / 3600)}:${String(mm).padStart(2, "0")}:${ss}`;
}

/** Tokens a second: whole ones, tenths below ten. */
export function speed(perSecond: number): string {
  return `${perSecond < 10 ? perSecond.toFixed(1) : Math.round(perSecond)} tok/s`;
}

/** Like `seconds`, but milliseconds under one: a prefill from the cache takes 30 ms. */
export function duration(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : seconds(ms);
}

export function count(n: number): string {
  return n < 10_000 ? String(n) : `${(n / 1000).toFixed(n < 100_000 ? 1 : 0)}k`;
}

export function percent(part: number, whole: number): string {
  return whole ? `${Math.round((part / whole) * 100)}%` : "—";
}

/** The line a call gets in the thread: what it works on. */
export function summary(call: Call): string {
  switch (call.name) {
    case "Read":
      return call.offset && call.offset > 1 ? `${call.path} from line ${call.offset}` : call.path;
    case "Edit":
      return call.replaceAll ? `${call.path} · every occurrence` : call.path;
    case "Write": {
      // as wc -l, but a last line without its newline counts too
      const n = call.content ? call.content.split("\n").length - (call.content.endsWith("\n") ? 1 : 0) : 0;
      return `${call.path} · ${n} ${n === 1 ? "line" : "lines"}`;
    }
    case "Bash":
      return `$ ${call.command.split("\n")[0]}`;
    case "Grep":
      return `/${call.pattern}/${call.path ? ` in ${call.path}` : ""}${call.glob ? ` · ${call.glob}` : ""}`;
    case "Glob":
      return `${call.pattern}${call.path ? ` in ${call.path}` : ""}`;
    case "ApplyPatch": {
      const files = patchFiles(call.patch);
      return files.length ? files.join(", ") : "a patch";
    }
    case "Task":
      return `${call.tasks.length} ${call.tasks.length === 1 ? "task" : "tasks"}: ${call.tasks.map((t) => t.title).join(", ")}`;
    case "Mcp":
      return `${call.server} · ${call.tool}`;
    case "ToolSearch":
      return `«${call.query}»`;
    case "Skill":
      return call.arguments?.trim() ? `${call.skill} «${call.arguments}»` : call.skill;
  }
}

/** The tool's own first line, `[read: … — lines 1-11 of 11 lines, …]`, without its name. */
export function header(item: ToolItem): string | undefined {
  const first = item.result?.text.split("\n", 1)[0] ?? "";
  const m = /^\[(?:read|edit|write|grep|glob): (.*)\]$/.exec(first);
  return m?.[1];
}

/** A result without the header line, which the card shows itself. */
export function body(item: ToolItem): string {
  const text = item.result?.text ?? "";
  return /^\[(read|edit|write|grep|glob|shell|mcp)[: ]/.test(text)
    ? text.slice(text.indexOf("\n") + 1 || text.length)
    : text;
}

/** How a turn ended, as a person would say it. */
export function ending(end: string | undefined): string {
  switch (end) {
    case "answer":
      return "answered";
    case "step_limit":
      return "stopped at the step limit";
    case "time_limit":
      return "stopped at the time limit";
    case "stuck":
      return "stopped: the model was stuck";
    case "error":
      return "failed";
    default:
      return end ?? "running";
  }
}

/** Lines a change adds and removes, by a longest common subsequence of its lines; plain
 * counts when the two sides are too long for one. */
export function lineDelta(before: string, after: string): { added: number; removed: number } {
  const a = before ? before.split("\n") : [];
  const b = after ? after.split("\n") : [];
  if (a.length * b.length > 250_000) return { added: b.length, removed: a.length };
  let prev = new Array<number>(b.length + 1).fill(0);
  for (const line of a) {
    const row = new Array<number>(b.length + 1).fill(0);
    for (let j = 0; j < b.length; j++) {
      row[j + 1] = line === b[j] ? prev[j]! + 1 : Math.max(prev[j + 1]!, row[j]!);
    }
    prev = row;
  }
  const common = prev[b.length]!;
  return { added: b.length - common, removed: a.length - common };
}

/** Why a call waited for an approval, from ognivo's refusal: the reason it asked for one,
 * or the rule that denied it. */
export function refusalReason(text: string): string {
  const asked = /needs the user's approval \(([^)]*)\)/.exec(text);
  if (asked) return asked[1]!;
  const rule = /is denied by the rule (.+?)\. Do not/.exec(text);
  return rule ? `denied by the rule ${rule[1]}` : text.replace(/^\[refused\]\s*/, "");
}

/** `n` of a thing, in the plural where it is more than one: `1 file`, `3 searches`. */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The files a patch of gpt-oss's `apply_patch` (V4A) adds, updates or deletes. */
export function patchFiles(patch: string): string[] {
  return [...patch.matchAll(/^\*\*\* (?:Add|Update|Delete) File: (.+)$/gm)].map((m) => m[1]!.trim());
}

/** What an exploration's calls looked at, in few words: `3 files, 1 search`. */
export function explored(calls: readonly ToolItem[]): string {
  const files = new Set<string>();
  let searches = 0;
  let servers = 0;
  let skills = 0;
  for (const item of calls) {
    const c = item.call;
    switch (c?.name) {
      case "Read":
        files.add(c.path);
        break;
      case "Grep":
      case "Glob":
      case "ToolSearch":
        searches++;
        break;
      case "Mcp":
        servers++;
        break;
      case "Skill":
        skills++;
        break;
    }
  }
  const said = [
    files.size > 0 && plural(files.size, "file"),
    searches > 0 && plural(searches, "search", "searches"),
    servers > 0 && plural(servers, "MCP call"),
    skills > 0 && plural(skills, "skill"),
  ];
  return said.filter((s): s is string => Boolean(s)).join(", ");
}

/** A draft's argument by ognivo's name, as far as it came. */
export function field(draft: Draft, name: string): string | undefined {
  return draft.fields.find(([n]) => n === name)?.[1];
}

/** What a call being written writes, as the line at the conversation's end shows its last
 * lines: a file's new text and the file, a patch, a command. None for the other calls,
 * whose arguments are a few words. */
export function draftText(draft: Draft): { text: string; kind: "file" | "patch" | "command"; path?: string } | undefined {
  const of = (name: string, kind: "file" | "patch" | "command") => {
    const text = field(draft, name);
    return text ? { text, kind, path: field(draft, "path") } : undefined;
  };
  switch (draft.tool) {
    case "Edit":
      return of("new", "file");
    case "Write":
      return of("content", "file");
    case "ApplyPatch":
      return of("patch", "patch");
    case "Bash":
      return of("command", "command");
    default:
      return undefined;
  }
}

/** What the line at the end of a running turn says: what happens, then its facts. */
export interface Saying {
  /** `Thinking`, `Writing src/parse.rs`, `Running cargo test`, `Waiting for your answer`. */
  say: string;
  /** `418 tokens`, `34 tok/s`, `63% of 12.4K`, and the turn's time, `0:12`. */
  facts: string[];
  /** `ask` while a question waits for the user's answer. */
  tone: "work" | "ask";
  /** The call being written, whose last lines show under the line. */
  writing?: Draft;
}

/** What a running turn does, by what weighs most: a question that waits for the user, a
 * call that runs, a summary being written, the generation's progress, a call being
 * written, text that streams. `now` and the turn's start are the page's clock. */
export function saying(live: Live, opts: { asking: boolean; now: number; project: string }): Saying {
  const rel = (path: string) => relativeTo(opts.project, path);
  const p = live.progress;
  const draft = [...live.drafts].reverse().find((d) => d.state === "writing")?.draft;
  let said: Omit<Saying, "facts"> & { facts?: string[] };
  if (opts.asking || live.asking) said = { say: "Waiting for your answer", tone: "ask" };
  else if (live.calls.length) said = { say: runs(live.calls.at(-1)!, rel), tone: "work" };
  else if (live.compacting) said = { say: "Compacting the conversation", tone: "work", facts: p && generating(p) };
  else if (p) said = phase(p, draft, live.streaming, rel);
  else if (draft) said = { say: writes(draft, live.streaming, rel), tone: "work", writing: draft };
  else if (live.streaming === "reasoning") said = { say: "Thinking", tone: "work" };
  else if (live.streaming === "message") said = { say: "Writing the reply", tone: "work" };
  else said = { say: "Working", tone: "work" };
  const facts = [...(said.facts ?? [])];
  if (live.since !== undefined) facts.push(clock(opts.now - live.since));
  return { ...said, facts };
}

function phase(
  p: Progress,
  draft: Draft | undefined,
  streaming: Live["streaming"],
  rel: (path: string) => string,
): Omit<Saying, "facts"> & { facts?: string[] } {
  switch (p.phase) {
    case "waiting":
      return { say: "Waiting for the model", tone: "work" };
    case "prompt": {
      const read = reading(p);
      return { say: "Reading the conversation", tone: "work", facts: read ? [read] : [] };
    }
    case "reasoning":
      return { say: "Thinking", tone: "work", facts: generating(p) };
    case "writing":
      return { say: writes(draft, streaming, rel), tone: "work", facts: generating(p), writing: draft };
  }
}

/** How much of the prompt the server read: of what it did not have in its cache. */
function reading(p: Progress): string | undefined {
  if (p.prompt_total === null) return undefined;
  const cached = p.prompt_cached ?? 0;
  const todo = Math.max(0, p.prompt_total - cached);
  if (todo === 0) return `${short(p.prompt_total)} from the cache`;
  const done = Math.max(0, (p.prompt_done ?? cached) - cached);
  return `${Math.min(100, Math.round((done / todo) * 100))}% of ${short(todo)}`;
}

/** What the generation wrote so far, and how fast. */
function generating(p: Progress): string[] {
  const out: string[] = [];
  if (p.generated > 0) out.push(`${short(p.generated)} ${p.generated === 1 ? "token" : "tokens"}`);
  if (p.tokens_per_second !== null && Number.isFinite(p.tokens_per_second)) out.push(speed(p.tokens_per_second));
  return out;
}

/** What a call that runs does, in a few words. */
function runs(item: ToolItem, rel: (path: string) => string): string {
  const c = item.call;
  switch (c?.name) {
    case "Bash":
      return `Running ${c.command.split("\n")[0]}`;
    case "Read":
      return `Reading ${rel(c.path)}`;
    case "Grep":
      return `Searching for ${c.pattern}`;
    case "Glob":
      return `Listing ${c.pattern}`;
    case "Edit":
      return `Editing ${rel(c.path)}`;
    case "Write":
      return `Writing ${rel(c.path)}`;
    case "ApplyPatch": {
      const files = patchFiles(c.patch);
      return files.length ? `Editing ${files.map(rel).join(", ")}` : "Applying a patch";
    }
    case "Task":
      return c.tasks.length === 1 ? "Waiting for a subagent" : `Waiting for ${c.tasks.length} subagents`;
    case "Mcp":
      return `Calling ${c.server} · ${c.tool}`;
    case "ToolSearch":
      return "Looking for tools";
    case "Skill":
      return `Loading the skill ${c.skill}`;
    default:
      return `Running ${item.shown}`;
  }
}

/** What the model writes: a call, by the file it changes, or its text. */
function writes(draft: Draft | undefined, streaming: Live["streaming"], rel: (path: string) => string): string {
  if (!draft) return streaming === "message" ? "Writing the reply" : "Writing";
  const path = field(draft, "path");
  switch (draft.tool) {
    case "Write":
      return path ? `Writing ${rel(path)}` : "Writing a file";
    case "Edit":
      return path ? `Writing ${rel(path)}` : "Writing an edit";
    case "ApplyPatch": {
      // the files of the lines it finished: a name that still grows is not one yet
      const patch = field(draft, "patch") ?? "";
      const files = patchFiles(patch.slice(0, patch.lastIndexOf("\n") + 1));
      return files.length ? `Writing ${files.map(rel).join(", ")}` : "Writing a patch";
    }
    case "Bash":
      return "Writing a command";
    default:
      return draft.tool ? `Writing a call of ${draft.tool}` : "Writing a call";
  }
}
