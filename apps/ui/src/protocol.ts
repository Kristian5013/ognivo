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
user: string, } | { "event": "item_started", item: Item, } | { "event": "item_delta", item: ItemId, delta: Delta, } | { "event": "item_completed", item: Item, } | { "event": "generation", generation: Generation, } | { "event": "thread_rewound", turn: TurnId, } | { "event": "turn_completed", turn: TurnId, end: TurnEnd, 
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
user: string, } | { "event": "item_started", item: Item, } | { "event": "item_delta", item: ItemId, delta: Delta, } | { "event": "item_completed", item: Item, } | { "event": "generation", generation: Generation, } | { "event": "thread_rewound", turn: TurnId, } | { "event": "turn_completed", turn: TurnId, end: TurnEnd, 
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

export type Call = { "name": "Read", path: string, offset: number | null, limit: number | null, } | { "name": "Edit", path: string, old: string, new: string, replace_all: boolean, } | { "name": "Write", path: string, content: string, } | { "name": "ApplyPatch", patch: string, } | { "name": "Bash", command: string, timeout_ms: number | null, } | { "name": "Grep", pattern: string, path: string | null, glob: string | null, } | { "name": "Glob", pattern: string, path: string | null, } | { "name": "Task", tasks: Array<Task>, } | { "name": "Mcp", server: string, tool: string, 
/**
 * The arguments as the model wrote them, a JSON object.
 */
arguments: string, };

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

export type Initialize = { 
/**
 * The client and its version, e.g. `ognivo-desktop 0.0.1`.
 */
client: string, 
/**
 * The version of the event model it reads ([`crate::VERSION`]).
 */
protocol: number, };

export type Initialized = { protocol: number, 
/**
 * The core's version.
 */
core: string, 
/**
 * The last event's place in the stream: the client gets the events after it.
 */
seq: number, };

export type ListThreads = { 
/**
 * The project's root; every project's threads when `None`.
 */
project: string | null, limit: number | null, };

export type Threads = { 
/**
 * The users' threads, the last updated first: subagents' threads are their parents'.
 */
threads: Array<ThreadSummary>, };

export type ThreadSummary = { thread: ThreadId, 
/**
 * `chat` or `run`.
 */
kind: string, project: string, profile: string, level: string | null, 
/**
 * The server's URL, and the model behind llama-server's router.
 */
server: string, model: string | null, 
/**
 * The first line of its first message.
 */
title: string, 
/**
 * Unix milliseconds.
 */
created: number, updated: number, turns: number, 
/**
 * Files with unreviewed changes in its edit journal.
 */
unreviewed: number, 
/**
 * Whether the core has it open, and whether a turn of it runs.
 */
open: boolean, running: boolean, 
/**
 * Open on the Native path: `turn/answer_now` ends the model's reasoning.
 */
answer_now: boolean, };

export type Server = { 
/**
 * The server's base URL, e.g. `http://127.0.0.1:8080`.
 */
url: string, 
/**
 * The bearer token of a server started with `--api-key`.
 */
api_key: string | null, 
/**
 * For llama-server's router: the model, by its id or alias.
 */
model: string | null, };

export type StartThread = { 
/**
 * The project's root.
 */
project: string, server: Server, 
/**
 * The profile; the server's model decides when `None`.
 */
profile: string | null, 
/**
 * The reasoning level; the profile's default when `None`.
 */
level: string | null, 
/**
 * The Native path (gpt-oss): ognivo renders the model's format itself.
 */
native: boolean, 
/**
 * With `native`: gpt-oss gets the tools of its training.
 */
priors: boolean, 
/**
 * The agent may hand tasks to subagents.
 */
subagents: boolean, };

export type ResumeThread = { thread: ThreadId, 
/**
 * Where its model is now; the server it last had when `None`.
 */
server: Server | null, native: boolean, priors: boolean, subagents: boolean, };

export type Opened = { thread: ThreadId, info: ThreadInfo, 
/**
 * What the start found, as `ognivo chat` says it under its banner: instruction and
 * rule files, a tree that is not clean, a context smaller than the profile plans for.
 */
notes: Array<string>, 
/**
 * On the Native path: `turn/answer_now` ends the model's reasoning.
 */
answer_now: boolean, };

export type ThreadRef = { thread: ThreadId, };

export type ReadThread = { thread: ThreadId, 
/**
 * Only what came after this place in the stream.
 */
after: number | null, };

export type ThreadEvents = { 
/**
 * The kept events of the thread and of its subagents' threads, in order. None for a
 * thread from before the core kept them, or from `chat` and `run`: its trajectory
 * tells what happened.
 */
events: Array<Event>, };

export type Trajectory = { 
/**
 * The thread in ATIF, as `ognivo run --atif` writes it.
 */
trajectory: unknown, };

export type StartTurn = { thread: ThreadId, 
/**
 * The user's message.
 */
text: string, };

export type EditTurn = { thread: ThreadId, turn: TurnId, text: string, };

export type Changes = { files: Array<ChangedFile>, 
/**
 * Commands with side effects, which a revert does not undo.
 */
commands: Array<string>, 
/**
 * The project may hold changes ognivo did not make: builds and tests ask until they
 * are accepted.
 */
inherited: boolean, };

export type ChangedFile = { 
/**
 * Relative to the project root inside it, absolute outside.
 */
path: string, created: boolean, 
/**
 * The file is gone.
 */
deleted: boolean, 
/**
 * Something else changed it too since the agent's last edit: a revert loses that.
 */
drifted: boolean, 
/**
 * Against what it was before the session.
 */
hunks: Array<Hunk>, };

export type Accept = { thread: ThreadId, 
/**
 * One file; all of it when `None`.
 */
path: string | null, };

export type Accepted = { 
/**
 * What was accepted.
 */
accepted: Array<string>, 
/**
 * Whether anything is still unreviewed: builds and tests ask until nothing is.
 */
unreviewed: boolean, };

export type Revert = { thread: ThreadId, path: string, 
/**
 * Revert a drifted file too, losing what else changed it.
 */
force: boolean, };

export type Reverted = { 
/**
 * The agent had created the file, and it is gone.
 */
removed: boolean, };

export type Done = Record<symbol, never>;

export type ItemRef = { thread: ThreadId, item: ItemId, };

export type ApprovalAsked = { 
/**
 * The user's thread whose agent, or subagent, asks.
 */
thread: ThreadId, 
/**
 * The question's item in the stream: in a subagent's own thread when one asks.
 */
item: ItemRef | null, tool: string, 
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
agent: string | null, };

export type Decision = "allow" | "allow_session" | "deny";

export type ApprovalAnswer = { decision: Decision, 
/**
 * With `deny`: what the agent should do instead.
 */
instead: string | null, };

export type TrustAsked = { project: string, rules: Array<Rule>, };

export type Rule = { text: string, 
/**
 * The file it comes from.
 */
source: string, 
/**
 * It allows any command at all.
 */
any: boolean, };

export type TrustAnswer = { trust: boolean, };

export type McpTrustAsked = { project: string, servers: Array<McpServer>, };

export type McpServer = { name: string, 
/**
 * What starts it: the command line, or the URL.
 */
command: string, };

export type Cancel = { id: number, };

/** The core's methods. */
export type Methods = {
  "initialize": { params: Initialize; result: Initialized };
  "thread/list": { params: ListThreads; result: Threads };
  "thread/start": { params: StartThread; result: Opened };
  "thread/resume": { params: ResumeThread; result: Opened };
  "thread/read": { params: ReadThread; result: ThreadEvents };
  "thread/trajectory": { params: ThreadRef; result: Trajectory };
  "thread/clear": { params: ThreadRef; result: Done };
  "thread/close": { params: ThreadRef; result: Done };
  "thread/delete": { params: ThreadRef; result: Done };
  "turn/start": { params: StartTurn; result: Done };
  "turn/interrupt": { params: ThreadRef; result: Done };
  "turn/answer_now": { params: ThreadRef; result: Done };
  "turn/edit": { params: EditTurn; result: Done };
  "changes/list": { params: ThreadRef; result: Changes };
  "changes/accept": { params: Accept; result: Accepted };
  "changes/revert": { params: Revert; result: Reverted };
};

/** What the core asks its clients. */
export type Asks = {
  "approval/ask": { params: ApprovalAsked; result: ApprovalAnswer };
  "rules/trust": { params: TrustAsked; result: TrustAnswer };
  "mcp/trust": { params: McpTrustAsked; result: TrustAnswer };
};

export const NOTIFY_EVENT = "event";
export const NOTIFY_CANCEL = "$/cancelRequest";

export const ERRORS = { noThread: -32001, busy: -32002, notStarted: -32003, journal: -32004, unsupported: -32005, noTurn: -32006 } as const;
