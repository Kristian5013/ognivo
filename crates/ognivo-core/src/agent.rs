//! The agent loop (docs/architecture.md §6): generate, check, decide, execute, append,
//! until the model answers.
//!
//! - Only [`Outcome::Calls`] runs anything; the driver grants it once the generation
//!   ended normally with `finish_reason=tool_calls` and complete arguments.
//! - A cut-off or malformed generation runs nothing and is not added to the history; the
//!   model gets a user-role note instead. An assistant turn without calls in the middle of
//!   a tool loop would make gpt-oss's template drop all earlier analysis (M0, cpuset).
//! - Every call is checked against its declared schema, computed without side effects,
//!   decided by the approval policy ([`crate::approval`]), and only then executed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use futures::StreamExt;
use futures::future::BoxFuture;
use ognivo_models::{
    ChatMessage, DriverError, DriverEvent, Family, ModelDriver, Outcome, StepRequest, ToolCall,
    ToolChoice, ToolDecl,
};
use ognivo_tools::exec::{self, ShellOptions};
use ognivo_tools::files::{self, EditPlan, FileError, LineStyle};
use ognivo_tools::projection::{self, Call};
use ognivo_tools::{Effect, ExecContext, classify_command, search};
use serde::Serialize;

use crate::approval::{self, Decision, FileEdit, WorkspaceState};

/// How many generations in a row may end without usable calls before the turn stops.
const MAX_UNUSABLE_IN_A_ROW: u32 = 3;
/// Calls executed from one generation; the rest get an error result.
const MAX_CALLS_PER_STEP: usize = 8;

/// An action that needs the user's yes (§8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub tool: String,
    /// What would happen, e.g. the command line or the file path.
    pub action: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refused; the text tells the model why.
    Refuse(String),
}

pub trait Approver: Send {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict>;
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
}

/// Approves everything. Only for runs inside a disposable eval container: the caller must
/// check that before using it (owner's decision of 2026-10-31).
#[derive(Debug, Default)]
pub struct AllowAll;

