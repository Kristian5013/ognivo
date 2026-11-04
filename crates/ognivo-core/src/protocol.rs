//! The event model of the clients ([`ognivo_protocol`]) from the agent's events: the items
//! of a thread with their numbers, a turn's start and end, each generation's usage, and a
//! thread of its own for each subagent.
//!
//! - The text or reasoning that streams is one item until something else comes: a call,
//!   a note, the generation's end.
//! - A call's item completes with its result, the files it changed, and what came of it:
//!   ran, failed, refused or not run, and a command's exit code, read from the result's
//!   first line as the tools write it.
//! - A generation that ran nothing (calls that did not parse, no answer, cut, repeating,
//!   restarted) is a `void` item.

use std::collections::HashMap;
use std::time::Instant;

use ognivo_protocol::{self as p, ItemBody};
use ognivo_tools::projection::Call;

use crate::agent::AgentEvent;

/// Events for the clients, each with its thread; the stream numbers them.
pub type Out = Vec<(p::ThreadId, p::EventKind)>;

/// What a thread's agent events become, and those of its subagents.
#[derive(Debug)]
pub struct Projector {
    thread: p::ThreadId,
    info: p::ThreadInfo,
    turn: u32,
    /// A turn began and has not ended.
    turn_open: bool,
    items: u32,
    generations: u32,
    /// The text or reasoning streaming now.
    open: Option<p::Item>,
    /// Running calls by the model's id, with when each began.
    calls: HashMap<String, (p::Item, Instant)>,
    /// The call whose changes come now.
    running: Option<String>,
    compaction: Option<p::Item>,
    approval: Option<p::Item>,
    /// The subagents of the running `delegate`, by their tasks' numbers: their items here
    /// and their threads.
    subagents: HashMap<usize, (p::Item, Projector)>,
}

impl Projector {
    pub fn new(thread: p::ThreadId, info: p::ThreadInfo) -> Self {
        Self {
            thread,
            info,
            turn: 0,
            turn_open: false,
            items: 0,
            generations: 0,
            open: None,
            calls: HashMap::new(),
            running: None,
            compaction: None,
            approval: None,
            subagents: HashMap::new(),
        }
    }

    /// A thread picked up again: its turns, items and generations go on from these.
    pub fn resumed(
        thread: p::ThreadId,
        info: p::ThreadInfo,
        turns: u32,
        items: u32,
        generations: u32,
    ) -> Self {
        Self {
            turn: turns,
            items,
            generations,
            ..Self::new(thread, info)
        }
    }

    /// The question that waits for the user now, in this thread or in a subagent's: the
    /// thread and the item that ask `tool` about `action`, for `agent` (a subagent by its
    /// task's title) or the thread's own agent.
    pub fn asking(
        &self,
        agent: Option<&str>,
        tool: &str,
        action: &str,
    ) -> Option<(p::ThreadId, p::ItemId)> {
        let asks = |item: &p::Item| {
            matches!(&item.body, ItemBody::Approval { tool: t, action: a, allowed: None, .. }
                if t == tool && a == action)
        };
        let Some(title) = agent else {
            return self
                .approval
                .as_ref()
                .filter(|item| asks(item))
                .map(|item| (self.thread, item.id));
        };
        let mut subagents: Vec<_> = self.subagents.iter().collect();
        subagents.sort_by_key(|(index, _)| **index);
        subagents.into_iter().find_map(|(_, (item, child))| {
            let ours = matches!(&item.body, ItemBody::Subagent { title: t, .. } if t == title);
            child
                .approval
                .as_ref()
                .filter(|a| ours && asks(a))
                .map(|a| (child.thread, a.id))
        })
    }

    pub fn thread(&self) -> p::ThreadId {
        self.thread
    }

    /// The number of the thread's last turn; 0 before the first.
    pub fn turns(&self) -> u32 {
        self.turn
    }

    /// The turns from `turn` on were taken back; the next turn is numbered on.
    pub fn rewound(&self, turn: u32) -> Out {
        vec![(
            self.thread,
            p::EventKind::ThreadRewound {
                turn: p::TurnId(turn),
            },
        )]
    }

    /// The thread's start.
    pub fn started(&self) -> Out {
        vec![(
            self.thread,
            p::EventKind::ThreadStarted {
                info: self.info.clone(),
            },
        )]
    }

