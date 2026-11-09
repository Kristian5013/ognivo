//! The event model of ognivo: what the core tells its clients (the Electron GUI, the VS
//! Code extension, `ognivo run --jsonl`) about its threads (docs/architecture.md §3).
//!
//! - A thread is a session: the user's, or a subagent's, whose thread names the item of
//!   the thread it works for.
//! - A thread holds turns, a turn holds items in the order they start, and an item starts,
//!   may get deltas, and completes. `started` and `completed` carry the whole item as it is
//!   then, so a client that missed deltas has it all at the end.
//! - Each generation's usage and timings come as an event of their own.
//! - Events are numbered by `seq` across all threads: a client that comes back asks for
//!   what came after the last one it saw.
//!
//! Frozen at the end of M2 as version 1. From here on it changes only by additions: a new
//! event, a new item kind, a new field. A client ignores what it does not know.
//!
//! [`rpc`] has the methods of `ognivo core`, which serves the model over its socket.

pub mod rpc;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The version of the event model.
pub const VERSION: u32 = 1;

/// A thread, by its session's id in the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(type = "number")]
pub struct ThreadId(pub i64);

/// A turn of a thread, from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct TurnId(pub u32);

/// An item of a thread, from 1 in the order items start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct ItemId(pub u32);

/// One event of the stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Event {
    /// Its place in the core's stream, growing across all threads.
    #[ts(type = "number")]
    pub seq: u64,
    pub thread: ThreadId,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// The thread's session began, or was picked up again.
    ThreadStarted {
        info: ThreadInfo,
    },
    TurnStarted {
        turn: TurnId,
        /// The user's message, as the user wrote it; for a subagent, its task.
        user: String,
    },
    ItemStarted {
        item: Item,
    },
    ItemDelta {
        item: ItemId,
        delta: Delta,
    },
    ItemCompleted {
        item: Item,
    },
    /// A generation of the model ended.
    Generation {
        generation: Generation,
    },
    /// The user took the conversation back to before turn `turn` and wrote its message anew:
    /// the turns from it on are out of the conversation, the model no longer sees them, and
    /// what they did to files stays. The next turn is numbered on.
    ThreadRewound {
        turn: TurnId,
    },
    TurnCompleted {
        turn: TurnId,
        end: TurnEnd,
        /// Why it ended so, when it did not end with an answer.
        why: Option<String>,
        /// The accepted answer.
        answer: Option<String>,
        /// What the facts of the turn say of the answer, e.g. `NOT verified: …`.
        verification: Option<String>,
    },
    /// The user switched the thread's mode: it holds from its agent's next call on, its
    /// subagents' too.
    ModeChanged {
        mode: Mode,
    },
    /// More of a call the model is writing, for display while it writes it. Not kept, as
    /// deltas are not: the call's item starts once the generation ended, or a `void` item
    /// says why nothing ran.
    CallDraft {
        draft: CallDraft,
    },
}

/// How far a thread's agent goes without the user (docs/architecture.md §8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Every edit, and every command that does more than read, asks first.
    Manual,
    /// The agent's edits inside the project apply at once and are reviewed in Changes;
    /// commands ask as the policy says.
    #[default]
    EditAutomatically,
    /// The agent reads and plans, and changes nothing: edits, and commands that do more
    /// than read, are refused.
    Plan,
    /// What would ask goes ahead, except edits of protected paths and outside the project
    /// and what a rule asks for or denies.
    Auto,
}

