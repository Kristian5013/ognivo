// The core's events (protocol.ts, the event model frozen in M2) as a thread's store takes
// them. One thread at a time: a subagent's events are those of its own thread, which its
// item in the parent names. A thread whose earlier turns came from its trajectory has its
// generations numbered on from those (`offset`).

import type { Call, Generation, Item, ToolResult } from "./model.ts";
import type * as P from "../../shared/protocol.ts";
import type { LiveEvent } from "./store.ts";

/** What the store makes of one event of the thread it shows. */
export function toLive(e: P.Event, offset = 0): LiveEvent[] {
  switch (e.event) {
    case "thread_started":
      return [];
    case "turn_started":
      return [{ type: "turn_started", index: e.turn, user: e.user, images: e.images ?? [] }];
    case "item_started":
      return [{ type: "item_started", item: toItem(e.item, offset) }];
    case "item_delta":
      return [{ type: "item_delta", id: String(e.item), text: e.delta.text }];
    case "item_completed": {
      const item = toItem(e.item, offset);
      return [{ type: "item_completed", id: item.id, item, result: item.kind === "tool" ? item.result : undefined }];
    }
    case "generation":
      return [{ type: "generation", generation: toGeneration(e.generation, offset) }];
    case "turn_completed":
      return [{ type: "turn_completed", index: e.turn, end: e.end, why: e.why ?? undefined }];
    case "thread_rewound":
      return [{ type: "rewound", turn: e.turn }];
    case "call_draft":
      return [
        {
          type: "draft",
          generation: e.draft.generation + offset,
          index: e.draft.index,
          tool: e.draft.tool ?? undefined,
          field: e.draft.field ?? undefined,
          text: e.draft.text,
        },
      ];
    // the thread's, not its store's (threads.ts)
    case "mode_changed":
      return [];
  }
}

function toItem(item: P.Item, offset: number): Item {
  const id = String(item.id);
  const generation = item.generation === null ? undefined : item.generation + offset;
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
    case "approval": {
      const answer = item.allowed === null ? "waits for your answer" : item.allowed ? "allowed" : "denied";
      return {
        kind: "notice",
        id,
        text: `${item.agent ? `subagent «${item.agent}» asks` : "asks"}: ${item.tool} ${item.action} (${item.reason}) · ${answer}`,
      };
    }
    case "instructions":
      return {
        kind: "notice",
        id,
        text: `instructions: ${item.files.join(", ")}${item.skipped.map((s) => ` · not read: ${s.path} (${s.why})`).join("")}`,
      };
    case "subagent":
      return {
        kind: "subagent",
        id,
        index: item.index,
        title: item.title,
        write: item.write ?? undefined,
        thread: item.thread ?? undefined,
        end: item.end ?? undefined,
        steps: item.steps ?? undefined,
        changed: item.changed,
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

/** The call in the UI's terms. */
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
      return { name: "ApplyPatch", patch: c.patch };
    case "Task":
      return { name: "Task", tasks: c.tasks.map((t) => ({ title: t.title, write: t.write, files: t.files })) };
    case "Mcp":
      return { name: "Mcp", server: c.server, tool: c.tool, arguments: c.arguments };
    case "ToolSearch":
      return { name: "ToolSearch", query: c.query };
    case "Skill":
      return { name: "Skill", skill: c.skill, arguments: c.arguments ?? undefined };
  }
}

function toResult(r: P.ToolResult): ToolResult {
  return {
    outcome: r.outcome === "skipped" ? "error" : r.outcome,
    text: r.text,
    ...(r.images?.length ? { images: r.images } : {}),
    exit: r.exit_code ?? undefined,
    seconds: r.duration_ms / 1000,
  };
}

function toGeneration(g: P.Generation, offset: number): Generation {
  return {
    index: g.index + offset,
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
