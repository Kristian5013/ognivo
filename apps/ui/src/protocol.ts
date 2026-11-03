// The event model of ognivo, generated from crates/ognivo-protocol: edit the Rust types,
// then `OGNIVO_BLESS=1 cargo test -p ognivo-protocol` writes this again.

export const VERSION = 1;

export type Event = { 
/**
 * Its place in the core's stream, growing across all threads.
 */
seq: number, thread: ThreadId, } & ({ "event": "thread_started", info: ThreadInfo, } | { "event": "turn_started", turn: TurnId, 
/**
 * The user's message, as the user wrote it; for a subagent, its task.
 */
user: string, } | { "event": "item_started", item: Item, } | { "event": "item_delta", item: ItemId, delta: Delta, } | { "event": "item_completed", item: Item, } | { "event": "generation", generation: Generation, } | { "event": "turn_completed", turn: TurnId, end: TurnEnd, 
/**
 * Why it ended so, when it did not end with an answer.
 */
why: string | null, 
/**
 * The accepted answer.
 */
answer: string | null, 
/**
 * What the facts of the turn say of the answer, e.g. `NOT verified: …`.
 */
verification: string | null, });

export type EventKind = { "event": "thread_started", info: ThreadInfo, } | { "event": "turn_started", turn: TurnId, 
/**
 * The user's message, as the user wrote it; for a subagent, its task.
 */
user: string, } | { "event": "item_started", item: Item, } | { "event": "item_delta", item: ItemId, delta: Delta, } | { "event": "item_completed", item: Item, } | { "event": "generation", generation: Generation, } | { "event": "turn_completed", turn: TurnId, end: TurnEnd, 
/**
 * Why it ended so, when it did not end with an answer.
 */
why: string | null, 
/**
 * The accepted answer.
 */
answer: string | null, 
/**
 * What the facts of the turn say of the answer, e.g. `NOT verified: …`.
 */
verification: string | null, };

export type ThreadId = number;

export type TurnId = number;

export type ItemId = number;

export type ThreadInfo = { 
/**
 * `chat`, `run` or `subagent`.
 */
kind: string, 
/**
 * The project's root.
 */
project: string, 
/**
 * The profile, e.g. `gpt-oss-20b`.
 */
profile: string, 
/**
 * The reasoning level, where the profile has levels.
 */
level: string | null, 
/**
 * The server's URL.
 */
server: string, 
/**
 * The model each request names, for llama-server in router mode.
 */
model: string | null, 
/**
 * For a subagent: the thread and the item it works for.
 */
parent: Parent | null, };

export type Parent = { thread: ThreadId, item: ItemId, };

export type Item = { id: ItemId, turn: TurnId, 
/**
 * The generation it came from, by its number in the thread (from 1); `None` for what
 * is ognivo's.
 */
generation: number | null, } & ({ "kind": "agent_message", text: string, } | { "kind": "reasoning", text: string, } | { "kind": "tool_call", 
/**
 * The model's id for the call.
 */
call_id: string, 
/**
 * The tool by the model's name for it.
 */
tool: string, 
/**
 * The arguments as the model wrote them.
 */
arguments: string, 
/**
 * The call as ognivo runs it; `None` when the arguments do not make one.
 */
call: Call | null, 
/**
 * `None` while it runs.
 */
result: ToolResult | null, 
/**
 * The files it changed, each against what it was.
 */
changes: Array<FileChange>, } | { "kind": "notice", text: string, } | { "kind": "void", why: VoidReason, 
/**
 * Its calls as the model wrote them, which did not run.
 */
calls: Array<RawCall>, } | { "kind": "compaction", tokens: number, limit: number, 
/**
 * Messages it replaced; `None` while the model writes its summary.
 */
replaced: number | null, 
/**
 * Steps kept as they were.
 */
kept: number | null, summary: string | null, } | { "kind": "approval", tool: string, 
/**
 * What would happen: the command line, the file path.
 */
action: string, reason: string, 
/**
 * For running project code: what is unreviewed.
 */
unreviewed: Array<string>, 
/**
 * The subagent that asks, by its task's title.
 */
agent: string | null, allowed: boolean | null, } | { "kind": "instructions", files: Array<string>, skipped: Array<Skipped>, } | { "kind": "subagent", 
/**
 * Its number among the call's tasks, from 0.
 */
index: number, title: string, 
/**
 * The files of a write task; `None` for a read task.
 */
write: Array<string> | null, thread: ThreadId | null, 
/**
 * How its turn ended, as [`EventKind::TurnCompleted`] says; `None` while it works.
 */
end: TurnEnd | null, steps: number | null, 
/**
 * The files it changed.
 */
changed: Array<string>, });

