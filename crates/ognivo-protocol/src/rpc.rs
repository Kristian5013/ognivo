//! The methods of `ognivo core` (docs/architecture.md §3): JSON-RPC 2.0 over its Unix
//! socket, one message a line.
//!
//! - A client calls `initialize` first. From then on the core sends it every thread's
//!   events as `event` notifications, numbered by `seq` (see [`crate::Event`]).
//! - What only the user can answer, the core asks as requests of its own:
//!   [`APPROVAL_ASK`] and [`RULES_TRUST`]. It asks every client, and a client that comes
//!   while a question waits gets it too. The first answer counts. A question that is
//!   answered elsewhere or no longer waits is withdrawn with [`CANCEL`].
//! - A client that comes back reads what it missed: `thread/read` with the last `seq` it
//!   saw. Text deltas are not kept; the items they make up are.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Event, Hunk, ItemId, ThreadId, ThreadInfo};

pub const INITIALIZE: &str = "initialize";
pub const THREAD_LIST: &str = "thread/list";
pub const THREAD_START: &str = "thread/start";
pub const THREAD_RESUME: &str = "thread/resume";
pub const THREAD_READ: &str = "thread/read";
pub const THREAD_TRAJECTORY: &str = "thread/trajectory";
pub const THREAD_CLEAR: &str = "thread/clear";
pub const THREAD_CLOSE: &str = "thread/close";
pub const THREAD_DELETE: &str = "thread/delete";
pub const TURN_START: &str = "turn/start";
pub const TURN_INTERRUPT: &str = "turn/interrupt";
pub const TURN_ANSWER_NOW: &str = "turn/answer_now";
pub const TURN_EDIT: &str = "turn/edit";
pub const CHANGES_LIST: &str = "changes/list";
pub const CHANGES_ACCEPT: &str = "changes/accept";
pub const CHANGES_REVERT: &str = "changes/revert";

/// The core's notification of an event.
pub const EVENT: &str = "event";
/// The core's requests.
pub const APPROVAL_ASK: &str = "approval/ask";
pub const RULES_TRUST: &str = "rules/trust";
pub const MCP_TRUST: &str = "mcp/trust";
/// The core's notification that a request of its own no longer waits for an answer.
pub const CANCEL: &str = "$/cancelRequest";

