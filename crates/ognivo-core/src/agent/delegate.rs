//! Subagents (docs/architecture.md §7). `delegate` hands tasks to agents of their own. Each
//! starts from a system prompt for subagents and the project's context, with its task as
//! the user's message, and works with the session's model through a driver of its own,
//! whose steps wait for their turn at the server's slots behind the user's agent. They work
//! at the same time; write tasks that list the same file, one after another.
//!
//! - A read task gets no tool that changes files, and a write task's changes to other files
//!   are errors. Their commands go through the same approvals, and their questions to the
//!   session's approver one at a time, each saying which task asks.
//! - What a subagent changed and ran becomes the session's: its journal, for review and
//!   revert, and its ledger, for the answer's verification.
//! - Its conversation is kept as a session of its own, which the session's log names.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{
    ChatDriver, ChatMessage, DriverError, Family, ModelDriver, ModelProfile, NativeDriver,
    Priority, Router, Scheduled,
};
use ognivo_tools::files;
use ognivo_tools::projection::{Layout, Task, Toolset};
use tokio::sync::Mutex;

use super::{
    Agent, AgentEvent, AgentOptions, ApprovalRequest, Approver, NoUser, TurnEnd, Verdict,
    system_prompt,
};
use crate::compact;
use crate::verify;

/// Steps of a subagent's turn.
const SUBAGENT_STEPS: u32 = 40;
/// Characters of a subagent's answer in the report the main agent gets.
const REPORT_CHARS: usize = 6000;

/// Where the subagents of a session come from.
pub trait Subagents: Send + Sync + std::fmt::Debug {
    /// A driver for one subagent: the session's model at the subagents' reasoning level,
    /// each step waiting for its turn at the server's slots behind the user's agent.
    fn driver(&self) -> Box<dyn ModelDriver>;
    /// That reasoning level, for the subagents' sessions.
    fn level(&self) -> Option<String>;
}

/// Subagents of the session's model: drivers made as the session's is, at medium reasoning
/// where the profile has that level, each step waiting for its turn at the server's slots
/// behind the user's agent.
#[derive(Debug)]
pub struct SameModel {
    server: LlamaServer,
    profile: ModelProfile,
    /// The day of the session, for the Native path's system message; `None` on the Generic
    /// path.
    date: Option<String>,
    level: Option<String>,
    router: Router,
}

impl SameModel {
    pub fn new(
        server: LlamaServer,
        profile: ModelProfile,
        date: Option<String>,
        session_level: Option<&str>,
        router: Router,
    ) -> Result<Self, DriverError> {
        let level = if profile.reasoning.levels.iter().any(|l| l == "medium") {
            Some("medium".to_owned())
        } else {
            session_level.map(str::to_owned)
        };
        let model = Self {
            server,
            profile,
            date,
            level,
            router,
        };
        // the level and the path hold for every driver to come
        model.unscheduled()?;
        Ok(model)
    }

    fn unscheduled(&self) -> Result<Box<dyn ModelDriver>, DriverError> {
        let (server, profile, level) = (
            self.server.clone(),
            self.profile.clone(),
            self.level.as_deref(),
        );
        Ok(match &self.date {
            Some(date) => Box::new(NativeDriver::new(server, profile, level, None, date)?),
            None => Box::new(ChatDriver::new(server, profile, level, None)?),
        })
    }
}

impl Subagents for SameModel {
    fn driver(&self) -> Box<dyn ModelDriver> {
        let driver = self.unscheduled().expect("checked when it was made");
        Box::new(Scheduled::new(
            driver,
            self.router.clone(),
            &self.profile.id,
            Priority::Subagent,
        ))
    }

    fn level(&self) -> Option<String> {
        self.level.clone()
    }
}

/// The system prompt of a subagent: the session's, and what it is for.
pub fn subagent_prompt(layout: Layout) -> String {
    format!(
        "{}\n- You are a subagent: the main agent gave you one task and sees only your final answer. Make that answer the report the task asks for: what you found or changed, with file paths and line numbers, what you ran and how it went, and what is left.",
        system_prompt(layout)
    )
}

