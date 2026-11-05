//! The agent loop (docs/architecture.md §6): generate, check, decide, execute, append,
//! until the model answers.
//!
//! - Only [`Outcome::Calls`] runs anything; the driver grants it once the generation
//!   ended normally with `finish_reason=tool_calls` and complete arguments.
//! - A cut-off or malformed generation runs nothing and is not added to the history; the
//!   model gets a user-role note instead. An assistant turn without calls in the middle of
//!   a tool loop would make gpt-oss's template drop all earlier analysis (M0, cpuset). An
//!   empty answer is such a generation too: the model stopped inside its reasoning (M1
//!   stamp A', gpt-oss, twice).
//! - Every call is checked against its declared schema, computed without side effects,
//!   decided by the approval policy ([`crate::approval`]), and only then executed.
//! - A call that only repeats the result of an identical call while nothing changed is
//!   marked, then skipped, then ends the turn ([`crate::repeats`]).
//! - Before an answer is accepted, the loop compares it with the facts of the turn
//!   ([`crate::verify`]): a change after the last passing build or test sends the model
//!   back once to run one (when it can), an answer claiming checks that never happened is
//!   sent back once to be corrected, and the user always gets the verification status.
//! - A turn may have a time limit (M0 requirement 4). The model is told the limit, the
//!   elapsed half and 80%; a command never runs past it, and at the limit the loop stops
//!   the generation or the command and ends the turn itself.
//! - With subagents, `delegate` hands tasks to agents of their own that work at the same
//!   time ([`delegate`]).

mod delegate;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use ognivo_models::profile::ContextBudget;
use ognivo_models::{
    Channel, ChatMessage, DriverError, DriverEvent, Family, FinishReason, Finished, ModelDriver,
    Outcome, StepRequest, ToolCall, ToolChoice, ToolDecl,
};
use ognivo_tools::diff::Hunk;
use ognivo_tools::exec::{self, ShellOptions};
use ognivo_tools::files::{self, EditPlan, FileError, LineStyle, Target};
use ognivo_tools::projection::{self, ArgError, Call};
pub use ognivo_tools::projection::{Layout, Toolset};
use ognivo_tools::{Effect, ExecContext, ToolId, classify_command, search};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::agents::Templates;
use crate::approval::{self, Decision, FileEdit, WorkspaceState};
use crate::compact;
use crate::hooks::{self, Event, Hooks, Permission};
use crate::instructions::{Given, Instructions, Load, Skipped, UserInstructions};
use crate::journal::{FileChange, Journal, JournalError, Reverted};
use crate::repeats::{Check, Fingerprint, Repeats};
use crate::rules::{Rules, Ruling};
use crate::session::{Notes, Resumed, Session};
use crate::skills::{self, Part, Skill, Skills, Vars};
use crate::verify::{self, Ledger, Run, Verification};

pub use delegate::{SameModel, Subagents};

/// How many generations in a row may end without usable calls before the turn stops.
const MAX_UNUSABLE_IN_A_ROW: u32 = 3;
/// Calls executed from one generation; the rest get an error result.
const MAX_CALLS_PER_STEP: usize = 8;
/// Time a command leaves before the turn's limit, for the answer and the exit.
const TIME_RESERVE: Duration = Duration::from_secs(5);

/// An action that needs the user's yes (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub tool: String,
    /// What would happen, e.g. the command line or the file path.
    pub action: String,
    pub reason: String,
    /// For running project code: what is unreviewed, one item each (§8); otherwise empty.
    pub unreviewed: Vec<String>,
    /// What the model changes files with, for a refusal to point there: `the edit and
    /// write tools`, or `apply_patch` with the tools of gpt-oss's training.
    pub edit_with: &'static str,
    /// The subagent that asks, by its task's title; `None`: the user's agent.
    pub agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refused; the text tells the model why.
    Refuse(String),
}

pub trait Approver: Send {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict>;
    /// Whether a question can get a yes at all; a run without a user cannot.
    fn can_ask(&self) -> bool;
}

/// Headless default: nobody can answer, so every question is a refusal. The refusal says
/// not to work around it: told to "do the task another way", models went on from a refused
/// `cargo test` to `cargo build`, `cargo check` and `rustc` until the step limit (the T2
/// night of 2026-11-02).
#[derive(Debug, Default)]
pub struct NoUser;

impl Approver for NoUser {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        let text = format!(
            "{} needs the user's approval ({}), and this run has no user. Do not retry it or work around it with another command: change files with {}, and if it was a check, finish without it. Say in your final answer what did not run.",
            req.action, req.reason, req.edit_with
        );
        Box::pin(async move { Verdict::Refuse(text) })
    }

    fn can_ask(&self) -> bool {
        false
    }
}

/// Approves everything. Only for runs inside a disposable eval container: the caller must
/// check that before using it (owner's decision of 2026-10-31).
#[derive(Debug, Default)]
pub struct AllowAll;

impl Approver for AllowAll {
    fn approve<'a>(&'a mut self, _: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async { Verdict::Allow })
    }

    fn can_ask(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A turn began: the user's message, or a subagent's task.
    TurnStarted {
        user: String,
    },
    Reasoning {
        text: String,
    },
    Text {
        text: String,
    },
    /// The server rejected the output format; the step restarted and what was shown of it
    /// is void.
    Restarted {
        reason: String,
    },
    ToolStarted {
        /// The model's id for the call.
        id: String,
        tool: String,
        arguments: String,
        /// The call as ognivo runs it; `None` when the arguments do not parse.
        #[serde(skip)]
        call: Option<Call>,
    },
    /// A file the call wrote, as a line diff against what was there.
    FileChanged {
        /// Relative to the project root inside it, absolute outside.
        path: String,
        created: bool,
        hunks: Vec<Hunk>,
    },
    ToolFinished {
        id: String,
        tool: String,
        result: String,
    },
    /// Instruction files (AGENTS.md, CLAUDE.md, their imports) the model got after a
    /// call's result, from the directories on the path of the file it read or changed, and
    /// those it could not get.
    Instructions {
        files: Vec<Given>,
        skipped: Vec<Skipped>,
    },
    Refused {
        tool: String,
        reason: String,
    },
    Notice {
        text: String,
    },
    /// The history is about to be compacted (§5.6): the next prompt would hold about
    /// `tokens`, over the `limit`.
    Compacting {
        tokens: u64,
        limit: u64,
    },
    /// The earlier history became one message with the user's requests, the model's
    /// summary and ognivo's facts.
    Compacted {
        /// Messages it replaced.
        replaced: usize,
        /// Steps kept as they were.
        kept: usize,
        /// The model's summary; `None` when it wrote none.
        summary: Option<String>,
    },
    /// `delegate` started a subagent on the task numbered `index` (from 0).
    SubagentStarted {
        index: usize,
        title: String,
        /// The files of a write task; `None` for a read task.
        write: Option<Vec<String>>,
        /// The session it is kept in, if the session is kept.
        session: Option<i64>,
        /// Its user's message: the task as it gets it.
        brief: String,
        level: Option<String>,
    },
    /// An event of a subagent's turn.
    Subagent {
        index: usize,
        event: Box<AgentEvent>,
    },
    /// A subagent's turn ended: `end` is `answer`, `step_limit`, `stuck`, `time_limit` or
    /// `error`.
    SubagentFinished {
        index: usize,
        end: String,
        steps: u32,
        /// The files it changed.
        changed: Vec<String>,
    },
    /// A generation ended, a compaction's summary too: how, and what it cost. `outcome`:
    /// `calls`, `answer`, `empty`, `truncated`, `repeating` or `malformed`.
    Generation {
        purpose: String,
        outcome: String,
        finish: Option<String>,
        seed: u32,
        /// With those the server took from its cache.
        prompt_tokens: u32,
        cached_tokens: u32,
        completion_tokens: u32,
        prompt_ms: Option<f64>,
        generation_ms: Option<f64>,
        prompt_hash: String,
        /// The calls of a generation that ran nothing, as the model wrote them.
        rejected: Vec<ToolCall>,
    },
    /// A question to the user (`allowed` is `None`), then the answer.
    Approval {
        tool: String,
        action: String,
        reason: String,
        unreviewed: Vec<String>,
        agent: Option<String>,
        allowed: Option<bool>,
    },
    /// The turn ended: `end` as [`AgentEvent::SubagentFinished`] has it.
    TurnEnded {
        end: String,
        why: Option<String>,
        answer: Option<String>,
        /// What the facts of the turn say of the answer.
        verification: Option<String>,
    },
}

/// Counters behind the M1 exit metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub steps: u32,
    pub calls: u32,
    /// Generations whose calls could not be used (parse failures on the Generic path).
    pub malformed: u32,
    /// Generations cut by `max_tokens`.
    pub truncated: u32,
    /// Generations the repetition guard stopped.
    pub repeating: u32,
    /// Generations that ended with neither a call nor any answer text.
    pub empty_answers: u32,
    /// Format errors the driver retried.
    pub restarts: u32,
    /// Calls whose arguments did not match the declared schema.
    pub bad_arguments: u32,
    pub edits_applied: u32,
    pub edits_already_applied: u32,
    pub edits_failed: u32,
    pub refused: u32,
    /// Calls that gave the same result as an identical call while nothing changed.
    pub repeated: u32,
    /// Repeated calls that were not run.
    pub repeats_skipped: u32,
    /// Answers sent back before acceptance: to run a build or test, or to drop a claim.
    pub answers_sent_back: u32,
    pub compactions: u32,
    /// Prompt tokens the server processed, and those it took from the KV cache.
    pub prompt_tokens_processed: u64,
    pub prompt_tokens_cached: u64,
    pub completion_tokens: u64,
    /// Subagents `delegate` started, their steps and the tokens they generated.
    pub subagents: u32,
    pub subagent_steps: u32,
    pub subagent_tokens: u64,
}

/// The accepted final answer and what the turn's facts say about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub text: String,
    pub verification: Verification,
    /// The text still claims a build or test that did not happen after the last change.
    pub unsupported_claim: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    Answer(Answer),
    /// The step limit came before the model answered on its own: the answer it gave then,
    /// asked for once more without tools, if it gave one.
    StepLimit(Option<Answer>),
    /// Several generations in a row ended without usable calls, or a call kept repeating
    /// itself with nothing new in between.
    Stuck(String),
    /// The turn's time limit ran out before an answer.
    TimeLimit,
    /// Nothing reached the model: the user's command could not be rendered.
    NotRun(String),
}

#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Project root, canonical.
    pub root: PathBuf,
    /// `~/.ognivo/data` or the run's own data directory.
    pub data_dir: PathBuf,
    pub max_steps: u32,
    /// The profile's working budget and `max_tokens` of a step.
    pub context: ContextBudget,
    /// The server slot's context, if the server says.
    pub n_ctx: Option<u32>,
    /// Seed of the first step; each step adds one, so a run is reproducible.
    pub seed: u32,
    /// Review state at the start: clean only when nothing unreviewed is known to exist
    /// (for now: a clean git tree). The edit journal follows the session's own changes.
    pub workspace: WorkspaceState,
    /// Wall-clock limit of each turn.
    pub time_limit: Option<Duration>,
    /// The user's own instruction files; the project's are found from `root`.
    pub user_instructions: UserInstructions,
    /// Permission rules on top of the policy (§8).
    pub rules: Rules,
    /// Which tools the model gets ([`Layout::Priors`]: gpt-oss's own, on the Native path).
    pub layout: Layout,
    /// Which of the layout's tools: [`Toolset::Delegating`] gives `delegate` too, when
    /// there are `subagents`.
    pub toolset: Toolset,
    /// A subagent's write task: the only files it may change.
    pub writable: Option<Vec<PathBuf>>,
    /// Where the subagents of `delegate` come from.
    pub subagents: Option<Arc<dyn Subagents>>,
    /// The session's MCP servers, whose tools come after ognivo's.
    pub mcp: Option<Arc<ognivo_mcp::Gateway>>,
    /// The user's and the project's skills and commands ([`crate::skills`]).
    pub skills: Skills,
    /// The hooks of the user's and the project's settings ([`crate::hooks`]).
    pub hooks: Arc<Hooks>,
    /// The subagent templates of `.claude/agents`, which `delegate` gives tasks to.
    pub agents: Templates,
    /// A subagent of a template: the only tools it has.
    pub only: Option<Vec<ToolId>>,
}

/// The time limit of the running turn.
#[derive(Debug, Clone, Copy)]
struct Clock {
    start: Instant,
    limit: Duration,
    /// Shares of the limit already announced to the model.
    told: u8,
}

impl Clock {
    fn deadline(&self) -> Instant {
        self.start + self.limit
    }

    fn left(&self) -> Duration {
        self.deadline().saturating_duration_since(Instant::now())
    }

    /// The note for a share of the limit passed since the last one: half, then 80%.
    fn note(&mut self) -> Option<String> {
        let used = self.start.elapsed().as_secs_f64() / self.limit.as_secs_f64();
        let left = about(self.left());
        if used >= 0.8 && self.told < 2 {
            self.told = 2;
            Some(format!(
                "[ognivo] 80% of the time limit is used, about {left} left. Finish now: complete the change, check it once, then give the final answer; start no long commands or experiments."
            ))
        } else if used >= 0.5 && self.told < 1 {
            self.told = 1;
            Some(format!(
                "[ognivo] Half of the time limit is used, about {left} left."
            ))
        } else {
            None
        }
    }
}

/// What the agent asked the model, as a fingerprint for traces: sha256 of its messages
/// and tools as JSON.
fn prompt_hash(req: &StepRequest) -> String {
    let body = json!({"messages": req.messages, "tools": req.tools}).to_string();
    format!("sha256:{}", ognivo_models::template::sha256_hex(&body))
}

/// `15.9K` tokens.
fn about_tokens(n: u64) -> String {
    format!("{:.1}K", n as f64 / 1000.0)
}

/// `3 min` or `40 s`.
fn about(d: Duration) -> String {
    match d.as_secs() {
        s if s >= 120 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

pub struct Agent {
    driver: Box<dyn ModelDriver>,
    family: Family,
    /// The tools it has: those of its options, without `delegate` when there are no
    /// subagents.
    toolset: Toolset,
    tools: Vec<ToolDecl>,
    /// The MCP tools the prompt does not declare ([`StepRequest::callable`]).
    callable: Vec<ToolDecl>,
    opts: AgentOptions,
    approver: Box<dyn Approver>,
    messages: Vec<ChatMessage>,
    cwd: PathBuf,
    /// Files this session has read or written: only those may be overwritten whole.
    seen: HashSet<PathBuf>,
    clock: Option<Clock>,
    instructions: Instructions,
    /// The instruction files of the system prompt.
    prompt_instructions: Load,
    /// The skills, and which of those of subdirectories the conversation has heard of.
    skills: Skills,
    /// The skills the conversation has loaded, with the text each gave.
    loaded_skills: HashMap<String, String>,
    /// What the PreToolUse hooks decided about the running call, and whether it edits.
    hook_permission: Option<(Permission, bool)>,
    /// The SessionStart and SessionEnd hooks due before the next turn: their source and
    /// reason.
    session_start: Option<&'static str>,
    session_end: Option<&'static str>,
    /// A subagent's type: its stop is a SubagentStop.
    subagent: Option<String>,
    journal: Journal,
    /// Why the last turn was stopped, for the model's next message.
    stopped: Option<String>,
    /// Files the user reverted since the model's last turn, for its next message.
    reverted: Vec<String>,
    /// What this turn changed, ran and was refused.
    ledger: Ledger,
    /// This turn's calls and results, for the repeat detector.
    repeats: Repeats,
    /// Set by a call that ends the turn: why.
    stuck: Option<String>,
    /// The id of the call being executed.
    running: Option<String>,
    /// Tokens the server held after the last step.
    context: u64,
    /// Messages the server held then; those after came since.
    counted: usize,
    /// The user's requests of the conversation, oldest first, for a compaction.
    requests: Vec<String>,
    /// Where the running turn's request is in the history.
    turn_start: usize,
    /// Files the turn has read, and whether, since the last compaction, the agent read one
    /// it had not or changed anything: a compaction without that is idle.
    turn_reads: HashSet<PathBuf>,
    news: bool,
    /// Idle compactions in a row.
    idle_compactions: u32,
    /// Where the session is kept, if it is.
    session: Option<Session>,
    /// A turn is running: it began and has not ended.
    in_turn: bool,
    pub stats: Stats,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("driver", &self.driver)
            .field("family", &self.family)
            .field("root", &self.opts.root)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// How many steps of a turn are left when the model is told to finish.
const STEPS_LEFT_NOTE: u32 = 2;

/// How long an MCP tool may take; a turn's time limit cuts it shorter.
const MCP_TIMEOUT: Duration = Duration::from_secs(120);

/// The tools that stand for the MCP servers' when there are too many to declare
/// ([`ognivo_mcp::Listing`]).
const TOOL_SEARCH: &str = "tool_search";
const MCP_CALL: &str = "mcp_call";
/// Tools one search gives at most.
const SEARCH_LIMIT: usize = 5;
/// How long a command of a skill's body may run.
const SKILL_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Times in a row a Stop hook may send the model back (Claude Code: 8).
const STOP_CONTINUES: u32 = 8;

/// The skill tool by the name each family knows such a tool by: Claude Code's for Qwen.
pub fn skill_tool_name(family: Family) -> &'static str {
    match family {
        Family::QwenXml => "Skill",
        Family::Harmony | Family::Gemma4 => "skill",
    }
}

/// The skill tool's arguments in `family`'s terms: the skill, then what it should work on.
/// Gemma writes keys in alphabetical order, so its second one sorts after `name`.
fn skill_keys(family: Family) -> (&'static str, &'static str) {
    match family {
        Family::QwenXml => ("skill", "args"),
        Family::Harmony => ("name", "arguments"),
        Family::Gemma4 => ("name", "task"),
    }
}

/// The skill tool, when there are skills the model may load. Its names are all of them,
/// those of subdirectories too, so that it stays the same for the whole session.
fn skill_tool(family: Family, skills: &Skills) -> Option<ToolDecl> {
    let names: Vec<Value> = skills.for_model().iter().map(|s| json!(s.name)).collect();
    if names.is_empty() {
        return None;
    }
    let (name, args) = skill_keys(family);
    let mut properties = serde_json::Map::new();
    properties.insert(
        name.into(),
        json!({"type": "string", "enum": names, "description": "The skill."}),
    );
    properties.insert(
        args.into(),
        json!({"type": "string", "description": "What the skill should work on, if it takes anything: the words that would follow its name, such as an issue number. Leave it out otherwise."}),
    );
    Some(ToolDecl {
        name: skill_tool_name(family).into(),
        description: "Loads a skill: instructions for a kind of task, from the user or the project, which the project message lists with what each is for. When the task fits one, load it before you start and follow it; the files it names are in its directory.".into(),
        parameters: json!({
            "type": "object",
            "properties": properties,
            "required": [name],
            "additionalProperties": false,
        }),
    })
}

/// A skill tool call: the skill and what it should work on.
fn skill_call(family: Family, arguments: &str) -> Result<Call, ArgError> {
    let tool = || skill_tool_name(family).to_owned();
    let (name_key, args_key) = skill_keys(family);
    let v: Value =
        serde_json::from_str(arguments).map_err(|_| ArgError::NotObject { tool: tool() })?;
    let Some(m) = v.as_object() else {
        return Err(ArgError::NotObject { tool: tool() });
    };
    if let Some(k) = m.keys().find(|k| *k != name_key && *k != args_key) {
        return Err(ArgError::Unknown {
            tool: tool(),
            arg: k.clone(),
            known: format!("{name_key}, {args_key}"),
        });
    }
    let name = match m.get(name_key) {
        Some(Value::String(n)) if !n.trim().is_empty() => n.trim().trim_start_matches('/'),
        _ => {
            return Err(ArgError::Missing {
                tool: tool(),
                arg: name_key.into(),
            });
        }
    };
    let arguments = match m.get(args_key) {
        None | Some(Value::Null) => None,
        Some(Value::String(a)) => Some(a.trim().to_owned()).filter(|a| !a.is_empty()),
        Some(_) => {
            return Err(ArgError::Type {
                tool: tool(),
                arg: args_key.into(),
                want: "string",
            });
        }
    };
    Ok(Call::Skill {
        name: name.to_owned(),
        arguments,
    })
}

/// A command's output without the status lines exec puts before it.
fn shell_output(text: &str) -> &str {
    let mut rest = text;
    for status in ["[shell: ", "[cwd: "] {
        if rest.starts_with(status) {
            rest = rest.split_once('\n').map_or("", |(_, r)| r);
        }
    }
    rest
}

/// The tools an agent declares: ognivo's for its family and layout, then those of its MCP
/// servers, by name, or `tool_search` and `mcp_call` in their place.
fn tools_of(family: Family, opts: &AgentOptions, toolset: Toolset, by_name: bool) -> Vec<ToolDecl> {
    let mut tools = projection::declarations_for(family, opts.layout, toolset);
    tools.extend(skill_tool(family, &opts.skills));
    if toolset == Toolset::Delegating && !opts.agents.all.is_empty() {
        let name = projection::delegate_name(family);
        if let Some(d) = tools.iter_mut().find(|t| t.name == name) {
            let mut names: Vec<Value> = opts.agents.all.iter().map(|a| json!(a.name)).collect();
            if opts.agents.get("general-purpose").is_none() {
                names.push(json!("general-purpose"));
            }
            d.parameters["properties"]["tasks"]["items"]["properties"]["agent"] = json!({
                "type": "string",
                "enum": names,
                "description": "The subagent to give it to, by what it is for; general-purpose when none fits.",
            });
            let list: Vec<String> = opts
                .agents
                .all
                .iter()
                .map(|a| {
                    let about: String = a.description.chars().take(300).collect();
                    format!("- {}: {about}", a.name)
                })
                .collect();
            d.description.push_str(&format!(
                "\nThe subagents of the project and the user, for `agent`:\n{}",
                list.join("\n")
            ));
        }
    }
    if let Some(only) = &opts.only {
        let skill = skill_tool_name(family);
        tools.retain(|t| match projection::id_of(family, opts.layout, &t.name) {
            Some(id) => only.contains(&id),
            None => t.name == skill && only.contains(&ToolId::Skill),
        });
    }
    if let Some(g) = &opts.mcp {
        if g.searched() {
            tools.extend(search_tools(g, by_name));
        } else {
            tools.extend(g.tools().iter().map(declaration_of));
        }
    }
    tools
}

/// The MCP tools the prompt does not declare, which the model may still call by name
/// where its driver can ([`ModelDriver::calls_undeclared`]).
fn callable_of(opts: &AgentOptions) -> Vec<ToolDecl> {
    match &opts.mcp {
        Some(g) if g.searched() => g.tools().iter().map(declaration_of).collect(),
        _ => Vec::new(),
    }
}

/// An MCP tool as the model sees it declared.
fn declaration_of(t: &ognivo_mcp::Tool) -> ToolDecl {
    ToolDecl {
        name: t.name.clone(),
        description: if t.description.is_empty() {
            format!("A tool of the MCP server {}.", t.server)
        } else {
            format!("{} (MCP server {})", t.description, t.server)
        },
        parameters: t.schema.clone(),
    }
}

/// Characters of the catalog in `tool_search`'s description: past them it names the tools
/// without what they do.
const CATALOG_CHARS: usize = 8_000;
/// Characters of a tool's line there.
const CATALOG_LINE: usize = 100;

/// The tools for `tool_search`'s description: each with the first sentence of what it
/// does, or only their names when that takes more than [`CATALOG_CHARS`]. T2, 2026-11-05:
/// with the names alone, gpt-oss chose among 40 tools by name and searched only for the
/// one it chose; it took get_exchange_rate for convert_currency, search_issues for
/// create_issue (tool_correct 8 of 15, declared 12 of 15).
fn catalog(g: &ognivo_mcp::Gateway) -> String {
    let lines: Vec<String> = g
        .tools()
        .iter()
        .map(|t| {
            let first = t
                .description
                .split_inclusive(". ")
                .next()
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
                .trim();
            let mut about: String = first.chars().take(CATALOG_LINE).collect();
            if about.len() < first.len() {
                about.push('…');
            }
            if about.is_empty() {
                format!("- {}", t.name)
            } else {
                format!("- {}: {about}", t.name)
            }
        })
        .collect();
    let text = lines.join("\n");
    if text.len() <= CATALOG_CHARS {
        text
    } else {
        let names: Vec<&str> = g.tools().iter().map(|t| t.name.as_str()).collect();
        names.join(", ")
    }
}

/// `tool_search` and `mcp_call`. `tool_search`'s description has the catalog of the tools,
/// and their names are `mcp_call`'s choices, so the model knows what there is and cannot
/// call what there is not; the prompt holds them whatever a search finds. `by_name`: the
/// driver lets the model call a found tool by its name.
fn search_tools(g: &ognivo_mcp::Gateway, by_name: bool) -> [ToolDecl; 2] {
    let servers: Vec<&str> = g
        .started()
        .iter()
        .filter(|s| s.outcome.as_ref().is_ok_and(|n| *n > 0))
        .map(|s| s.name.as_str())
        .collect();
    let names: Vec<Value> = g.tools().iter().map(|t| json!(t.name)).collect();
    [
        ToolDecl {
            name: TOOL_SEARCH.into(),
            description: format!(
                "Finds tools of the MCP servers {} ({} tools) by what they do and gives their declarations; then call one {}. `query`: a few words of what the tool should do, or `select:` and tool names, separated by commas. The tools:\n{}",
                servers.join(", "),
                g.tools().len(),
                if by_name {
                    "by its name"
                } else {
                    "with mcp_call"
                },
                catalog(g)
            ),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
            }),
        },
        ToolDecl {
            name: MCP_CALL.into(),
            description: "Calls a tool of the MCP servers: `tool` is its name, `arguments` its parameters, as tool_search declares them.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "enum": names},
                    "arguments": {"type": "object"},
                },
                "required": ["tool", "arguments"],
            }),
        },
    ]
}

