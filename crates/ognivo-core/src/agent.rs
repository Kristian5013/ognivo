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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use ognivo_models::{
    Channel, ChatMessage, DriverError, DriverEvent, Family, ModelDriver, Outcome, StepRequest,
    ToolCall, ToolChoice, ToolDecl,
};
use ognivo_tools::diff::Hunk;
use ognivo_tools::exec::{self, ShellOptions};
use ognivo_tools::files::{self, EditPlan, FileError, LineStyle, Target};
use ognivo_tools::projection::{self, Call};
use ognivo_tools::{Effect, ExecContext, classify_command, search};
use serde::Serialize;
use tokio::time::Instant;

use crate::approval::{self, Decision, FileEdit, WorkspaceState};
use crate::journal::{FileChange, Journal, JournalError, Reverted};
use crate::repeats::{Check, Fingerprint, Repeats};
use crate::verify::{self, Ledger, Run, Verification};

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

/// Headless default: nobody can answer, so every question is a refusal.
#[derive(Debug, Default)]
pub struct NoUser;

impl Approver for NoUser {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        let text = format!(
            "{} needs the user's approval ({}), and this run has no user. Do not retry it; do the task another way or explain the limit in your final answer.",
            req.action, req.reason
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
        tool: String,
        result: String,
    },
    Refused {
        tool: String,
        reason: String,
    },
    Notice {
        text: String,
    },
}

/// Counters behind the M1 exit metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
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
    /// Prompt tokens the server processed, and those it took from the KV cache.
    pub prompt_tokens_processed: u64,
    pub prompt_tokens_cached: u64,
    pub completion_tokens: u64,
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
    /// The step limit was reached before an answer.
    StepLimit,
    /// Several generations in a row ended without usable calls, or a call kept repeating
    /// itself with nothing new in between.
    Stuck(String),
    /// The turn's time limit ran out before an answer.
    TimeLimit,
}

#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// Project root, canonical.
    pub root: PathBuf,
    /// `~/.ognivo/data` or the run's own data directory.
    pub data_dir: PathBuf,
    pub max_steps: u32,
    pub max_tokens: u32,
    /// Seed of the first step; each step adds one, so a run is reproducible.
    pub seed: u32,
    /// Review state at the start: clean only when nothing unreviewed is known to exist
    /// (for now: a clean git tree). The edit journal follows the session's own changes.
    pub workspace: WorkspaceState,
    /// Wall-clock limit of each turn.
    pub time_limit: Option<Duration>,
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
    tools: Vec<ToolDecl>,
    opts: AgentOptions,
    approver: Box<dyn Approver>,
    messages: Vec<ChatMessage>,
    cwd: PathBuf,
    /// Files this session has read or written: only those may be overwritten whole.
    seen: HashSet<PathBuf>,
    clock: Option<Clock>,
    journal: Journal,
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

/// The fixed instructions of every session. Short on purpose: the reference agents that
/// work best with small models keep their prefix small (docs/research/2026-10-29).
pub fn system_prompt(root: &Path) -> String {
    format!(
        "You are ognivo, a coding agent. You work in the project at {} through tools: read and search files, edit them, run shell commands.\n\
         - Read a file before you change it. In edit, copy the old text exactly as the file has it, without the line numbers that read shows.\n\
         - Change only what the task needs.\n\
         - After changing code, run the project's build or tests with the shell tool.\n\
         - In the final answer, report only what tool results above show. Never write that a command ran or passed unless its result is above; if a command was refused or failed, say so. ognivo adds the verification status to your answer itself.\n\
         - Commands run without a terminal; do not start interactive programs.",
        root.display()
    )
}

impl Agent {
    pub fn new(
        driver: Box<dyn ModelDriver>,
        family: Family,
        approver: Box<dyn Approver>,
        opts: AgentOptions,
    ) -> Self {
        let cwd = opts.root.clone();
        Self {
            messages: vec![ChatMessage::System(system_prompt(&opts.root))],
            tools: projection::declarations(family),
            driver,
            family,
            approver,
            cwd,
            seen: HashSet::new(),
            clock: None,
            journal: Journal::new(opts.workspace.unreviewed_changes),
            reverted: Vec::new(),
            ledger: Ledger::default(),
            repeats: Repeats::default(),
            stuck: None,
            running: None,
            context: 0,
            stats: Stats::default(),
            opts,
        }
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
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
        self.journal.changes()
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
        self.journal.accept(path)
    }

