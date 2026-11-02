// What the UI shows of a session. The core's event model freezes in M2
// (docs/architecture.md §3): its events will be folded into these same types. Until then
// a thread is read from a recorded ATIF trace (atif.ts).

/** Claude Code's tool names, which ognivo uses for its own tools. */
export type ToolName = "Read" | "Edit" | "Write" | "Bash" | "Grep" | "Glob";

/** A call as ognivo runs it, with ognivo's argument names whatever the model's were. */
export type Call =
  | { name: "Read"; path: string; offset?: number; limit?: number }
  | { name: "Edit"; path: string; old: string; new: string; replaceAll: boolean }
  | { name: "Write"; path: string; content: string }
  | { name: "Bash"; command: string; timeoutMs?: number }
  | { name: "Grep"; pattern: string; path?: string; glob?: string }
  | { name: "Glob"; pattern: string; path?: string };

export interface ToolResult {
  /** `refused` when it needed an approval it did not get, `error` when it failed. */
  outcome: "ok" | "error" | "refused";
  /** The result as the model got it. */
  text: string;
  /** Bash: the exit code, `undefined` when the command was killed or timed out. */
  exit?: number;
  seconds?: number;
}

/** One generation of the model: what it cost and how it ended. */
export interface Generation {
  index: number;
  /** `calls`, `answer` or `malformed`. */
  outcome: string;
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

/** A generation that ran nothing: calls that did not parse, an empty answer. */
export interface VoidItem extends Base {
  kind: "void";
  why: "malformed" | "empty";
  /** The calls as the model wrote them, for `malformed`. */
  calls: { name: string; arguments: string }[];
}

export type Item = ReasoningItem | MessageItem | ToolItem | NoticeItem | VoidItem;

export interface Turn {
  index: number;
  user: string;
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