/// A `tool_search` call's query.
fn search_call(arguments: &str) -> Result<Call, ArgError> {
    let tool = || TOOL_SEARCH.to_owned();
    let v: Value =
        serde_json::from_str(arguments).map_err(|_| ArgError::NotObject { tool: tool() })?;
    let Some(m) = v.as_object() else {
        return Err(ArgError::NotObject { tool: tool() });
    };
    match m.get("query") {
        Some(Value::String(q)) if !q.trim().is_empty() => Ok(Call::ToolSearch {
            query: q.trim().to_owned(),
        }),
        _ => Err(ArgError::Missing {
            tool: tool(),
            arg: "query".into(),
        }),
    }
}

/// An `mcp_call` call: the tool it names, with its arguments. Arguments written as a JSON
/// text are read as the object they hold. Nothing holds them to the tool's schema as a
/// grammar holds a declared tool's, so they are checked here, and a call that does not
/// fit gets the tool's declaration back (T2, 2026-11-05: gpt-oss called create_issue
/// through it with `repository` for `repo`, never having searched).
fn mcp_call(g: &ognivo_mcp::Gateway, arguments: &str, family: Family) -> Result<Call, ArgError> {
    let tool = || MCP_CALL.to_owned();
    let v: Value =
        serde_json::from_str(arguments).map_err(|_| ArgError::NotObject { tool: tool() })?;
    let Some(m) = v.as_object() else {
        return Err(ArgError::NotObject { tool: tool() });
    };
    let Some(name) = m.get("tool").and_then(Value::as_str) else {
        return Err(ArgError::Missing {
            tool: tool(),
            arg: "tool".into(),
        });
    };
    let Some(t) = g.tool(name.trim()) else {
        let names: Vec<&str> = g.tools().iter().map(|t| t.name.as_str()).collect();
        return Err(ArgError::UnknownTool(name.to_owned(), names.join(", ")));
    };
    let arguments = match m.get("arguments") {
        None | Some(Value::Null) => "{}".to_owned(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(o)) => Value::Object(o).to_string(),
            _ => {
                return Err(ArgError::NotObject {
                    tool: t.name.clone(),
                });
            }
        },
        Some(v) => v.to_string(),
    };
    if let Err(why) = fits(&t.schema, &arguments) {
        return Err(ArgError::Invalid {
            tool: t.name.clone(),
            why: format!(
                "{why}. It is declared so:\n{}",
                ognivo_models::declarations(family, &[declaration_of(t)])
            ),
        });
    }
    Ok(Call::Mcp {
        server: t.server.clone(),
        tool: t.tool.clone(),
        arguments,
    })
}

/// Whether `arguments` (a JSON object) have what `schema` requires, and nothing it does not
/// allow when it allows nothing more than its properties.
fn fits(schema: &Value, arguments: &str) -> Result<(), String> {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let Some(args) = args.as_object() else {
        return Err("the arguments are not a JSON object".into());
    };
    for need in schema["required"].as_array().into_iter().flatten() {
        if let Some(k) = need.as_str()
            && !args.contains_key(k)
        {
            return Err(format!("missing required argument `{k}`"));
        }
    }
    if let Some(props) = schema["properties"].as_object()
        && schema.get("additionalProperties") == Some(&Value::Bool(false))
        && let Some(k) = args.keys().find(|k| !props.contains_key(k.as_str()))
    {
        let known: Vec<&str> = props.keys().map(String::as_str).collect();
        return Err(format!(
            "unknown argument `{k}`; the arguments are: {}",
            known.join(", ")
        ));
    }
    Ok(())
}

/// The fixed instructions of every session. Short on purpose: the reference agents that
/// work best with small models keep their prefix small (docs/research/2026-10-29). Nothing
/// of the project is in it, so every session on a server starts with the same tokens and
/// the slot's cache serves them; the project comes in the next message
/// ([`project_context`]).
pub fn system_prompt(layout: Layout) -> String {
    // the tools by the names the model has for them
    let (tools, edit, run) = match layout {
        Layout::Standard => (
            "read and search files, edit them, run shell commands",
            "Read a file before you change it. In edit, copy the old text exactly as the file has it, without the line numbers that read shows.",
            "the shell tool",
        ),
        Layout::Priors => (
            "open and search files, change them with apply_patch, run commands with container.exec",
            "Open a file before you change it. In apply_patch, copy the context and removed lines exactly as the file has them, without the line numbers that open_file shows.",
            "container.exec",
        ),
    };
    format!(
        "You are ognivo, a coding agent. You work in the user's project through tools: {tools}.\n\
         - {edit}\n\
         - Change only what the task needs.\n\
         - After changing code, run the project's build or tests with {run}, if it has them.\n\
         - Answer a question that needs nothing from the project, such as arithmetic or a general concept, directly, without tools.\n\
         - In the final answer, report only what tool results above show. Never write that a command ran or passed unless its result is above; if a command was refused or failed, say so. ognivo adds the verification status to your answer itself.\n\
         - Commands run without a terminal; do not start interactive programs."
    )
}

/// How the message that follows the system prompt begins: where the project is.
const PROJECT_AT: &str = "[ognivo] The project is at ";

/// The project's part of a conversation's start, the message after the system prompt:
/// where the project is, what checks a change in it, what its MCP servers and skills are
/// for, and the text of the user's and the project's instruction files.
pub fn project_context(
    root: &Path,
    instructions: &str,
    mcp: Option<&ognivo_mcp::Gateway>,
    skills: Option<&str>,
) -> String {
    let mut text = format!("{PROJECT_AT}{}.", root.display());
    let checks = verify::project_checks(root);
    if checks.is_empty() {
        // T2, 2026-11-03: told to run "the project's build or tests" in a project of three
        // files, Gemma wrote test programs of its own and asked to compile them
        text.push_str(" It has no build or test files (no Cargo.toml, package.json, pyproject.toml, go.mod, Makefile, tests/ or the like), so no build can check a change: read the changed lines again instead, and do not write programs or files only to test it.");
    } else {
        text.push_str(&format!(
            " Its build and test files: {}.",
            checks.join(", ")
        ));
    }
    if let Some(note) = mcp.and_then(mcp_note) {
        text.push_str("\n\n");
        text.push_str(&note);
    }
    if let Some(note) = skills {
        text.push_str("\n\n");
        text.push_str(note);
    }
    if !instructions.is_empty() {
        text.push_str("\n\n");
        text.push_str(instructions);
    }
    text
}

/// What the session's MCP servers are for. T2, 2026-11-05: with the system prompt's "a
/// question that needs nothing from the project ... directly, without tools", gpt-oss gave
/// a rate from memory beside convert_currency, and searched the empty project for "our
/// internal docs" beside search_docs.
fn mcp_note(g: &ognivo_mcp::Gateway) -> Option<String> {
    let servers: Vec<String> = g
        .started()
        .iter()
        .filter_map(|s| match &s.outcome {
            Ok(n) if *n > 0 => Some(format!(
                "{} ({n} tool{})",
                s.name,
                if *n == 1 { "" } else { "s" }
            )),
            _ => None,
        })
        .collect();
    if servers.is_empty() {
        return None;
    }
    let found = if g.searched() {
        ", found with tool_search,"
    } else {
        ""
    };
    let mut note = format!(
        "MCP servers give tools that reach beyond the project: {}. A question about what they cover (the user's services, live data, documents that are not among the project's files) needs them: call the tool{found} rather than answer from memory or search the project for it.",
        servers.join(", ")
    );
    // what each server says about using it, as Claude Code gives it
    for s in g.started() {
        if let Some(text) = &s.instructions {
            note.push_str(&format!(
                "\n\nThe MCP server {} says how to use it:\n{text}",
                s.name
            ));
        }
    }
    Some(note)
}

impl Agent {
    pub fn new(
        driver: Box<dyn ModelDriver>,
        family: Family,
        approver: Box<dyn Approver>,
        opts: AgentOptions,
    ) -> Self {
        let cwd = opts.root.clone();
        let mut instructions = Instructions::new(opts.root.clone(), opts.user_instructions.clone());
        let prompt_instructions = instructions.start();
        let toolset = match (opts.toolset, &opts.subagents) {
            (Toolset::Delegating, None) => Toolset::All,
            (t, _) => t,
        };
        Self {
            messages: vec![
                ChatMessage::System(system_prompt(opts.layout)),
                ChatMessage::User(project_context(
                    &opts.root,
                    &prompt_instructions.text,
                    opts.mcp.as_deref(),
                    opts.skills.note(skill_tool_name(family)).as_deref(),
                )),
            ],
            tools: tools_of(family, &opts, toolset, driver.calls_undeclared()),
            callable: callable_of(&opts),
            toolset,
            driver,
            family,
            approver,
            cwd,
            seen: HashSet::new(),
            clock: None,
            instructions,
            prompt_instructions,
            skills: opts.skills.clone(),
            loaded_skills: HashMap::new(),
            hook_permission: None,
            session_start: Some("startup"),
            session_end: None,
            subagent: None,
            journal: Journal::new(opts.workspace.unreviewed_changes),
            stopped: None,
            reverted: Vec::new(),
            ledger: Ledger::default(),
            repeats: Repeats::default(),
            stuck: None,
            running: None,
            context: 0,
            counted: 0,
            requests: Vec::new(),
            turn_start: 0,
            turn_reads: HashSet::new(),
            news: true,
            idle_compactions: 0,
            session: None,
            in_turn: false,
            stats: Stats::default(),
            opts,
        }
    }

    /// The messages that start the conversation: the system prompt, and the project's
    /// context after it (a conversation of an older ognivo has the project in its system
    /// prompt and starts with that alone).
    fn head(&self) -> usize {
        match self.messages.get(1) {
            Some(ChatMessage::User(t)) if t.starts_with(PROJECT_AT) => 2,
            _ => 1,
        }
    }

    /// Keeps this new agent's session in `session` from now on.
    pub fn keep_in(&mut self, mut session: Session) {
        session.start_conversation(&self.messages, self.head(), &self.notes(), "start");
        self.session = Some(session);
        self.save();
    }

    /// Picks up a stored session where it ended: its last conversation as the model saw
    /// it, its edit journal and its counters. A turn that ognivo stopped in is closed the
    /// way a stopped turn is; changes found in the project now count as unreviewed.
    pub fn resume(
        driver: Box<dyn ModelDriver>,
        family: Family,
        approver: Box<dyn Approver>,
        opts: AgentOptions,
        mut session: Session,
        resumed: Resumed,
    ) -> Self {
        let mut agent = Self::new(driver, family, approver, opts);
        agent.session_start = Some("resume");
        let Resumed {
            messages,
            notes,
            seen,
            looked,
            journal,
            stats,
        } = resumed;
        agent.journal = Journal::restore(journal, &agent.opts.root);
        if agent.opts.workspace.unreviewed_changes {
            agent.journal.inherit();
        }
        agent.stats = stats;
        if messages.is_empty() {
            session.start_conversation(&agent.messages, agent.head(), &agent.notes(), "start");
        } else {
            agent.messages = messages;
            agent.prompt_instructions = Load {
                text: String::new(),
                files: notes.instructions,
                skipped: notes.skipped,
            };
            agent.instructions.mark_looked(looked);
            agent.seen = seen.into_iter().collect();
            agent.stopped = notes.stopped;
            agent.reverted = notes.reverted;
            agent.requests = notes.requests;
        }
        agent.session = Some(session);
        if notes.in_turn {
            agent.in_turn = true;
            agent.abandon_turn(
                "ognivo stopped before the last turn ended; the session goes on now.",
            );
        } else {
            agent.save();
        }
        agent
    }

    /// The session's id in the store, if it is kept there.
    pub fn session_id(&self) -> Option<ognivo_store::SessionId> {
        self.session.as_ref().map(Session::id)
    }

    /// Why the store stopped saving the session, once: the session goes on in memory.
    pub fn store_failure(&mut self) -> Option<String> {
        self.session.as_mut().and_then(Session::failure)
    }

