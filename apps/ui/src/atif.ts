// A recorded session (an ATIF trace, as `ognivo run --atif` and T2 write it) as a thread.

import type {
  Call,
  Generation,
  Item,
  StoredMessage,
  Thread,
  ToolDecl,
  ToolItem,
  ToolName,
  ToolResult,
  Turn,
} from "./model.ts";

interface AtifCall {
  tool_call_id: string;
  function_name: string;
  arguments: unknown;
}

interface AtifStep {
  step_id: number;
  source: "system" | "user" | "agent";
  message?: string;
  reasoning_content?: string;
  tool_calls?: AtifCall[];
  observation?: { results?: { source_call_id?: string; content?: string }[] };
  metrics?: {
    prompt_tokens?: number;
    cached_tokens?: number;
    completion_tokens?: number;
    extra?: { prompt_ms?: number; generation_ms?: number };
  };
  extra?: {
    outcome?: string;
    finish?: string;
    seed?: number;
    prompt_hash?: string;
    /** Added by the fixtures script from the session store (scripts/fixtures.mjs). */
    request?: { conversation: number; messages: number };
    reason?: string;
    rejected_calls?: { name?: string; arguments?: unknown }[];
  };
}

export interface AtifTrace {
  schema_version: string;
  session_id: string;
  agent?: { model_name?: string; tool_definitions?: { function: ToolDecl }[] };
  steps: AtifStep[];
  extra?: {
    ognivo?: {
      project?: string;
      profile?: string;
      server?: string;
      title?: string;
      turns?: { turn: number; end?: string; why?: string }[];
      /** Added by the fixtures script from the session store. */
      conversations?: Record<string, StoredMessage[]>;
    };
  };
}

// The tool names and argument names of the three families (ognivo-tools projection.rs),
// which do not collide: Harmony `shell` and `old_string`, Gemma `file`, `find`, `replace`
// and `text`, Qwen `file_path`.
const NAMES: Record<string, ToolName> = {
  read: "Read",
  Read: "Read",
  edit: "Edit",
  Edit: "Edit",
  write: "Write",
  Write: "Write",
  shell: "Bash",
  bash: "Bash",
  Bash: "Bash",
  grep: "Grep",
  Grep: "Grep",
  glob: "Glob",
  Glob: "Glob",
};

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

/** Integers may come as their string forms, as ognivo accepts them. */
function int(v: unknown): number | undefined {
  if (typeof v === "number" && Number.isInteger(v)) return v;
  if (typeof v === "string" && /^\s*\d+\s*$/.test(v)) return Number(v);
  return undefined;
}

/** The call ognivo would run, or `undefined` when the arguments do not make one. */
export function toCall(name: string, raw: unknown): Call | undefined {
  const tool = NAMES[name];
  if (!tool || typeof raw !== "object" || raw === null) return undefined;
  const a = raw as Record<string, unknown>;
  const path = str(a.path) ?? str(a.file) ?? str(a.file_path);
  switch (tool) {
    case "Read":
      return path === undefined
        ? undefined
        : { name: tool, path, offset: int(a.offset), limit: int(a.limit) };
    case "Edit": {
      const old = str(a.old_string) ?? str(a.find);
      const replacement = str(a.new_string) ?? str(a.replace);
      if (path === undefined || old === undefined || replacement === undefined) return undefined;
      const all = a.replace_all;
      return {
        name: tool,
        path,
        old,
        new: replacement,
        replaceAll: all === true || all === "true",
      };
    }
    case "Write": {
      const content = str(a.content) ?? str(a.text);
      return path === undefined || content === undefined
        ? undefined
        : { name: tool, path, content };
    }
    case "Bash": {
      const command = str(a.command);
      if (command === undefined) return undefined;
      const s = int(a.timeout_s);
      return { name: tool, command, timeoutMs: int(a.timeout_ms) ?? int(a.timeout) ?? (s === undefined ? undefined : s * 1000) };
    }
    case "Grep": {
      const pattern = str(a.pattern);
      // an empty filter is no filter (ognivo-tools projection.rs)
      const glob = str(a.glob)?.trim() ? str(a.glob) : undefined;
      return pattern === undefined ? undefined : { name: tool, pattern, path: str(a.path), glob };
    }
    case "Glob": {
      const pattern = str(a.pattern);
      return pattern === undefined ? undefined : { name: tool, pattern, path: str(a.path) };
    }
  }
}