export type ItemBody = { "kind": "agent_message", text: string, } | { "kind": "reasoning", text: string, } | { "kind": "tool_call", 
/**
 * The model's id for the call.
 */
call_id: string, 
/**
 * The tool by the model's name for it.
 */
tool: string, 
/**
 * The arguments as the model wrote them.
 */
arguments: string, 
/**
 * The call as ognivo runs it; `None` when the arguments do not make one.
 */
call: Call | null, 
/**
 * `None` while it runs.
 */
result: ToolResult | null, 
/**
 * The files it changed, each against what it was.
 */
changes: Array<FileChange>, } | { "kind": "notice", text: string, } | { "kind": "void", why: VoidReason, 
/**
 * Its calls as the model wrote them, which did not run.
 */
calls: Array<RawCall>, } | { "kind": "compaction", tokens: number, limit: number, 
/**
 * Messages it replaced; `None` while the model writes its summary.
 */
replaced: number | null, 
/**
 * Steps kept as they were.
 */
kept: number | null, summary: string | null, } | { "kind": "approval", tool: string, 
/**
 * What would happen: the command line, the file path.
 */
action: string, reason: string, 
/**
 * For running project code: what is unreviewed.
 */
unreviewed: Array<string>, 
/**
 * The subagent that asks, by its task's title.
 */
agent: string | null, allowed: boolean | null, } | { "kind": "instructions", files: Array<string>, skipped: Array<Skipped>, } | { "kind": "subagent", 
/**
 * Its number among the call's tasks, from 0.
 */
index: number, title: string, 
/**
 * The files of a write task; `None` for a read task.
 */
write: Array<string> | null, thread: ThreadId | null, 
/**
 * How its turn ended, as [`EventKind::TurnCompleted`] says; `None` while it works.
 */
end: TurnEnd | null, steps: number | null, 
/**
 * The files it changed.
 */
changed: Array<string>, };

export type Call = { "name": "Read", path: string, offset: number | null, limit: number | null, } | { "name": "Edit", path: string, old: string, new: string, replace_all: boolean, } | { "name": "Write", path: string, content: string, } | { "name": "ApplyPatch", patch: string, } | { "name": "Bash", command: string, timeout_ms: number | null, } | { "name": "Grep", pattern: string, path: string | null, glob: string | null, } | { "name": "Glob", pattern: string, path: string | null, } | { "name": "Task", tasks: Array<Task>, };

export type Task = { title: string, goal: string, context: string | null, write: boolean, files: Array<string>, };

export type ToolResult = { outcome: Outcome, 
/**
 * The result as the model got it.
 */
text: string, 
/**
 * A command's exit code; `None` for a command a signal killed, and for other tools.
 */
exit_code: number | null, duration_ms: number, };

export type Outcome = "ok" | "error" | "refused" | "skipped";

export type FileChange = { 
/**
 * Relative to the project root inside it, absolute outside.
 */
path: string, created: boolean, hunks: Array<Hunk>, };

export type Hunk = { 
/**
 * The first line in the old and the new text, from 1.
 */
old_start: number, new_start: number, removed: Array<string>, added: Array<string>, };

export type VoidReason = "malformed" | "empty" | "truncated" | "repeating" | "restarted";

export type RawCall = { name: string, arguments: string, };

export type Skipped = { path: string, why: string, };

export type Generation = { 
/**
 * Its number in the thread, from 1.
 */
index: number, turn: TurnId, 
/**
 * `step`, or `summary` for a compaction's.
 */
purpose: string, 
/**
 * `calls`, `answer`, `truncated`, `repeating` or `malformed`.
 */
outcome: string, 
/**
 * The server's finish reason: `tool_calls`, `stop`, `length`.
 */
finish: string | null, seed: number, prompt_tokens: number, 
/**
 * Of `prompt_tokens`, those the server took from its KV cache.
 */
cached_tokens: number, completion_tokens: number, prompt_ms: number | null, generation_ms: number | null, 
/**
 * sha256 of the request's messages and tools.
 */
prompt_hash: string, };

export type Delta = { "type": "text", text: string, };

export type TurnEnd = "answer" | "step_limit" | "stuck" | "time_limit" | "error" | "interrupted";