    /// Puts a file back as it was before the session's unreviewed changes (`force`: also
    /// when something else changed it too). The model learns of it with the next message.
    pub fn revert(&mut self, path: &Path, force: bool) -> Result<Reverted, JournalError> {
        let how = self.journal.revert(path, force)?;
        self.seen.remove(path);
        let name = Target::new(path.to_owned(), &self.opts.root).shown;
        let item = match how {
            Reverted::Restored => name.display().to_string(),
            Reverted::Removed => format!("{} (removed: you had created it)", name.display()),
        };
        if !self.reverted.contains(&item) {
            self.reverted.push(item);
        }
        Ok(how)
    }

    /// Ends a turn that was dropped before its end (the user stopped it, or the server
    /// failed): every call of the last generation gets a result, so the history stays
    /// well-formed, and the model reads `why` with the next message.
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
        self.append_to_last(&format!("[ognivo] {why}"));
        self.running = None;
        self.stuck = None;
    }

    /// Starts a new conversation. What the session changed stays unreviewed (§8), and the
    /// counters go on.
    pub fn clear(&mut self) {
        self.messages.truncate(1);
        self.seen.clear();
        self.context = 0;
    }

    /// Runs one user turn to its end.
    pub async fn turn(
        &mut self,
        user: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnEnd, DriverError> {
        self.clock = self.opts.time_limit.map(|limit| Clock {
            start: Instant::now(),
            limit,
            told: 0,
        });
        let mut user = match self.opts.time_limit {
            Some(limit) => format!(
                "{user}\n\n[ognivo] Time limit: {}. ognivo stops the turn when it runs out.",
                about(limit)
            ),
            None => user.to_owned(),
        };
        if !self.reverted.is_empty() {
            user.push_str(&format!(
                "\n\n[ognivo] Since your last turn the user reverted your changes to {}: they are as they were before your edits. Read them again before you edit them.",
                self.reverted.join(", ")
            ));
            self.reverted.clear();
        }
        self.messages.push(ChatMessage::User(user));
        // §9: every turn starts in the project root
        self.cwd = self.opts.root.clone();
        self.ledger = Ledger::default();
        self.repeats = Repeats::default();
        let (mut sent_to_verify, mut sent_to_correct) = (false, false);
        let mut unusable = 0;
        for _ in 0..self.opts.max_steps {
            if let Some(note) = self.clock.as_mut().and_then(Clock::note) {
                on(AgentEvent::Notice { text: note.clone() });
                self.append_to_last(&note);
            }
            let deadline = self.clock.map(|c| c.deadline());
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(TurnEnd::TimeLimit);
            }
            self.stats.steps += 1;
            let req = StepRequest {
                messages: self.messages.clone(),
                tools: self.tools.clone(),
                tool_choice: ToolChoice::Auto,
                max_tokens: self.opts.max_tokens,
                seed: self.opts.seed.wrapping_add(self.stats.steps),
            };
            let mut events = self.driver.step(req);
            let mut finished = None;
            loop {
                let next = match deadline {
                    // dropping the stream at the limit cancels the generation
                    Some(d) => match tokio::time::timeout_at(d, events.next()).await {
                        Ok(next) => next,
                        Err(_) => return Ok(TurnEnd::TimeLimit),
                    },
                    None => events.next().await,
                };
                let Some(ev) = next else { break };
                match ev? {
                    DriverEvent::Reasoning(text) => on(AgentEvent::Reasoning { text }),
                    DriverEvent::Text(text) => on(AgentEvent::Text { text }),
                    DriverEvent::ToolCallDelta { .. } => {}
                    DriverEvent::Restarted { reason, .. } => {
                        self.stats.restarts += 1;
                        on(AgentEvent::Restarted { reason });
                    }
                    DriverEvent::Finished(f) => finished = Some(*f),
                }
            }
            let Some(f) = finished else {
                return Err(DriverError::InvalidRequest(
                    "the driver ended a step without a result".into(),
                ));
            };
            self.stats.prompt_tokens_processed += u64::from(f.usage.prompt_n.unwrap_or(0));
            self.stats.prompt_tokens_cached += u64::from(f.usage.cache_n.unwrap_or(0));
            self.stats.completion_tokens += u64::from(f.usage.predicted_n.unwrap_or(0));
            if let Some(prompt) = f.usage.prompt_n {
                self.context = u64::from(prompt)
                    + u64::from(f.usage.cache_n.unwrap_or(0))
                    + u64::from(f.usage.predicted_n.unwrap_or(0));
            }
            let reasoning = (!f.reasoning.is_empty()).then_some(f.reasoning);
            match f.outcome {
                Outcome::Answer if f.content.trim().is_empty() => {
                    self.stats.empty_answers += 1;
                    unusable += 1;
                    let note = "[ognivo] Your last output ended with neither a tool call nor an answer: it stopped inside your reasoning, so nothing was run. Make the call you were planning directly, or give the final answer.".to_owned();
                    on(AgentEvent::Notice { text: note.clone() });
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
                    let back = if unverified && !sent_to_verify && self.can_run_project_code() {
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
                    if let Some(note) = back {
                        self.stats.answers_sent_back += 1;
                        on(AgentEvent::Notice { text: note.clone() });
                        self.messages.push(ChatMessage::User(note));
                        continue;
                    }
                    self.messages.push(ChatMessage::Assistant {
                        content: f.content.clone(),
                        reasoning,
                        tool_calls: Vec::new(),
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
                        self.opts.max_tokens
                    );
                    on(AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
                Outcome::Malformed { reason, .. } => {
                    self.stats.malformed += 1;
                    unusable += 1;
                    let note = format!(
                        "[ognivo] Your last tool call could not be used ({reason}), so nothing was run. Call the tool again with a JSON object of arguments."
                    );
                    on(AgentEvent::Notice { text: note.clone() });
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
                    on(AgentEvent::Notice { text: note.clone() });
                    self.messages.push(ChatMessage::User(note));
                }
            }
            if unusable >= MAX_UNUSABLE_IN_A_ROW {
                return Ok(TurnEnd::Stuck(format!(
                    "{unusable} generations in a row ended without a usable tool call"
                )));
            }
        }
        Ok(TurnEnd::StepLimit)
    }

    async fn execute(
        &mut self,
        call: &ToolCall,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        self.stats.calls += 1;
        self.running = Some(call.id.clone());
        let parsed = projection::parse(self.family, &call.name, &call.arguments);
        on(AgentEvent::ToolStarted {
            tool: call.name.clone(),
            arguments: call.arguments.clone(),
            call: parsed.as_ref().ok().cloned(),
        });
        let key = match &parsed {
            Ok(c) => Fingerprint::of(&self.cwd, c),
            Err(_) => Fingerprint::Unparsed {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        };
        let result = match self.repeats.check(&key) {
            Check::Run => {
                let mut result = match parsed {
                    Ok(parsed) => self.run(&call.name, parsed, on).await,
                    Err(e) => {
                        self.stats.bad_arguments += 1;
                        format!("[error] {e}")
                    }
                };
                if let Some(note) = self.repeats.record(key, &result) {
                    self.stats.repeated += 1;
                    result.push_str("\n\n");
                    result.push_str(&note);
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
        on(AgentEvent::ToolFinished {
            tool: call.name.clone(),
            result: result.clone(),
        });
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
                if let Some(refusal) = self
                    .permit(
                        tool,
                        approval::decide_read(&p, &root),
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
                if let Some(refusal) = self
                    .permit(
                        tool,
                        approval::decide_read(&base, &root),
                        &base.display().to_string(),
                        "outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                search::grep(&base, &root, &pattern, glob.as_deref())
                    .unwrap_or_else(|e| format!("[error] {e}"))
            }
            Call::Glob { pattern, path } => {
                let base = path.map_or_else(|| self.cwd.clone(), |p| files::resolve(&self.cwd, &p));
                if let Some(refusal) = self
                    .permit(
                        tool,
                        approval::decide_read(&base, &root),
                        &base.display().to_string(),
                        "outside the project",
                        Vec::new(),
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                search::glob(&base, &root, &pattern).unwrap_or_else(|e| format!("[error] {e}"))
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
                        let decision = approval::decide_edit(
                            &[FileEdit {
                                path: &change.path,
                                before: change.before.as_deref(),
                                after: Some(&change.after),
                            }],
                            &root,
                        );
                        if let Some(refusal) = self
                            .permit(
                                tool,
                                decision,
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
                        let decision = approval::decide_edit(
                            &[FileEdit {
                                path: &change.path,
                                before: change.before.as_deref(),
                                after: Some(&change.after),
                            }],
                            &root,
                        );
                        if let Some(refusal) = self
                            .permit(
                                tool,
                                decision,
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
                let unreviewed =
                    if effect == Effect::ExecutesProjectCode && decision == Decision::Ask {
                        self.journal.summary()
                    } else {
                        Vec::new()
                    };
                if let Some(refusal) = self
                    .permit(tool, decision, &command, reason, unreviewed, on)
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
                            self.repeats.changed();
                        }
                        self.cwd = r.cwd;
                        r.text
                    }
                    Err(e) => format!("[error] {e}"),
                }
            }
        }
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
                self.repeats.changed();
                self.ledger.changed(path.clone());
                self.seen.insert(path);
                on(changed);
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

    /// `None` when the action may go ahead; otherwise the refusal the model reads.
    async fn permit(
        &mut self,
        tool: &str,
        decision: Decision,
        action: &str,
        reason: &str,
        unreviewed: Vec<String>,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Option<String> {
        if decision == Decision::Allow {
            return None;
        }
        let req = ApprovalRequest {
            tool: tool.to_owned(),
            action: action.to_owned(),
            reason: reason.to_owned(),
            unreviewed,
        };
        match self.approver.approve(&req).await {
            Verdict::Allow => None,
            Verdict::Refuse(text) => {
                self.stats.refused += 1;
                on(AgentEvent::Refused {
                    tool: tool.to_owned(),
                    reason: text.clone(),
                });
                Some(format!("[refused] {text}"))
            }
        }
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
        let seen = Arc::new(Mutex::new(Vec::new()));
        let driver = Script {
            steps: Mutex::new(steps),
            seen: seen.clone(),
        };
        let opts = AgentOptions {
            root: root.clone(),
            data_dir: root.parent().unwrap().join("data"),
            max_steps: 10,
            max_tokens: 16384,
            seed: 1,
            workspace,
            time_limit,
        };
        Run {
            agent: Agent::new(Box::new(driver), Family::Harmony, approver, opts),
            seen,
            root,
            _dir: dir,
        }
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

    fn notes(a: &Agent) -> Vec<String> {
        a.messages()
            .iter()
            .filter_map(|m| match m {
                ChatMessage::User(t) if t.starts_with("[ognivo]") => Some(t.clone()),
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
        // the UI gets the parsed call and, for a change, the diff
        assert!(matches!(
            &events[0],
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
        assert_eq!(seen[2].messages.len(), 6); // system, user, (assistant, tool) x 2
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
        assert_eq!(roles, ["system", "user", "user", "assistant"]);
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
        let ChatMessage::User(task) = &r.agent.messages()[1] else {
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
        r.agent.abandon_turn("The user stopped this turn.");
        assert_eq!(
            tool_results(&r.agent),
            [
                "[error] stopped before it finished; it may have done part of its work",
                "[error] not run: the turn was stopped\n\n[ognivo] The user stopped this turn."
            ]
        );
    }

    #[tokio::test]
    async fn a_turn_dropped_in_a_generation_leaves_a_note_for_the_model() {
        let mut r = script(
            vec![Step::Hang],
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
        r.agent.abandon_turn("The user stopped this turn.");
        assert_eq!(
            r.agent.messages().last(),
            Some(&ChatMessage::User(
                "fix it\n\n[ognivo] The user stopped this turn.".into()
            ))
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
        assert_eq!(r.agent.messages().len(), 1);
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
}