/** What a result's first line says (ognivo-tools exec.rs and the tools' headers). */
export function toResult(text: string): ToolResult {
  if (text.startsWith("[refused]")) return { outcome: "refused", text };
  if (text.startsWith("[error]")) return { outcome: "error", text };
  const exit = /^\[shell: exit (-?\d+), ([\d.]+) s\]/.exec(text);
  if (exit) return { outcome: "ok", text, exit: Number(exit[1]), seconds: Number(exit[2]) };
  const timedOut = /^\[shell: timed out after ([\d.]+) s/.exec(text);
  if (timedOut) return { outcome: "ok", text, seconds: Number(timedOut[1]) };
  return { outcome: "ok", text };
}

export function fromAtif(trace: AtifTrace): Thread {
  const meta = trace.extra?.ognivo ?? {};
  const turns: Turn[] = [];
  const generations: Generation[] = [];
  let system = "";
  let turn: Turn | undefined;
  let n = 0;
  const id = () => `i${n++}`;
  const push = (item: Item) => {
    if (!turn) {
      turn = { index: 1, user: "", items: [] };
      turns.push(turn);
    }
    turn.items.push(item);
  };

  for (const step of trace.steps) {
    if (step.source === "system") {
      if (step.extra?.reason === "start" && !system) system = step.message ?? "";
      else if (step.message) push({ kind: "notice", id: id(), text: step.message });
      continue;
    }
    if (step.source === "user") {
      turn = { index: turns.length + 1, user: step.message ?? "", items: [] };
      turns.push(turn);
      continue;
    }
    const g = generations.length;
    const outcome = step.extra?.outcome ?? (step.tool_calls?.length ? "calls" : "answer");
    generations.push({
      index: g,
      outcome,
      finish: step.extra?.finish,
      seed: step.extra?.seed,
      promptHash: step.extra?.prompt_hash,
      request: step.extra?.request,
      prompt: step.metrics?.prompt_tokens ?? 0,
      cached: step.metrics?.cached_tokens ?? 0,
      completion: step.metrics?.completion_tokens ?? 0,
      promptMs: step.metrics?.extra?.prompt_ms ?? 0,
      generationMs: step.metrics?.extra?.generation_ms ?? 0,
    });
    if (step.reasoning_content) {
      push({ kind: "reasoning", id: id(), generation: g, text: step.reasoning_content });
    }
    if (outcome === "malformed") {
      const calls = (step.extra?.rejected_calls ?? []).map((c) => ({
        name: c.name ?? "?",
        arguments: typeof c.arguments === "string" ? c.arguments : JSON.stringify(c.arguments ?? null),
      }));
      push({ kind: "void", id: id(), generation: g, why: "malformed", calls });
      continue;
    }
    const text = step.message ?? "";
    if (text.trim()) push({ kind: "message", id: id(), generation: g, text });
    const results = new Map(
      (step.observation?.results ?? []).map((r) => [r.source_call_id ?? "", r.content ?? ""]),
    );
    const calls = step.tool_calls ?? [];
    for (const c of calls) {
      const content = results.get(c.tool_call_id);
      const item: ToolItem = {
        kind: "tool",
        id: id(),
        generation: g,
        shown: c.function_name,
        raw: c.arguments,
        call: toCall(c.function_name, c.arguments),
        result: content === undefined ? undefined : toResult(content),
      };
      push(item);
    }
    if (!calls.length && !text.trim()) {
      push({ kind: "void", id: id(), generation: g, why: "empty", calls: [] });
    }
  }

  for (const t of meta.turns ?? []) {
    const target = turns[t.turn - 1];
    if (target) {
      target.end = t.end;
      target.why = t.why;
    }
  }
  return {
    id: trace.session_id,
    title: meta.title ?? turns[0]?.user.split("\n")[0] ?? trace.session_id,
    project: meta.project ?? "",
    profile: meta.profile ?? "",
    model: trace.agent?.model_name ?? meta.profile ?? "",
    server: meta.server ?? "",
    system,
    conversations: Object.fromEntries(
      Object.entries(meta.conversations ?? {}).map(([n, list]) => [Number(n), list]),
    ),
    tools: (trace.agent?.tool_definitions ?? []).map((d) => d.function),
    turns,
    generations,
  };
}