    pub fn event(&mut self, e: AgentEvent) -> Out {
        let mut out = Vec::new();
        match e {
            AgentEvent::TurnStarted { user } => {
                self.turn += 1;
                self.turn_open = true;
                self.emit(
                    &mut out,
                    p::EventKind::TurnStarted {
                        turn: p::TurnId(self.turn),
                        user,
                    },
                );
            }
            AgentEvent::Reasoning { text } => self.stream(&mut out, text, true),
            AgentEvent::Text { text } => self.stream(&mut out, text, false),
            AgentEvent::Restarted { .. } => {
                self.close(&mut out);
                self.whole(
                    &mut out,
                    Some(self.generations + 1),
                    ItemBody::Void {
                        why: p::VoidReason::Restarted,
                        calls: Vec::new(),
                    },
                );
            }
            AgentEvent::ToolStarted {
                id,
                tool,
                arguments,
                call,
            } => {
                self.close(&mut out);
                let item = self.item(
                    Some(self.generations),
                    ItemBody::ToolCall {
                        call_id: id.clone(),
                        tool,
                        arguments,
                        call: call.as_ref().map(call_of),
                        result: None,
                        changes: Vec::new(),
                    },
                );
                self.emit(&mut out, p::EventKind::ItemStarted { item: item.clone() });
                self.calls.insert(id.clone(), (item, Instant::now()));
                self.running = Some(id);
            }
            AgentEvent::FileChanged {
                path,
                created,
                hunks,
            } => {
                let running = self.running.as_ref().and_then(|id| self.calls.get_mut(id));
                if let Some((
                    p::Item {
                        body: ItemBody::ToolCall { changes, .. },
                        ..
                    },
                    _,
                )) = running
                {
                    changes.push(p::FileChange {
                        path,
                        created,
                        hunks: hunks
                            .into_iter()
                            .map(|h| p::Hunk {
                                old_start: h.old_start as u32,
                                new_start: h.new_start as u32,
                                removed: h.removed,
                                added: h.added,
                            })
                            .collect(),
                    });
                }
            }
            AgentEvent::ToolFinished { id, result, .. } => {
                if self.running.as_ref() == Some(&id) {
                    self.running = None;
                }
                if let Some((mut item, began)) = self.calls.remove(&id) {
                    if let ItemBody::ToolCall { result: slot, .. } = &mut item.body {
                        *slot = Some(result_of(&result, began.elapsed().as_millis() as u64));
                    }
                    self.emit(&mut out, p::EventKind::ItemCompleted { item });
                }
            }
            AgentEvent::Instructions { files, skipped } => {
                self.close(&mut out);
                self.whole(
                    &mut out,
                    None,
                    ItemBody::Instructions {
                        files: files.into_iter().map(|g| g.path).collect(),
                        skipped: skipped
                            .into_iter()
                            .map(|s| p::Skipped {
                                path: s.path,
                                why: s.why,
                            })
                            .collect(),
                    },
                );
            }
            // a refusal is the call's result
            AgentEvent::Refused { .. } => {}
            AgentEvent::Notice { text } => {
                self.close(&mut out);
                self.whole(&mut out, None, ItemBody::Notice { text });
            }
            AgentEvent::Compacting { tokens, limit } => {
                self.close(&mut out);
                let item = self.item(
                    None,
                    ItemBody::Compaction {
                        tokens: tokens as u32,
                        limit: limit as u32,
                        replaced: None,
                        kept: None,
                        summary: None,
                    },
                );
                self.emit(&mut out, p::EventKind::ItemStarted { item: item.clone() });
                self.compaction = Some(item);
            }
            AgentEvent::Compacted {
                replaced,
                kept,
                summary,
            } => {
                if let Some(mut item) = self.compaction.take() {
                    if let ItemBody::Compaction {
                        replaced: r,
                        kept: k,
                        summary: s,
                        ..
                    } = &mut item.body
                    {
                        (*r, *k, *s) = (Some(replaced as u32), Some(kept as u32), summary);
                    }
                    self.emit(&mut out, p::EventKind::ItemCompleted { item });
                }
            }
            AgentEvent::Approval {
                tool,
                action,
                reason,
                unreviewed,
                agent,
                allowed: None,
            } => {
                self.close(&mut out);
                let item = self.item(
                    None,
                    ItemBody::Approval {
                        tool,
                        action,
                        reason,
                        unreviewed,
                        agent,
                        allowed: None,
                    },
                );
                self.emit(&mut out, p::EventKind::ItemStarted { item: item.clone() });
                self.approval = Some(item);
            }
            AgentEvent::Approval {
                allowed: Some(yes), ..
            } => {
                if let Some(mut item) = self.approval.take() {
                    if let ItemBody::Approval { allowed, .. } = &mut item.body {
                        *allowed = Some(yes);
                    }
                    self.emit(&mut out, p::EventKind::ItemCompleted { item });
                }
            }
            AgentEvent::Generation {
                purpose,
                outcome,
                finish,
                seed,
                prompt_tokens,
                cached_tokens,
                completion_tokens,
                prompt_ms,
                generation_ms,
                prompt_hash,
                rejected,
            } => {
                self.close(&mut out);
                self.generations += 1;
                let why = match outcome.as_str() {
                    "empty" => Some(p::VoidReason::Empty),
                    "truncated" => Some(p::VoidReason::Truncated),
                    "repeating" => Some(p::VoidReason::Repeating),
                    "malformed" => Some(p::VoidReason::Malformed),
                    _ => None,
                };
                // a summary's generation shows nothing of its own
                if let (Some(why), "step") = (why, purpose.as_str()) {
                    let calls = rejected
                        .into_iter()
                        .map(|c| p::RawCall {
                            name: c.name,
                            arguments: c.arguments,
                        })
                        .collect();
                    self.whole(
                        &mut out,
                        Some(self.generations),
                        ItemBody::Void { why, calls },
                    );
                }
                let generation = p::Generation {
                    index: self.generations,
                    turn: p::TurnId(self.turn),
                    purpose,
                    outcome,
                    finish,
                    seed,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                    prompt_ms,
                    generation_ms,
                    prompt_hash,
                };
                self.emit(&mut out, p::EventKind::Generation { generation });
            }
            AgentEvent::SubagentStarted {
                index,
                title,
                write,
                session,
                brief: _,
                level,
            } => {
                self.close(&mut out);
                // a session not kept has no id: one of its own, apart from the store's
                let thread = p::ThreadId(
                    session.unwrap_or(-(self.thread.0.abs() * 1000 + index as i64 + 1)),
                );
                let item = self.item(
                    None,
                    ItemBody::Subagent {
                        index: index as u32,
                        title,
                        write,
                        thread: Some(thread),
                        end: None,
                        steps: None,
                        changed: Vec::new(),
                    },
                );
                self.emit(&mut out, p::EventKind::ItemStarted { item: item.clone() });
                let info = p::ThreadInfo {
                    kind: "subagent".into(),
                    level,
                    parent: Some(p::Parent {
                        thread: self.thread,
                        item: item.id,
                    }),
                    ..self.info.clone()
                };
                let child = Projector::new(thread, info);
                out.extend(child.started());
                self.subagents.insert(index, (item, child));
            }
            AgentEvent::Subagent { index, event } => {
                if let Some((_, child)) = self.subagents.get_mut(&index) {
                    out.extend(child.event(*event));
                }
            }
            AgentEvent::SubagentFinished {
                index,
                end,
                steps,
                changed,
            } => {
                if let Some((mut item, mut child)) = self.subagents.remove(&index) {
                    // a turn that never ended in its own events ends here
                    out.extend(child.interrupted());
                    if let ItemBody::Subagent {
                        end: e,
                        steps: s,
                        changed: c,
                        ..
                    } = &mut item.body
                    {
                        (*e, *s, *c) = (Some(end_of(&end)), Some(steps), changed);
                    }
                    self.emit(&mut out, p::EventKind::ItemCompleted { item });
                }
            }
            AgentEvent::TurnEnded {
                end,
                why,
                answer,
                verification,
            } => {
                self.close(&mut out);
                self.emit(
                    &mut out,
                    p::EventKind::TurnCompleted {
                        turn: p::TurnId(self.turn),
                        end: end_of(&end),
                        why,
                        answer,
                        verification,
                    },
                );
                self.turn_open = false;
            }
        }
        out
    }