/// The user's message of a subagent: its task.
fn brief(task: &Task) -> String {
    let mut text = format!(
        "Task from the main agent: {}\n\nGoal: {}",
        task.title, task.goal
    );
    if let Some(context) = &task.context {
        text.push_str(&format!("\n\nContext: {context}"));
    }
    if task.write {
        text.push_str(&format!(
            "\n\nYou may change only these files: {}. If something else needs changing, say so in your report.",
            task.files.join(", ")
        ));
    } else {
        text.push_str("\n\nThis is a read task: change no file, with commands neither.");
        if !task.files.is_empty() {
            text.push_str(&format!(" Start with {}.", task.files.join(", ")));
        }
    }
    text
}

/// The session's approver, which subagents ask one at a time, each question naming its
/// task.
struct Asking {
    shared: Arc<Mutex<Box<dyn Approver>>>,
    can_ask: bool,
    who: String,
}

impl std::fmt::Debug for Asking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asking").field("who", &self.who).finish()
    }
}

impl Approver for Asking {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            let req = ApprovalRequest {
                agent: Some(self.who.clone()),
                ..req.clone()
            };
            self.shared.lock().await.approve(&req).await
        })
    }

    fn can_ask(&self) -> bool {
        self.can_ask
    }
}

/// A subagent's turn, boxed: a turn of the main agent holds the subagents' turns.
type Turn<'a> = Pin<Box<dyn Future<Output = Result<TurnEnd, DriverError>> + Send + 'a>>;

impl Agent {
    /// An agent for a subagent's task.
    fn subagent(
        driver: Box<dyn ModelDriver>,
        family: Family,
        approver: Box<dyn Approver>,
        opts: AgentOptions,
    ) -> Agent {
        let mut agent = Agent::new(driver, family, approver, opts);
        agent.messages[0] = ChatMessage::System(subagent_prompt(agent.opts.layout));
        agent
    }

