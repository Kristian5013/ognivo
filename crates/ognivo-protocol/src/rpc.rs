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

use crate::{Event, Hunk, ItemId, Mode, ThreadId, ThreadInfo};

pub const INITIALIZE: &str = "initialize";
pub const THREAD_LIST: &str = "thread/list";
pub const SERVER_DETECT: &str = "server/detect";
pub const MCP_SERVERS: &str = "mcp/servers";
pub const MCP_CONFIG: &str = "mcp/config";
pub const MCP_CONFIG_SET: &str = "mcp/config/set";
pub const MCP_LOGIN: &str = "mcp/login";
pub const MCP_LOGOUT: &str = "mcp/logout";
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
pub const THREAD_MODE: &str = "thread/mode";
pub const CHANGES_LIST: &str = "changes/list";
pub const CHANGES_ACCEPT: &str = "changes/accept";
pub const CHANGES_REVERT: &str = "changes/revert";

/// The core's notification of an event.
pub const EVENT: &str = "event";
/// The core's requests.
pub const APPROVAL_ASK: &str = "approval/ask";
pub const RULES_TRUST: &str = "rules/trust";
pub const MCP_TRUST: &str = "mcp/trust";
pub const MCP_SIGN_IN: &str = "mcp/sign_in";
pub const HOOKS_TRUST: &str = "hooks/trust";
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
    /// Open: the context it works with.
    pub context: Option<Context>,
    /// Open: its mode.
    pub mode: Option<Mode>,
}

/// The context a thread works with, in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Context {
    /// Its working context: what the user chose, or else the server's window, or else the
    /// profile's; never past what the model works with.
    pub working: u32,
    /// What the conversation may hold before it is compacted.
    #[ts(type = "number")]
    pub compacts_at: u64,
    /// The context of the server's slot, as `/props` says it.
    pub window: Option<u32>,
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

/// `server/detect`: the profile of the model a server runs, as a thread there would find it,
/// with its reasoning levels; params are a `Server`. Behind llama-server's router it is of
/// the model the params name, or else of the one model loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Detected {
    /// `None`: no profile fits the server's model.
    pub profile: Option<String>,
    /// The profile's levels, and the one a thread gets without a level of its own.
    pub levels: Vec<String>,
    pub default_level: Option<String>,
    /// Whether the Native path reads the model's format.
    pub native: bool,
    /// The context of the server's slot, as `/props` says it; `None` for a router's model
    /// that is not loaded.
    pub window: Option<u32>,
    /// llama-server's router: its models; empty for a server of one model.
    pub models: Vec<RoutedModel>,
    /// The router's model the rest is of; `None` when the router runs several, or none,
    /// and the params name none of its models.
    pub model: Option<String>,
}

/// A model of llama-server's router.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RoutedModel {
    /// Its id, as a `Server` names it.
    pub id: String,
    /// `loaded`, `loading`, `unloaded`, ...: a thread has the router start one not loaded.
    pub status: String,
    /// The profile its names pick, if one does.
    pub profile: Option<String>,
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
    /// Tokens of working context; the server's window when `None`.
    #[serde(default)]
    pub context: Option<u32>,
    /// How far its agent goes without the user; `edit_automatically` when `None`.
    #[serde(default)]
    pub mode: Option<Mode>,
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
    /// Tokens of working context; the server's window when `None`.
    #[serde(default)]
    pub context: Option<u32>,
    /// How far its agent goes without the user; `edit_automatically` when `None`.
    #[serde(default)]
    pub mode: Option<Mode>,
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
    pub context: Context,
    pub mode: Mode,
}

/// `thread/mode`: the open thread's mode, from its agent's next call on, while a turn runs
/// too; an event `mode_changed` tells every client. The answer is `Done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SetMode {
    pub thread: ThreadId,
    pub mode: Mode,
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

/// `mcp/sign_in`: a remote server of the thread wants the user to sign in (OAuth). The
/// answer is `SignInAnswer`; with yes the core opens the user's browser on the server's
/// page and waits for it to come back, five minutes at most, before the thread starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpSignInAsked {
    pub project: String,
    pub server: McpServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SignInAnswer {
    pub sign_in: bool,
}

/// `hooks/trust`: the project's settings run commands on the agent's events (Claude Code's
/// hooks); they run once the user trusts exactly them. The answer is `TrustAnswer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HooksTrustAsked {
    pub project: String,
    pub hooks: Vec<HookShown>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HookShown {
    /// The event and the matcher, then the command: `PreToolUse Bash: ./check.sh`.
    pub hook: String,
    /// The settings file it comes from.
    pub source: String,
}

/// `mcp/servers`: the MCP servers of a thread as it started them, with their tools and
/// prompts; params are a `ThreadRef`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpServers {
    pub servers: Vec<McpServerState>,
    /// Servers and files of the config that could not be used, with why.
    pub problems: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpServerState {
    pub name: String,
    /// The config file it comes from.
    pub source: String,
    /// What starts it: the command line, or the URL.
    pub command: String,
    /// A server over HTTP, which may want a sign-in.
    pub remote: bool,
    /// `running`, `failed`, `sign_in` (it wants one) or `untrusted` (the project's, which
    /// waits for the user's trust).
    pub state: String,
    pub why: Option<String>,
    pub signed_in: bool,
    /// What the server says about using it.
    pub instructions: Option<String>,
    pub tools: Vec<McpToolInfo>,
    pub prompts: Vec<McpPromptInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpToolInfo {
    /// The name the model calls it by: `mcp__<server>__<tool>`.
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpPromptInfo {
    /// The name the user runs it by, after `/`.
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpPromptArgument {
    pub name: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRef {
    pub project: String,
}

/// `mcp/config`: the files that configure a project's MCP servers, with their servers as
/// written; params are a `ProjectRef`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct McpConfig {
    pub files: Vec<McpConfigFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct McpConfigFile {
    /// `user` (`~/.ognivo/mcp.json`), `project` (`.mcp.json`) or `claude` (Claude Code's
    /// `~/.claude.json`, which ognivo reads and does not write).
    pub scope: String,
    pub path: String,
    pub servers: Vec<McpEntry>,
    /// Why the file could not be read.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct McpEntry {
    pub name: String,
    /// The entry as the file holds it: `command`, `args`, `env`, or `type`, `url`,
    /// `headers`, `oauth`.
    #[ts(type = "Record<string, unknown>")]
    pub value: serde_json::Value,
}

/// `mcp/config/set`: the server `name` of the `user` or `project` file set to `value`, or
/// removed when it is null. A thread that starts after it gets it. The answer is `Done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct McpConfigSet {
    pub project: String,
    pub scope: String,
    pub name: String,
    #[ts(type = "Record<string, unknown> | null")]
    pub value: Option<serde_json::Value>,
}

/// `mcp/login` (in the user's browser, as `ognivo mcp login`) and `mcp/logout`: a remote
/// server of the project's config; the answer is `Done`. A thread that starts after it uses
/// the sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct McpServerRef {
    pub project: String,
    pub server: String,
}

/// `$/cancelRequest`: the core's request `id` no longer waits for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Cancel {
    #[ts(type = "number")]
    pub id: u64,
}
