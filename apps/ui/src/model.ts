// What the UI shows of a session: the core's events fold into these types (live.ts), and
// so does a recorded ATIF trace (atif.ts).

/** Claude Code's tool names, which ognivo uses for its own tools. */
export type ToolName = "Read" | "Edit" | "Write" | "Bash" | "Grep" | "Glob";

/** A call as ognivo runs it, with ognivo's argument names whatever the model's were. */
export type Call =
  | { name: "Read"; path: string; offset?: number; limit?: number }
  | { name: "Edit"; path: string; old: string; new: string; replaceAll: boolean }
  | { name: "Write"; path: string; content: string }
  | { name: "Bash"; command: string; timeoutMs?: number }
  | { name: "Grep"; pattern: string; path?: string; glob?: string }
  | { name: "Glob"; pattern: string; path?: string }
  /** gpt-oss's `apply_patch`, in the V4A format. */
  | { name: "ApplyPatch"; patch: string }
  /** `delegate`: tasks for subagents. */
  | { name: "Task"; tasks: { title: string; write: boolean; files: string[] }[] }
  /** A tool of an MCP server; the arguments as the model wrote them. */
  | { name: "Mcp"; server: string; tool: string; arguments: string }
  /** `tool_search`: the MCP tools that fit a query, when there are too many to declare. */
  | { name: "ToolSearch"; query: string }
  /** The skill tool: a skill of the user or the project, loaded for the model. */
  | { name: "Skill"; skill: string; arguments?: string };

/** An image the model sees: of the user's message, or the image file a call read. */
export interface Picture {
  /** `image/png`, `image/jpeg`, `image/gif` or `image/bmp`. */
  mime: string;
  /** Its bytes in base64. */
  data: string;
  width: number;
  height: number;
}

/** A picture as an `<img>` takes it. */
export function pictureUrl(p: Pick<Picture, "mime" | "data">): string {
  return `data:${p.mime};base64,${p.data}`;
}

export interface ToolResult {
  /** `refused` when it needed an approval it did not get, `error` when it failed. */
  outcome: "ok" | "error" | "refused";
  /** The result as the model got it. */
  text: string;
  /** The image file it read, which the model got after the text. */
  images?: Picture[];
  /** Bash: the exit code, `undefined` when the command was killed or timed out. */
  exit?: number;
  seconds?: number;
}

/** A message of the conversation as ognivo keeps it and sends it (ognivo-models
 * generic.rs: StoredMessage). */
export interface StoredMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string;
  reasoning?: string;
  tool_calls?: { id: string; name: string; arguments: string }[];
  tool_call_id?: string;
  /** The user's images, or the image file a call read. */
  images?: { mime: string; data: string }[];
}

/** A tool as the model is offered it. */
export interface ToolDecl {
  name: string;
  description: string;
  parameters: unknown;
}

/** One generation of the model: what it was asked, what it cost and how it ended. */
export interface Generation {
  index: number;
  /** `calls`, `answer` or `malformed`. */
  outcome: string;
  /** The server's finish reason: `tool_calls`, `stop`, `length`. */
  finish?: string;
  seed?: number;
  /** sha256 of the request's messages and tools (ognivo-core agent.rs: prompt_hash). */
  promptHash?: string;
  /** Its request: the first `messages` messages of conversation `conversation`. */
  request?: { conversation: number; messages: number };
  prompt: number;
  cached: number;
  completion: number;
  promptMs: number;
  generationMs: number;
}

interface Base {
  id: string;
  /** The generation it came from; notes and user messages have none. */
  generation?: number;
}

export interface ReasoningItem extends Base {
  kind: "reasoning";
  text: string;
}

/** Text the model shows the user: a preamble or the final answer. */
export interface MessageItem extends Base {
  kind: "message";
  text: string;
}

export interface ToolItem extends Base {
  kind: "tool";
  /** The model's own name for the tool. */
  shown: string;
  /** The arguments as the model wrote them. */
  raw: unknown;
  /** `undefined` when the arguments do not make a call ognivo knows. */
  call?: Call;
  /** `undefined` while it runs. */
  result?: ToolResult;
}

/** What ognivo told the model between generations: notes, limits, corrections. */
export interface NoticeItem extends Base {
  kind: "notice";
  text: string;
}

/** A generation that ran nothing: calls that did not parse, an empty answer, one cut at its
 * token limit or stopped for repeating itself, or one the server's format check restarted. */
export interface VoidItem extends Base {
  kind: "void";
  why: "malformed" | "empty" | "truncated" | "repeating" | "restarted";
  /** The calls as the model wrote them, for `malformed`. */
  calls: { name: string; arguments: string }[];
}

/** A task of `delegate`, which a subagent works on in a thread of its own. */
export interface SubagentItem extends Base {
  kind: "subagent";
  /** Its number among the call's tasks, from 0. */
  index: number;
  title: string;
  /** The files of a write task; `undefined` for a read task. */
  write?: string[];
  /** Its thread on the core. */
  thread?: number;
  /** How its turn ended, as a turn's end says it; `undefined` while it works. */
  end?: string;
  steps?: number;
  changed: string[];
}

export type Item = ReasoningItem | MessageItem | ToolItem | NoticeItem | VoidItem | SubagentItem;

export interface Turn {
  index: number;
  user: string;
  /** The images of the user's message, which the model saw before its text. */
  images?: Picture[];
  items: Item[];
  /** How the turn ended: `answer`, `step_limit`, `time_limit`, `stuck` or `error`. */
  end?: string;
  /** Why it ended so, when it did not end with an answer. */
  why?: string;
}

export interface Thread {
  id: string;
  title: string;
  project: string;
  /** The ognivo profile, e.g. `gpt-oss-20b`, and the server it ran against. */
  profile: string;
  model: string;
  server: string;
  system: string;
  /** The conversations as ognivo kept them, by number: a compaction starts a new one. */
  conversations: Record<number, StoredMessage[]>;
  tools: ToolDecl[];
  turns: Turn[];
  generations: Generation[];
}

export interface Totals {
  prompt: number;
  cached: number;
  completion: number;
  generationMs: number;
}

export function totals(t: Thread): Totals {
  return t.generations.reduce(
    (a, g) => ({
      prompt: a.prompt + g.prompt,
      cached: a.cached + g.cached,
      completion: a.completion + g.completion,
      generationMs: a.generationMs + g.generationMs,
    }),
    { prompt: 0, cached: 0, completion: 0, generationMs: 0 },
  );
}