/// What a call the model is writing adds ([`EventKind::CallDraft`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CallDraft {
    /// The generation that writes it, by its number in the thread, as its items have it.
    pub generation: u32,
    /// The call's place among the generation's calls, from 0: its items start in this
    /// order.
    pub index: u32,
    /// The tool by Claude Code's name (`Write`, `Edit`, `ApplyPatch`, `Bash`, ...), or by
    /// the model's for one ognivo does not know; `None` until the model named it.
    pub tool: Option<String>,
    /// The argument `text` adds to, by ognivo's name for it (`path`, `content`, `old`,
    /// `new`, `patch`, `command`, ...); `None` with the first event of a call.
    pub field: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ThreadInfo {
    /// `chat`, `run` or `subagent`.
    pub kind: String,
    /// The project's root.
    pub project: String,
    /// The profile, e.g. `gpt-oss-20b`.
    pub profile: String,
    /// The reasoning level, where the profile has levels.
    pub level: Option<String>,
    /// The server's URL.
    pub server: String,
    /// The model each request names, for llama-server in router mode.
    pub model: Option<String>,
    /// For a subagent: the thread and the item it works for.
    pub parent: Option<Parent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Parent {
    pub thread: ThreadId,
    pub item: ItemId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Item {
    pub id: ItemId,
    pub turn: TurnId,
    /// The generation it came from, by its number in the thread (from 1); `None` for what
    /// is ognivo's.
    pub generation: Option<u32>,
    #[serde(flatten)]
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    /// Text the model shows the user: a preamble, or an answer.
    AgentMessage { text: String },
    /// Its chain of thought: Harmony analysis, Gemma's thought channel, Qwen's `<think>`.
    Reasoning { text: String },
    ToolCall {
        /// The model's id for the call.
        call_id: String,
        /// The tool by the model's name for it.
        tool: String,
        /// The arguments as the model wrote them.
        arguments: String,
        /// The call as ognivo runs it; `None` when the arguments do not make one.
        call: Option<Call>,
        /// `None` while it runs.
        result: Option<ToolResult>,
        /// The files it changed, each against what it was.
        changes: Vec<FileChange>,
    },
    /// What ognivo told the model between generations: limits, corrections, notes.
    Notice { text: String },
    /// A generation that ran nothing.
    Void {
        why: VoidReason,
        /// Its calls as the model wrote them, which did not run.
        calls: Vec<RawCall>,
    },
    /// The history became one message (docs/architecture.md §5.6): the prompt would
    /// have held about `tokens`, over the `limit`.
    Compaction {
        tokens: u32,
        limit: u32,
        /// Messages it replaced; `None` while the model writes its summary.
        replaced: Option<u32>,
        /// Steps kept as they were.
        kept: Option<u32>,
        summary: Option<String>,
    },
    /// A question to the user, and the answer once there is one.
    Approval {
        tool: String,
        /// What would happen: the command line, the file path.
        action: String,
        reason: String,
        /// For running project code: what is unreviewed.
        unreviewed: Vec<String>,
        /// The subagent that asks, by its task's title.
        agent: Option<String>,
        allowed: Option<bool>,
    },
    /// Instruction files (AGENTS.md, CLAUDE.md) the model got, and those it did not.
    Instructions {
        files: Vec<String>,
        skipped: Vec<Skipped>,
    },
    /// A task of `delegate`, worked on in a thread of its own.
    Subagent {
        /// Its number among the call's tasks, from 0.
        index: u32,
        title: String,
        /// The files of a write task; `None` for a read task.
        write: Option<Vec<String>>,
        thread: Option<ThreadId>,
        /// How its turn ended, as [`EventKind::TurnCompleted`] says; `None` while it works.
        end: Option<TurnEnd>,
        steps: Option<u32>,
        /// The files it changed.
        changed: Vec<String>,
    },
}

/// A call as ognivo runs it, by Claude Code's name for the tool, with ognivo's names for
/// the arguments whatever the model's were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "name")]
pub enum Call {
    Read {
        path: String,
        offset: Option<u32>,
        limit: Option<u32>,
    },
    Edit {
        path: String,
        old: String,
        new: String,
        replace_all: bool,
    },
    Write {
        path: String,
        content: String,
    },
    ApplyPatch {
        patch: String,
    },
    Bash {
        command: String,
        timeout_ms: Option<u32>,
    },
    Grep {
        pattern: String,
        path: Option<String>,
        glob: Option<String>,
    },
    Glob {
        pattern: String,
        path: Option<String>,
    },
    /// `delegate`.
    Task {
        tasks: Vec<Task>,
    },
    /// A tool of an MCP server, `mcp__<server>__<tool>` to the model, or one it called
    /// through `mcp_call`.
    Mcp {
        server: String,
        tool: String,
        /// The arguments as the model wrote them, a JSON object.
        arguments: String,
    },
    /// `tool_search`: the MCP servers' tools that fit `query`, when there are too many to
    /// declare.
    ToolSearch {
        query: String,
    },
    /// The skill tool: a skill of the user or the project, loaded for the model.
    Skill {
        skill: String,
        arguments: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Task {
    pub title: String,
    pub goal: String,
    pub context: Option<String>,
    pub write: bool,
    pub files: Vec<String>,
    /// The subagent of `.claude/agents` it went to; `None`: the general-purpose one.
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ToolResult {
    pub outcome: Outcome,
    /// The result as the model got it.
    pub text: String,
    /// A command's exit code; `None` for a command a signal killed, and for other tools.
    pub exit_code: Option<i32>,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It ran; a command may still have failed, as its exit code says.
    Ok,
    Error,
    /// It needed an approval and did not get it, or a rule denies it.
    Refused,
    /// ognivo did not run it: a repeat with nothing changed, or past the turn's end.
    Skipped,
}

/// A file a call wrote, as a line diff against what was there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct FileChange {
    /// Relative to the project root inside it, absolute outside.
    pub path: String,
    pub created: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Hunk {
    /// The first line in the old and the new text, from 1.
    pub old_start: u32,
    pub new_start: u32,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum VoidReason {
    /// Its calls did not parse.
    Malformed,
    /// Neither a call nor an answer: it stopped inside its reasoning.
    Empty,
    /// It reached the step's token limit.
    Truncated,
    /// It kept repeating itself and was stopped.
    Repeating,
    /// The server rejected its format, and the step started over: what it showed is void.
    Restarted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RawCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Skipped {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Generation {
    /// Its number in the thread, from 1.
    pub index: u32,
    pub turn: TurnId,
    /// `step`, or `summary` for a compaction's.
    pub purpose: String,
    /// `calls`, `answer`, `truncated`, `repeating` or `malformed`.
    pub outcome: String,
    /// The server's finish reason: `tool_calls`, `stop`, `length`.
    pub finish: Option<String>,
    pub seed: u32,
    pub prompt_tokens: u32,
    /// Of `prompt_tokens`, those the server took from its KV cache.
    pub cached_tokens: u32,
    pub completion_tokens: u32,
    pub prompt_ms: Option<f64>,
    pub generation_ms: Option<f64>,
    /// sha256 of the request's messages and tools.
    pub prompt_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    /// More text of an agent message or of reasoning.
    Text { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    Answer,
    /// The step limit came before an answer.
    StepLimit,
    /// Generations without usable calls, or a call that kept repeating.
    Stuck,
    TimeLimit,
    /// The server failed.
    Error,
    /// The user stopped it.
    Interrupted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_its_place_its_thread_and_what_happened() {
        let ev = Event {
            seq: 7,
            thread: ThreadId(3),
            kind: EventKind::ItemDelta {
                item: ItemId(2),
                delta: Delta::Text { text: "hi".into() },
            },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "seq": 7, "thread": 3, "event": "item_delta", "item": 2,
                "delta": {"type": "text", "text": "hi"}
            })
        );
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), ev);
    }
}