    /// The running turn was stopped from outside, by the user or a signal: everything open
    /// completes as it is, a call that was running as not run, and the turn with it.
    pub fn interrupted(&mut self) -> Out {
        let mut out = Vec::new();
        self.close(&mut out);
        let mut subagents: Vec<_> = std::mem::take(&mut self.subagents).into_iter().collect();
        subagents.sort_by_key(|(index, _)| *index);
        for (_, (mut item, mut child)) in subagents {
            out.extend(child.interrupted());
            if let ItemBody::Subagent { end, .. } = &mut item.body {
                *end = Some(p::TurnEnd::Interrupted);
            }
            self.emit(&mut out, p::EventKind::ItemCompleted { item });
        }
        let mut calls: Vec<_> = std::mem::take(&mut self.calls).into_values().collect();
        calls.sort_by_key(|(item, _)| item.id.0);
        for (mut item, began) in calls {
            if let ItemBody::ToolCall { result, .. } = &mut item.body {
                *result = Some(p::ToolResult {
                    outcome: p::Outcome::Skipped,
                    text: "[ognivo] stopped with the turn".into(),
                    exit_code: None,
                    duration_ms: began.elapsed().as_millis() as u64,
                });
            }
            self.emit(&mut out, p::EventKind::ItemCompleted { item });
        }
        self.running = None;
        for item in [self.approval.take(), self.compaction.take()]
            .into_iter()
            .flatten()
        {
            self.emit(&mut out, p::EventKind::ItemCompleted { item });
        }
        if self.turn_open {
            self.emit(
                &mut out,
                p::EventKind::TurnCompleted {
                    turn: p::TurnId(self.turn),
                    end: p::TurnEnd::Interrupted,
                    why: None,
                    answer: None,
                    verification: None,
                },
            );
            self.turn_open = false;
        }
        out
    }

