// The core's events (protocol.ts, the event model frozen in M2) as a thread's store takes
// them. One thread at a time: a subagent's events are those of its own thread, which its
// item in the parent names.

import type { Call, Generation, Item, ToolResult } from "./model.ts";
import type * as P from "./protocol.ts";
import type { LiveEvent } from "./store.ts";

/** What the store makes of one event of the thread it shows. */
export function toLive(e: P.Event): LiveEvent[] {
  switch (e.event) {
    case "thread_started":
      return [];
    case "turn_started":
      return [{ type: "turn_started", index: e.turn, user: e.user }];
    case "item_started":
      return [{ type: "item_started", item: toItem(e.item) }];
    case "item_delta":
      return [{ type: "item_delta", id: String(e.item), text: e.delta.text }];
    case "item_completed": {
      const result = e.item.kind === "tool_call" && e.item.result ? toResult(e.item.result) : undefined;
      return [{ type: "item_completed", id: String(e.item.id), result }];
    }
    case "generation":
      return [{ type: "generation", generation: toGeneration(e.generation) }];
    case "turn_completed":
      return [{ type: "turn_completed", index: e.turn, end: e.end, why: e.why ?? undefined }];
  }
}

function toItem(item: P.Item): Item {
  const id = String(item.id);
  const generation = item.generation ?? undefined;
  switch (item.kind) {
    case "reasoning":
      return { kind: "reasoning", id, generation, text: item.text };
    case "agent_message":
      return { kind: "message", id, generation, text: item.text };
    case "tool_call":
      return {
        kind: "tool",
        id,
        generation,
        shown: item.tool,
        raw: parsed(item.arguments),
        call: item.call ? toCall(item.call) : undefined,
        result: item.result ? toResult(item.result) : undefined,
      };
    case "notice":
      return { kind: "notice", id, text: item.text };
    case "void":
      return { kind: "void", id, generation, why: item.why, calls: item.calls };
    // the store has no rows of their own for these yet
    case "compaction":
      return { kind: "notice", id, text: `compacting: about ${item.tokens} tokens, over the ${item.limit} budget` };
    case "approval":
      return {
        kind: "notice",
        id,
        text: `${item.agent ? `subagent «${item.agent}» asks: ` : ""}${item.tool} ${item.action} (${item.reason})`,
      };
    case "instructions":
      return { kind: "notice", id, text: `instructions: ${item.files.join(", ")}` };
    case "subagent":
      return {
        kind: "notice",
        id,
        text: `subagent ${item.index + 1} «${item.title}»${item.write ? ` writes ${item.write.join(", ")}` : ""}`,
      };
  }
}

function parsed(arguments_: string): unknown {
  try {
    return JSON.parse(arguments_);
  } catch {
    return arguments_;
  }
}

/** The calls the UI knows as such; apply_patch and delegate show by their tools' names. */
function toCall(c: P.Call): Call | undefined {
  switch (c.name) {
    case "Read":
      return { name: "Read", path: c.path, offset: c.offset ?? undefined, limit: c.limit ?? undefined };
    case "Edit":
      return { name: "Edit", path: c.path, old: c.old, new: c.new, replaceAll: c.replace_all };
    case "Write":
      return { name: "Write", path: c.path, content: c.content };
    case "Bash":
      return { name: "Bash", command: c.command, timeoutMs: c.timeout_ms ?? undefined };
    case "Grep":
      return { name: "Grep", pattern: c.pattern, path: c.path ?? undefined, glob: c.glob ?? undefined };
    case "Glob":
      return { name: "Glob", pattern: c.pattern, path: c.path ?? undefined };
    case "ApplyPatch":
    case "Task":
      return undefined;
  }
}

function toResult(r: P.ToolResult): ToolResult {
  return {
    outcome: r.outcome === "skipped" ? "error" : r.outcome,
    text: r.text,
    exit: r.exit_code ?? undefined,
    seconds: r.duration_ms / 1000,
  };
}

function toGeneration(g: P.Generation): Generation {
  return {
    index: g.index,
    outcome: g.outcome,
    finish: g.finish ?? undefined,
    seed: g.seed,
    promptHash: g.prompt_hash,
    prompt: g.prompt_tokens,
    cached: g.cached_tokens,
    completion: g.completion_tokens,
    promptMs: g.prompt_ms ?? 0,
    generationMs: g.generation_ms ?? 0,
  };
}