impl Approver for AllowAll {
    fn approve<'a>(&'a mut self, _: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async { Verdict::Allow })
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
    /// Format errors the driver retried.
    pub restarts: u32,
    /// Calls whose arguments did not match the declared schema.
    pub bad_arguments: u32,
    pub edits_applied: u32,
    pub edits_already_applied: u32,
    pub edits_failed: u32,
    pub refused: u32,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    Answer(String),
    /// The step limit was reached before an answer.
    StepLimit,
    /// Several generations in a row ended without usable calls.
    Stuck(String),
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
    /// (for now: a clean git tree; the edit journal takes over with the store).
    pub workspace: WorkspaceState,
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
    workspace: WorkspaceState,
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
         - Before you say the task is done, build or test the changed code and check that it passes. In the final answer, say what you changed and which command showed that it works; if you could not check something, say so.\n\
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
            workspace: opts.workspace,
            stats: Stats::default(),
            opts,
        }
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Runs one user turn to its end.
    pub async fn turn(
        &mut self,
        user: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnEnd, DriverError> {
        self.messages.push(ChatMessage::User(user.to_owned()));
        // §9: every turn starts in the project root
        self.cwd = self.opts.root.clone();
        let mut unusable = 0;
        for _ in 0..self.opts.max_steps {
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
            while let Some(ev) = events.next().await {
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
            self.stats.prompt_tokens += u64::from(f.usage.prompt_n.unwrap_or(0));
            self.stats.cached_tokens += u64::from(f.usage.cache_n.unwrap_or(0));
            self.stats.completion_tokens += u64::from(f.usage.predicted_n.unwrap_or(0));
            let reasoning = (!f.reasoning.is_empty()).then_some(f.reasoning);
            match f.outcome {
                Outcome::Answer => {
                    self.messages.push(ChatMessage::Assistant {
                        content: f.content.clone(),
                        reasoning,
                        tool_calls: Vec::new(),
                    });
                    return Ok(TurnEnd::Answer(f.content));
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
                        let result = if i < MAX_CALLS_PER_STEP {
                            self.execute(call, on).await
                        } else {
                            format!("[error] not run: at most {MAX_CALLS_PER_STEP} calls per step")
                        };
                        self.messages.push(ChatMessage::Tool {
                            tool_call_id: call.id.clone(),
                            content: result,
                        });
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
        on(AgentEvent::ToolStarted {
            tool: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        let result = match projection::parse(self.family, &call.name, &call.arguments) {
            Ok(parsed) => self.run(&call.name, parsed, on).await,
            Err(e) => {
                self.stats.bad_arguments += 1;
                format!("[error] {e}")
            }
        };
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
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                match files::read(&p, offset, limit, style) {
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
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                search::grep(&base, &pattern, glob.as_deref())
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
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                search::glob(&base, &pattern).unwrap_or_else(|e| format!("[error] {e}"))
            }
            Call::Edit {
                path,
                old,
                new,
                replace_all,
            } => {
                let p = files::resolve(&self.cwd, &path);
                match files::plan_edit(&p, &old, &new, replace_all, style) {
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
                                on,
                            )
                            .await
                        {
                            return refusal;
                        }
                        self.commit(change, p)
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
                match files::plan_write(&p, &content, self.seen.contains(&p)) {
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
                                on,
                            )
                            .await
                        {
                            return refusal;
                        }
                        self.commit(change, p)
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
                if let Some(refusal) = self
                    .permit(
                        tool,
                        approval::decide(effect, self.workspace),
                        &command,
                        reason,
                        on,
                    )
                    .await
                {
                    return refusal;
                }
                let opts = ShellOptions {
                    cwd: self.cwd.clone(),
                    timeout: timeout
                        .unwrap_or(exec::DEFAULT_TIMEOUT)
                        .min(exec::MAX_TIMEOUT),
                    output_budget: exec::DEFAULT_OUTPUT_BUDGET,
                    log_dir: self.opts.data_dir.join("logs"),
                };
                match exec::run(&command, &opts).await {
                    Ok(r) => {
                        if effect == Effect::SideEffects {
                            self.workspace = WorkspaceState::DIRTY;
                        }
                        self.cwd = r.cwd;
                        r.text
                    }
                    Err(e) => format!("[error] {e}"),
                }
            }
        }
    }

    fn commit(&mut self, change: files::Change, path: PathBuf) -> String {
        match change.commit() {
            Ok(text) => {
                self.stats.edits_applied += u32::from(text.starts_with("[edit:"));
                self.workspace = WorkspaceState::DIRTY;
                self.seen.insert(path);
                text
            }
            Err(e) => error_text(&e),
        }
    }

    /// `None` when the action may go ahead; otherwise the refusal the model reads.
    async fn permit(
        &mut self,
        tool: &str,
        decision: Decision,
        action: &str,
        reason: &str,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Option<String> {
        if decision == Decision::Allow {
            return None;
        }
        let req = ApprovalRequest {
            tool: tool.to_owned(),
            action: action.to_owned(),
            reason: reason.to_owned(),
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
    use ognivo_models::{ChatCompletion, FinishReason, Finished};
    use std::sync::{Arc, Mutex};

    /// Plays back prepared generations and records what each step was sent.
    #[derive(Debug)]
    struct Script {
        steps: Mutex<Vec<ChatCompletion>>,
        seen: Arc<Mutex<Vec<StepRequest>>>,
    }

    impl ModelDriver for Script {
        fn step(&self, req: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
            self.seen.lock().unwrap().push(req);
            let c = self.steps.lock().unwrap().remove(0);
            let f = Finished {
                outcome: c.outcome(),
                content: c.content,
                reasoning: c.reasoning,
                finish: c.finish,
                usage: c.usage,
                seed: 0,
            };
            stream::iter(vec![Ok(DriverEvent::Finished(Box::new(f)))]).boxed()
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
        };
        Run {
            agent: Agent::new(Box::new(driver), Family::Harmony, Box::new(NoUser), opts),
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
        let (end, _) = turn(&mut r).await;
        assert_eq!(end, TurnEnd::Answer("done".into()));
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
        assert_eq!(end, TurnEnd::Answer("gave up".into()));
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
}