    fn emit(&self, out: &mut Out, kind: p::EventKind) {
        out.push((self.thread, kind));
    }

    /// A new item of the running turn.
    fn item(&mut self, generation: Option<u32>, body: ItemBody) -> p::Item {
        self.items += 1;
        p::Item {
            id: p::ItemId(self.items),
            turn: p::TurnId(self.turn),
            generation,
            body,
        }
    }

    /// An item that starts and completes at once.
    fn whole(&mut self, out: &mut Out, generation: Option<u32>, body: ItemBody) {
        let item = self.item(generation, body);
        self.emit(out, p::EventKind::ItemStarted { item: item.clone() });
        self.emit(out, p::EventKind::ItemCompleted { item });
    }

    /// More streamed text: the open item's, or a new one's.
    fn stream(&mut self, out: &mut Out, text: String, reasoning: bool) {
        let same = matches!(
            (&self.open, reasoning),
            (
                Some(p::Item {
                    body: ItemBody::Reasoning { .. },
                    ..
                }),
                true
            ) | (
                Some(p::Item {
                    body: ItemBody::AgentMessage { .. },
                    ..
                }),
                false
            )
        );
        if !same {
            self.close(out);
            let body = if reasoning {
                ItemBody::Reasoning {
                    text: String::new(),
                }
            } else {
                ItemBody::AgentMessage {
                    text: String::new(),
                }
            };
            // the generation that streams it is the next to end
            let item = self.item(Some(self.generations + 1), body);
            self.emit(out, p::EventKind::ItemStarted { item: item.clone() });
            self.open = Some(item);
        }
        let Some(item) = self.open.as_mut() else {
            return;
        };
        if let ItemBody::Reasoning { text: t } | ItemBody::AgentMessage { text: t } = &mut item.body
        {
            t.push_str(&text);
        }
        let id = item.id;
        self.emit(
            out,
            p::EventKind::ItemDelta {
                item: id,
                delta: p::Delta::Text { text },
            },
        );
    }

    /// The streamed item completes.
    fn close(&mut self, out: &mut Out) {
        if let Some(item) = self.open.take() {
            self.emit(out, p::EventKind::ItemCompleted { item });
        }
    }
}

/// A call in the clients' terms.
fn call_of(c: &Call) -> p::Call {
    let n = |n: &Option<usize>| n.map(|n| n as u32);
    match c.clone() {
        Call::Read {
            path,
            offset,
            limit,
        } => p::Call::Read {
            path,
            offset: n(&offset),
            limit: n(&limit),
        },
        Call::Edit {
            path,
            old,
            new,
            replace_all,
        } => p::Call::Edit {
            path,
            old,
            new,
            replace_all,
        },
        Call::Write { path, content } => p::Call::Write { path, content },
        Call::Patch { patch } => p::Call::ApplyPatch { patch },
        Call::Shell { command, timeout } => p::Call::Bash {
            command,
            timeout_ms: timeout.map(|d| d.as_millis() as u32),
        },
        Call::Grep {
            pattern,
            path,
            glob,
        } => p::Call::Grep {
            pattern,
            path,
            glob,
        },
        Call::Glob { pattern, path } => p::Call::Glob { pattern, path },
        Call::Delegate { tasks } => p::Call::Task {
            tasks: tasks
                .into_iter()
                .map(|t| p::Task {
                    title: t.title,
                    goal: t.goal,
                    context: t.context,
                    write: t.write,
                    files: t.files,
                })
                .collect(),
        },
    }
}

/// A call's result as the model got it, and what came of it: by the first line, as the
/// tools and the loop write it (`[refused] …`, `[error] …`, `[ognivo] Not run: …`,
/// `[shell: exit 0, 0.1 s]`).
fn result_of(text: &str, duration_ms: u64) -> p::ToolResult {
    let first = text.lines().next().unwrap_or_default();
    let outcome = if first.starts_with("[refused]") {
        p::Outcome::Refused
    } else if first.starts_with("[ognivo] Not run") || first.starts_with("[error] not run") {
        p::Outcome::Skipped
    } else if first.starts_with("[error]") {
        p::Outcome::Error
    } else {
        p::Outcome::Ok
    };
    // also `[shell: timed out after 5 s and was killed; exit 124]`
    let exit_code = first.strip_prefix("[shell: ").and_then(|rest| {
        let at = rest.find("exit ")? + "exit ".len();
        let digits: String = rest[at..]
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse().ok()
    });
    p::ToolResult {
        outcome,
        text: text.to_owned(),
        exit_code,
        duration_ms,
    }
}