    /// Runs `tasks` on subagents and returns their reports. A future of a named type: a
    /// turn of the main agent holds the turns of its subagents.
    pub(super) fn delegate<'a>(
        &'a mut self,
        tasks: Vec<Task>,
        on: &'a mut (dyn FnMut(AgentEvent) + Send),
    ) -> BoxFuture<'a, String> {
        Box::pin(self.run_subagents(tasks, on))
    }

    async fn run_subagents(
        &mut self,
        tasks: Vec<Task>,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        let Some(subagents) = self.opts.subagents.clone() else {
            return "[error] this session has no subagents".to_owned();
        };
        // they must end before the turn's limit
        let time_limit = match self.room() {
            Ok(room) => room,
            Err(e) => return e,
        };
        let approver = std::mem::replace(&mut self.approver, Box::new(NoUser));
        let can_ask = approver.can_ask();
        let shared = Arc::new(Mutex::new(approver));
        // tasks that may change one file take turns at it, in the order of their files
        let mut locks: HashMap<PathBuf, Arc<Mutex<()>>> = HashMap::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(usize, AgentEvent)>();
        let mut runs = FuturesUnordered::new();
        let workspace = self.workspace();
        for (i, task) in tasks.iter().enumerate() {
            let files: Vec<PathBuf> = task
                .files
                .iter()
                .map(|f| files::resolve(&self.opts.root, f))
                .collect();
            let opts = AgentOptions {
                max_steps: SUBAGENT_STEPS.min(self.opts.max_steps),
                seed: self
                    .opts
                    .seed
                    .wrapping_add(self.stats.steps.wrapping_mul(1009))
                    .wrapping_add(7919 * (i as u32 + 1)),
                workspace,
                time_limit,
                toolset: if task.write {
                    Toolset::All
                } else {
                    Toolset::Reading
                },
                writable: task.write.then(|| files.clone()),
                subagents: None,
                // a read task changes nothing, and an MCP tool may: they stay the session's
                mcp: None,
                ..self.opts.clone()
            };
            let seed = opts.seed;
            let approver = Box::new(Asking {
                shared: shared.clone(),
                can_ask,
                who: task.title.clone(),
            });
            let mut sub = Agent::subagent(subagents.driver(), self.family, approver, opts);
            if let Some(session) = self
                .session
                .as_mut()
                .and_then(|s| s.subagent(subagents.level(), seed, &task.title))
            {
                sub.keep_in(session);
            }
            let mut mine: Vec<PathBuf> = if task.write {
                files.clone()
            } else {
                Vec::new()
            };
            mine.sort();
            mine.dedup();
            let held: Vec<Arc<Mutex<()>>> = mine
                .into_iter()
                .map(|f| locks.entry(f).or_default().clone())
                .collect();
            let brief = brief(task);
            self.emit(
                on,
                AgentEvent::SubagentStarted {
                    index: i,
                    title: task.title.clone(),
                    write: task.write.then(|| task.files.clone()),
                    session: sub.session_id(),
                    brief: brief.clone(),
                    level: subagents.level(),
                },
            );
            let tx = tx.clone();
            runs.push(async move {
                let mut guards = Vec::new();
                for lock in held {
                    guards.push(lock.lock_owned().await);
                }
                let mut forward = move |e: AgentEvent| {
                    let _ = tx.send((i, e));
                };
                let turn: Turn<'_> = Box::pin(sub.turn(&brief, &mut forward));
                let end = turn.await;
                drop(guards);
                (i, sub, end)
            });
        }
        drop(tx);
        let mut reports = vec![String::new(); tasks.len()];
        loop {
            tokio::select! {
                Some((i, e)) = rx.recv() => on(AgentEvent::Subagent { index: i, event: Box::new(e) }),
                Some((i, sub, end)) = runs.next() => {
                    // what it showed comes before its end
                    while let Ok((j, e)) = rx.try_recv() {
                        on(AgentEvent::Subagent { index: j, event: Box::new(e) });
                    }
                    reports[i] = self.take_in(i, &tasks[i], sub, end, on);
                }
                else => break,
            }
        }
        self.approver = match Arc::try_unwrap(shared) {
            Ok(approver) => approver.into_inner(),
            // every subagent is gone by now, and their approvers with them
            Err(_) => Box::new(NoUser),
        };
        format!(
            "[delegate] {} of {} tasks done:\n\n{}",
            reports.len(),
            tasks.len(),
            reports.join("\n\n")
        )
    }

    /// What a finished subagent did becomes the session's; its report.
    fn take_in(
        &mut self,
        i: usize,
        task: &Task,
        mut sub: Agent,
        end: Result<TurnEnd, DriverError>,
        on: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> String {
        let changes = sub.journal.changes();
        let runs: Vec<String> = sub
            .ledger
            .runs()
            .map(|r| format!("{} {}", verify::quoted(&r.command), r.outcome()))
            .collect();
        let steps = sub.stats.steps;
        self.stats.subagents += 1;
        self.stats.subagent_steps += steps;
        self.stats.subagent_tokens += sub.stats.completion_tokens;
        if !changes.is_empty() || !sub.journal.side_effects().is_empty() {
            self.changed();
        }
        self.journal.absorb(std::mem::take(&mut sub.journal));
        let ledger = std::mem::take(&mut sub.ledger);
        self.ledger.extend(if task.write {
            ledger
        } else {
            ledger.runs_only()
        });
        sub.close();
        let (label, said) = match end {
            Ok(TurnEnd::Answer(a)) => {
                let mut text = cut(a.text.trim(), REPORT_CHARS);
                if let Some(line) = verify::summary(&a.verification) {
                    text.push_str(&format!("\n[ognivo] {line}"));
                }
                ("answer", format!("answered after {steps} steps:\n{text}"))
            }
            Ok(TurnEnd::StepLimit) => (
                "step_limit",
                format!("ran out of its {steps} steps before a report."),
            ),
            Ok(TurnEnd::Stuck(why)) => ("stuck", format!("stopped after {steps} steps: {why}.")),
            Ok(TurnEnd::TimeLimit) => (
                "time_limit",
                "the turn's time limit ran out before its report.".to_owned(),
            ),
            Err(e) => ("error", format!("failed: {e}.")),
        };
        let changed: Vec<String> = changes
            .iter()
            .map(|c| format!("{} ({})", c.shown, compact::counts(c)))
            .collect();
        let mut report = format!(
            "{}. {} ({}) {said}",
            i + 1,
            task.title,
            if task.write { "write" } else { "read" }
        );
        if !changed.is_empty() {
            report.push_str(&format!("\nChanged: {}.", changed.join(", ")));
        }
        if !runs.is_empty() {
            report.push_str(&format!("\nRan: {}.", runs.join("; ")));
        }
        self.emit(
            on,
            AgentEvent::SubagentFinished {
                index: i,
                end: label.to_owned(),
                steps,
                changed: changes.iter().map(|c| c.shown.clone()).collect(),
            },
        );
        report
    }
}

/// `text` up to `chars` characters, with a note where it was cut.
fn cut(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!(
            "{}\n[ognivo] the report is cut at {chars} characters",
            &text[..at]
        ),
        None => text.to_owned(),
    }
}