    /// Stops keeping the session: one without a turn is removed from the store. Returns
    /// the id of a session that stays there.
    pub fn close(&mut self) -> Option<ognivo_store::SessionId> {
        self.save();
        let s = self.session.take()?;
        let kept = (s.turns() > 0).then(|| s.id());
        s.close();
        kept
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The instruction files of the system prompt, and those it could not take.
    pub fn instructions(&self) -> &Load {
        &self.prompt_instructions
    }

    /// The user's and the project's skills and commands.
    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    /// What the current or last turn's tool calls show, answer or not.
    pub fn verification(&self) -> Verification {
        self.ledger.status(&self.opts.root)
    }

    /// Tokens of the conversation the server held after the last step: its prompt and
    /// what it generated.
    pub fn context_tokens(&self) -> u64 {
        self.context
    }

    /// Whether the project has changes nobody has reviewed yet (§8).
    pub fn unreviewed_changes(&self) -> bool {
        !self.journal.is_clean()
    }

    fn workspace(&self) -> WorkspaceState {
        if self.journal.is_clean() {
            WorkspaceState::CLEAN
        } else {
            WorkspaceState::DIRTY
        }
    }

    /// The files the session changed and nobody reviewed yet, as they are now (§8).
    pub fn changes(&mut self) -> Vec<FileChange> {
        let changes = self.journal.changes();
        self.save();
        changes
    }

    /// Commands with side effects since the last accept: listed, never undone.
    pub fn side_effects(&self) -> &[String] {
        self.journal.side_effects()
    }

    /// Whether changes from before the session still wait for an accept.
    pub fn inherited_changes(&self) -> bool {
        self.journal.inherited()
    }

    /// Marks one file's change as reviewed, or everything with `None`; once nothing is
    /// left, builds and tests run without a question again. Returns the files it covered.
    pub fn accept(&mut self, path: Option<&Path>) -> Result<Vec<String>, JournalError> {
        let accepted = self.journal.accept(path);
        self.save();
        accepted
    }

    /// Puts a file back as it was before the session's unreviewed changes (`force`: also
    /// when something else changed it too). The model learns of it with the next message.
    pub fn revert(&mut self, path: &Path, force: bool) -> Result<Reverted, JournalError> {
        let how = self.journal.revert(path, force);
        if how.is_err() {
            // a revert that failed may still have pruned the journal
            self.save();
        }
        let how = how?;
        self.seen.remove(path);
        let name = Target::new(path.to_owned(), &self.opts.root).shown;
        let item = match how {
            Reverted::Restored => name.display().to_string(),
            Reverted::Removed => format!("{} (removed: you had created it)", name.display()),
        };
        if !self.reverted.contains(&item) {
            self.reverted.push(item);
        }
        self.save();
        Ok(how)
    }

    /// Ends a turn that was dropped before its end (the user stopped it, or the server
    /// failed): every call of the last generation gets a result, so the history stays
    /// well-formed, and the model reads `why` with the next user message. What the server
    /// was sent stays as it was, so the next request goes on from its cache.
    pub fn abandon_turn(&mut self, why: &str) {
        if let Some(at) = self
            .messages
            .iter()
            .rposition(|m| matches!(m, ChatMessage::Assistant { .. }))
            && let ChatMessage::Assistant { tool_calls, .. } = &self.messages[at]
        {
            let answered: HashSet<&str> = self.messages[at + 1..]
                .iter()
                .filter_map(|m| match m {
                    ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                    _ => None,
                })
                .collect();
            let missing: Vec<String> = tool_calls
                .iter()
                .filter(|c| !answered.contains(c.id.as_str()))
                .map(|c| c.id.clone())
                .collect();
            for id in missing {
                let content = if self.running.as_deref() == Some(id.as_str()) {
                    "[error] stopped before it finished; it may have done part of its work"
                } else {
                    "[error] not run: the turn was stopped"
                };
                self.messages.push(ChatMessage::Tool {
                    tool_call_id: id,
                    content: content.to_owned(),
                });
            }
        }
        self.stopped = Some(why.to_owned());
        self.running = None;
        self.stuck = None;
        let open = std::mem::take(&mut self.in_turn);
        self.save();
        if open && let Some(s) = self.session.as_mut() {
            s.end_turn(json!({"end": "stopped", "why": why}), None, &self.stats);
        }
    }

    /// Starts a new conversation, with the instruction files read again. What the session
    /// changed stays unreviewed (§8), and the counters go on.
    pub fn clear(&mut self) {
        self.prompt_instructions = self.instructions.start();
        self.stopped = None;
        self.skills.forget();
        self.loaded_skills.clear();
        self.session_end = Some("clear");
        self.session_start = Some("clear");
        self.messages = vec![
            ChatMessage::System(system_prompt(self.opts.layout)),
            ChatMessage::User(project_context(
                &self.opts.root,
                &self.prompt_instructions.text,
                self.opts.mcp.as_deref(),
                self.skills.note(skill_tool_name(self.family)).as_deref(),
            )),
        ];
        self.seen.clear();
        self.context = 0;
        self.counted = 0;
        self.requests.clear();
        let (notes, head) = (self.notes(), self.head());
        if let Some(s) = self.session.as_mut() {
            s.start_conversation(&self.messages, head, &notes, "clear");
        }
        self.save();
    }

    /// Where each turn of the conversation begins, the last turn last: the user's messages,
    /// not ognivo's notes. A turn before `/clear` or a compaction has none.
    pub fn turn_starts(&self) -> Vec<usize> {
        self.messages
            .iter()
            .enumerate()
            .skip(self.head())
            .filter(|(_, m)| matches!(m, ChatMessage::User(t) if !t.starts_with("[ognivo]")))
            .map(|(i, _)| i)
            .collect()
    }

    /// Takes the conversation back to before the user's message at `at`, one of
    /// [`turn_starts`](Self::turn_starts): the turns from it on are gone from it, and the
    /// model reads so with the next message. What they did to files stays, unreviewed. The
    /// server's cache serves the conversation up to there.
    pub fn rewind(&mut self, at: usize) {
        let dropped = self.turn_starts().iter().filter(|&&i| i >= at).count();
        self.messages.truncate(at.max(self.head()));
        self.loaded_skills.clear();
        self.requests
            .truncate(self.requests.len().saturating_sub(dropped));
        let files: Vec<String> = self.changes().into_iter().map(|c| c.shown).collect();
        let mut why = "The user took the conversation back to before this message and wrote it anew: the turns that followed it are gone from the conversation".to_owned();
        if !files.is_empty() {
            why.push_str(&format!(
                ", but what they changed in files stays, unreviewed: {}. Read those files again before you edit them",
                files.join(", ")
            ));
        }
        why.push('.');
        self.stopped = Some(why);
        self.context = 0;
        self.counted = 0;
        self.repeats = Repeats::default();
        let (notes, head) = (self.notes(), self.head());
        if let Some(s) = self.session.as_mut() {
            s.start_conversation(&self.messages, head, &notes, "rewind");
        }
        self.save();
    }

    /// Runs one user turn to its end.
    pub async fn turn(
        &mut self,
        user: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnEnd, DriverError> {
        self.in_turn = true;
        if let Some(s) = self.session.as_mut() {
            s.user(user);
        }
        // the session keeps the user's message and the turn's end on its own
        on(AgentEvent::TurnStarted {
            user: user.to_owned(),
        });
        let end = self.steps(user, on).await;
        self.in_turn = false;
        // a skill's allowed-tools count in the turn that ran it
        self.opts.rules.deactivate();
        let facts = || verify::summary(&self.ledger.status(&self.opts.root));
        on(match &end {
            Ok(TurnEnd::Answer(a)) => AgentEvent::TurnEnded {
                end: "answer".into(),
                why: None,
                answer: Some(a.text.clone()),
                verification: verify::summary(&a.verification),
            },
            Ok(TurnEnd::StepLimit(a)) => AgentEvent::TurnEnded {
                end: "step_limit".into(),
                why: Some(format!(
                    "the step limit ({}) came first",
                    self.opts.max_steps
                )),
                answer: a.as_ref().map(|a| a.text.clone()),
                verification: facts(),
            },
            Ok(TurnEnd::Stuck(why)) => AgentEvent::TurnEnded {
                end: "stuck".into(),
                why: Some(why.clone()),
                answer: None,
                verification: facts(),
            },
            Ok(TurnEnd::TimeLimit) => AgentEvent::TurnEnded {
                end: "time_limit".into(),
                why: None,
                answer: None,
                verification: facts(),
            },
            Ok(TurnEnd::NotRun(why)) => AgentEvent::TurnEnded {
                end: "not_run".into(),
                why: Some(why.clone()),
                answer: None,
                verification: None,
            },
            Err(e) => AgentEvent::TurnEnded {
                end: "error".into(),
                why: Some(e.to_string()),
                answer: None,
                verification: None,
            },
        });
        let (end_data, answer) = match &end {
            Ok(TurnEnd::Answer(a)) => (
                json!({"end": "answer", "verification": a.verification, "unsupported_claim": a.unsupported_claim}),
                Some(a.text.as_str()),
            ),
            Ok(TurnEnd::StepLimit(a)) => (
                json!({"end": "step_limit", "answered": a.is_some()}),
                a.as_ref().map(|a| a.text.as_str()),
            ),
            Ok(TurnEnd::Stuck(why)) => (json!({"end": "stuck", "why": why}), None),
            Ok(TurnEnd::TimeLimit) => (json!({"end": "time_limit"}), None),
            Ok(TurnEnd::NotRun(why)) => (json!({"end": "not_run", "why": why}), None),
            Err(e) => (json!({"end": "error", "why": e.to_string()}), None),
        };
        self.save();
        if let Some(s) = self.session.as_mut() {
            s.end_turn(end_data, answer, &self.stats);
        }
        end
    }

    /// What the agent keeps about the conversation besides its messages.
    fn notes(&self) -> Notes {
        Notes {
            instructions: self.prompt_instructions.files.clone(),
            skipped: self.prompt_instructions.skipped.clone(),
            stopped: self.stopped.clone(),
            reverted: self.reverted.clone(),
            requests: self.requests.clone(),
            in_turn: self.in_turn,
        }
    }

    /// Saves what changed since the last save, if the session is kept in the store.
    fn save(&mut self) {
        let notes = self.notes();
        let Some(s) = self.session.as_mut() else {
            return;
        };
        s.save_messages(&self.messages);
        s.save_journal(&self.journal.unsaved());
        s.save_paths(&self.seen, self.instructions.looked());
        s.save_notes(&notes);
    }

    /// Shows an event and keeps it in the session's log.
    fn emit(&mut self, on: &mut (dyn FnMut(AgentEvent) + Send), e: AgentEvent) {
        if let Some(s) = self.session.as_mut() {
            s.event(&e);
        }
        on(e);
    }

    /// The next generation's request; it counts as a step, and its seed follows from that.
    fn request(&mut self, tool_choice: ToolChoice, messages: Vec<ChatMessage>) -> StepRequest {
        self.stats.steps += 1;
        StepRequest {
            messages,
            tools: self.tools.clone(),
            callable: self.callable.clone(),
            tool_choice,
            max_tokens: self.opts.context.max_tokens,
            seed: self.opts.seed.wrapping_add(self.stats.steps),
        }
    }

    /// Runs one generation to its end, showing its text and reasoning through `on` when
    /// `shown`, and keeps it in the session's log under `purpose`. `None`: the deadline
    /// passed, and dropping the stream cancelled it.
    async fn generate(
        &mut self,
        req: StepRequest,
        deadline: Option<Instant>,
        on: &mut (dyn FnMut(AgentEvent) + Send),
        shown: bool,
        purpose: &str,
    ) -> Result<Option<Finished>, DriverError> {
        let prompt_hash = prompt_hash(&req);
        let started = Instant::now();
        let mut events = self.driver.step(req);
        let mut finished = None;
        loop {
            let next = match deadline {
                Some(d) => match tokio::time::timeout_at(d, events.next()).await {
                    Ok(next) => next,
                    Err(_) => return Ok(None),
                },
                None => events.next().await,
            };
            let Some(ev) = next else { break };
            let ev = match ev? {
                DriverEvent::Reasoning(text) => AgentEvent::Reasoning { text },
                DriverEvent::Text(text) => AgentEvent::Text { text },
                DriverEvent::ToolCallDelta { .. } => continue,
                DriverEvent::Restarted { reason, .. } => {
                    self.stats.restarts += 1;
                    AgentEvent::Restarted { reason }
                }
                DriverEvent::Finished(f) => {
                    finished = Some(*f);
                    continue;
                }
            };
            if shown {
                self.emit(on, ev);
            }
        }
        let f = finished.ok_or_else(|| {
            DriverError::InvalidRequest("the driver ended a step without a result".into())
        })?;
        self.stats.prompt_tokens_processed += u64::from(f.usage.prompt_n.unwrap_or(0));
        self.stats.prompt_tokens_cached += u64::from(f.usage.cache_n.unwrap_or(0));
        self.stats.completion_tokens += u64::from(f.usage.predicted_n.unwrap_or(0));
        if let Some(s) = self.session.as_mut() {
            s.generation(purpose, &f, &prompt_hash, started.elapsed());
        }
        // the session keeps the generation on its own
        let (outcome, rejected): (&str, &[ToolCall]) = match &f.outcome {
            Outcome::Calls(_) => ("calls", &[]),
            Outcome::Answer if f.content.trim().is_empty() => ("empty", &[]),
            Outcome::Answer => ("answer", &[]),
            Outcome::Truncated { partial } => ("truncated", partial),
            Outcome::Repeating { partial, .. } => ("repeating", partial),
            Outcome::Malformed { calls, .. } => ("malformed", calls),
        };
        on(AgentEvent::Generation {
            purpose: purpose.to_owned(),
            outcome: outcome.to_owned(),
            finish: f.finish.as_ref().map(|r| match r {
                FinishReason::Stop => "stop".to_owned(),
                FinishReason::ToolCalls => "tool_calls".to_owned(),
                FinishReason::Length => "length".to_owned(),
                FinishReason::Other(other) => other.clone(),
            }),
            seed: f.seed,
            prompt_tokens: f.usage.prompt_n.unwrap_or(0) + f.usage.cache_n.unwrap_or(0),
            cached_tokens: f.usage.cache_n.unwrap_or(0),
            completion_tokens: f.usage.predicted_n.unwrap_or(0),
            prompt_ms: f.usage.prompt_ms,
            generation_ms: f.usage.predicted_ms,
            prompt_hash,
            rejected: rejected.to_vec(),
        });
        Ok(Some(f))
    }

    /// The tokens the next prompt will hold, about: what the server held after the last
    /// step and an estimate of the messages since.
    fn prompt_estimate(&self) -> u64 {
        let counted = self.counted.min(self.messages.len());
        let held = if self.context > 0 {
            self.context
        } else {
            compact::estimate(&self.messages[..counted])
        };
        held + compact::estimate(&self.messages[counted..])
    }

    /// Compacts the history when the next prompt would pass the limit (§5.6). `Some` ends
    /// the turn: the time limit ran out while the model wrote its summary.
    async fn compact_if_needed(
        &mut self,
        on: &mut (dyn FnMut(AgentEvent) + Send),
        deadline: Option<Instant>,
    ) -> Result<Option<TurnEnd>, DriverError> {
        let limit = compact::limit(&self.opts.context, self.opts.n_ctx);
        let tokens = self.prompt_estimate();
        if tokens <= limit {
            return Ok(None);
        }
        let from = compact::kept_from(&self.messages, self.turn_start, limit / 4);
        // dropping too little would not make room
        if compact::estimate(&self.messages[1..from]) < tokens / 4 {
            return Ok(None);
        }
        // M1 live run (gpt-oss, 15.9K): seven compactions, each followed by reading the same
        // files again, and not one edit
        let idle = if self.news {
            0
        } else {
            self.idle_compactions + 1
        };
        if idle >= compact::MAX_IDLE {
            let planned = compact::limit(&self.opts.context, None);
            let mut why = format!(
                "the work does not fit the context: twice in a row the history had to be compacted with nothing new read and nothing changed since the compaction before; the limit is about {} tokens",
                about_tokens(limit)
            );
            if limit < planned {
                why.push_str(&format!(
                    " where the profile plans for {}, so give the server more context (llama-server -c) or narrow the task",
                    about_tokens(planned)
                ));
            } else {
                why.push_str(", so narrow the task");
            }
            return Ok(Some(TurnEnd::Stuck(why)));
        }
        let o = self
            .hook(
                Event::PreCompact,
                "auto",
                None,
                json!({"trigger": "auto", "custom_instructions": null}),
                on,
            )
            .await;
        if let Some(why) = o.blocked {
            self.emit(
                on,
                AgentEvent::Notice {
                    text: format!(
                        "[ognivo] a PreCompact hook kept the history from being compacted: {why}"
                    ),
                },
            );
            return Ok(None);
        }
        self.idle_compactions = idle;
        self.news = false;
        self.emit(on, AgentEvent::Compacting { tokens, limit });
        let mut messages = self.messages.clone();
        messages.push(ChatMessage::User(compact::SUMMARY_REQUEST.to_owned()));
        let req = self.request(ToolChoice::None, messages);
        let summary = match self.generate(req, deadline, on, false, "summary").await? {
            None => return Ok(Some(TurnEnd::TimeLimit)),
            Some(f) => match f.outcome {
                Outcome::Answer if !f.content.trim().is_empty() => Some(f.content),
                _ => None,
            },
        };
        let facts = self.facts();
        let mut kept = self.messages.split_off(from);
        compact::fit(&mut kept, limit / 4);
        let steps = kept
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .count();
        let note = compact::note(&self.requests, summary.as_deref(), &facts, steps > 0);
        let head = self.head();
        let replaced = self.messages.len() - head;
        self.messages.truncate(head);
        let mut note = note;
        let o = self
            .hook(
                Event::SessionStart,
                "compact",
                None,
                json!({"source": "compact"}),
                on,
            )
            .await;
        if let Some(n) = hook_note(Event::SessionStart, &o.context) {
            note.push_str("\n\n");
            note.push_str(&n);
        }
        self.messages.push(ChatMessage::User(note));
        self.messages.extend(kept);
        self.turn_start = head;
        self.context = 0;
        self.counted = 0;
        // its memory of results is gone with them
        self.repeats = Repeats::default();
        self.instructions.forget_near();
        self.loaded_skills.clear();
        self.stats.compactions += 1;
        // a new cache epoch: the session's next conversation
        let notes = self.notes();
        if let Some(s) = self.session.as_mut() {
            s.start_conversation(&self.messages, head, &notes, "compaction");
        }
        self.save();
        self.emit(
            on,
            AgentEvent::Compacted {
                replaced,
                kept: steps,
                summary,
            },
        );
        Ok(None)
    }

    /// Something in the project changed: repeats are no longer repeats, and the turn moves.
    fn changed(&mut self) {
        self.repeats.changed();
        self.news = true;
    }

    /// What ognivo knows for certain, for a compaction's note.
    fn facts(&mut self) -> Vec<String> {
        let mut facts = Vec::new();
        let changes: Vec<String> = self
            .journal
            .changes()
            .iter()
            .map(|c| format!("{} ({})", c.shown, compact::counts(c)))
            .collect();
        if !changes.is_empty() {
            facts.push(format!(
                "Changed in this session and not reviewed by the user yet: {}.",
                changes.join(", ")
            ));
        }
        let commands: Vec<String> = self
            .journal
            .side_effects()
            .iter()
            .map(|c| verify::quoted(c))
            .collect();
        if !commands.is_empty() {
            facts.push(format!(
                "Commands with side effects since the last review: {}.",
                commands.join(", ")
            ));
        }
        let runs: Vec<String> = self
            .ledger
            .runs()
            .map(|r| format!("{} {}", verify::quoted(&r.command), r.outcome()))
            .collect();
        if !runs.is_empty() {
            facts.push(format!(
                "Builds and tests in this turn, in order: {}.",
                runs.join("; ")
            ));
        }
        if let Some(line) = verify::summary(&self.ledger.status(&self.opts.root)) {
            facts.push(format!("{line}."));
        }
        if let Some(c) = &self.clock {
            facts.push(format!(
                "About {} of the turn's time limit are left.",
                about(c.left())
            ));
        }
        facts
    }

    /// The steps of a turn, until it ends.
    async fn steps(
        &mut self,
        user: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnEnd, DriverError> {
        let request = user;
        self.clock = self.opts.time_limit.map(|limit| Clock {
            start: Instant::now(),
            limit,
            told: 0,
        });
        // §9: every turn starts in the project root
        self.cwd = self.opts.root.clone();
        self.ledger = Ledger::default();
        self.repeats = Repeats::default();
        self.turn_reads.clear();
        self.news = true;
        self.idle_compactions = 0;
        // the hooks that come before the model sees the prompt
        let mut hook_notes = Vec::new();
        if let Some(reason) = self.session_end.take() {
            self.hook(
                Event::SessionEnd,
                reason,
                None,
                json!({"reason": reason}),
                on,
            )
            .await;
        }
        if let Some(source) = self.session_start.take() {
            let o = self
                .hook(
                    Event::SessionStart,
                    source,
                    None,
                    json!({"source": source}),
                    on,
                )
                .await;
            hook_notes.extend(hook_note(Event::SessionStart, &o.context));
        }
        let o = self
            .hook(
                Event::UserPromptSubmit,
                "",
                None,
                json!({"prompt": request}),
                on,
            )
            .await;
        if let Some(why) = o.blocked.or(o.stop) {
            return Ok(TurnEnd::NotRun(format!(
                "a UserPromptSubmit hook blocked the prompt: {why}"
            )));
        }
        hook_notes.extend(hook_note(Event::UserPromptSubmit, &o.context));
        // a command or skill the user runs: the model gets the line and what it says
        let command = self
            .skills
            .command(request)
            .map(|(skill, arguments)| (skill.clone(), arguments));
        let mut user = match command {
            Some((skill, arguments)) => match self.render_skill(&skill, &arguments).await {
                Ok(body) => {
                    self.loaded_skills.insert(skill.name.clone(), body.clone());
                    format!(
                        "{request}\n\n[ognivo] The user ran /{}, {} {}, which says:\n\n{}{body}",
                        skill.name,
                        skill.kind(),
                        skill.shown,
                        base_line(&skill)
                    )
                }
                Err(why) => return Ok(TurnEnd::NotRun(format!("/{}: {why}", skill.name))),
            },
            None => request.to_owned(),
        };
        for note in hook_notes {
            user.push_str("\n\n");
            user.push_str(&note);
        }
        if let Some(why) = self.stopped.take() {
            user.push_str(&format!("\n\n[ognivo] {why}"));
        }
        if let Some(limit) = self.opts.time_limit {
            user.push_str(&format!(
                "\n\n[ognivo] Time limit: {}. ognivo stops the turn when it runs out.",
                about(limit)
            ));
        }
        if !self.reverted.is_empty() {
            user.push_str(&format!(
                "\n\n[ognivo] Since your last turn the user reverted your changes to {}: they are as they were before your edits. Read them again before you edit them.",
                self.reverted.join(", ")
            ));
            self.reverted.clear();
        }
        self.messages.push(ChatMessage::User(user));
        self.turn_start = self.messages.len() - 1;
        self.requests.push(request.to_owned());
        self.save();
        let (mut sent_to_verify, mut sent_to_correct) = (false, false);
        let mut unusable = 0;
        let mut stop_continues = 0;
        for step in 0..self.opts.max_steps {
            if let Some(note) = self.clock.as_mut().and_then(Clock::note) {
                self.emit(on, AgentEvent::Notice { text: note.clone() });
                self.append_to_last(&note);
            }
            // T2, 2026-11-03: Gemma had its answer by step 4 of 10 and searched on, sure
            // there must be more, until the limit came with no answer
            if step > 0 && self.opts.max_steps - step == STEPS_LEFT_NOTE {
                let note = format!(
                    "[ognivo] {STEPS_LEFT_NOTE} steps of this turn are left. Finish now: if the results above answer the task, give the final answer; otherwise make the change that is left, then answer and say what is not done."
                );
                self.emit(on, AgentEvent::Notice { text: note.clone() });
                self.append_to_last(&note);
            }
            let deadline = self.clock.map(|c| c.deadline());
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(TurnEnd::TimeLimit);
            }
            if let Some(end) = self.compact_if_needed(on, deadline).await? {
                return Ok(end);
            }
            let req = self.request(ToolChoice::Auto, self.messages.clone());
            let Some(f) = self.generate(req, deadline, on, true, "step").await? else {
                return Ok(TurnEnd::TimeLimit);
            };
            if let Some(prompt) = f.usage.prompt_n {
                self.context = u64::from(prompt)
                    + u64::from(f.usage.cache_n.unwrap_or(0))
                    + u64::from(f.usage.predicted_n.unwrap_or(0));
                self.counted = self.messages.len();
            }
            let reasoning = (!f.reasoning.is_empty()).then_some(f.reasoning);
            match f.outcome {
                Outcome::Answer if f.content.trim().is_empty() => {
                    self.stats.empty_answers += 1;
                    unusable += 1;
                    // "nothing was run" alone read as "your calls did not run": gpt-oss
                    // made a write that had run again, three times (2026-11-02, second run)
                    let note = "[ognivo] Your last output ended with neither a tool call nor an answer: it stopped inside your reasoning, so that output ran nothing. The calls before it ran, and their results are above. Make the call you were planning directly, or give the final answer.".to_owned();
                    self.emit(on, AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
                Outcome::Answer => {
                    let verification = self.ledger.status(&self.opts.root);
                    let unverified = matches!(verification, Verification::Unverified { .. });
                    let claims = match verification {
                        Verification::NoChanges => verify::claims_changes(&f.content),
                        Verification::Unverified { .. } => verify::claims_a_passing_run(&f.content),
                        Verification::Passed { .. } => false,
                    };
                    // The draft is not kept: an assistant turn without calls before more
                    // tool steps would make gpt-oss's template drop all earlier analysis.
                    let back = if unverified
                        && !sent_to_verify
                        && self.can_run_project_code()
                        && !verify::project_checks(&self.opts.root).is_empty()
                    {
                        sent_to_verify = true;
                        Some(format!(
                            "[ognivo] Not finished yet: {}. Run the project's build or tests now with the shell tool, then give the final answer.",
                            verify::summary(&verification).unwrap_or_default()
                        ))
                    } else if claims && !sent_to_correct {
                        sent_to_correct = true;
                        Some(match &verification {
                            Verification::NoChanges => "[ognivo] Your answer says you changed something, but no file was changed in this turn. Make the change, or answer without claiming it.".to_owned(),
                            _ => format!(
                                "[ognivo] Your answer says a build or test ran or passed, but the facts are: {}. Rewrite the final answer without that claim, and say what is not verified.",
                                verify::summary(&verification).unwrap_or_default()
                            ),
                        })
                    } else {
                        None
                    };
                    let back = match back {
                        Some(note) => Some(note),
                        None if stop_continues < STOP_CONTINUES => {
                            let held = self.stop_hooks(&f.content, stop_continues > 0, on).await;
                            if held.is_some() {
                                stop_continues += 1;
                            }
                            held
                        }
                        None => None,
                    };
                    if let Some(note) = back {
                        self.stats.answers_sent_back += 1;
                        self.emit(on, AgentEvent::Notice { text: note.clone() });
                        self.messages.push(ChatMessage::User(note));
                        continue;
                    }
                    self.messages.push(ChatMessage::Assistant {
                        content: f.content.clone(),
                        reasoning,
                        tool_calls: Vec::new(),
                        tokens: f.tokens,
                    });
                    return Ok(TurnEnd::Answer(Answer {
                        text: f.content,
                        verification,
                        unsupported_claim: claims,
                    }));
                }
                Outcome::Calls(calls) => {
                    unusable = 0;
                    let calls = calls.calls().to_vec();
                    self.messages.push(ChatMessage::Assistant {
                        content: f.content,
                        reasoning,
                        tool_calls: calls.clone(),
                        tokens: f.tokens,
                    });
                    for (i, call) in calls.iter().enumerate() {
                        let result = if self.stuck.is_some() {
                            "[error] not run: the turn ended".to_owned()
                        } else if i < MAX_CALLS_PER_STEP {
                            self.execute(call, on).await
                        } else {
                            format!("[error] not run: at most {MAX_CALLS_PER_STEP} calls per step")
                        };
                        self.messages.push(ChatMessage::Tool {
                            tool_call_id: call.id.clone(),
                            content: result,
                        });
                        self.save();
                    }
                    if let Some(why) = self.stuck.take() {
                        return Ok(TurnEnd::Stuck(why));
                    }
                }
                Outcome::Truncated { partial } => {
                    self.stats.truncated += 1;
                    unusable += 1;
                    let what = if partial.is_empty() {
                        "before it finished"
                    } else {
                        "in the middle of a tool call"
                    };
                    let note = format!(
                        "[ognivo] Your last output reached the {} token limit {what}, so nothing was run and it was discarded. Think less, then call the tool directly with complete arguments.",
                        self.opts.context.max_tokens
                    );
                    self.emit(on, AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
                Outcome::Malformed { reason, .. } => {
                    self.stats.malformed += 1;
                    unusable += 1;
                    let note = format!(
                        "[ognivo] Your last tool call could not be used ({reason}), so nothing was run. Call the tool again with a JSON object of arguments."
                    );
                    self.emit(on, AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
                Outcome::Repeating {
                    channel, sample, ..
                } => {
                    self.stats.repeating += 1;
                    unusable += 1;
                    let advice = match channel {
                        Channel::Reasoning => {
                            "Stop deliberating: act on what you decided with a tool call, or give the final answer."
                        }
                        Channel::Arguments { .. } => {
                            "Make the call again without repeating that text; write a long file in smaller edits."
                        }
                        Channel::Text => "Continue without repeating that text.",
                    };
                    let note = format!(
                        "[ognivo] Your last output was stopped because {channel} kept repeating {sample:?}; nothing was run and it was discarded. {advice}"
                    );
                    self.emit(on, AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
            }
            self.save();
            if unusable >= MAX_UNUSABLE_IN_A_ROW {
                return Ok(TurnEnd::Stuck(format!(
                    "{unusable} generations in a row ended without a usable tool call"
                )));
            }
        }
        Ok(TurnEnd::StepLimit(self.answer_at_limit(on).await?))
    }

    /// The answer of a turn whose steps ran out: asked for once more, without tools, so the
    /// user, or the main agent of a subagent, gets what the turn found instead of nothing
    /// (T2, 2026-11-05: told that two steps were left, Gemma searched on). `None` when the
    /// time is up or the model still does not answer.
    async fn answer_at_limit(
        &mut self,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<Option<Answer>, DriverError> {
        let deadline = self.clock.map(|c| c.deadline());
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Ok(None);
        }
        let note = format!(
            "[ognivo] This turn has used all its {} steps, so no more tools run. Give the final answer now from the results above: what you found or changed, and what is not done.",
            self.opts.max_steps
        );
        self.emit(on, AgentEvent::Notice { text: note.clone() });
        self.append_to_last(&note);
        let req = self.request(ToolChoice::None, self.messages.clone());
        let Some(f) = self.generate(req, deadline, on, true, "answer").await? else {
            return Ok(None);
        };
        // calls, if any came, are not run, and they stay out of the history
        if !matches!(f.outcome, Outcome::Answer) || f.content.trim().is_empty() {
            return Ok(None);
        }
        let verification = self.ledger.status(&self.opts.root);
        let unsupported_claim = match verification {
            Verification::NoChanges => verify::claims_changes(&f.content),
            Verification::Unverified { .. } => verify::claims_a_passing_run(&f.content),
            Verification::Passed { .. } => false,
        };
        self.messages.push(ChatMessage::Assistant {
            content: f.content.clone(),
            reasoning: (!f.reasoning.is_empty()).then_some(f.reasoning),
            tool_calls: Vec::new(),
            tokens: f.tokens,
        });
        self.save();
        Ok(Some(Answer {
            text: f.content,
            verification,
            unsupported_claim,
        }))
    }

    async fn execute(
        &mut self,
        call: &ToolCall,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        self.stats.calls += 1;
        self.running = Some(call.id.clone());
        // a tool of an MCP server is the server's to check
        let gateway = self.opts.mcp.clone();
        let searched = gateway.as_ref().is_some_and(|g| g.searched());
        let mcp = gateway.as_ref().and_then(|g| g.tool(&call.name));
        let mut parsed = match (mcp, gateway.as_deref()) {
            (Some(t), _) => Ok(Call::Mcp {
                server: t.server.clone(),
                tool: t.tool.clone(),
                arguments: call.arguments.clone(),
            }),
            (None, Some(_)) if searched && call.name == TOOL_SEARCH => search_call(&call.arguments),
            (None, Some(g)) if searched && call.name == MCP_CALL => {
                mcp_call(g, &call.arguments, self.family)
            }
            (None, _) if self.has_skill_tool() && call.name == skill_tool_name(self.family) => {
                skill_call(self.family, &call.arguments)
            }
            (None, _) => projection::parse_for(
                self.family,
                self.opts.layout,
                self.toolset,
                &call.name,
                &call.arguments,
            ),
        };
        // a subagent of a template has only its tools
        if let (Some(only), Ok(c)) = (&self.opts.only, &parsed)
            && !only.contains(&c.id())
        {
            let names: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
            parsed = Err(ArgError::UnknownTool(call.name.clone(), names.join(", ")));
        }
        // the hooks before the call: they may block it, decide its question or change it
        self.hook_permission = None;
        let mut blocked = None;
        if let Ok(c) = &parsed
            && self.opts.hooks.has(Event::PreToolUse)
        {
            match self.pre_tool_use(c, &call.id, on).await {
                Ok((changed, permission)) => {
                    let edits =
                        matches!(changed.id(), ToolId::Edit | ToolId::Write | ToolId::Patch);
                    self.hook_permission = permission.map(|p| (p, edits));
                    parsed = Ok(changed);
                }
                Err(refusal) => blocked = Some(refusal),
            }
        }
        let hooked = parsed.as_ref().ok().cloned().filter(|_| {
            blocked.is_none()
                && (self.opts.hooks.has(Event::PostToolUse)
                    || self.opts.hooks.has(Event::PostToolUseFailure))
        });
        self.emit(
            on,
            AgentEvent::ToolStarted {
                id: call.id.clone(),
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                call: parsed.as_ref().ok().cloned(),
            },
        );
        let key = match &parsed {
            Ok(c) => Fingerprint::of(&self.cwd, c),
            Err(_) => Fingerprint::Unparsed {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        };
        // the file the call reads or changes: the instructions on its path come with it
        let touched = match &parsed {
            Ok(Call::Read { path, .. } | Call::Edit { path, .. } | Call::Write { path, .. }) => {
                Some(files::resolve(&self.cwd, path))
            }
            _ => None,
        };
        let mut near = Load::default();
        let result = match self.repeats.check(&key) {
            Check::Run => {
                let mut result = match (blocked, parsed) {
                    (Some(refusal), _) => refusal,
                    (None, Ok(parsed)) => self.run(&call.name, parsed, on).await,
                    (None, Err(e)) => {
                        self.stats.bad_arguments += 1;
                        format!("[error] {e}")
                    }
                };
                if let Some(c) = &hooked {
                    self.post_tool_use(c, &call.id, &mut result, on).await;
                }
                if let Some(note) = self.repeats.record(key, &result) {
                    self.stats.repeated += 1;
                    result.push_str("\n\n");
                    result.push_str(&note);
                }
                if let Some(path) = touched {
                    near = self.instructions.near(&path);
                    if !near.text.is_empty() {
                        result.push_str("\n\n");
                        result.push_str(&near.text);
                    }
                    if self.has_skill_tool()
                        && let Some(note) = self.skills.near(&path, skill_tool_name(self.family))
                    {
                        result.push_str("\n\n");
                        result.push_str(&note);
                    }
                }
                result
            }
            Check::Skip(text) => {
                self.stats.repeats_skipped += 1;
                text
            }
            Check::Stop(text) => {
                let mut args: String = call.arguments.chars().take(120).collect();
                if args.len() < call.arguments.len() {
                    args.push('…');
                }
                self.stuck = Some(format!(
                    "{} kept giving the same result with nothing new in between: {args}",
                    call.name
                ));
                text
            }
        };
        self.running = None;
        self.emit(
            on,
            AgentEvent::ToolFinished {
                id: call.id.clone(),
                tool: call.name.clone(),
                result: result.clone(),
            },
        );
        if !near.files.is_empty() || !near.skipped.is_empty() {
            self.emit(
                on,
                AgentEvent::Instructions {
                    files: near.files,
                    skipped: near.skipped,
                },
            );
        }
        result
    }

    async fn run(
        &mut self,
        tool: &str,
        call: Call,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        let style = match self.family {
            Family::QwenXml => LineStyle::Tab,
            Family::Harmony | Family::Gemma4 => LineStyle::Bar,
        };
        let root = self.opts.root.clone();
        match call {
            Call::Read {
                path,
                offset,
                limit,
            } => {
                let p = files::resolve(&self.cwd, &path);
                let ruling = self.opts.rules.read(&p, approval::decide_read(&p, &root));
                if let Some(refusal) = self
                    .permit(
                        tool,
                        ruling,
                        &p.display().to_string(),
                        "outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                match files::read(&Target::new(p.clone(), &root), offset, limit, style) {
                    Ok(text) => {
                        if self.turn_reads.insert(p.clone()) {
                            self.news = true;
                        }
                        self.seen.insert(p);
                        text
                    }
                    Err(e) => error_text(&e),
                }
            }
            Call::Grep {
                pattern,
                path,
                glob,
            } => {
                let base = path.map_or_else(|| self.cwd.clone(), |p| files::resolve(&self.cwd, &p));
                let ruling = self
                    .opts
                    .rules
                    .read(&base, approval::decide_read(&base, &root));
                if let Some(refusal) = self
                    .permit(
                        tool,
                        ruling,
                        &base.display().to_string(),
                        "outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                let hidden = |p: &Path| self.opts.rules.hides(p);
                search::grep(&base, &root, &pattern, glob.as_deref(), &hidden)
                    .unwrap_or_else(|e| format!("[error] {e}"))
            }
            Call::Glob { pattern, path } => {
                let base = path.map_or_else(|| self.cwd.clone(), |p| files::resolve(&self.cwd, &p));
                let ruling = self
                    .opts
                    .rules
                    .read(&base, approval::decide_read(&base, &root));
                if let Some(refusal) = self
                    .permit(
                        tool,
                        ruling,
                        &base.display().to_string(),
                        "outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                let hidden = |p: &Path| self.opts.rules.hides(p);
                search::glob(&base, &root, &pattern, &hidden)
                    .unwrap_or_else(|e| format!("[error] {e}"))
            }
            Call::Edit {
                path,
                old,
                new,
                replace_all,
            } => {
                let p = files::resolve(&self.cwd, &path);
                match files::plan_edit(
                    &Target::new(p.clone(), &root),
                    &old,
                    &new,
                    replace_all,
                    style,
                ) {
                    Ok(EditPlan::AlreadyApplied(text)) => {
                        self.stats.edits_already_applied += 1;
                        text
                    }
                    Ok(EditPlan::Change(change)) => {
                        if let Some(refusal) = self.outside_task(&[&p]) {
                            return refusal;
                        }
                        let decision = approval::decide_edit(
                            &[FileEdit {
                                path: &change.path,
                                before: change.before.as_deref(),
                                after: Some(&change.after),
                            }],
                            &root,
                        );
                        let ruling = self.opts.rules.edit(&change.path, decision);
                        if let Some(refusal) = self
                            .permit(
                                tool,
                                ruling,
                                &p.display().to_string(),
                                "a protected path or outside the project",
                                Vec::new(),
                                on,
                            )
                            .await
                        {
                            return refusal;
                        }
                        self.commit(change, p, on)
                    }
                    Err(e) => {
                        if matches!(e, FileError::Edit { .. }) {
                            self.stats.edits_failed += 1;
                        }
                        error_text(&e)
                    }
                }
            }
            Call::Write { path, content } => {
                let p = files::resolve(&self.cwd, &path);
                match files::plan_write(
                    &Target::new(p.clone(), &root),
                    &content,
                    self.seen.contains(&p),
                ) {
                    Ok(change) => {
                        if let Some(refusal) = self.outside_task(&[&p]) {
                            return refusal;
                        }
                        let decision = approval::decide_edit(
                            &[FileEdit {
                                path: &change.path,
                                before: change.before.as_deref(),
                                after: Some(&change.after),
                            }],
                            &root,
                        );
                        let ruling = self.opts.rules.edit(&change.path, decision);
                        if let Some(refusal) = self
                            .permit(
                                tool,
                                ruling,
                                &p.display().to_string(),
                                "a protected path or outside the project",
                                Vec::new(),
                                on,
                            )
                            .await
                        {
                            return refusal;
                        }
                        self.commit(change, p, on)
                    }
                    Err(e) => error_text(&e),
                }
            }
            Call::Delegate { tasks } => self.delegate(tasks, on).await,
            Call::Skill { name, arguments } => {
                self.load_skill(&name, arguments.as_deref().unwrap_or(""))
                    .await
            }
            Call::ToolSearch { query } => match self.opts.mcp.clone() {
                Some(g) => self.search_result(&g, &query),
                None => "[error] this session has no MCP servers".into(),
            },
            Call::Mcp {
                server,
                tool: name,
                arguments,
            } => {
                let Some(gateway) = self.opts.mcp.clone() else {
                    return "[error] this session has no MCP servers".into();
                };
                // the model's name for it, whether it called it by that or through mcp_call
                let Some(tool) = gateway
                    .tools()
                    .iter()
                    .find(|t| t.server == server && t.tool == name)
                    .map(|t| t.name.clone())
                else {
                    return format!("[error] the server {server} has no tool {name}");
                };
                let tool = tool.as_str();
                let ruling = self.opts.rules.mcp(&server, &name);
                // the question is the tool's: allowed for the session, it runs with any
                // arguments
                let mut shown: String = arguments.chars().take(300).collect();
                if shown.len() < arguments.len() {
                    shown.push('…');
                }
                let reason =
                    format!("an MCP tool, which does what its server does; arguments {shown}");
                if let Some(refusal) = self
                    .permit(
                        tool,
                        ruling,
                        &format!("{server} · {name}"),
                        &reason,
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                let timeout = self
                    .clock
                    .as_ref()
                    .map_or(MCP_TIMEOUT, |c| c.left().min(MCP_TIMEOUT));
                match gateway.call(tool, &arguments, timeout).await {
                    Ok(text) => text,
                    Err(why) => format!("[error] {why}"),
                }
            }
            Call::Patch { patch } => {
                let seen = &self.seen;
                let planned =
                    files::plan_patch(&self.cwd, &root, &patch, |p| seen.contains(p), style);
                let changes = match planned {
                    Ok(changes) => changes,
                    Err(e) => {
                        if matches!(e, FileError::PatchHunk { .. }) {
                            self.stats.edits_failed += 1;
                        }
                        return error_text(&e);
                    }
                };
                let paths: Vec<&Path> = changes.iter().map(|c| c.path.as_path()).collect();
                if let Some(refusal) = self.outside_task(&paths) {
                    return refusal;
                }
                let edits: Vec<FileEdit> = changes
                    .iter()
                    .map(|c| FileEdit {
                        path: &c.path,
                        before: c.before.as_deref(),
                        after: Some(&c.after),
                    })
                    .collect();
                let decision = approval::decide_edit(&edits, &root);
                // the strictest ruling of the files the patch changes rules the patch
                let mut ruling = Ruling::Allow;
                for c in &changes {
                    match self.opts.rules.edit(&c.path, decision) {
                        Ruling::Deny(why) => {
                            ruling = Ruling::Deny(why);
                            break;
                        }
                        Ruling::Ask => ruling = Ruling::Ask,
                        Ruling::Allow => {}
                    }
                }
                let action = changes
                    .iter()
                    .map(|c| c.shown.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                if let Some(refusal) = self
                    .permit(
                        tool,
                        ruling,
                        &action,
                        "a protected path or outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                // Codex's summary, as gpt-oss learnt it
                let mut lines = vec!["Success. Updated the following files:".to_owned()];
                for change in changes {
                    let path = change.path.clone();
                    let text = self.commit(change, path, on);
                    if text.starts_with("[error]") {
                        lines.push(text);
                        return lines.join("\n");
                    }
                    lines.push(text);
                }
                self.stats.edits_applied += 1;
                lines.join("\n")
            }
            Call::Shell { command, timeout } => {
                let ctx = ExecContext {
                    root: root.clone(),
                    cwd: self.cwd.clone(),
                };
                let effect = classify_command(&command, &ctx);
                let reason = match effect {
                    Effect::ExecutesProjectCode => {
                        "it runs project code while there are unreviewed changes"
                    }
                    Effect::Network => "it may use the network",
                    _ => "it may change things the edit journal cannot undo",
                };
                let checks = verify::is_build_or_test(&command);
                // a command never runs into the turn's time limit, nor asks when it could not run
                if let Err(e) = self.room() {
                    return e;
                }
                let decision = approval::decide(effect, self.workspace());
                let ruling = self
                    .opts
                    .rules
                    .command(&command, &self.cwd, decision, |part| {
                        classify_command(part, &ctx) == Effect::ReadOnly
                    });
                let unreviewed = if effect == Effect::ExecutesProjectCode && ruling == Ruling::Ask {
                    self.journal.summary()
                } else {
                    Vec::new()
                };
                if let Some(refusal) = self
                    .permit(tool, ruling, &command, reason, unreviewed, on)
                    .await
                {
                    if checks {
                        self.ledger.refused(&command);
                    }
                    return refusal;
                }
                let asked = timeout
                    .unwrap_or(exec::DEFAULT_TIMEOUT)
                    .min(exec::MAX_TIMEOUT);
                // the question may have taken time
                let cut = match self.room() {
                    Ok(room) => room.filter(|r| *r < asked),
                    Err(e) => return e,
                };
                let opts = ShellOptions {
                    cwd: self.cwd.clone(),
                    timeout: cut.unwrap_or(asked),
                    output_budget: exec::DEFAULT_OUTPUT_BUDGET,
                    log_dir: self.opts.data_dir.join("logs"),
                };
                match exec::run(&command, &opts).await {
                    Ok(mut r) => {
                        if let (Some(cut), true) = (cut, r.timed_out) {
                            r.text.push_str(&format!(
                                "\n[ognivo] its timeout was cut to {} s by the time limit of the turn",
                                cut.as_secs()
                            ));
                        }
                        if checks {
                            self.ledger.ran(Run {
                                command: command.clone(),
                                exit_code: r.exit_code,
                                timed_out: r.timed_out,
                            });
                        } else if effect == Effect::SideEffects {
                            self.ledger.side_effect(&command);
                        }
                        if effect == Effect::SideEffects {
                            self.journal.side_effect(&command);
                        }
                        if effect != Effect::ReadOnly {
                            self.changed();
                        }
                        self.cwd = r.cwd;
                        r.text
                    }
                    Err(e) => format!("[error] {e}"),
                }
            }
        }
    }

    /// The result for a change to `paths` when one is outside a subagent's write task.
    fn outside_task(&self, paths: &[&Path]) -> Option<String> {
        let files = self.opts.writable.as_ref()?;
        let outside: Vec<String> = paths
            .iter()
            .filter(|p| !files.iter().any(|f| f == *p))
            .map(|p| {
                p.strip_prefix(&self.opts.root)
                    .unwrap_or(p)
                    .display()
                    .to_string()
            })
            .collect();
        if outside.is_empty() {
            return None;
        }
        let allowed: Vec<String> = files
            .iter()
            .map(|f| {
                f.strip_prefix(&self.opts.root)
                    .unwrap_or(f)
                    .display()
                    .to_string()
            })
            .collect();
        Some(format!(
            "[error] not changed: {} is outside this task, which may change only {}. Say in your report what else needs changing.",
            outside.join(", "),
            allowed.join(", ")
        ))
    }

    /// Time a command may take before the turn's limit (`None`: no limit), or the result
    /// for a command that cannot start any more.
    fn room(&self) -> Result<Option<Duration>, String> {
        match self.clock.map(|c| c.left().saturating_sub(TIME_RESERVE)) {
            Some(r) if r < Duration::from_secs(1) => {
                Err("[error] not run: the time limit of the turn is reached".to_owned())
            }
            room => Ok(room),
        }
    }

    /// Adds a note to the newest message, a tool result or ognivo's own note, so that no
    /// extra message enters the history.
    fn append_to_last(&mut self, note: &str) {
        match self.messages.last_mut() {
            Some(ChatMessage::Tool { content, .. } | ChatMessage::User(content)) => {
                content.push_str("\n\n");
                content.push_str(note);
            }
            _ => self.messages.push(ChatMessage::User(note.to_owned())),
        }
    }

    fn commit(
        &mut self,
        change: files::Change,
        path: PathBuf,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        // what the write replaces, for the journal; without it the write must not happen
        let before = match Journal::before_write(&path) {
            Ok(b) => b,
            Err(e) => return format!("[error] not written: {e}"),
        };
        let changed = AgentEvent::FileChanged {
            path: change.shown.display().to_string(),
            created: change.before.is_none(),
            hunks: change.hunks(),
        };
        let (shown, after) = (change.shown.clone(), change.after.clone().into_bytes());
        match change.commit() {
            Ok(text) => {
                self.stats.edits_applied += u32::from(text.starts_with("[edit:"));
                self.journal.record(path.clone(), shown, before, after);
                // the old content is the one thing a crash must not lose
                self.save();
                self.changed();
                self.ledger.changed(path.clone());
                self.seen.insert(path);
                self.emit(on, changed);
                text
            }
            Err(e) => error_text(&e),
        }
    }

    /// Whether a build or test could run now: silently, or after a question someone can
    /// answer.
    fn can_run_project_code(&self) -> bool {
        approval::decide(Effect::ExecutesProjectCode, self.workspace()) == Decision::Allow
            || self.approver.can_ask()
    }

    /// What `tool_search` gives the model: the tools that fit `query`, declared as its
    /// prompt declares tools, and how to call them; all their names when none fits.
    /// What a hook is told of the session and where it runs.
    fn hook_context(&self) -> hooks::Context {
        hooks::Context {
            session_id: self
                .session_id()
                .map(|id| id.to_string())
                .unwrap_or_default(),
            cwd: self.cwd.clone(),
            root: self.opts.root.clone(),
        }
    }

    /// Runs the hooks of `event` for `target`, shows the user their notices, and gives what
    /// they said.
    async fn hook(
        &mut self,
        event: Event,
        target: &str,
        command: Option<&str>,
        fields: Value,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> hooks::Outcome {
        if !self.opts.hooks.has(event) {
            return hooks::Outcome::default();
        }
        let (hooks, ctx) = (self.opts.hooks.clone(), self.hook_context());
        let fields = match fields {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        let out = hooks.run(event, target, command, fields, &ctx).await;
        for n in &out.notices {
            self.emit(
                on,
                AgentEvent::Notice {
                    text: format!("[hook] {n}"),
                },
            );
        }
        out
    }

    /// The PreToolUse hooks of a call (of each file, for a patch): the call as they leave it
    /// and their decision about its question, or the refusal when one blocks it.
    async fn pre_tool_use(
        &mut self,
        call: &Call,
        id: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<(Call, Option<Permission>), String> {
        let targets = hook_targets(call, &self.cwd);
        let command = match call {
            Call::Shell { command, .. } => Some(command.clone()),
            _ => None,
        };
        let mut permission = None;
        let mut changed = None;
        for (name, input) in &targets {
            let o = self
                .hook(
                    Event::PreToolUse,
                    name,
                    command.as_deref(),
                    json!({"tool_name": name, "tool_input": input, "tool_use_id": id}),
                    on,
                )
                .await;
            if let Some(why) = o.stop {
                self.stuck = Some(format!("a PreToolUse hook stopped the turn: {why}"));
                return Err(format!(
                    "[refused] a PreToolUse hook stopped the turn: {why}"
                ));
            }
            if let Some(why) = o.blocked {
                self.stats.refused += 1;
                return Err(format!("[refused] a PreToolUse hook blocked {name}: {why}"));
            }
            permission = match (permission, o.permission) {
                (Some(Permission::Ask), _) | (_, Some(Permission::Ask)) => Some(Permission::Ask),
                (p, None) => p,
                (_, Some(Permission::Allow)) => Some(Permission::Allow),
            };
            if let (Some(update), 1) = (o.updated_input, targets.len()) {
                let mut merged = input.clone();
                if let (Value::Object(m), Value::Object(u)) = (&mut merged, update) {
                    m.extend(u);
                }
                changed = from_hook_input(call, &merged);
            }
        }
        Ok((changed.unwrap_or_else(|| call.clone()), permission))
    }

    /// The PostToolUse (or, for a failed call, PostToolUseFailure) hooks of a call that ran:
    /// what they tell the model goes after its result.
    async fn post_tool_use(
        &mut self,
        call: &Call,
        id: &str,
        result: &mut String,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) {
        let first = result.lines().next().unwrap_or_default();
        if first.starts_with("[refused]")
            || first.starts_with("[ognivo] Not run")
            || first.starts_with("[error] not run")
        {
            return;
        }
        let event = if first.starts_with("[error]") {
            Event::PostToolUseFailure
        } else {
            Event::PostToolUse
        };
        let command = match call {
            Call::Shell { command, .. } => Some(command.clone()),
            _ => None,
        };
        for (name, input) in hook_targets(call, &self.cwd) {
            let mut fields = json!({"tool_name": name, "tool_input": input, "tool_use_id": id});
            if event == Event::PostToolUse {
                fields["tool_response"] = hook_response(call, &input, result);
            } else {
                fields["error"] = json!(result.clone());
            }
            let o = self
                .hook(event, &name, command.as_deref(), fields, on)
                .await;
            if let Some(note) = hook_note(event, &o.context) {
                result.push_str("\n\n");
                result.push_str(&note);
            }
            if let Some(why) = o.stop {
                self.stuck = Some(format!("a {} hook stopped the turn: {why}", event.name()));
            }
        }
    }

    /// The Stop hooks (SubagentStop for a subagent) of an answer: the note that sends the
    /// model back when one does not let the turn end.
    async fn stop_hooks(
        &mut self,
        answer: &str,
        active: bool,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Option<String> {
        let (event, target) = match &self.subagent {
            Some(kind) => (Event::SubagentStop, kind.clone()),
            None => (Event::Stop, String::new()),
        };
        let mut fields = json!({"stop_hook_active": active, "last_assistant_message": answer});
        if event == Event::SubagentStop {
            fields["agent_type"] = json!(target);
            fields["agent_id"] = json!(self.hook_context().session_id);
            fields["agent_transcript_path"] = json!("");
        }
        let o = self.hook(event, &target, None, fields, on).await;
        let why = match (o.blocked, o.context.is_empty()) {
            (Some(why), _) => why,
            (None, false) => o.context.join("\n\n"),
            (None, true) => return None,
        };
        Some(format!(
            "[ognivo] Not finished yet: a {} hook does not let the turn end: {why}",
            event.name()
        ))
    }

    /// Runs the SessionEnd hooks: the session ends for `reason` (`prompt_input_exit`,
    /// `other`); one cleared before them runs first.
    pub async fn end_session(&mut self, reason: &'static str) {
        let mut quiet = |_: AgentEvent| {};
        if let Some(cleared) = self.session_end.take() {
            self.hook(
                Event::SessionEnd,
                cleared,
                None,
                json!({"reason": cleared}),
                &mut quiet,
            )
            .await;
        }
        self.hook(
            Event::SessionEnd,
            reason,
            None,
            json!({"reason": reason}),
            &mut quiet,
        )
        .await;
    }

    /// Whether the model has the skill tool.
    fn has_skill_tool(&self) -> bool {
        let name = skill_tool_name(self.family);
        self.tools.iter().any(|t| t.name == name)
    }

    /// A skill the model loads: its rendered body, or why it does not get it.
    async fn load_skill(&mut self, name: &str, arguments: &str) -> String {
        let Some(skill) = self.skills.get(name).cloned() else {
            let names: Vec<&str> = self
                .skills
                .for_model()
                .iter()
                .map(|s| s.name.as_str())
                .collect();
            return format!(
                "[error] there is no skill {name}; the skills are: {}",
                names.join(", ")
            );
        };
        if !skill.model_invocable {
            return format!(
                "[error] the skill {name} is the user's to run, as /{name}; do not do what it does another way."
            );
        }
        let body = match self.render_skill(&skill, arguments).await {
            Ok(body) => body,
            Err(why) => return format!("[error] the skill {name} did not load: {why}"),
        };
        if self.loaded_skills.get(name) == Some(&body) {
            return format!(
                "[ognivo] The skill {name} is loaded above already, with this same text: follow it there."
            );
        }
        self.loaded_skills.insert(name.to_owned(), body.clone());
        format!("{}{body}", base_line(&skill))
    }

    /// A skill's body rendered: its placeholders filled and the output of its commands in
    /// their place. Its `allowed-tools` count from now to the end of the turn, for its own
    /// commands too. Nothing asks while a skill renders (as in Claude Code), so a command
    /// that the rules and the policy do not allow without a question stops it, as does one
    /// that fails.
    async fn render_skill(&mut self, skill: &Skill, arguments: &str) -> Result<String, String> {
        self.opts.rules.activate(&skill.name);
        let session_id = self
            .session_id()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let root = self.opts.root.clone();
        let vars = Vars {
            project_dir: &root,
            session_id: &session_id,
        };
        let mut text = String::new();
        for part in skill.render(arguments, &vars)? {
            let (command, block) = match part {
                Part::Text(t) => {
                    text.push_str(&t);
                    continue;
                }
                Part::Run { command, block } => (command, block),
            };
            let pattern = Part::pattern(&command, block);
            let ctx = ExecContext {
                root: root.clone(),
                cwd: root.clone(),
            };
            let effect = classify_command(&command, &ctx);
            let decision = approval::decide(effect, self.workspace());
            let ruling = self.opts.rules.command(&command, &root, decision, |part| {
                classify_command(part, &ctx) == Effect::ReadOnly
            });
            match ruling {
                Ruling::Allow => {}
                Ruling::Deny(rule) => {
                    return Err(format!(
                        "Shell command permission check failed for pattern \"{pattern}\": {rule} denies it"
                    ));
                }
                Ruling::Ask => {
                    return Err(format!(
                        "Shell command permission check failed for pattern \"{pattern}\": it would need a question, and nothing asks while a skill renders; the skill's allowed-tools or a permission rule can allow it"
                    ));
                }
            }
            let room = self.room()?;
            let opts = ShellOptions {
                cwd: root.clone(),
                timeout: room.map_or(SKILL_COMMAND_TIMEOUT, |r| r.min(SKILL_COMMAND_TIMEOUT)),
                output_budget: exec::DEFAULT_OUTPUT_BUDGET,
                log_dir: self.opts.data_dir.join("logs"),
            };
            let r = exec::run(&command, &opts)
                .await
                .map_err(|e| format!("Shell command failed for pattern \"{pattern}\": {e}"))?;
            if verify::is_build_or_test(&command) {
                self.ledger.ran(Run {
                    command: command.clone(),
                    exit_code: r.exit_code,
                    timed_out: r.timed_out,
                });
            } else if effect == Effect::SideEffects {
                self.ledger.side_effect(&command);
            }
            if effect == Effect::SideEffects {
                self.journal.side_effect(&command);
            }
            if effect != Effect::ReadOnly {
                self.changed();
            }
            let output = shell_output(&r.text);
            let ok = match r.exit_code {
                Some(0) => true,
                Some(1) => skills::tolerates_one(&command),
                _ => false,
            };
            if !ok {
                let status = r.text.lines().next().unwrap_or_default();
                let mut shown: String = output.chars().take(500).collect();
                if shown.len() < output.len() {
                    shown.push('…');
                }
                return Err(format!(
                    "Shell command failed for pattern \"{pattern}\": {status}\n{shown}"
                ));
            }
            if block {
                text.push_str(output);
                if !output.ends_with('\n') {
                    text.push('\n');
                }
            } else {
                text.push_str(output.trim_end_matches('\n'));
            }
        }
        Ok(text)
    }

    fn search_result(&self, g: &ognivo_mcp::Gateway, query: &str) -> String {
        let found = g.search(query, SEARCH_LIMIT);
        let mut text = String::new();
        for n in &found.unknown {
            text.push_str(&format!("No tool is named {n}.\n"));
        }
        if found.tools.is_empty() {
            let names: Vec<&str> = g.tools().iter().map(|t| t.name.as_str()).collect();
            return format!(
                "[tool_search: no tool fits \"{query}\"]\n{text}The tools of the MCP servers: {}. Search with other words, or with select: and their names.",
                names.join(", ")
            );
        }
        let n = found.tools.len();
        let decls: Vec<ToolDecl> = found.tools.iter().map(|t| declaration_of(t)).collect();
        let how = if self.driver.calls_undeclared() {
            if n == 1 {
                "Call it by its name, as the other tools."
            } else {
                "Call them by their names, as the other tools."
            }
        } else if n == 1 {
            "Call it with mcp_call: `tool` is its name, `arguments` its parameters."
        } else {
            "Call one with mcp_call: `tool` is its name, `arguments` its parameters."
        };
        format!(
            "[tool_search: {n} tool{} for \"{query}\"]\n{text}{}\n\n{how}",
            if n == 1 { "" } else { "s" },
            ognivo_models::declarations(self.family, &decls)
        )
    }

    /// `None` when the action may go ahead; otherwise the refusal the model reads.
    async fn permit(
        &mut self,
        tool: &str,
        ruling: Ruling,
        action: &str,
        reason: &str,
        unreviewed: Vec<String>,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Option<String> {
        let req = ApprovalRequest {
            tool: tool.to_owned(),
            action: action.to_owned(),
            reason: reason.to_owned(),
            unreviewed,
            edit_with: match self.opts.layout {
                Layout::Standard => "the edit and write tools",
                Layout::Priors => "apply_patch",
            },
            agent: None,
        };
        // a PreToolUse hook's decision: ask over any allow; allow over a question, but not
        // over a rule's refusal nor an edit's question, which always asks (§8)
        let ruling = match (ruling, self.hook_permission) {
            (Ruling::Deny(rule), _) => Ruling::Deny(rule),
            (_, Some((Permission::Ask, _))) => Ruling::Ask,
            (Ruling::Ask, Some((Permission::Allow, false))) => Ruling::Allow,
            (ruling, _) => ruling,
        };
        match ruling {
            Ruling::Allow => return None,
            Ruling::Ask => {}
            Ruling::Deny(rule) => {
                self.stats.refused += 1;
                if let Some(s) = self.session.as_mut() {
                    s.approval(&req, false);
                }
                let text = format!(
                    "{action} is denied by the rule {rule}. Do not retry it; do the task another way, or say in the final answer what the rule kept you from doing."
                );
                self.emit(
                    on,
                    AgentEvent::Refused {
                        tool: tool.to_owned(),
                        reason: text.clone(),
                    },
                );
                return Some(format!("[refused] {text}"));
            }
        }
        // the session keeps the answer on its own
        let asked = |allowed| AgentEvent::Approval {
            tool: req.tool.clone(),
            action: req.action.clone(),
            reason: req.reason.clone(),
            unreviewed: req.unreviewed.clone(),
            agent: req.agent.clone(),
            allowed,
        };
        on(asked(None));
        if self.approver.can_ask() && self.opts.hooks.has(Event::Notification) {
            let (hooks, ctx) = (self.opts.hooks.clone(), self.hook_context());
            let message = format!("ognivo needs your permission to use {tool}");
            tokio::spawn(async move {
                let fields = json!({"message": message, "notification_type": "permission_prompt"});
                hooks
                    .run(
                        Event::Notification,
                        "permission_prompt",
                        None,
                        fields.as_object().cloned().unwrap_or_default(),
                        &ctx,
                    )
                    .await;
            });
        }
        let verdict = self.approver.approve(&req).await;
        if let Some(s) = self.session.as_mut() {
            s.approval(&req, verdict == Verdict::Allow);
        }
        on(asked(Some(verdict == Verdict::Allow)));
        match verdict {
            Verdict::Allow => None,
            Verdict::Refuse(text) => {
                self.stats.refused += 1;
                self.emit(
                    on,
                    AgentEvent::Refused {
                        tool: tool.to_owned(),
                        reason: text.clone(),
                    },
                );
                Some(format!("[refused] {text}"))
            }
        }
    }
}

/// What hooks tell the model, as a note after the text they are about.
fn hook_note(event: Event, context: &[String]) -> Option<String> {
    (!context.is_empty()).then(|| {
        format!(
            "[ognivo] From the {} hooks of the settings:\n{}",
            event.name(),
            context.join("\n\n")
        )
    })
}

/// A call as hooks see it: Claude Code's tool name and input, with absolute paths and
/// without the fields left out. A patch is a Write of each file it adds and an Edit of each
/// it changes or removes.
fn hook_targets(call: &Call, cwd: &Path) -> Vec<(String, Value)> {
    let abs = |p: &str| files::resolve(cwd, p).display().to_string();
    let clean = |mut v: Value| {
        if let Value::Object(m) = &mut v {
            m.retain(|_, x| !x.is_null());
        }
        v
    };
    let one = |name: &str, v: Value| vec![(name.to_owned(), clean(v))];
    match call {
        Call::Shell { command, timeout } => one(
            "Bash",
            json!({"command": command, "timeout": timeout.map(|t| t.as_millis() as u64)}),
        ),
        Call::Read {
            path,
            offset,
            limit,
        } => one(
            "Read",
            json!({"file_path": abs(path), "offset": offset, "limit": limit}),
        ),
        Call::Edit {
            path,
            old,
            new,
            replace_all,
        } => one(
            "Edit",
            json!({"file_path": abs(path), "old_string": old, "new_string": new, "replace_all": replace_all}),
        ),
        Call::Write { path, content } => one(
            "Write",
            json!({"file_path": abs(path), "content": content}),
        ),
        Call::Grep {
            pattern,
            path,
            glob,
        } => one(
            "Grep",
            json!({"pattern": pattern, "path": path.as_deref().map(abs), "glob": glob}),
        ),
        Call::Glob { pattern, path } => one(
            "Glob",
            json!({"pattern": pattern, "path": path.as_deref().map(abs)}),
        ),
        Call::Patch { patch } => {
            let mut targets = Vec::new();
            let mut adding: Option<(String, String)> = None;
            for line in patch.lines() {
                let header = |m: &str| line.trim().strip_prefix(m).map(str::trim);
                if line.starts_with("***") {
                    if let Some((file, content)) = adding.take() {
                        targets.push(("Write".to_owned(), json!({"file_path": file, "content": content})));
                    }
                    if let Some(p) = header("*** Add File:") {
                        adding = Some((abs(p), String::new()));
                    } else if let Some(p) =
                        header("*** Update File:").or_else(|| header("*** Delete File:"))
                    {
                        targets.push(("Edit".to_owned(), json!({"file_path": abs(p)})));
                    }
                } else if let (Some((_, content)), Some(added)) =
                    (adding.as_mut(), line.strip_prefix('+'))
                {
                    content.push_str(added);
                    content.push('\n');
                }
            }
            if let Some((file, content)) = adding {
                targets.push(("Write".to_owned(), json!({"file_path": file, "content": content})));
            }
            targets
        }
        Call::Mcp {
            server,
            tool,
            arguments,
        } => one(
            &format!("mcp__{server}__{tool}"),
            serde_json::from_str(arguments).unwrap_or_else(|_| json!({})),
        ),
        Call::Delegate { tasks } => tasks
            .iter()
            .map(|t| {
                (
                    "Task".to_owned(),
                    json!({"description": t.title, "prompt": t.goal, "subagent_type": "general-purpose"}),
                )
            })
            .collect(),
        Call::Skill { name, arguments } => {
            one("Skill", json!({"skill": name, "args": arguments}))
        }
        Call::ToolSearch { query } => one("ToolSearch", json!({"query": query})),
    }
}

/// A call's result as PostToolUse hooks see it, in Claude Code's shape where it has one.
fn hook_response(call: &Call, input: &Value, result: &str) -> Value {
    match call {
        Call::Shell { .. } => json!({
            "stdout": shell_output(result),
            "stderr": "",
            "interrupted": false,
            "isImage": false,
        }),
        Call::Read { .. } => json!({
            "type": "text",
            "file": {"filePath": input["file_path"], "content": result},
        }),
        Call::Edit { .. } => json!({
            "filePath": input["file_path"],
            "oldString": input["old_string"],
            "newString": input["new_string"],
            "replaceAll": input["replace_all"],
            "userModified": false,
        }),
        Call::Write { .. } | Call::Patch { .. } => json!({
            "filePath": input["file_path"],
            "success": true,
        }),
        Call::Mcp { .. } => json!([{"type": "text", "text": result}]),
        _ => json!({"content": result}),
    }
}

/// The call a PreToolUse hook's `updatedInput` makes of `call`, in Claude Code's shape.
fn from_hook_input(call: &Call, v: &Value) -> Option<Call> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| v.get(k).and_then(Value::as_u64).map(|n| n as usize);
    Some(match call {
        Call::Shell { timeout, .. } => Call::Shell {
            command: s("command")?,
            timeout: v
                .get("timeout")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .or(*timeout),
        },
        Call::Read { .. } => Call::Read {
            path: s("file_path")?,
            offset: n("offset"),
            limit: n("limit"),
        },
        Call::Edit { .. } => Call::Edit {
            path: s("file_path")?,
            old: s("old_string")?,
            new: s("new_string")?,
            replace_all: v
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        Call::Write { .. } => Call::Write {
            path: s("file_path")?,
            content: s("content")?,
        },
        Call::Grep { .. } => Call::Grep {
            pattern: s("pattern")?,
            path: s("path"),
            glob: s("glob"),
        },
        Call::Glob { .. } => Call::Glob {
            pattern: s("pattern")?,
            path: s("path"),
        },
        Call::Mcp { server, tool, .. } => Call::Mcp {
            server: server.clone(),
            tool: tool.clone(),
            arguments: v.to_string(),
        },
        _ => return None,
    })
}

/// Where a skill's files are, before its body, as Claude Code gives it; a command has none
/// of its own.
fn base_line(skill: &Skill) -> String {
    if skill.command {
        String::new()
    } else {
        format!("Base directory for this skill: {}\n\n", skill.dir.display())
    }
}

fn error_text(e: &FileError) -> String {
    match e {
        FileError::Edit {
            nearest: Some(n), ..
        } => format!("[error] {e}\nThe most similar lines in the file are:\n{n}"),
        _ => format!("[error] {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, BoxStream};
    use ognivo_models::{Channel, ChatCompletion, FinishReason, Finished, Guard};
    use std::sync::{Arc, Mutex};

    /// One prepared step: a result, or a generation that never ends.
    #[derive(Debug)]
    enum Step {
        Done(Box<Finished>),
        Hang,
    }

    impl From<ChatCompletion> for Step {
        fn from(c: ChatCompletion) -> Self {
            Step::Done(Box::new(Finished {
                outcome: c.outcome(&Guard::default()),
                content: c.content,
                reasoning: c.reasoning,
                finish: c.finish,
                usage: c.usage,
                seed: 0,
                tokens: None,
            }))
        }
    }

    /// Plays back prepared generations and records what each step was sent.
    #[derive(Debug)]
    struct Script {
        steps: Mutex<Vec<Step>>,
        seen: Arc<Mutex<Vec<StepRequest>>>,
    }

    impl ModelDriver for Script {
        fn step(&self, req: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
            self.seen.lock().unwrap().push(req);
            match self.steps.lock().unwrap().remove(0) {
                Step::Done(f) => stream::iter(vec![Ok(DriverEvent::Finished(f))]).boxed(),
                Step::Hang => stream::pending().boxed(),
            }
        }
    }

    fn calls(list: &[(&str, serde_json::Value)], finish: FinishReason) -> ChatCompletion {
        ChatCompletion {
            tool_calls: list
                .iter()
                .enumerate()
                .map(|(i, (name, args))| ToolCall {
                    id: format!("c{i}"),
                    name: (*name).into(),
                    arguments: args.to_string(),
                })
                .collect(),
            finish: Some(finish),
            ..ChatCompletion::default()
        }
    }

    fn answer(text: &str) -> ChatCompletion {
        ChatCompletion {
            content: text.into(),
            finish: Some(FinishReason::Stop),
            ..ChatCompletion::default()
        }
    }

    struct Run {
        agent: Agent,
        seen: Arc<Mutex<Vec<StepRequest>>>,
        root: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn agent(steps: Vec<ChatCompletion>, workspace: WorkspaceState) -> Run {
        agent_with(steps, workspace, Box::new(NoUser))
    }

    fn agent_with(
        steps: Vec<ChatCompletion>,
        workspace: WorkspaceState,
        approver: Box<dyn Approver>,
    ) -> Run {
        let steps = steps.into_iter().map(Step::from).collect();
        script(steps, workspace, approver, None)
    }

    fn script(
        steps: Vec<Step>,
        workspace: WorkspaceState,
        approver: Box<dyn Approver>,
        time_limit: Option<Duration>,
    ) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "fn a() -> u32 {\n    1\n}\n").unwrap();
        let (driver, seen) = played(steps);
        Run {
            agent: Agent::new(
                Box::new(driver),
                Family::Harmony,
                approver,
                options(&root, workspace, time_limit),
            ),
            seen,
            root,
            _dir: dir,
        }
    }

    fn played(steps: Vec<Step>) -> (Script, Arc<Mutex<Vec<StepRequest>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let driver = Script {
            steps: Mutex::new(steps),
            seen: seen.clone(),
        };
        (driver, seen)
    }

    fn options(
        root: &Path,
        workspace: WorkspaceState,
        time_limit: Option<Duration>,
    ) -> AgentOptions {
        AgentOptions {
            root: root.to_owned(),
            data_dir: root.parent().unwrap().join("data"),
            max_steps: 10,
            context: ContextBudget {
                working_max: 98_304,
                compact_at: 0.70,
                max_tokens: 16384,
            },
            n_ctx: None,
            seed: 1,
            workspace,
            time_limit,
            user_instructions: UserInstructions::default(),
            rules: Rules::default(),
            layout: Layout::Standard,
            toolset: Toolset::All,
            writable: None,
            subagents: None,
            mcp: None,
            skills: Skills::default(),
            hooks: Arc::default(),
            agents: Templates::default(),
            only: None,
        }
    }

    #[tokio::test]
    async fn a_patch_changes_its_files_through_the_journal_with_one_approval() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n fn a() -> u32 {\n-    1\n+    2\n }\n*** Add File: notes.txt\n+done\n*** End Patch";
        let steps = vec![
            calls(
                &[("repo_browser.apply_patch", json!({ "patch": patch }))],
                FinishReason::ToolCalls,
            ),
            answer("a() returns 2 now."),
        ];
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "fn a() -> u32 {\n    1\n}\n").unwrap();
        let (driver, seen) = played(steps.into_iter().map(Step::from).collect());
        let mut opts = options(&root, WorkspaceState::CLEAN, None);
        opts.layout = Layout::Priors;
        let mut r = Run {
            agent: Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts),
            seen,
            root,
            _dir: dir,
        };
        let (end, _) = turn(&mut r).await;
        assert!(matches!(end, TurnEnd::Answer(_)), "{end:?}");
        assert_eq!(
            std::fs::read_to_string(r.root.join("lib.rs")).unwrap(),
            "fn a() -> u32 {\n    2\n}\n"
        );
        assert_eq!(
            std::fs::read_to_string(r.root.join("notes.txt")).unwrap(),
            "done\n"
        );
        // Codex's summary, as gpt-oss learnt it
        let result = r
            .agent
            .messages()
            .iter()
            .find_map(|m| match m {
                ChatMessage::Tool { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            result,
            "Success. Updated the following files:\nM lib.rs\nA notes.txt"
        );
        // the journal holds both, so both can be reverted
        let changed: Vec<String> = r.agent.changes().into_iter().map(|c| c.shown).collect();
        assert_eq!(changed, ["lib.rs", "notes.txt"]);
        assert_eq!(r.agent.stats.edits_applied, 1);
        // the model got the tools of its training, and the prompt names them
        let req = &r.seen.lock().unwrap()[0];
        assert!(
            req.tools
                .iter()
                .any(|t| t.name == "repo_browser.apply_patch")
        );
        let ChatMessage::System(prompt) = &req.messages[0] else {
            panic!()
        };
        assert!(prompt.contains("In apply_patch, copy the context and removed lines"));
    }

    async fn turn(r: &mut Run) -> (TurnEnd, Vec<AgentEvent>) {
        let mut events = Vec::new();
        let end = r
            .agent
            .turn("fix it", &mut |e| events.push(e))
            .await
            .unwrap();
        (end, events)
    }

    fn answered(end: TurnEnd) -> Answer {
        match end {
            TurnEnd::Answer(a) => a,
            other => panic!("{other:?}"),
        }
    }

    /// ognivo's notes to the model, the project's context aside.
    fn notes(a: &Agent) -> Vec<String> {
        a.messages()
            .iter()
            .filter_map(|m| match m {
                ChatMessage::User(t) if t.starts_with("[ognivo]") && !t.starts_with(PROJECT_AT) => {
                    Some(t.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn tool_results(a: &Agent) -> Vec<String> {
        a.messages()
            .iter()
            .filter_map(|m| match m {
                ChatMessage::Tool { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn a_turn_taken_back_leaves_the_conversation_and_the_next_says_so() {
        let mut r = agent(
            vec![
                answer("one"),
                calls(
                    &[(
                        "edit",
                        serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"}),
                    )],
                    FinishReason::ToolCalls,
                ),
                answer("two"),
                answer("two, again"),
            ],
            WorkspaceState::CLEAN,
        );
        let mut on = |_: AgentEvent| {};
        r.agent.turn("first", &mut on).await.unwrap();
        r.agent.turn("second", &mut on).await.unwrap();
        let starts = r.agent.turn_starts();
        assert_eq!(starts.len(), 2);
        assert!(matches!(&r.agent.messages()[starts[1]], ChatMessage::User(t) if t == "second"));
        r.agent.rewind(starts[1]);
        assert_eq!(r.agent.messages().len(), starts[1]);
        assert_eq!(r.agent.turn_starts(), &starts[..1]);
        r.agent.turn("second, better", &mut on).await.unwrap();
        let seen = r.seen.lock().unwrap();
        let sent = &seen.last().unwrap().messages;
        // the history goes on from the first turn's answer, and the change stays, said
        assert!(
            matches!(&sent[starts[1] - 1], ChatMessage::Assistant { content, .. } if content == "one")
        );
        let ChatMessage::User(t) = &sent[starts[1]] else {
            panic!("{sent:?}")
        };
        assert!(
            t.starts_with("second, better\n\n[ognivo] The user took the conversation back to before this message and wrote it anew") && t.contains("unreviewed: lib.rs"),
            "{t}"
        );
        assert!(
            !sent
                .iter()
                .any(|m| matches!(m, ChatMessage::User(t) if t == "second"))
        );
        // the file keeps what the turn taken back did
        assert_eq!(
            std::fs::read_to_string(r.root.join("lib.rs")).unwrap(),
            "fn a() -> u32 {\n    2\n}\n"
        );
    }

    #[tokio::test]
    async fn read_edit_answer() {
        let mut r = agent(
            vec![
                calls(
                    &[("read", serde_json::json!({"path": "lib.rs"}))],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[(
                        "edit",
                        serde_json::json!({"path": "lib.rs", "old_string": "1│    1", "new_string": "    2"}),
                    )],
                    FinishReason::ToolCalls,
                ),
                answer("done"),
            ],
            WorkspaceState::CLEAN,
        );
        let (end, events) = turn(&mut r).await;
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "done"),
            "{end:?}"
        );
        // the turn starts, its first generation ends with its usage, then the call runs
        let first: Vec<&str> = events
            .iter()
            .take(3)
            .map(|e| match e {
                AgentEvent::TurnStarted { .. } => "turn",
                AgentEvent::Generation { .. } => "generation",
                AgentEvent::ToolStarted { .. } => "call",
                _ => "other",
            })
            .collect();
        assert_eq!(first, ["turn", "generation", "call"]);
        assert!(
            matches!(events.last(), Some(AgentEvent::TurnEnded { end, .. }) if end == "answer")
        );
        // the UI gets the parsed call and, for a change, the diff
        assert!(matches!(
            &events[2],
            AgentEvent::ToolStarted { call: Some(Call::Read { path, .. }), .. } if path == "lib.rs"
        ));
        let changed: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::FileChanged { .. }))
            .collect();
        assert_eq!(
            changed,
            [&AgentEvent::FileChanged {
                path: "lib.rs".into(),
                created: false,
                hunks: vec![Hunk {
                    old_start: 2,
                    new_start: 2,
                    removed: vec!["    1".into()],
                    added: vec!["    2".into()],
                }],
            }]
        );
        assert_eq!(
            std::fs::read_to_string(r.root.join("lib.rs")).unwrap(),
            "fn a() -> u32 {\n    2\n}\n"
        );
        let results = tool_results(&r.agent);
        assert!(results[0].starts_with("[read: "), "{}", results[0]);
        assert!(
            results[1].starts_with("[edit: applied line numbers stripped]"),
            "{}",
            results[1]
        );
        assert_eq!((r.agent.stats.edits_applied, r.agent.stats.steps), (1, 3));
        // each step sends the whole history and the gpt-oss tool set; seeds advance
        let seen = r.seen.lock().unwrap();
        assert_eq!(seen[2].messages.len(), 7); // system, project, user, (assistant, tool) x 2
        assert_eq!(seen[0].tools.len(), 6);
        assert_eq!((seen[0].seed, seen[2].seed), (2, 4));
    }

    #[tokio::test]
    async fn a_cut_call_runs_nothing() {
        let mut r = agent(
            vec![
                calls(
                    &[(
                        "write",
                        serde_json::json!({"path": "new.rs", "content": "x"}),
                    )],
                    FinishReason::Length,
                ),
                answer("gave up"),
            ],
            WorkspaceState::CLEAN,
        );
        let (end, events) = turn(&mut r).await;
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "gave up"),
            "{end:?}"
        );
        assert!(!r.root.join("new.rs").exists());
        assert_eq!(r.agent.stats.truncated, 1);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Notice { text } if text.contains("token limit")))
        );
        // the cut generation is not in the history; a user note is
        let roles: Vec<&str> = r
            .agent
            .messages()
            .iter()
            .map(|m| match m {
                ChatMessage::System(_) => "system",
                ChatMessage::User(_) => "user",
                ChatMessage::Assistant { .. } => "assistant",
                ChatMessage::Tool { .. } => "tool",
            })
            .collect();
        assert_eq!(roles, ["system", "user", "user", "user", "assistant"]);
    }

    #[tokio::test]
    async fn headless_refuses_what_would_ask() {
        let mut r = agent(
            vec![
                calls(
                    &[("read", serde_json::json!({"path": "/etc/hostname"}))],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[("shell", serde_json::json!({"command": "cargo test"}))],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[("shell", serde_json::json!({"command": "rm -rf build"}))],
                    FinishReason::ToolCalls,
                ),
                answer("could not"),
            ],
            WorkspaceState::DIRTY,
        );
        let (_, _) = turn(&mut r).await;
        let results = tool_results(&r.agent);
        assert!(
            results.iter().all(|t| t.starts_with("[refused] ")),
            "{results:?}"
        );
        assert!(results[1].contains("unreviewed changes"), "{}", results[1]);
        assert_eq!(r.agent.stats.refused, 3);
    }

    #[tokio::test]
    async fn bad_arguments_and_repeated_malformed_output() {
        let mut r = agent(
            vec![
                calls(
                    &[(
                        "edit",
                        serde_json::json!({"path": "lib.rs", "oldString": "1"}),
                    )],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[("read", serde_json::json!({"path": "lib.rs"}))],
                    FinishReason::Stop,
                ),
                calls(
                    &[("read", serde_json::json!({"path": "lib.rs"}))],
                    FinishReason::Stop,
                ),
                calls(
                    &[("read", serde_json::json!({"path": "lib.rs"}))],
                    FinishReason::Stop,
                ),
            ],
            WorkspaceState::CLEAN,
        );
        let (end, _) = turn(&mut r).await;
        assert!(matches!(end, TurnEnd::Stuck(_)), "{end:?}");
        assert!(tool_results(&r.agent)[0].contains("unknown argument `oldString`"));
        assert_eq!(
            (r.agent.stats.bad_arguments, r.agent.stats.malformed),
            (1, 3)
        );
    }

    /// The M1 live run: the test run is refused, the answer still says it passed.
    #[tokio::test]
    async fn a_claimed_check_that_never_ran_is_sent_back() {
        let edit =
            serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"});
        let mut r = agent(
            vec![
                calls(&[("edit", edit)], FinishReason::ToolCalls),
                calls(
                    &[("shell", serde_json::json!({"command": "cargo test -p x"}))],
                    FinishReason::ToolCalls,
                ),
                answer("I have run the test with cargo test -p x.\ntest tests::a ... ok"),
                answer(
                    "I changed lib.rs; the test run was refused, so the change is not verified.",
                ),
            ],
            WorkspaceState::CLEAN,
        );
        let a = answered(turn(&mut r).await.0);
        assert!(a.text.starts_with("I changed lib.rs"), "{}", a.text);
        assert!(!a.unsupported_claim);
        assert_eq!(
            a.verification,
            Verification::Unverified {
                changed: vec!["lib.rs".into()],
                side_effects: vec![],
                last_run: None,
                refused: vec!["cargo test -p x".into()],
            }
        );
        let n = notes(&r.agent);
        assert_eq!(n.len(), 1);
        assert!(
            n[0].contains("Rewrite the final answer without that claim")
                && n[0].contains("refused: `cargo test -p x`"),
            "{}",
            n[0]
        );
        // the discarded draft is not in the history
        assert!(!r.agent.messages().iter().any(
            |m| matches!(m, ChatMessage::Assistant { content, .. } if content.contains("... ok"))
        ));
        assert_eq!(r.agent.stats.answers_sent_back, 1);
    }

    /// When a build can run, an answer right after a change is sent back to run one.
    #[tokio::test]
    async fn an_unverified_answer_is_sent_back_to_build_when_it_can() {
        let edit =
            serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"});
        let mut r = agent_with(
            vec![
                calls(&[("edit", edit)], FinishReason::ToolCalls),
                answer("done"),
                calls(
                    &[(
                        "shell",
                        serde_json::json!({"command": "cargo build --help"}),
                    )],
                    FinishReason::ToolCalls,
                ),
                answer("done; cargo build --help exited 0"),
            ],
            WorkspaceState::CLEAN,
            Box::new(AllowAll),
        );
        std::fs::write(r.root.join("Cargo.toml"), "[package]\n").unwrap();
        let a = answered(turn(&mut r).await.0);
        assert_eq!(
            a.verification,
            Verification::Passed {
                command: "cargo build --help".into()
            }
        );
        assert!(
            notes(&r.agent)[0]
                .starts_with("[ognivo] Not finished yet: NOT verified: lib.rs changed")
        );
        assert_eq!(r.agent.stats.answers_sent_back, 1);
    }

    #[test]
    fn an_mcp_call_must_fit_its_tools_schema() {
        let schema = json!({
            "type": "object",
            "properties": {"repo": {"type": "string"}, "title": {"type": "string"}},
            "required": ["repo", "title"],
        });
        assert_eq!(fits(&schema, r#"{"repo": "a/b", "title": "x"}"#), Ok(()));
        assert_eq!(
            fits(&schema, r#"{"repository": "a/b", "title": "x"}"#),
            Err("missing required argument `repo`".into())
        );
        // more than its properties only where it says so
        assert_eq!(
            fits(&schema, r#"{"repo": "a", "title": "x", "z": 1}"#),
            Ok(())
        );
        let closed = json!({"properties": {"q": {}}, "additionalProperties": false});
        assert_eq!(
            fits(&closed, r#"{"q": 1, "z": 1}"#),
            Err("unknown argument `z`; the arguments are: q".into())
        );
        assert!(fits(&schema, "[1]").is_err());
    }

    #[tokio::test]
    async fn a_project_with_nothing_to_build_says_so_and_its_answer_is_not_sent_back() {
        let edit =
            serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"});
        let mut r = agent_with(
            vec![
                calls(&[("edit", edit)], FinishReason::ToolCalls),
                answer("Changed a to return 2; there is no build to run."),
            ],
            WorkspaceState::CLEAN,
            Box::new(AllowAll),
        );
        let ChatMessage::User(project) = &r.agent.messages()[1] else {
            panic!("{:?}", r.agent.messages()[1]);
        };
        assert!(
            project.contains("It has no build or test files (no Cargo.toml"),
            "{project}"
        );
        let a = answered(turn(&mut r).await.0);
        assert!(
            matches!(a.verification, Verification::Unverified { .. }),
            "{:?}",
            a.verification
        );
        assert_eq!(r.agent.stats.answers_sent_back, 0);
        // a new conversation looks again
        std::fs::write(r.root.join("Cargo.toml"), "[package]\n").unwrap();
        r.agent.clear();
        let ChatMessage::User(project) = &r.agent.messages()[1] else {
            panic!("{:?}", r.agent.messages()[1]);
        };
        assert!(
            project.contains(" Its build and test files: Cargo.toml."),
            "{project}"
        );
    }

    #[tokio::test]
    async fn two_steps_before_the_limit_the_model_is_told_to_finish_and_at_it_asked_to_answer() {
        // different commands: the same one again is a repeat of its own
        let ls = |dir: &str| {
            calls(
                &[(
                    "shell",
                    serde_json::json!({ "command": format!("ls {dir}") }),
                )],
                FinishReason::ToolCalls,
            )
        };
        let mut r = agent_with(
            vec![
                ls("."),
                ls("/"),
                ls("/tmp"),
                ls(".."),
                answer("Listed four directories; nothing else was done."),
            ],
            WorkspaceState::CLEAN,
            Box::new(AllowAll),
        );
        r.agent.opts.max_steps = 4;
        let (end, events) = turn(&mut r).await;
        // the limit came: asked once more without tools, the model answered
        let TurnEnd::StepLimit(Some(a)) = &end else {
            panic!("{end:?}");
        };
        assert_eq!(a.text, "Listed four directories; nothing else was done.");
        let asked = r.seen.lock().unwrap();
        assert_eq!(asked.len(), 5);
        assert_eq!(asked[4].tool_choice, ToolChoice::None);
        drop(asked);
        let results = tool_results(&r.agent);
        let told: Vec<bool> = results
            .iter()
            .map(|t| t.ends_with("[ognivo] 2 steps of this turn are left. Finish now: if the results above answer the task, give the final answer; otherwise make the change that is left, then answer and say what is not done."))
            .collect();
        assert_eq!(told, [false, true, false, false]);
        assert!(
            results[3].ends_with("[ognivo] This turn has used all its 4 steps, so no more tools run. Give the final answer now from the results above: what you found or changed, and what is not done."),
            "{}",
            results[3]
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Notice { text } if text.contains("steps of this turn are left")))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn calls_made_when_the_steps_ran_out_do_not_run() {
        let ls = |dir: &str| {
            calls(
                &[(
                    "shell",
                    serde_json::json!({ "command": format!("ls {dir}") }),
                )],
                FinishReason::ToolCalls,
            )
        };
        let mut r = agent_with(
            vec![ls("."), ls("/")],
            WorkspaceState::CLEAN,
            Box::new(AllowAll),
        );
        r.agent.opts.max_steps = 1;
        let (end, _) = turn(&mut r).await;
        assert!(matches!(end, TurnEnd::StepLimit(None)), "{end:?}");
        // one ls ran; the second call is not in the history, so nothing waits for its result
        assert_eq!(tool_results(&r.agent).len(), 1);
        assert!(matches!(
            r.agent.messages().last(),
            Some(ChatMessage::Tool { .. })
        ));
    }

    #[tokio::test]
    async fn claimed_changes_without_any_are_sent_back() {
        let mut r = agent(
            vec![
                answer("I added the missing test."),
                answer("No change was needed: the test exists."),
            ],
            WorkspaceState::CLEAN,
        );
        let a = answered(turn(&mut r).await.0);
        assert_eq!(
            (a.verification, a.unsupported_claim),
            (Verification::NoChanges, false)
        );
        assert!(notes(&r.agent)[0].contains("no file was changed"));
    }

    #[tokio::test]
    async fn a_repeating_generation_runs_nothing() {
        let looped = Finished {
            outcome: Outcome::Repeating {
                partial: vec![ToolCall {
                    id: "w".into(),
                    name: "write".into(),
                    arguments: "{\"path\":\"new.rs\",\"content\":\"abab".into(),
                }],
                channel: Channel::Arguments {
                    index: 0,
                    name: Some("write".into()),
                },
                sample: "ab".into(),
            },
            content: String::new(),
            reasoning: String::new(),
            finish: None,
            usage: Default::default(),
            seed: 0,
            tokens: None,
        };
        let steps = vec![Step::Done(Box::new(looped)), answer("stopped").into()];
        let mut r = script(steps, WorkspaceState::CLEAN, Box::new(NoUser), None);
        let (end, _) = turn(&mut r).await;
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "stopped"),
            "{end:?}"
        );
        assert!(!r.root.join("new.rs").exists());
        assert_eq!(r.agent.stats.repeating, 1);
        let n = notes(&r.agent);
        assert!(
            n[0].contains("because the arguments of write kept repeating \"ab\""),
            "{n:?}"
        );
    }

    #[tokio::test]
    async fn the_time_limit_ends_a_turn_mid_generation() {
        let limit = Duration::from_secs(1);
        let mut r = script(
            vec![Step::Hang],
            WorkspaceState::CLEAN,
            Box::new(NoUser),
            Some(limit),
        );
        let t = std::time::Instant::now();
        let (end, _) = turn(&mut r).await;
        assert_eq!(end, TurnEnd::TimeLimit);
        assert!(t.elapsed() < limit * 3, "{:?}", t.elapsed());
        let ChatMessage::User(task) = &r.agent.messages()[2] else {
            panic!()
        };
        assert!(
            task.ends_with("\n\n[ognivo] Time limit: 1 s. ognivo stops the turn when it runs out."),
            "{task}"
        );
    }

    #[tokio::test]
    async fn no_command_starts_without_room_before_the_limit() {
        let steps = vec![
            calls(
                &[("shell", serde_json::json!({"command": "echo hi"}))],
                FinishReason::ToolCalls,
            )
            .into(),
            answer("done").into(),
        ];
        // less time than the reserve a command must leave
        let limit = TIME_RESERVE - Duration::from_secs(1);
        let mut r = script(steps, WorkspaceState::CLEAN, Box::new(NoUser), Some(limit));
        let (end, _) = turn(&mut r).await;
        assert!(matches!(end, TurnEnd::Answer(_)), "{end:?}");
        assert_eq!(
            tool_results(&r.agent),
            ["[error] not run: the time limit of the turn is reached"]
        );
    }

    #[test]
    fn the_clock_tells_half_and_eighty_percent_once() {
        let now = Instant::now();
        let limit = Duration::from_secs(100);
        let mut c = Clock {
            start: now - Duration::from_secs(10),
            limit,
            told: 0,
        };
        assert_eq!(c.note(), None);
        c.start = now - Duration::from_secs(55);
        assert!(
            c.note()
                .unwrap()
                .starts_with("[ognivo] Half of the time limit is used, about 4")
        );
        assert_eq!(c.note(), None);
        c.start = now - Duration::from_secs(85);
        assert!(
            c.note()
                .unwrap()
                .starts_with("[ognivo] 80% of the time limit is used, about 1")
        );
        assert_eq!(c.note(), None);
        assert_eq!(
            (
                about(Duration::from_secs(150)),
                about(Duration::from_secs(119))
            ),
            ("2 min".into(), "119 s".into())
        );
    }

    #[tokio::test]
    async fn an_empty_answer_is_sent_back() {
        // M1 stamp A', gpt-oss: finish_reason stop inside the reasoning, no text at all
        let mut stopped = answer("");
        stopped.reasoning = "the edit should be {\"path\": ...".into();
        let steps = vec![stopped.into(), answer("  \n").into(), answer("done").into()];
        let mut r = script(steps, WorkspaceState::CLEAN, Box::new(NoUser), None);
        let (end, _) = turn(&mut r).await;
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "done"),
            "{end:?}"
        );
        assert_eq!(r.agent.stats.empty_answers, 2);
        let n = notes(&r.agent);
        assert_eq!(n.len(), 2);
        assert!(n[0].contains("neither a tool call nor an answer"), "{n:?}");
        // the empty generations are not in the history
        let assistant = r
            .agent
            .messages()
            .iter()
            .filter(|m| matches!(m, ChatMessage::Assistant { .. }))
            .count();
        assert_eq!(assistant, 1);
    }

    #[tokio::test]
    async fn a_reasoning_loop_is_told_to_act() {
        let looped = Finished {
            outcome: Outcome::Repeating {
                partial: Vec::new(),
                channel: Channel::Reasoning,
                sample: "Wait, I'll try to use `diff |= x`".into(),
            },
            content: String::new(),
            reasoning: String::new(),
            finish: None,
            usage: Default::default(),
            seed: 0,
            tokens: None,
        };
        let steps = vec![Step::Done(Box::new(looped)), answer("ok").into()];
        let mut r = script(steps, WorkspaceState::CLEAN, Box::new(NoUser), None);
        turn(&mut r).await;
        let n = notes(&r.agent);
        assert!(
            n[0].contains("because the reasoning kept repeating")
                && n[0].ends_with("Stop deliberating: act on what you decided with a tool call, or give the final answer."),
            "{n:?}"
        );
    }

    #[tokio::test]
    async fn a_failing_edit_again_is_marked_then_skipped_then_ends_the_turn() {
        // M1 stamp A', Gemma: one wrong old_string again, with another new text each time
        let edit = |new: &str| {
            calls(
                &[(
                    "edit",
                    serde_json::json!({"path": "lib.rs", "old_string": "    7", "new_string": new}),
                )],
                FinishReason::ToolCalls,
            )
        };
        let steps = vec![
            edit("    2"),
            edit("    3"),
            edit("    4"),
            edit("    5"),
            answer("never"),
        ];
        let mut r = agent(steps, WorkspaceState::CLEAN);
        let (end, _) = turn(&mut r).await;
        assert!(
            matches!(&end, TurnEnd::Stuck(why) if why.starts_with("edit kept giving the same result")),
            "{end:?}"
        );
        let results = tool_results(&r.agent);
        assert_eq!(results.len(), 4);
        assert!(
            results[0].starts_with("[error] lib.rs: old_string not found")
                && !results[0].contains("[ognivo]"),
            "{}",
            results[0]
        );
        assert!(
            results[1].ends_with("an edit with this old_string gave this same result before, and nothing has changed since."),
            "{}",
            results[1]
        );
        assert!(
            results[2].starts_with("[ognivo] Not run: an edit with this old_string"),
            "{}",
            results[2]
        );
        assert!(
            results[3].starts_with("[ognivo] Not run: this call kept giving"),
            "{}",
            results[3]
        );
        let s = &r.agent.stats;
        assert_eq!(
            (s.edits_failed, s.repeated, s.repeats_skipped, s.steps),
            (2, 1, 1, 4)
        );
    }

    #[tokio::test]
    async fn only_the_same_result_with_nothing_changed_is_a_repeat() {
        let grep = || {
            calls(
                &[("grep", serde_json::json!({"pattern": "fn a"}))],
                FinishReason::ToolCalls,
            )
        };
        let steps = vec![
            grep(),
            grep(),
            calls(
                &[(
                    "edit",
                    serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"}),
                )],
                FinishReason::ToolCalls,
            ),
            grep(),
            answer("done"),
        ];
        let mut r = agent(steps, WorkspaceState::CLEAN);
        let (end, _) = turn(&mut r).await;
        assert!(matches!(&end, TurnEnd::Answer(_)), "{end:?}");
        let results = tool_results(&r.agent);
        assert!(
            results[1].ends_with("nothing has changed since."),
            "{}",
            results[1]
        );
        assert!(
            results[2].starts_with("[edit: applied exact]"),
            "{}",
            results[2]
        );
        // the same result as before the edit, which could have changed it
        assert_eq!(results[3], results[0]);
        assert_eq!(r.agent.stats.repeated, 1);
    }

    #[tokio::test]
    async fn a_turn_dropped_in_a_command_leaves_a_well_formed_history() {
        let steps = vec![calls(
            &[
                ("shell", serde_json::json!({"command": "sleep 5"})),
                ("read", serde_json::json!({"path": "lib.rs"})),
            ],
            FinishReason::ToolCalls,
        )];
        let mut r = agent_with(steps, WorkspaceState::CLEAN, Box::new(AllowAll));
        let mut on = |_: AgentEvent| {};
        let turn = r.agent.turn("fix it", &mut on);
        assert!(
            tokio::time::timeout(Duration::from_millis(500), turn)
                .await
                .is_err()
        );
        r.agent.abandon_turn("The user stopped the last turn.");
        assert_eq!(
            tool_results(&r.agent),
            [
                "[error] stopped before it finished; it may have done part of its work",
                "[error] not run: the turn was stopped"
            ]
        );
    }

    #[tokio::test]
    async fn a_turn_dropped_in_a_generation_leaves_a_note_for_the_next_message() {
        let mut r = script(
            vec![Step::Hang, Step::from(answer("ok"))],
            WorkspaceState::CLEAN,
            Box::new(NoUser),
            None,
        );
        let mut on = |_: AgentEvent| {};
        let turn = r.agent.turn("fix it", &mut on);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), turn)
                .await
                .is_err()
        );
        r.agent.abandon_turn("The user stopped the last turn.");
        // the server has the message as it was sent
        assert_eq!(
            r.agent.messages().last(),
            Some(&ChatMessage::User("fix it".into()))
        );
        r.agent.turn("go on", &mut on).await.unwrap();
        let seen = r.seen.lock().unwrap();
        assert_eq!(
            seen[1].messages[2..],
            [
                ChatMessage::User("fix it".into()),
                ChatMessage::User("go on\n\n[ognivo] The user stopped the last turn.".into())
            ]
        );
    }

    #[tokio::test]
    async fn clear_starts_over_and_keeps_the_changes_unreviewed() {
        let mut done = answer("done");
        done.usage.prompt_n = Some(10);
        done.usage.cache_n = Some(90);
        done.usage.predicted_n = Some(20);
        let steps = vec![
            calls(
                &[(
                    "edit",
                    serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"}),
                )],
                FinishReason::ToolCalls,
            ),
            done,
        ];
        let mut r = agent(steps, WorkspaceState::CLEAN);
        assert!(!r.agent.unreviewed_changes());
        turn(&mut r).await;
        assert_eq!(r.agent.context_tokens(), 120);
        assert!(r.agent.unreviewed_changes());
        r.agent.clear();
        // the system prompt and the project
        assert_eq!(r.agent.messages().len(), 2);
        assert_eq!(r.agent.context_tokens(), 0);
        assert!(r.agent.unreviewed_changes());
    }

    #[tokio::test]
    async fn a_command_that_may_change_things_is_never_a_repeat() {
        let shell = |command: &str| {
            calls(
                &[("shell", serde_json::json!({"command": command}))],
                FinishReason::ToolCalls,
            )
        };
        let grep = || {
            calls(
                &[("grep", serde_json::json!({"pattern": "fn a"}))],
                FinishReason::ToolCalls,
            )
        };
        let steps = vec![
            grep(),
            shell("touch x"),
            grep(),
            shell("touch x"),
            shell("touch x"),
            shell("touch x"),
            answer("done"),
            answer("done"),
        ];
        let mut r = agent_with(steps, WorkspaceState::CLEAN, Box::new(AllowAll));
        let (end, _) = turn(&mut r).await;
        assert!(matches!(&end, TurnEnd::Answer(_)), "{end:?}");
        let results = tool_results(&r.agent);
        assert_eq!(results.len(), 6);
        assert!(
            results.iter().all(|t| !t.contains("[ognivo]")),
            "{results:?}"
        );
    }

    /// Refuses every question and keeps it.
    #[derive(Debug, Default)]
    struct Asked(Arc<Mutex<Vec<ApprovalRequest>>>);

    impl Approver for Asked {
        fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
            self.0.lock().unwrap().push(req.clone());
            Box::pin(async { Verdict::Refuse("not now".into()) })
        }

        fn can_ask(&self) -> bool {
            true
        }
    }

    fn edit_lib() -> ChatCompletion {
        calls(
            &[(
                "edit",
                serde_json::json!({"path": "lib.rs", "old_string": "    1", "new_string": "    2"}),
            )],
            FinishReason::ToolCalls,
        )
    }

    #[tokio::test]
    async fn a_question_about_project_code_lists_what_is_unreviewed() {
        let steps = vec![
            edit_lib(),
            calls(
                &[("shell", serde_json::json!({"command": "touch made"}))],
                FinishReason::ToolCalls,
            ),
            calls(
                &[("shell", serde_json::json!({"command": "cargo test"}))],
                FinishReason::ToolCalls,
            ),
            answer("done"),
            answer("done"),
        ];
        let asked = Arc::new(Mutex::new(Vec::new()));
        let mut r = agent_with(steps, WorkspaceState::CLEAN, Box::new(Asked(asked.clone())));
        turn(&mut r).await;
        let asked = asked.lock().unwrap();
        // `touch` asks for itself; `cargo test` asks because of what is unreviewed
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[0].unreviewed.is_empty());
        assert_eq!(asked[1].action, "cargo test");
        assert_eq!(asked[1].unreviewed, ["lib.rs"]);
    }

    #[tokio::test]
    async fn a_reverted_change_is_undone_and_the_model_hears_of_it() {
        let mut r = agent(
            vec![edit_lib(), answer("done"), answer("ok")],
            WorkspaceState::CLEAN,
        );
        turn(&mut r).await;
        assert!(r.agent.unreviewed_changes());
        let lib = r.root.join("lib.rs");
        let changes = r.agent.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].shown.as_str(), changes[0].hunks.len()),
            ("lib.rs", 1)
        );
        assert_eq!(r.agent.revert(&lib, false).unwrap(), Reverted::Restored);
        assert_eq!(
            std::fs::read_to_string(&lib).unwrap(),
            "fn a() -> u32 {\n    1\n}\n"
        );
        assert!(!r.agent.unreviewed_changes());
        turn(&mut r).await;
        let seen = r.seen.lock().unwrap();
        let last = seen.last().unwrap().messages.last().unwrap().clone();
        assert_eq!(
            last,
            ChatMessage::User(
                "fix it\n\n[ognivo] Since your last turn the user reverted your changes to lib.rs: they are as they were before your edits. Read them again before you edit them.".into()
            )
        );
        // told once
        drop(seen);
        assert!(r.agent.reverted.is_empty());
    }

    #[tokio::test]
    async fn accepted_changes_leave_nothing_unreviewed() {
        let mut r = agent(vec![edit_lib(), answer("done")], WorkspaceState::DIRTY);
        turn(&mut r).await;
        assert!(r.agent.inherited_changes());
        assert_eq!(r.agent.accept(None).unwrap(), ["lib.rs"]);
        assert!(!r.agent.unreviewed_changes());
        assert!(r.agent.changes().is_empty());
        assert_eq!(
            std::fs::read_to_string(r.root.join("lib.rs")).unwrap(),
            "fn a() -> u32 {\n    2\n}\n"
        );
    }

    #[tokio::test]
    async fn instructions_reach_the_system_prompt_and_a_subdirectory_s_its_first_read() {
        let read = |path: &str| {
            calls(
                &[("read", serde_json::json!({ "path": path }))],
                FinishReason::ToolCalls,
            )
        };
        let mut r = agent(
            vec![read("sub/x.rs"), read("sub/y.rs"), answer("done")],
            WorkspaceState::CLEAN,
        );
        std::fs::write(r.root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        std::fs::create_dir(r.root.join("sub")).unwrap();
        std::fs::write(r.root.join("sub/CLAUDE.md"), "No unsafe here.").unwrap();
        std::fs::write(r.root.join("sub/x.rs"), "x\n").unwrap();
        std::fs::write(r.root.join("sub/y.rs"), "y\n").unwrap();
        // they were not there when the session began; a new conversation reads them again
        r.agent.clear();
        let (end, events) = turn(&mut r).await;
        answered(end);
        let ChatMessage::System(system) = &r.agent.messages()[0] else {
            panic!("{:?}", r.agent.messages()[0]);
        };
        // the files come with the project, after a system prompt that has nothing of it
        assert!(system.starts_with("You are ognivo"), "{system}");
        assert!(!system.contains("<instructions"), "{system}");
        let ChatMessage::User(project) = &r.agent.messages()[1] else {
            panic!("{:?}", r.agent.messages()[1]);
        };
        assert!(
            project.starts_with(PROJECT_AT)
                && project
                    .contains("<instructions file=\"AGENTS.md\">\nUse tabs.\n</instructions>"),
            "{project}"
        );
        assert!(
            r.agent
                .instructions()
                .files
                .iter()
                .any(|f| f.path == "AGENTS.md")
        );
        let results = tool_results(&r.agent);
        assert!(results[0].starts_with("[read: sub/x.rs"), "{}", results[0]);
        assert!(
            results[0].ends_with("\n\n[ognivo] Instructions for the files under sub/ follow; where they disagree with the earlier ones, they win.\n\n<instructions file=\"sub/CLAUDE.md\">\nNo unsafe here.\n</instructions>"),
            "{}",
            results[0]
        );
        assert!(!results[1].contains("instructions"), "{}", results[1]);
        // the user hears of them after the call's result
        let at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::Instructions { .. }))
            .unwrap();
        assert!(matches!(events[at - 1], AgentEvent::ToolFinished { .. }));
        let AgentEvent::Instructions { files, skipped } = &events[at] else {
            unreachable!()
        };
        assert_eq!(files[0].path, "sub/CLAUDE.md");
        assert!(skipped.is_empty());
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::Instructions { .. }))
                .count(),
            1
        );
    }

    fn store_path(root: &Path) -> PathBuf {
        root.parent().unwrap().join("data/ognivo.db")
    }

    /// Keeps the run's session in a store file next to its project.
    fn kept(r: &mut Run) -> ognivo_store::SessionId {
        let store = ognivo_store::Store::open(&store_path(&r.root)).unwrap();
        let new = ognivo_store::NewSession {
            kind: "chat".into(),
            root: r.root.clone(),
            profile: "gpt-oss-20b".into(),
            level: None,
            server: "http://127.0.0.1:1".into(),
            seed: 1,
        };
        let session = Session::create(store, &new).unwrap();
        let id = session.id();
        r.agent.keep_in(session);
        id
    }

    /// The session picked up again from its store, as a later run of ognivo does.
    fn resumed(
        root: &Path,
        id: ognivo_store::SessionId,
        steps: Vec<ChatCompletion>,
    ) -> (Agent, Arc<Mutex<Vec<StepRequest>>>) {
        let store = ognivo_store::Store::open(&store_path(root)).unwrap();
        let (session, left) = Session::open(store, id).unwrap();
        let (driver, seen) = played(steps.into_iter().map(Step::from).collect());
        let agent = Agent::resume(
            Box::new(driver),
            Family::Harmony,
            Box::new(NoUser),
            options(root, WorkspaceState::DIRTY, None),
            session,
            left,
        );
        (agent, seen)
    }

    fn event_kinds(root: &Path, id: ognivo_store::SessionId) -> Vec<String> {
        let store = ognivo_store::Store::open(&store_path(root)).unwrap();
        store
            .events(id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[tokio::test]
    async fn a_session_is_kept_as_it_goes_and_picked_up_where_it_ended() {
        let mut r = agent(vec![edit_lib(), answer("done")], WorkspaceState::CLEAN);
        let id = kept(&mut r);
        answered(turn(&mut r).await.0);
        let Run {
            agent, root, _dir, ..
        } = r;
        let (before, stats) = (agent.messages().to_vec(), agent.stats.clone());
        drop(agent);
        assert_eq!(
            event_kinds(&root, id),
            [
                "conversation",
                "user",
                "generation",
                "tool_started",
                "file_changed",
                "tool_finished",
                "generation",
                "end"
            ]
        );

        let (mut agent, seen) = resumed(&root, id, vec![answer("still done")]);
        assert_eq!(agent.messages(), before.as_slice());
        assert_eq!(agent.stats, stats);
        // the journal came through: the change can still be reverted
        assert_eq!(agent.changes().len(), 1);
        assert_eq!(
            agent.revert(&root.join("lib.rs"), false).unwrap(),
            Reverted::Restored
        );
        assert_eq!(
            std::fs::read_to_string(root.join("lib.rs")).unwrap(),
            "fn a() -> u32 {\n    1\n}\n"
        );
        agent.turn("and now?", &mut |_| {}).await.unwrap();
        let sent = seen.lock().unwrap()[0].messages.clone();
        assert_eq!(&sent[..before.len()], before.as_slice());
        let ChatMessage::User(next) = &sent[before.len()] else {
            panic!("{sent:?}");
        };
        assert!(next.starts_with("and now?"), "{next}");
        assert!(next.contains("reverted your changes to lib.rs"), "{next}");
        // the seed goes on from the steps before
        assert_eq!(seen.lock().unwrap()[0].seed, 1 + stats.steps + 1);
        drop(agent);
        let store = ognivo_store::Store::open(&store_path(&root)).unwrap();
        let row = store.session(id).unwrap().unwrap();
        assert_eq!((row.turns, row.title.as_str()), (2, "fix it"));
        assert_eq!(row.unreviewed_files, 0);
    }

    #[tokio::test]
    async fn a_turn_ognivo_stopped_in_is_closed_when_the_session_goes_on() {
        // the edit is applied, then the generation after it never ends: ognivo dies there
        let mut r = script(
            vec![Step::from(edit_lib()), Step::Hang],
            WorkspaceState::CLEAN,
            Box::new(NoUser),
            None,
        );
        let id = kept(&mut r);
        let mut on = |_: AgentEvent| {};
        let turn = r.agent.turn("fix it", &mut on);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), turn)
                .await
                .is_err()
        );
        let Run {
            agent, root, _dir, ..
        } = r;
        drop(agent);

        let (mut agent, seen) = resumed(&root, id, vec![answer("ok")]);
        assert_eq!(agent.changes().len(), 1);
        let Some(ChatMessage::Tool { content, .. }) = agent.messages().last() else {
            panic!("{:?}", agent.messages());
        };
        assert!(content.starts_with("[edit: applied"), "{content}");
        agent.turn("go on", &mut on).await.unwrap();
        assert_eq!(
            seen.lock().unwrap()[0].messages.last(),
            Some(&ChatMessage::User(
                "go on\n\n[ognivo] ognivo stopped before the last turn ended; the session goes on now."
                    .into()
            ))
        );
        drop(agent);
        let store = ognivo_store::Store::open(&store_path(&root)).unwrap();
        let ends: Vec<String> = store
            .events(id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "end")
            .map(|e| e.data)
            .collect();
        assert!(
            ends.len() == 2
                && ends[0].contains(r#""end":"stopped""#)
                && ends[1].contains(r#""end":"answer""#),
            "{ends:?}"
        );
    }

    #[tokio::test]
    async fn clear_keeps_the_session_and_starts_its_next_conversation() {
        let mut r = agent(vec![answer("one"), answer("two")], WorkspaceState::CLEAN);
        let id = kept(&mut r);
        turn(&mut r).await;
        r.agent.clear();
        r.agent.turn("again", &mut |_| {}).await.unwrap();
        let Run {
            agent, root, _dir, ..
        } = r;
        let now = agent.messages().to_vec();
        drop(agent);
        let (agent, _) = resumed(&root, id, vec![]);
        // the last conversation is the one that goes on
        assert_eq!(agent.messages(), now.as_slice());
        assert_eq!(now.len(), 4);
    }

    /// A turn that reads two files of 20 KB, under the limit, and then a small one, after
    /// which the server says it holds more than the limit.
    fn filling(summary: ChatCompletion) -> Run {
        let read = |path: &str| {
            calls(
                &[("read", serde_json::json!({ "path": path }))],
                FinishReason::ToolCalls,
            )
        };
        let mut full = read("lib.rs");
        full.usage.prompt_n = Some(30_000);
        let mut r = agent(
            vec![
                read("big1.txt"),
                read("big2.txt"),
                full,
                summary,
                answer("done"),
            ],
            WorkspaceState::CLEAN,
        );
        let line = format!("{}\n", "x".repeat(59));
        for name in ["big1.txt", "big2.txt"] {
            std::fs::write(r.root.join(name), line.repeat(340)).unwrap();
        }
        // a 40K slot: compaction at 40K - 16K - 512
        r.agent.opts.n_ctx = Some(40_000);
        r
    }

    #[tokio::test]
    async fn a_full_context_is_compacted_into_a_summary_and_the_last_step() {
        let mut r = filling(answer("Task: fix it.\nNext: change lib.rs."));
        let id = kept(&mut r);
        let (end, events) = turn(&mut r).await;
        answered(end);
        let seen = r.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 5);
        // the summary is asked for over the whole history, with the same tools and no calls
        let ask = &seen[3];
        assert_eq!(ask.tool_choice, ToolChoice::None);
        assert_eq!(ask.tools, seen[2].tools);
        assert_eq!(&ask.messages[..7], &seen[2].messages[..]);
        assert_eq!(ask.messages.len(), 7 + 2 + 1);
        assert_eq!(
            ask.messages.last(),
            Some(&ChatMessage::User(compact::SUMMARY_REQUEST.into()))
        );
        // then: the system message, the project, the note, and the last step as it was
        let next = &seen[4].messages[1..];
        assert_eq!(next.len(), 4);
        assert_eq!(seen[4].messages[..2], seen[2].messages[..2]);
        let ChatMessage::User(note) = &next[1] else {
            panic!("{next:?}");
        };
        assert!(note.contains("\n1. fix it\n"), "{note}");
        assert!(
            note.contains("written before the compaction:\nTask: fix it.\nNext: change lib.rs.\n"),
            "{note}"
        );
        assert!(
            note.ends_with("Go on with the latest request from where the steps below leave off."),
            "{note}"
        );
        assert!(
            matches!(&next[2], ChatMessage::Assistant { tool_calls, .. } if tool_calls[0].arguments.contains("lib.rs"))
        );
        assert!(
            matches!(&next[3], ChatMessage::Tool { content, .. } if content.starts_with("[read: lib.rs"))
        );
        let compacting = events.iter().find_map(|e| match e {
            AgentEvent::Compacting { tokens, limit } => Some((*tokens, *limit)),
            _ => None,
        });
        let (tokens, limit) = compacting.unwrap();
        assert!(tokens > 30_000, "{tokens}");
        assert_eq!(limit, 40_000 - 16_384 - 512);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                replaced: 5,
                kept: 1,
                summary: Some(_)
            }
        )));
        assert_eq!(r.agent.stats.compactions, 1);
        // the session goes on from the compacted conversation
        let Run {
            agent, root, _dir, ..
        } = r;
        let now = agent.messages().to_vec();
        drop(agent);
        let (agent, _) = resumed(&root, id, vec![]);
        assert_eq!(agent.messages(), now.as_slice());
        assert_eq!(agent.requests, ["fix it"]);
    }

    #[tokio::test]
    async fn without_a_summary_the_facts_and_the_last_step_remain() {
        let tries_a_call = calls(
            &[("read", serde_json::json!({"path": "lib.rs"}))],
            FinishReason::ToolCalls,
        );
        let mut r = filling(tries_a_call);
        let (end, events) = turn(&mut r).await;
        answered(end);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Compacted {
                kept: 1,
                summary: None,
                ..
            }
        )));
        let seen = r.seen.lock().unwrap();
        let ChatMessage::User(note) = &seen[4].messages[2] else {
            panic!();
        };
        assert!(
            note.contains("There is no summary: writing it failed."),
            "{note}"
        );
    }

    #[tokio::test]
    async fn compacting_again_with_nothing_new_read_or_changed_ends_the_turn() {
        let read = |path: &str, held: Option<u32>| {
            let mut c = calls(
                &[("read", serde_json::json!({ "path": path }))],
                FinishReason::ToolCalls,
            );
            c.usage.prompt_n = held;
            c
        };
        // the same three files each time, the last read leaving the context full
        let round = || {
            vec![
                read("big1.txt", None),
                read("big2.txt", None),
                read("lib.rs", Some(30_000)),
            ]
        };
        let mut steps = round();
        steps.push(answer("Task: one."));
        steps.extend(round());
        steps.push(answer("Task: two."));
        steps.extend(round());
        let mut r = agent(steps, WorkspaceState::CLEAN);
        let line = format!("{}\n", "x".repeat(59));
        for name in ["big1.txt", "big2.txt"] {
            std::fs::write(r.root.join(name), line.repeat(340)).unwrap();
        }
        r.agent.opts.n_ctx = Some(40_000);
        r.agent.opts.max_steps = 20;
        let (end, events) = turn(&mut r).await;
        let TurnEnd::Stuck(why) = end else {
            panic!("{end:?}");
        };
        assert!(
            why.starts_with("the work does not fit the context: twice in a row the history had to be compacted with nothing new read and nothing changed"),
            "{why}"
        );
        assert!(
            why.ends_with("the limit is about 23.1K tokens where the profile plans for 68.8K, so give the server more context (llama-server -c) or narrow the task"),
            "{why}"
        );
        // the first compaction followed new reads, the second did not, a third would not help
        let compactions = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Compacted { .. }))
            .count();
        assert_eq!(compactions, 2);
        assert_eq!(r.seen.lock().unwrap().len(), 11);
    }

    #[tokio::test]
    async fn rules_refuse_without_a_question_and_allow_what_would_ask() {
        let shell = |command: &str| {
            calls(
                &[("shell", serde_json::json!({ "command": command }))],
                FinishReason::ToolCalls,
            )
        };
        let grep = calls(
            &[("grep", serde_json::json!({"pattern": "fn", "path": "."}))],
            FinishReason::ToolCalls,
        );
        let asked = Asked::default();
        let questions = asked.0.clone();
        let mut r = agent_with(
            vec![
                shell("touch made.txt"),
                shell("rm -f made.txt"),
                grep,
                answer("done"),
                answer("done"),
            ],
            WorkspaceState::CLEAN,
            Box::new(asked),
        );
        std::fs::create_dir(r.root.join(".ognivo")).unwrap();
        std::fs::write(
            r.root.join(".ognivo/config.toml"),
            "[permissions]\nallow = [\"Bash(touch:*)\"]\ndeny = [\"Bash(rm:*)\", \"Read(secret.rs)\"]\n",
        )
        .unwrap();
        std::fs::write(r.root.join("secret.rs"), "fn hidden() {}\n").unwrap();
        let loaded = crate::rules::load(&crate::rules::Places {
            root: r.root.clone(),
            home: None,
            claude_dir: None,
        });
        let mut rules = loaded.rules;
        // the project's allow rule counts once trusted
        rules.trust(loaded.pending.unwrap());
        r.agent.opts.rules = rules;
        let (end, events) = turn(&mut r).await;
        answered(end);
        // neither command asked: one was allowed, the other denied by a rule
        assert!(
            questions.lock().unwrap().is_empty(),
            "{:?}",
            questions.lock().unwrap()
        );
        assert!(r.root.join("made.txt").exists());
        let results = tool_results(&r.agent);
        assert!(results[0].starts_with("[shell: exit 0"), "{}", results[0]);
        assert_eq!(
            results[1],
            "[refused] rm -f made.txt is denied by the rule Bash(rm:*) in .ognivo/config.toml. Do not retry it; do the task another way, or say in the final answer what the rule kept you from doing."
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Refused { .. }))
        );
        // a file a deny rule names is left out of a search, which says so
        assert!(results[2].contains("lib.rs:1:fn a()"), "{}", results[2]);
        assert!(!results[2].contains("hidden"), "{}", results[2]);
        assert!(
            results[2].contains("[1 file left out by the permission rules]"),
            "{}",
            results[2]
        );
        assert_eq!(r.agent.stats.refused, 1);
    }

    /// What Harbor's ATIF models accept (harbor.models.trajectories, `extra: forbid`).
    fn atif_shape(v: &serde_json::Value) -> Result<(), String> {
        let only = |v: &serde_json::Value, keys: &[&str], what: &str| -> Result<(), String> {
            let m = v.as_object().ok_or(format!("{what} is not an object"))?;
            match m.keys().find(|k| !keys.contains(&k.as_str())) {
                Some(k) => Err(format!("{what} has {k}")),
                None => Ok(()),
            }
        };
        only(
            v,
            &[
                "schema_version",
                "session_id",
                "trajectory_id",
                "agent",
                "steps",
                "notes",
                "final_metrics",
                "continued_trajectory_ref",
                "extra",
                "subagent_trajectories",
            ],
            "trajectory",
        )?;
        only(
            &v["agent"],
            &["name", "version", "model_name", "tool_definitions", "extra"],
            "agent",
        )?;
        let steps = v["steps"].as_array().ok_or("no steps")?;
        if steps.is_empty() {
            return Err("no steps".into());
        }
        for (i, s) in steps.iter().enumerate() {
            only(
                s,
                &[
                    "step_id",
                    "timestamp",
                    "source",
                    "model_name",
                    "reasoning_effort",
                    "message",
                    "reasoning_content",
                    "tool_calls",
                    "observation",
                    "metrics",
                    "is_copied_context",
                    "llm_call_count",
                    "extra",
                ],
                "step",
            )?;
            if s["step_id"] != serde_json::json!(i + 1) {
                return Err(format!("step {i} has id {}", s["step_id"]));
            }
            if !s["message"].is_string()
                || !s["timestamp"].as_str().is_some_and(|t| t.ends_with('Z'))
            {
                return Err(format!("step {i}: {s}"));
            }
            match s["source"].as_str() {
                Some("agent") => {}
                Some("system" | "user") => {
                    for f in [
                        "model_name",
                        "reasoning_effort",
                        "reasoning_content",
                        "tool_calls",
                        "metrics",
                    ] {
                        if !s[f].is_null() {
                            return Err(format!("step {i} is not the agent's but has {f}"));
                        }
                    }
                }
                _ => return Err(format!("step {i} has source {}", s["source"])),
            }
            let ids: Vec<&serde_json::Value> = s["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    only(
                        c,
                        &["tool_call_id", "function_name", "arguments", "extra"],
                        "tool call",
                    )
                    .and_then(|()| {
                        if c["arguments"].is_object() {
                            Ok(&c["tool_call_id"])
                        } else {
                            Err("arguments".into())
                        }
                    })
                })
                .collect::<Result<_, _>>()?;
            if !s["observation"].is_null() {
                only(&s["observation"], &["results"], "observation")?;
                for r in s["observation"]["results"].as_array().ok_or("results")? {
                    only(
                        r,
                        &[
                            "source_call_id",
                            "content",
                            "subagent_trajectory_ref",
                            "extra",
                        ],
                        "result",
                    )?;
                    if !r["source_call_id"].is_null() && !ids.contains(&&r["source_call_id"]) {
                        return Err(format!("step {i}: result for an unknown call"));
                    }
                }
            }
            if !s["metrics"].is_null() {
                only(
                    &s["metrics"],
                    &[
                        "prompt_tokens",
                        "completion_tokens",
                        "cached_tokens",
                        "cost_usd",
                        "prompt_token_ids",
                        "completion_token_ids",
                        "logprobs",
                        "extra",
                    ],
                    "metrics",
                )?;
            }
        }
        only(
            &v["final_metrics"],
            &[
                "total_prompt_tokens",
                "total_completion_tokens",
                "total_cached_tokens",
                "total_cost_usd",
                "total_steps",
                "extra",
            ],
            "final metrics",
        )
    }

    #[tokio::test]
    async fn a_session_becomes_an_atif_trajectory() {
        let mut read = calls(
            &[("read", serde_json::json!({"path": "lib.rs"}))],
            FinishReason::ToolCalls,
        );
        read.reasoning = "look first".into();
        read.usage.prompt_n = Some(900);
        read.usage.cache_n = Some(100);
        read.usage.predicted_n = Some(30);
        let mut done = answer("done");
        done.usage.prompt_n = Some(50);
        done.usage.cache_n = Some(1000);
        done.usage.predicted_n = Some(5);
        let mut r = agent(vec![read, edit_lib(), done], WorkspaceState::CLEAN);
        let id = kept(&mut r);
        answered(turn(&mut r).await.0);
        let Run {
            agent, root, _dir, ..
        } = r;
        drop(agent);
        let store = ognivo_store::Store::open(&store_path(&root)).unwrap();
        let t = crate::trace::atif(&store, id).unwrap();
        atif_shape(&t).unwrap();
        assert_eq!(t["schema_version"], "ATIF-v1.7");
        assert_eq!(t["agent"]["name"], "ognivo");
        assert_eq!(t["agent"]["model_name"], "gpt-oss-20b");
        assert!(t["agent"]["tool_definitions"].as_array().unwrap().len() >= 5);
        let mut steps = t["steps"].as_array().unwrap().clone();
        let sources: Vec<&str> = steps
            .iter()
            .map(|s| s["source"].as_str().unwrap())
            .collect();
        assert_eq!(
            sources,
            ["system", "system", "user", "agent", "agent", "agent"]
        );
        // after the system prompt, the project's context, an ognivo note to the model
        let project = steps.remove(1);
        assert!(project["message"].as_str().unwrap().starts_with(PROJECT_AT));
        assert!(
            steps[0]["message"]
                .as_str()
                .unwrap()
                .starts_with("You are ognivo")
        );
        assert_eq!(steps[1]["message"], "fix it");
        // the read: its reasoning, its call and the result, its tokens (cached ones counted)
        let first = &steps[2];
        assert_eq!(first["reasoning_content"], "look first");
        assert_eq!(first["tool_calls"][0]["function_name"], "read");
        assert_eq!(
            first["tool_calls"][0]["arguments"],
            serde_json::json!({"path": "lib.rs"})
        );
        let result = &first["observation"]["results"][0];
        assert_eq!(
            result["source_call_id"],
            first["tool_calls"][0]["tool_call_id"]
        );
        assert!(
            result["content"]
                .as_str()
                .unwrap()
                .starts_with("[read: lib.rs")
        );
        assert_eq!(
            first["metrics"],
            serde_json::json!({"prompt_tokens": 1000, "cached_tokens": 100, "completion_tokens": 30})
        );
        assert_eq!(first["llm_call_count"], 1);
        assert!(
            first["extra"]["prompt_hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(first["extra"]["outcome"], "calls");
        // the edit's change, and the answer
        assert_eq!(
            steps[3]["extra"]["changes"],
            serde_json::json!([{"path": "lib.rs", "created": false, "added": 1, "removed": 1}])
        );
        assert_eq!(steps[4]["message"], "done");
        assert_eq!(t["final_metrics"]["total_prompt_tokens"], 2050);
        assert_eq!(t["final_metrics"]["total_cached_tokens"], 1100);
        assert_eq!(t["final_metrics"]["total_steps"], 6);
        assert_eq!(t["extra"]["ognivo"]["turns"][0]["end"], "answer");
        // the same request asks for the same hash; another one does not
        assert_ne!(
            steps[2]["extra"]["prompt_hash"],
            steps[3]["extra"]["prompt_hash"]
        );
    }

    /// What each subagent was sent, in the order they were made.
    type Seen = Arc<Mutex<Vec<Arc<Mutex<Vec<StepRequest>>>>>>;

    /// Subagents whose drivers play prepared steps, one script per subagent in the order
    /// they are made, and keep what each was sent.
    #[derive(Debug)]
    struct Scripts {
        scripts: Mutex<Vec<Vec<ChatCompletion>>>,
        seen: Seen,
    }

    impl Subagents for Scripts {
        fn driver(&self) -> Box<dyn ModelDriver> {
            let steps = self.scripts.lock().unwrap().remove(0);
            let (driver, seen) = played(steps.into_iter().map(Step::from).collect());
            self.seen.lock().unwrap().push(seen);
            Box::new(driver)
        }

        fn level(&self) -> Option<String> {
            Some("medium".into())
        }
    }

    /// The main agent playing `main` in a project with a.txt and b.txt, its subagents
    /// playing `scripts`; and what each subagent was sent.
    fn delegating(main: Vec<ChatCompletion>, scripts: Vec<Vec<ChatCompletion>>) -> (Run, Seen) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let (driver, seen) = played(main.into_iter().map(Step::from).collect());
        let subagents = Arc::new(Mutex::new(Vec::new()));
        // a tree with unreviewed changes: nobody may run the project's code, so no answer
        // goes back for a build
        let mut opts = options(&root, WorkspaceState::DIRTY, None);
        opts.toolset = Toolset::Delegating;
        opts.subagents = Some(Arc::new(Scripts {
            scripts: Mutex::new(scripts),
            seen: subagents.clone(),
        }));
        let agent = Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts);
        let r = Run {
            agent,
            seen,
            root,
            _dir: dir,
        };
        (r, subagents)
    }

    fn edit(path: &str, old: &str, new: &str) -> ChatCompletion {
        calls(
            &[(
                "edit",
                json!({"path": path, "old_string": old, "new_string": new}),
            )],
            FinishReason::ToolCalls,
        )
    }

    #[tokio::test]
    async fn delegate_runs_tasks_on_subagents_whose_changes_become_the_session_s() {
        let tasks = json!({"tasks": [
            {"title": "Find", "goal": "What does b.txt say?", "mode": "read"},
            {"title": "Change a", "goal": "Make a.txt say two", "mode": "write", "files": ["a.txt"]}
        ]});
        let (mut r, subagents) = delegating(
            vec![
                calls(&[("delegate", tasks)], FinishReason::ToolCalls),
                answer("Done."),
            ],
            vec![
                vec![
                    calls(
                        &[("read", json!({"path": "b.txt"}))],
                        FinishReason::ToolCalls,
                    ),
                    answer("b.txt says b."),
                ],
                vec![
                    // outside its task first
                    edit("b.txt", "b", "c"),
                    edit("a.txt", "one", "two"),
                    answer("Changed it."),
                ],
            ],
        );
        let (end, events) = turn(&mut r).await;
        assert_eq!(answered(end).text, "Done.");
        assert_eq!(
            std::fs::read_to_string(r.root.join("a.txt")).unwrap(),
            "two\n"
        );
        assert_eq!(
            std::fs::read_to_string(r.root.join("b.txt")).unwrap(),
            "b\n"
        );
        // the main agent has delegate; its result is the reports
        let main = r.seen.lock().unwrap();
        assert_eq!(main[0].tools.last().unwrap().name, "delegate");
        let report = tool_results(&r.agent)
            .into_iter()
            .find(|t| t.starts_with("[delegate]"))
            .unwrap();
        assert!(report.starts_with("[delegate] 2 of 2 tasks done:\n\n1. Find (read) answered after 2 steps:\nb.txt says b.\n\n2. Change a (write) answered after 3 steps:\nChanged it.\n[ognivo] NOT verified: a.txt changed"), "{report}");
        assert!(report.ends_with("\nChanged: a.txt (+1 -1)."), "{report}");
        // a reading subagent has no tool that changes files, and both know what they are
        let subs = subagents.lock().unwrap();
        let read = subs[0].lock().unwrap();
        let names: Vec<&str> = read[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read", "shell", "grep", "glob"]);
        let ChatMessage::System(system) = &read[0].messages[0] else {
            panic!()
        };
        assert!(system.contains("You are a subagent"), "{system}");
        let ChatMessage::User(task) = &read[0].messages[2] else {
            panic!()
        };
        assert!(task.starts_with("Task from the main agent: Find\n\nGoal: What does b.txt say?\n\nThis is a read task"), "{task}");
        // the write task's edit outside its files did not happen
        let write = subs[1].lock().unwrap();
        let ChatMessage::Tool { content, .. } = write[1].messages.last().unwrap() else {
            panic!()
        };
        assert!(
            content.starts_with(
                "[error] not changed: b.txt is outside this task, which may change only a.txt."
            ),
            "{content}"
        );
        // what it changed is the session's to review
        let changes = r.agent.changes();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].shown, "a.txt");
        assert_eq!(
            (r.agent.stats.subagents, r.agent.stats.subagent_steps),
            (2, 5)
        );
        let started: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::SubagentStarted { index, write, .. } => Some((*index, write.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(started, [(0, None), (1, Some(vec!["a.txt".to_owned()]))]);
        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::SubagentFinished {
                    index,
                    end,
                    changed,
                    ..
                } => Some((*index, end.as_str(), changed.clone())),
                _ => None,
            })
            .collect();
        assert!(finished.contains(&(0, "answer", vec![])));
        assert!(finished.contains(&(1, "answer", vec!["a.txt".to_owned()])));
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::Subagent { index: 1, event } if matches!(**event, AgentEvent::FileChanged { .. }))));
    }

    #[tokio::test]
    async fn a_subagent_out_of_steps_still_reports_what_it_found() {
        let tasks = json!({"tasks": [
            {"title": "Look", "goal": "What do a.txt and b.txt say?", "mode": "read"}
        ]});
        let read = |path: &str| {
            calls(
                &[("read", json!({ "path": path }))],
                FinishReason::ToolCalls,
            )
        };
        let (mut r, subagents) = delegating(
            vec![
                calls(&[("delegate", tasks)], FinishReason::ToolCalls),
                answer("Done."),
            ],
            vec![vec![
                read("b.txt"),
                read("a.txt"),
                answer("b.txt says b and a.txt says one."),
            ]],
        );
        // the subagent's limit is the session's when that is lower
        r.agent.opts.max_steps = 2;
        let (end, events) = turn(&mut r).await;
        assert_eq!(answered(end).text, "Done.");
        let report = tool_results(&r.agent)
            .into_iter()
            .find(|t| t.starts_with("[delegate]"))
            .unwrap();
        assert!(
            report.contains("1. Look (read) ran out of its 2 steps; asked then, it reported:\nb.txt says b and a.txt says one."),
            "{report}"
        );
        let subs = subagents.lock().unwrap();
        let asked = subs[0].lock().unwrap();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[2].tool_choice, ToolChoice::None);
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::SubagentFinished { index: 0, end, .. } if end == "step_limit")));
    }

    #[tokio::test]
    async fn write_tasks_on_one_file_take_turns() {
        let tasks = json!({"tasks": [
            {"title": "First", "goal": "one to two", "mode": "write", "files": ["a.txt"]},
            {"title": "Second", "goal": "two to three", "mode": "write", "files": ["./a.txt"]}
        ]});
        let (mut r, _) = delegating(
            vec![
                calls(&[("delegate", tasks)], FinishReason::ToolCalls),
                answer("Done."),
            ],
            vec![
                vec![edit("a.txt", "one", "two"), answer("Two.")],
                vec![edit("a.txt", "two", "three"), answer("Three.")],
            ],
        );
        let (end, _) = turn(&mut r).await;
        assert_eq!(answered(end).text, "Done.");
        assert_eq!(
            std::fs::read_to_string(r.root.join("a.txt")).unwrap(),
            "three\n"
        );
    }

    #[test]
    fn without_subagents_there_is_no_delegate() {
        let r = agent(vec![], WorkspaceState::CLEAN);
        let mut opts = r.agent.opts.clone();
        opts.toolset = Toolset::Delegating;
        let a = Agent::new(
            Box::new(played(vec![]).0),
            Family::Harmony,
            Box::new(NoUser),
            opts,
        );
        assert!(a.tools.iter().all(|t| t.name != "delegate"));
    }

    /// An agent of the project at `root` with the skills found there and in `home`, their
    /// allowed-tools trusted, that plays `steps`.
    fn with_skills(
        root: &Path,
        home: &Path,
        steps: Vec<ChatCompletion>,
    ) -> (Agent, Arc<Mutex<Vec<StepRequest>>>) {
        let places = crate::rules::Places {
            root: root.to_owned(),
            home: Some(home.to_owned()),
            claude_dir: None,
        };
        let skills = skills::load(&places);
        let mut opts = options(root, WorkspaceState::CLEAN, None);
        for s in skills.all() {
            let (rules, problems) =
                crate::rules::for_skill(&s.allowed_tools, &s.name, &s.shown, &places);
            assert!(problems.is_empty(), "{problems:?}");
            opts.rules.add(rules);
        }
        opts.skills = skills;
        let (driver, seen) = played(steps.into_iter().map(Step::from).collect());
        let agent = Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts);
        (agent, seen)
    }

    #[tokio::test]
    async fn the_model_loads_a_skill_whose_commands_run_and_whose_tools_its_turn_allows() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        let home = root.parent().unwrap().join("home");
        std::fs::create_dir_all(root.join(".claude/skills/release")).unwrap();
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(root.join("VERSION"), "1.2.3\n").unwrap();
        std::fs::write(
            root.join(".claude/skills/release/SKILL.md"),
            "---\ndescription: Prepares a release of the project.\nallowed-tools: Bash(touch released)\n---\nThe version is !`cat VERSION`.\nRun `touch released`, then report $ARGUMENTS.\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/commands/deploy.md"),
            "---\ndisable-model-invocation: true\n---\nDeploy.\n",
        )
        .unwrap();
        let (mut agent, seen) = with_skills(
            &root,
            &home,
            vec![
                calls(
                    &[("skill", json!({"name": "deploy"}))],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[(
                        "skill",
                        json!({"name": "release", "arguments": "the version"}),
                    )],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[("shell", json!({"command": "touch released"}))],
                    FinishReason::ToolCalls,
                ),
                answer("released 1.2.3"),
            ],
        );
        let end = agent.turn("make a release", &mut |_| {}).await.unwrap();
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "released 1.2.3"),
            "{end:?}"
        );
        let first = seen.lock().unwrap()[0].clone();
        let tool = first.tools.iter().find(|t| t.name == "skill").unwrap();
        assert_eq!(
            tool.parameters["properties"]["name"]["enum"],
            json!(["release"])
        );
        let ChatMessage::User(project) = &first.messages[1] else {
            panic!("{:?}", first.messages[1]);
        };
        assert!(
            project.contains("- release: Prepares a release of the project."),
            "{project}"
        );
        assert!(!project.contains("deploy"), "{project}");
        let results = tool_results(&agent);
        assert!(
            results[0].starts_with("[error] the skill deploy is the user's to run, as /deploy"),
            "{}",
            results[0]
        );
        assert_eq!(
            results[1],
            format!(
                "Base directory for this skill: {}\n\nThe version is 1.2.3.\nRun `touch released`, then report the version.\n",
                root.join(".claude/skills/release").display()
            )
        );
        // allowed by the skill's allowed-tools, in the turn that loaded it
        assert!(results[2].starts_with("[shell: exit 0"), "{}", results[2]);
        assert!(root.join("released").exists());
    }

    #[tokio::test]
    async fn the_user_runs_a_command_and_one_that_cannot_render_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        let home = root.parent().unwrap().join("home");
        std::fs::create_dir_all(root.join(".claude/commands")).unwrap();
        std::fs::write(
            root.join(".claude/commands/fix.md"),
            "---\nargument-hint: <issue>\ndisable-model-invocation: true\n---\nFix issue $0 in !`echo lib.rs`.\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/commands/push.md"),
            "---\ndisable-model-invocation: true\n---\nPush: !`git push`\n",
        )
        .unwrap();
        let (mut agent, seen) = with_skills(&root, &home, vec![answer("fixed")]);
        let end = agent.turn("/fix 42", &mut |_| {}).await.unwrap();
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "fixed"),
            "{end:?}"
        );
        let req = seen.lock().unwrap()[0].clone();
        // no skill the model may load: no skill tool
        assert!(req.tools.iter().all(|t| t.name != "skill"));
        let Some(ChatMessage::User(sent)) = req.messages.last() else {
            panic!("{:?}", req.messages.last());
        };
        assert_eq!(
            sent,
            "/fix 42\n\n[ognivo] The user ran /fix, the project's command .claude/commands/fix.md, which says:\n\nFix issue 42 in lib.rs.\n"
        );
        let mut events = Vec::new();
        let end = agent.turn("/push", &mut |e| events.push(e)).await.unwrap();
        let TurnEnd::NotRun(why) = &end else {
            panic!("{end:?}");
        };
        assert!(
            why.starts_with(
                "/push: Shell command permission check failed for pattern \"!`git push`\""
            ),
            "{why}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(
            matches!(events.last(), Some(AgentEvent::TurnEnded { end, .. }) if end == "not_run")
        );
    }

    #[test]
    fn each_family_calls_skills_by_its_own_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude/skills/pdf")).unwrap();
        std::fs::write(
            dir.path().join(".claude/skills/pdf/SKILL.md"),
            "Fills PDF forms.\n",
        )
        .unwrap();
        let skills = skills::load(&crate::rules::Places {
            root: dir.path().to_owned(),
            home: None,
            claude_dir: None,
        });
        for (family, tool, keys) in [
            (Family::QwenXml, "Skill", ["skill", "args"]),
            (Family::Harmony, "skill", ["name", "arguments"]),
            (Family::Gemma4, "skill", ["name", "task"]),
        ] {
            let decl = skill_tool(family, &skills).unwrap();
            assert_eq!(decl.name, tool);
            let props: Vec<&String> = decl.parameters["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            assert_eq!(props, keys);
            let args = format!(r#"{{"{}": "/pdf", "{}": "form.pdf"}}"#, keys[0], keys[1]);
            assert_eq!(
                skill_call(family, &args),
                Ok(Call::Skill {
                    name: "pdf".into(),
                    arguments: Some("form.pdf".into())
                })
            );
        }
        assert!(skill_tool(Family::Harmony, &Skills::default()).is_none());
        assert!(matches!(
            skill_call(Family::Harmony, r#"{"skill": "pdf"}"#),
            Err(ArgError::Unknown { .. })
        ));
    }

    #[tokio::test]
    async fn hooks_block_a_call_add_to_results_and_send_an_answer_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        let home = root.parent().unwrap().join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("lib.rs"), "fn a() {}\n").unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"hooks": {
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "echo 'The release is on Friday.'"}]}],
                "PreToolUse": [{"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "echo 'no pushing' >&2; exit 2", "if": "Bash(git push *)"}
                ]}],
                "PostToolUse": [{"matcher": "Read", "hooks": [
                    {"type": "command", "command": "echo '{\"hookSpecificOutput\": {\"hookEventName\": \"PostToolUse\", \"additionalContext\": \"read the tests too\"}}'"}
                ]}],
                "Stop": [{"hooks": [
                    {"type": "command", "command": "grep -q '\"stop_hook_active\":true' && exit 0; echo 'run the tests first' >&2; exit 2"}
                ]}]
            }}"#,
        )
        .unwrap();
        let loaded = hooks::load(&crate::rules::Places {
            root: root.clone(),
            home: Some(home),
            claude_dir: None,
        });
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let mut opts = options(&root, WorkspaceState::CLEAN, None);
        opts.hooks = Arc::new(loaded.hooks);
        let (driver, seen) = played(
            vec![
                calls(
                    &[("shell", json!({"command": "git push origin main"}))],
                    FinishReason::ToolCalls,
                ),
                calls(
                    &[("read", json!({"path": "lib.rs"}))],
                    FinishReason::ToolCalls,
                ),
                answer("done"),
                answer("done, tests too"),
            ]
            .into_iter()
            .map(Step::from)
            .collect(),
        );
        let mut agent = Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts);
        let end = agent.turn("ship it", &mut |_| {}).await.unwrap();
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "done, tests too"),
            "{end:?}"
        );
        let first = seen.lock().unwrap()[0].clone();
        let Some(ChatMessage::User(sent)) = first.messages.last() else {
            panic!("{:?}", first.messages.last());
        };
        assert_eq!(
            sent,
            "ship it\n\n[ognivo] From the UserPromptSubmit hooks of the settings:\nThe release is on Friday."
        );
        let results = tool_results(&agent);
        assert_eq!(
            results[0],
            "[refused] a PreToolUse hook blocked Bash: no pushing"
        );
        assert!(
            results[1].ends_with(
                "\n\n[ognivo] From the PostToolUse hooks of the settings:\nread the tests too"
            ),
            "{}",
            results[1]
        );
        assert!(agent.messages().iter().any(|m| matches!(
            m,
            ChatMessage::User(t) if t == "[ognivo] Not finished yet: a Stop hook does not let the turn end: run the tests first"
        )));
    }

    #[tokio::test]
    async fn a_task_for_a_template_gets_its_role_and_only_its_tools() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        let review = |agent: &str| json!({"tasks": [{"title": "Review", "goal": "Review a.txt", "mode": "write", "files": ["a.txt"], "agent": agent}]});
        let (driver, seen) = played(
            vec![
                calls(&[("delegate", review("nobody"))], FinishReason::ToolCalls),
                calls(&[("delegate", review("reviewer"))], FinishReason::ToolCalls),
                answer("Reviewed."),
            ]
            .into_iter()
            .map(Step::from)
            .collect(),
        );
        let subagents = Arc::new(Mutex::new(Vec::new()));
        let mut opts = options(&root, WorkspaceState::DIRTY, None);
        opts.toolset = Toolset::Delegating;
        opts.subagents = Some(Arc::new(Scripts {
            scripts: Mutex::new(vec![vec![
                calls(
                    &[("shell", json!({"command": "ls"}))],
                    FinishReason::ToolCalls,
                ),
                answer("a.txt is fine."),
            ]]),
            seen: subagents.clone(),
        }));
        opts.agents = Templates {
            all: vec![crate::agents::Template {
                name: "reviewer".into(),
                description: "Reviews code.".into(),
                tools: vec![ToolId::Read, ToolId::Grep, ToolId::Glob],
                max_turns: Some(3),
                body: "You review code.".into(),
                shown: ".claude/agents/reviewer.md".into(),
            }],
            problems: Vec::new(),
        };
        let mut agent = Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts);
        let end = agent.turn("review it", &mut |_| {}).await.unwrap();
        assert!(
            matches!(&end, TurnEnd::Answer(a) if a.text == "Reviewed."),
            "{end:?}"
        );
        let main = seen.lock().unwrap()[0].clone();
        let delegate = main.tools.iter().find(|t| t.name == "delegate").unwrap();
        assert_eq!(
            delegate.parameters["properties"]["tasks"]["items"]["properties"]["agent"]["enum"],
            json!(["reviewer", "general-purpose"])
        );
        assert!(
            delegate
                .description
                .ends_with("for `agent`:\n- reviewer: Reviews code.")
        );
        let results = tool_results(&agent);
        assert_eq!(
            results[0],
            "[error] there is no subagent nobody; the subagents are: reviewer, general-purpose"
        );
        let subs = subagents.lock().unwrap();
        let sub = subs[0].lock().unwrap();
        let names: Vec<&str> = sub[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["read", "grep", "glob"]);
        let ChatMessage::User(brief) = sub[0].messages.last().unwrap() else {
            panic!("{:?}", sub[0].messages.last());
        };
        assert!(
            brief.starts_with("[ognivo] You work as the subagent reviewer of .claude/agents/reviewer.md, which is:\n\nYou review code.\n\nTask from the main agent: Review"),
            "{brief}"
        );
        // it changes no file, so its task is a read task
        assert!(brief.contains("This is a read task"), "{brief}");
        let Some(ChatMessage::Tool { content, .. }) = sub[1].messages.last() else {
            panic!("{:?}", sub[1].messages.last());
        };
        assert!(
            content.starts_with("[error] unknown tool \"shell\""),
            "{content}"
        );
    }
}