fn end_of(end: &str) -> p::TurnEnd {
    match end {
        "answer" => p::TurnEnd::Answer,
        "step_limit" => p::TurnEnd::StepLimit,
        "stuck" => p::TurnEnd::Stuck,
        "time_limit" => p::TurnEnd::TimeLimit,
        "interrupted" => p::TurnEnd::Interrupted,
        _ => p::TurnEnd::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::Given;
    use ognivo_models::ToolCall;
    use ognivo_tools::diff::Hunk;
    use ognivo_tools::projection::Task;

    fn info() -> p::ThreadInfo {
        p::ThreadInfo {
            kind: "run".into(),
            project: "/home/user/proj".into(),
            profile: "gpt-oss-20b".into(),
            level: Some("high".into()),
            server: "http://127.0.0.1:8090".into(),
            model: None,
            parent: None,
        }
    }

    fn generation(purpose: &str, outcome: &str, seed: u32, rejected: Vec<ToolCall>) -> AgentEvent {
        AgentEvent::Generation {
            purpose: purpose.into(),
            outcome: outcome.into(),
            finish: Some(
                if outcome == "calls" {
                    "tool_calls"
                } else {
                    "stop"
                }
                .into(),
            ),
            seed,
            prompt_tokens: 700 + seed * 100,
            cached_tokens: 650 + seed * 100,
            completion_tokens: 40,
            prompt_ms: Some(12.5),
            generation_ms: Some(310.0),
            prompt_hash: format!("sha256:{seed:064}"),
            rejected,
        }
    }

    fn started(id: &str, tool: &str, arguments: &str, call: Call) -> AgentEvent {
        AgentEvent::ToolStarted {
            id: id.into(),
            tool: tool.into(),
            arguments: arguments.into(),
            call: Some(call),
        }
    }

    fn finished(id: &str, tool: &str, result: &str) -> AgentEvent {
        AgentEvent::ToolFinished {
            id: id.into(),
            tool: tool.into(),
            result: result.into(),
        }
    }

    fn sub(index: usize, e: AgentEvent) -> AgentEvent {
        AgentEvent::Subagent {
            index,
            event: Box::new(e),
        }
    }

    /// A turn with every kind of item.
    fn turn() -> Vec<AgentEvent> {
        let brief = "Task from the main agent: Check the README\n\nGoal: What greeting does it show?\n\nThis is a read task: change no file, with commands neither.";
        vec![
            AgentEvent::TurnStarted {
                user: "Say hello instead of hi, and check the README says the same.".into(),
            },
            AgentEvent::Instructions {
                files: vec![Given {
                    path: "AGENTS.md".into(),
                    bytes: 25,
                    cut: false,
                }],
                skipped: Vec::new(),
            },
            AgentEvent::Reasoning {
                text: "The greeting ".into(),
            },
            AgentEvent::Reasoning {
                text: "is in main.rs.".into(),
            },
            generation("step", "calls", 1, Vec::new()),
            started(
                "c1",
                "read",
                r#"{"path":"main.rs"}"#,
                Call::Read {
                    path: "main.rs".into(),
                    offset: None,
                    limit: None,
                },
            ),
            finished(
                "c1",
                "read",
                "[read: main.rs — 3 of 3 lines, 33 bytes, ~10 tokens]\n1│fn main() {\n2│    println!(\"hi\");\n3│}",
            ),
            generation("step", "calls", 2, Vec::new()),
            started(
                "c2",
                "edit",
                r#"{"path":"main.rs","old_string":"\"hi\"","new_string":"\"hello\""}"#,
                Call::Edit {
                    path: "main.rs".into(),
                    old: "\"hi\"".into(),
                    new: "\"hello\"".into(),
                    replace_all: false,
                },
            ),
            AgentEvent::FileChanged {
                path: "main.rs".into(),
                created: false,
                hunks: vec![Hunk {
                    old_start: 2,
                    new_start: 2,
                    removed: vec!["    println!(\"hi\");".into()],
                    added: vec!["    println!(\"hello\");".into()],
                }],
            },
            finished(
                "c2",
                "edit",
                "[edit: applied exact] main.rs — lines 1-3 now:\n1│fn main() {\n2│    println!(\"hello\");\n3│}",
            ),
            generation(
                "step",
                "malformed",
                3,
                vec![ToolCall {
                    id: "c3".into(),
                    name: "shell".into(),
                    arguments: r#"{"command": "cargo te"#.into(),
                }],
            ),
            AgentEvent::Notice {
                text: "[ognivo] Your last tool call could not be used (arguments are not JSON), so nothing was run. Call the tool again with a JSON object of arguments.".into(),
            },
            generation("step", "calls", 4, Vec::new()),
            AgentEvent::Approval {
                tool: "shell".into(),
                action: "cargo test".into(),
                reason: "it runs project code while there are unreviewed changes".into(),
                unreviewed: vec!["main.rs".into()],
                agent: None,
                allowed: None,
            },
            AgentEvent::Approval {
                tool: "shell".into(),
                action: "cargo test".into(),
                reason: "it runs project code while there are unreviewed changes".into(),
                unreviewed: vec!["main.rs".into()],
                agent: None,
                allowed: Some(true),
            },
            started(
                "c4",
                "shell",
                r#"{"command":"cargo test"}"#,
                Call::Shell {
                    command: "cargo test".into(),
                    timeout: None,
                },
            ),
            finished("c4", "shell", "[shell: exit 0, 0.4 s]\ntest result: ok. 1 passed"),
            generation("step", "calls", 5, Vec::new()),
            started(
                "c5",
                "delegate",
                r#"{"tasks":[{"title":"Check the README","goal":"What greeting does it show?","mode":"read"}]}"#,
                Call::Delegate {
                    tasks: vec![Task {
                        title: "Check the README".into(),
                        goal: "What greeting does it show?".into(),
                        context: None,
                        write: false,
                        files: Vec::new(),
                    }],
                },
            ),
            AgentEvent::SubagentStarted {
                index: 0,
                title: "Check the README".into(),
                write: None,
                session: Some(12),
                brief: brief.into(),
                level: Some("medium".into()),
            },
            sub(0, AgentEvent::TurnStarted { user: brief.into() }),
            sub(
                0,
                AgentEvent::Reasoning {
                    text: "Find it.".into(),
                },
            ),
            sub(0, generation("step", "calls", 101, Vec::new())),
            sub(
                0,
                started(
                    "s1",
                    "grep",
                    r#"{"pattern":"hello|hi"}"#,
                    Call::Grep {
                        pattern: "hello|hi".into(),
                        path: None,
                        glob: None,
                    },
                ),
            ),
            sub(
                0,
                finished(
                    "s1",
                    "grep",
                    "[grep: 1 matches in 1 files for /hello|hi/ under .]\nREADME.md:3:Prints hi.",
                ),
            ),
            sub(
                0,
                AgentEvent::Text {
                    text: "README.md line 3 says hi.".into(),
                },
            ),
            sub(0, generation("step", "answer", 102, Vec::new())),
            sub(
                0,
                AgentEvent::TurnEnded {
                    end: "answer".into(),
                    why: None,
                    answer: Some("README.md line 3 says hi.".into()),
                    verification: None,
                },
            ),
            AgentEvent::SubagentFinished {
                index: 0,
                end: "answer".into(),
                steps: 2,
                changed: Vec::new(),
            },
            finished(
                "c5",
                "delegate",
                "[delegate] 1 of 1 tasks done:\n\n1. Check the README (read) answered after 2 steps:\nREADME.md line 3 says hi.",
            ),
            AgentEvent::Compacting {
                tokens: 70_100,
                limit: 68_812,
            },
            generation("summary", "answer", 6, Vec::new()),
            AgentEvent::Compacted {
                replaced: 14,
                kept: 2,
                summary: Some("Task: say hello. Done: main.rs changed, cargo test passed.".into()),
            },
            AgentEvent::Text {
                text: "Changed the greeting in main.rs; ".into(),
            },
            AgentEvent::Text {
                text: "the README still says hi.".into(),
            },
            generation("step", "answer", 7, Vec::new()),
            AgentEvent::TurnEnded {
                end: "answer".into(),
                why: None,
                answer: Some("Changed the greeting in main.rs; the README still says hi.".into()),
                verification: Some("verified: `cargo test` passed after the last change".into()),
            },
        ]
    }

    /// The stream as the clients get it, numbered, without the calls' times.
    fn stream(events: Vec<AgentEvent>) -> Vec<p::Event> {
        let mut projector = Projector::new(p::ThreadId(11), info());
        let mut out = projector.started();
        for e in events {
            out.extend(projector.event(e));
        }
        out.into_iter()
            .enumerate()
            .map(|(i, (thread, mut kind))| {
                if let p::EventKind::ItemCompleted { item } = &mut kind
                    && let ItemBody::ToolCall {
                        result: Some(r), ..
                    } = &mut item.body
                {
                    r.duration_ms = 0;
                }
                p::Event {
                    seq: i as u64 + 1,
                    thread,
                    kind,
                }
            })
            .collect()
    }

    /// The stream every client is tested on (`apps/ui/fixtures/stream.jsonl`) is what the
    /// projector makes of the turn above: `OGNIVO_BLESS=1 cargo test -p ognivo-core
    /// protocol` writes it again.
    #[test]
    fn a_turn_becomes_the_stream_the_clients_are_tested_on() {
        let lines: String = stream(turn())
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/ui/fixtures/stream.jsonl");
        if std::env::var_os("OGNIVO_BLESS").is_some() {
            std::fs::write(&path, &lines).unwrap();
        }
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            have == lines,
            "{} is not what the projector makes: run OGNIVO_BLESS=1 cargo test -p ognivo-core protocol",
            path.display()
        );
        // every line reads back as the model's type
        for line in have.lines() {
            serde_json::from_str::<p::Event>(line).unwrap();
        }
    }

    #[test]
    fn items_are_numbered_and_each_comes_whole_at_its_end() {
        let events = stream(turn());
        let completed: Vec<&p::Item> = events
            .iter()
            .filter_map(|e| match &e.kind {
                p::EventKind::ItemCompleted { item } if e.thread == p::ThreadId(11) => Some(item),
                _ => None,
            })
            .collect();
        let ids: Vec<u32> = completed.iter().map(|i| i.id.0).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "each item completes once");
        // the reasoning streamed in two pieces is one item of the first generation
        let ItemBody::Reasoning { text } = &completed[1].body else {
            panic!("{:?}", completed[1])
        };
        assert_eq!(
            (text.as_str(), completed[1].generation),
            ("The greeting is in main.rs.", Some(1))
        );
        let call = |id: &str| {
            completed
                .iter()
                .find_map(|i| match &i.body {
                    ItemBody::ToolCall {
                        call_id,
                        result: Some(r),
                        changes,
                        ..
                    } if call_id == id => Some((r.clone(), changes.clone(), i.generation)),
                    _ => None,
                })
                .unwrap()
        };
        let (read, _, generation) = call("c1");
        assert_eq!(
            (read.outcome, read.exit_code, generation),
            (p::Outcome::Ok, None, Some(1))
        );
        let (_, changes, _) = call("c2");
        assert_eq!(changes[0].path, "main.rs");
        let (shell, _, _) = call("c4");
        assert_eq!(shell.exit_code, Some(0));
        // the generation whose call did not parse is void, with that call
        assert!(completed.iter().any(|i| matches!(&i.body,
            ItemBody::Void { why: p::VoidReason::Malformed, calls } if calls[0].name == "shell")));
        // the question, then its answer
        assert!(completed.iter().any(|i| matches!(
            &i.body,
            ItemBody::Approval {
                allowed: Some(true),
                ..
            }
        )));
        // the subagent works in a thread of its own, which names its item here
        let sub = events
            .iter()
            .find_map(|e| match &e.kind {
                p::EventKind::ThreadStarted { info } if e.thread == p::ThreadId(12) => {
                    Some(info.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (sub.kind.as_str(), sub.level.as_deref()),
            ("subagent", Some("medium"))
        );
        let parent = sub.parent.unwrap();
        assert!(completed.iter().any(|i| i.id == parent.item
            && matches!(
                &i.body,
                ItemBody::Subagent {
                    end: Some(p::TurnEnd::Answer),
                    thread: Some(p::ThreadId(12)),
                    ..
                }
            )));
        let ends: Vec<p::ThreadId> = events
            .iter()
            .filter(|e| matches!(e.kind, p::EventKind::TurnCompleted { .. }))
            .map(|e| e.thread)
            .collect();
        assert_eq!(ends, [p::ThreadId(12), p::ThreadId(11)]);
        // the summary's generation is no item of its own
        let voids = completed
            .iter()
            .filter(|i| matches!(i.body, ItemBody::Void { .. }))
            .count();
        assert_eq!(voids, 1);
    }

    #[test]
    fn a_turn_stopped_from_outside_completes_what_is_open() {
        let mut projector = Projector::new(p::ThreadId(1), info());
        projector.event(AgentEvent::TurnStarted { user: "go".into() });
        projector.event(AgentEvent::Reasoning {
            text: "thinking".into(),
        });
        let out = projector.interrupted();
        assert!(matches!(out[0].1, p::EventKind::ItemCompleted { .. }));
        assert!(matches!(
            out[1].1,
            p::EventKind::TurnCompleted {
                end: p::TurnEnd::Interrupted,
                ..
            }
        ));
        assert!(projector.interrupted().is_empty(), "once");
        // a running call, and a subagent with its own
        let events = turn();
        let at = events
            .iter()
            .position(|e| matches!(e, AgentEvent::Subagent { event, .. } if matches!(**event, AgentEvent::ToolStarted { .. })))
            .unwrap();
        let mut projector = Projector::new(p::ThreadId(11), info());
        for e in events.into_iter().take(at + 1) {
            projector.event(e);
        }
        let out = projector.interrupted();
        let completed: Vec<(p::ThreadId, &ItemBody)> = out
            .iter()
            .filter_map(|(t, k)| match k {
                p::EventKind::ItemCompleted { item } => Some((*t, &item.body)),
                _ => None,
            })
            .collect();
        // the subagent's grep, its item here, then the delegate call that waited for it
        assert!(
            matches!(completed[0], (p::ThreadId(12), ItemBody::ToolCall { result: Some(r), .. }) if r.outcome == p::Outcome::Skipped)
        );
        assert!(matches!(
            completed[1],
            (
                p::ThreadId(11),
                ItemBody::Subagent {
                    end: Some(p::TurnEnd::Interrupted),
                    ..
                }
            )
        ));
        assert!(
            matches!(completed[2], (p::ThreadId(11), ItemBody::ToolCall { call_id, .. }) if call_id == "c5")
        );
        let ends: Vec<p::ThreadId> = out
            .iter()
            .filter(|(_, k)| matches!(k, p::EventKind::TurnCompleted { .. }))
            .map(|(t, _)| *t)
            .collect();
        assert_eq!(ends, [p::ThreadId(12), p::ThreadId(11)]);
    }

    #[test]
    fn a_question_is_found_where_it_waits() {
        let ask = |agent: Option<&str>, allowed| AgentEvent::Approval {
            tool: "shell".into(),
            action: "cargo test".into(),
            reason: "runs project code".into(),
            unreviewed: Vec::new(),
            agent: agent.map(str::to_owned),
            allowed,
        };
        let mut projector = Projector::new(p::ThreadId(11), info());
        projector.event(AgentEvent::TurnStarted { user: "go".into() });
        assert_eq!(projector.asking(None, "shell", "cargo test"), None);
        projector.event(ask(None, None));
        assert_eq!(
            projector.asking(None, "shell", "cargo test"),
            Some((p::ThreadId(11), p::ItemId(1)))
        );
        assert_eq!(projector.asking(None, "shell", "cargo build"), None);
        projector.event(ask(None, Some(true)));
        assert_eq!(projector.asking(None, "shell", "cargo test"), None);
        // a subagent's question is in its own thread
        for (index, title) in [(0, "Suite a"), (1, "Suite b")] {
            projector.event(AgentEvent::SubagentStarted {
                index,
                title: title.into(),
                write: None,
                session: Some(20 + index as i64),
                brief: String::new(),
                level: None,
            });
            projector.event(sub(index, AgentEvent::TurnStarted { user: "go".into() }));
        }
        projector.event(sub(1, ask(Some("Suite b"), None)));
        assert_eq!(
            projector.asking(Some("Suite a"), "shell", "cargo test"),
            None
        );
        assert_eq!(
            projector.asking(Some("Suite b"), "shell", "cargo test"),
            Some((p::ThreadId(21), p::ItemId(1)))
        );
    }

    #[test]
    fn a_thread_picked_up_again_numbers_on() {
        let mut projector = Projector::resumed(p::ThreadId(11), info(), 3, 40, 12);
        let out = projector.event(AgentEvent::TurnStarted { user: "go".into() });
        assert!(matches!(
            out[0].1,
            p::EventKind::TurnStarted {
                turn: p::TurnId(4),
                ..
            }
        ));
        projector.event(AgentEvent::Text { text: "hi".into() });
        let out = projector.event(generation("step", "answer", 1, Vec::new()));
        let p::EventKind::ItemCompleted { item } = &out[0].1 else {
            panic!("{out:?}")
        };
        assert_eq!(
            (item.id, item.turn, item.generation),
            (p::ItemId(41), p::TurnId(4), Some(13))
        );
    }

    #[test]
    fn a_result_says_what_came_of_the_call() {
        let r = |t: &str| {
            let r = result_of(t, 0);
            (r.outcome, r.exit_code)
        };
        assert_eq!(
            r("[shell: exit 2, 1.0 s]\nerror"),
            (p::Outcome::Ok, Some(2))
        );
        assert_eq!(
            r("[shell: timed out after 5 s and was killed; exit 124]"),
            (p::Outcome::Ok, Some(124))
        );
        assert_eq!(r("[shell: killed by a signal]"), (p::Outcome::Ok, None));
        assert_eq!(
            r("[refused] cargo test needs the user's approval"),
            (p::Outcome::Refused, None)
        );
        assert_eq!(
            r("[error] not run: the turn ended"),
            (p::Outcome::Skipped, None)
        );
        assert_eq!(
            r("[ognivo] Not run: grep gave the same result twice"),
            (p::Outcome::Skipped, None)
        );
        assert_eq!(
            r("[error] main.rs: no such file"),
            (p::Outcome::Error, None)
        );
    }
}