/// Errors of the methods, besides JSON-RPC's own.
pub mod error {
    /// No such thread, or one the core does not have open.
    pub const NO_THREAD: i64 = -32001;
    /// A turn of the thread runs.
    pub const BUSY: i64 = -32002;
    /// The thread could not start: its server, its profile, the store.
    pub const NOT_STARTED: i64 = -32003;
    /// The edit journal could not do it.
    pub const JOURNAL: i64 = -32004;
    /// The thread's path cannot do it: only the Native path ends the reasoning.
    pub const UNSUPPORTED: i64 = -32005;
    /// The turn is not in the conversation any more: a `/clear` or a compaction came after.
    pub const NO_TURN: i64 = -32006;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Initialize {
    /// The client and its version, e.g. `ognivo-desktop 0.0.1`.
    pub client: String,
    /// The version of the event model it reads ([`crate::VERSION`]).
    pub protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Initialized {
    pub protocol: u32,
    /// The core's version.
    pub core: String,
    /// The last event's place in the stream: the client gets the events after it.
    #[ts(type = "number")]
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ListThreads {
    /// The project's root; every project's threads when `None`.
    pub project: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Threads {
    /// The users' threads, the last updated first: subagents' threads are their parents'.
    pub threads: Vec<ThreadSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ThreadSummary {
    pub thread: ThreadId,
    /// `chat` or `run`.
    pub kind: String,
    pub project: String,
    pub profile: String,
    pub level: Option<String>,
    /// The server's URL, and the model behind llama-server's router.
    pub server: String,
    pub model: Option<String>,
    /// The first line of its first message.
    pub title: String,
    /// Unix milliseconds.
    #[ts(type = "number")]
    pub created: i64,
    #[ts(type = "number")]
    pub updated: i64,
    pub turns: u32,
    /// Files with unreviewed changes in its edit journal.
    pub unreviewed: u32,
    /// Whether the core has it open, and whether a turn of it runs.
    pub open: bool,
    pub running: bool,
    /// Open on the Native path: `turn/answer_now` ends the model's reasoning.
    pub answer_now: bool,
}

/// How a thread reaches its model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Server {
    /// The server's base URL, e.g. `http://127.0.0.1:8080`.
    pub url: String,
    /// The bearer token of a server started with `--api-key`.
    pub api_key: Option<String>,
    /// For llama-server's router: the model, by its id or alias.
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StartThread {
    /// The project's root.
    pub project: String,
    pub server: Server,
    /// The profile; the server's model decides when `None`.
    pub profile: Option<String>,
    /// The reasoning level; the profile's default when `None`.
    pub level: Option<String>,
    /// The Native path (gpt-oss): ognivo renders the model's format itself.
    pub native: bool,
    /// With `native`: gpt-oss gets the tools of its training.
    pub priors: bool,
    /// The agent may hand tasks to subagents.
    pub subagents: bool,
}

/// A stored thread, picked up where it ended, with its profile, level and seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ResumeThread {
    pub thread: ThreadId,
    /// Where its model is now; the server it last had when `None`.
    pub server: Option<Server>,
    pub native: bool,
    pub priors: bool,
    pub subagents: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Opened {
    pub thread: ThreadId,
    pub info: ThreadInfo,
    /// What the start found, as `ognivo chat` says it under its banner: instruction and
    /// rule files, a tree that is not clean, a context smaller than the profile plans for.
    pub notes: Vec<String>,
    /// On the Native path: `turn/answer_now` ends the model's reasoning.
    pub answer_now: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ThreadRef {
    pub thread: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ReadThread {
    pub thread: ThreadId,
    /// Only what came after this place in the stream.
    #[ts(type = "number | null")]
    pub after: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct ThreadEvents {
    /// The kept events of the thread and of its subagents' threads, in order. None for a
    /// thread from before the core kept them, or from `chat` and `run`: its trajectory
    /// tells what happened.
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct Trajectory {
    /// The thread in ATIF, as `ognivo run --atif` writes it.
    #[ts(type = "unknown")]
    pub trajectory: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct StartTurn {
    pub thread: ThreadId,
    /// The user's message.
    pub text: String,
}

/// `turn/edit`: the conversation goes back to before `turn`, whose message becomes `text`,
/// and a turn starts with it (an event `thread_rewound`, then the turn's). What the turns
/// taken back did to files stays, and the model is told so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EditTurn {
    pub thread: ThreadId,
    pub turn: crate::TurnId,
    pub text: String,
}

/// What the agent changed and nobody reviewed yet: its edit journal (§8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Changes {
    pub files: Vec<ChangedFile>,
    /// Commands with side effects, which a revert does not undo.
    pub commands: Vec<String>,
    /// The project may hold changes ognivo did not make: builds and tests ask until they
    /// are accepted.
    pub inherited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ChangedFile {
    /// Relative to the project root inside it, absolute outside.
    pub path: String,
    pub created: bool,
    /// The file is gone.
    pub deleted: bool,
    /// Something else changed it too since the agent's last edit: a revert loses that.
    pub drifted: bool,
    /// Against what it was before the session.
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Accept {
    pub thread: ThreadId,
    /// One file; all of it when `None`.
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Accepted {
    /// What was accepted.
    pub accepted: Vec<String>,
    /// Whether anything is still unreviewed: builds and tests ask until nothing is.
    pub unreviewed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Revert {
    pub thread: ThreadId,
    pub path: String,
    /// Revert a drifted file too, losing what else changed it.
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Reverted {
    /// The agent had created the file, and it is gone.
    pub removed: bool,
}

/// The result of a method that returns nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Done {}

/// An item of a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ItemRef {
    pub thread: ThreadId,
    pub item: ItemId,
}

/// `approval/ask`: an action that needs the user's yes (§8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalAsked {
    /// The user's thread whose agent, or subagent, asks.
    pub thread: ThreadId,
    /// The question's item in the stream: in a subagent's own thread when one asks.
    pub item: Option<ItemRef>,
    pub tool: String,
    /// What would happen: the command line, the file path.
    pub action: String,
    pub reason: String,
    /// For running project code: what is unreviewed.
    pub unreviewed: Vec<String>,
    /// The subagent that asks, by its task's title.
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    /// This action, the same command or path, for the rest of the session.
    AllowSession,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApprovalAnswer {
    pub decision: Decision,
    /// With `deny`: what the agent should do instead.
    pub instead: Option<String>,
}

/// `rules/trust`: the project's settings allow actions without a question, and count only
/// once the user trusts them; the answer is kept until they change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TrustAsked {
    pub project: String,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Rule {
    pub text: String,
    /// The file it comes from.
    pub source: String,
    /// It allows any command at all.
    pub any: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TrustAnswer {
    pub trust: bool,
}

/// `mcp/trust`: the project's `.mcp.json` starts servers, each a command of the project's
/// or a URL; they start once the user trusts exactly them. The answer is `TrustAnswer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpTrustAsked {
    pub project: String,
    pub servers: Vec<McpServer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpServer {
    pub name: String,
    /// What starts it: the command line, or the URL.
    pub command: String,
}

/// `$/cancelRequest`: the core's request `id` no longer waits for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Cancel {
    #[ts(type = "number")]
    pub id: u64,
}
