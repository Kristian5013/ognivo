//! The core's threads. Each is a session of the store with its agent, on a task of its own
//! that does what `ognivo chat` does between and during turns: a turn's events go through
//! the thread's projector to the hub, its questions to the clients, and between turns the
//! clients review what it changed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use ognivo_core::agent::{Agent, AgentEvent, ApprovalRequest, Approver, TurnEnd, Verdict};
use ognivo_core::approval::{Mode, ModeSwitch};
use ognivo_core::journal::Reverted;
use ognivo_core::protocol::Projector;
use ognivo_models::{AnswerNow, DriverError, Image};
use ognivo_protocol::{self as p, rpc};
use ognivo_store::SessionRow;
use ognivo_tools::files;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::hub::{Hub, lock};
use super::{Reply, RpcError, reply};
use crate::chat::declined;
use crate::session::{self, Connected, Failure, MAX_STEPS, SessionArgs};
use crate::view::{counts, given, not_read, printable};

/// What the model reads after a turn the user stopped, as in `chat`.
const STOPPED: &str = "The user stopped the last turn.";
const CORE_STOPPED: &str = "ognivo's core was stopped.";

#[derive(Debug)]
pub struct ThreadHandle {
    commands: mpsc::UnboundedSender<Command>,
    running: Arc<AtomicBool>,
    /// On the Native path, which can end the reasoning.
    answer_now: bool,
    /// The context it works with.
    context: rpc::Context,
    /// Its mode, which its agent and subagents read at each call.
    mode: ModeSwitch,
    /// Whether its model takes images.
    vision: bool,
    /// Its MCP servers as it started them.
    pub mcp: super::mcp::McpView,
}

impl ThreadHandle {
    /// Closes the thread, stopping its turn, and waits until it is closed.
    pub async fn shut(self) {
        let (reply, closed) = oneshot::channel();
        if self.commands.send(Command::Close(reply)).is_ok() {
            let _ = closed.await;
        }
    }
}

/// What a client asks of an open thread.
#[derive(Debug)]
pub enum Command {
    Turn(String, Vec<Image>, oneshot::Sender<Reply>),
    /// The conversation back to before this turn, and a turn with its message anew.
    Edit(u32, String, Vec<Image>, oneshot::Sender<Reply>),
    Interrupt(oneshot::Sender<Reply>),
    AnswerNow(oneshot::Sender<Reply>),
    Changes(oneshot::Sender<Reply>),
    Accept(Option<String>, oneshot::Sender<Reply>),
    Revert(String, bool, oneshot::Sender<Reply>),
    Clear(oneshot::Sender<Reply>),
    Close(oneshot::Sender<Reply>),
}

impl Command {
    /// The answer to a command a running turn leaves no room for.
    fn busy(self) {
        let reply = match self {
            Command::Turn(_, _, r)
            | Command::Edit(_, _, _, r)
            | Command::Interrupt(r)
            | Command::AnswerNow(r)
            | Command::Changes(r)
            | Command::Accept(_, r)
            | Command::Revert(_, _, r)
            | Command::Clear(r)
            | Command::Close(r) => r,
        };
        let _ = reply.send(Err(RpcError::new(
            rpc::error::BUSY,
            "a turn is running: stop it or wait for it",
        )));
    }
}

fn done() -> Reply {
    reply(rpc::Done {})
}

fn no_thread(thread: p::ThreadId) -> RpcError {
    RpcError::new(
        rpc::error::NO_THREAD,
        format!("thread {} is not open", thread.0),
    )
}

fn not_started(f: Failure) -> RpcError {
    RpcError::new(rpc::error::NOT_STARTED, f.message)
}

fn store_error(e: impl std::fmt::Display) -> RpcError {
    RpcError::new(-32603, format!("the session store: {e}"))
}

/// Sends a command to an open thread and waits for its answer.
pub async fn command(
    hub: &Hub,
    thread: p::ThreadId,
    make: impl FnOnce(oneshot::Sender<Reply>) -> Command,
) -> Reply {
    let (reply, answered) = oneshot::channel();
    let sent = lock(&hub.threads)
        .get(&thread.0)
        .is_some_and(|h| h.commands.send(make(reply)).is_ok());
    if !sent {
        return Err(no_thread(thread));
    }
    answered.await.unwrap_or_else(|_| Err(no_thread(thread)))
}

pub fn list(hub: &Hub, a: rpc::ListThreads) -> Reply {
    let project = a.project.map(PathBuf::from);
    let rows = lock(&hub.reader)
        .sessions(project.as_deref(), a.limit.unwrap_or(200))
        .map_err(store_error)?;
    let threads = lock(&hub.threads);
    let threads = rows
        .into_iter()
        .filter(|r| r.kind != "subagent")
        .map(|r| {
            let (server, model) = split_server(&r.server);
            let open = threads.get(&r.id);
            rpc::ThreadSummary {
                thread: p::ThreadId(r.id),
                kind: r.kind,
                project: r.root.display().to_string(),
                profile: r.profile,
                level: r.level,
                server,
                model,
                title: r.title,
                created: r.created,
                updated: r.updated,
                turns: r.turns,
                unreviewed: r.unreviewed_files,
                open: open.is_some(),
                running: open.is_some_and(|h| h.running.load(Ordering::SeqCst)),
                answer_now: open.is_some_and(|h| h.answer_now),
                context: open.map(|h| h.context),
                mode: open.map(|h| wire_mode(h.mode.get())),
                vision: open.map(|h| h.vision),
            }
        })
        .collect();
    reply(rpc::Threads { threads })
}

/// A session's server as the store keeps it: the URL, and the model behind a router.
fn split_server(server: &str) -> (String, Option<String>) {
    match server.split_once(' ') {
        Some((url, model)) => (url.to_owned(), Some(model.to_owned())),
        None => (server.to_owned(), None),
    }
}

/// Deletes a thread, with its subagents' threads: its conversations, its events and its
/// edit journal. One the core has open closes first; one with a running turn stays.
pub async fn delete(hub: &Hub, a: rpc::ThreadRef) -> Reply {
    let running = lock(&hub.threads)
        .get(&a.thread.0)
        .map(|h| h.running.load(Ordering::SeqCst));
    match running {
        Some(true) => {
            return Err(RpcError::new(
                rpc::error::BUSY,
                "a turn is running: stop it first",
            ));
        }
        Some(false) => {
            command(hub, a.thread, Command::Close).await?;
        }
        None => {}
    }
    let mut store = lock(&hub.reader);
    let row = store.session(a.thread.0).map_err(store_error)?;
    match row {
        // a session without turns is gone with its close
        None => return done(),
        Some(r) if r.kind == "subagent" => {
            return Err(RpcError::new(
                rpc::error::NO_THREAD,
                format!(
                    "thread {} is a subagent's: delete its session's",
                    a.thread.0
                ),
            ));
        }
        Some(_) => {}
    }
    let children: Vec<i64> = store
        .events(a.thread.0)
        .map_err(store_error)?
        .iter()
        .filter(|e| e.kind == "subagent_session")
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok()?["session"].as_i64())
        .collect();
    for id in children.into_iter().chain([a.thread.0]) {
        store.delete_session(id).map_err(store_error)?;
    }
    done()
}

pub fn read(hub: &Hub, a: rpc::ReadThread) -> Reply {
    let after = a.after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let rows = lock(&hub.reader)
        .wire(a.thread.0, after)
        .map_err(store_error)?;
    let events: Vec<Value> = rows
        .iter()
        .filter_map(|r| serde_json::from_str(r).ok())
        .collect();
    Ok(json!({ "events": events }))
}

pub fn trajectory(hub: &Hub, a: rpc::ThreadRef) -> Reply {
    let trajectory = ognivo_core::trace::atif(&lock(&hub.reader), a.thread.0)
        .map_err(|e| RpcError::new(rpc::error::NO_THREAD, e.to_string()))?;
    reply(rpc::Trajectory { trajectory })
}

/// The flags of `chat` for a thread of the core.
fn args(hub: &Hub, server: &rpc::Server, cwd: Option<PathBuf>) -> SessionArgs {
    SessionArgs {
        url: server.url.clone(),
        api_key: server.api_key.clone(),
        profile: None,
        cwd,
        max_steps: MAX_STEPS,
        seed: None,
        level: None,
        context: None,
        slot: None,
        model: server.model.clone(),
        data_dir: Some(hub.data_dir.clone()),
        native: false,
        priors: false,
        subagents: false,
        mcp_tools: ognivo_mcp::Listing::Auto,
    }
}

pub async fn start(hub: &Arc<Hub>, a: rpc::StartThread) -> Reply {
    let mut args = args(hub, &a.server, Some(PathBuf::from(&a.project)));
    (args.profile, args.level, args.context) = (a.profile, a.level, a.context);
    (args.native, args.priors, args.subagents) = (a.native, a.priors, a.subagents);
    let root = args.root().map_err(not_started)?;
    let connected = args
        .connect(root.clone(), None)
        .await
        .map_err(not_started)?;
    open(
        hub,
        &args.url,
        connected,
        root,
        None,
        a.mode.unwrap_or_default(),
    )
    .await
}

pub async fn resume(hub: &Arc<Hub>, a: rpc::ResumeThread) -> Reply {
    if lock(&hub.threads).contains_key(&a.thread.0) {
        return Err(RpcError::new(
            rpc::error::BUSY,
            format!("thread {} is open already", a.thread.0),
        ));
    }
    let row = lock(&hub.reader).session(a.thread.0).map_err(store_error)?;
    let Some(row) = row.filter(|r| r.kind != "subagent") else {
        return Err(RpcError::new(
            rpc::error::NO_THREAD,
            format!("there is no thread {}", a.thread.0),
        ));
    };
    let server = a.server.unwrap_or_else(|| {
        let (url, model) = split_server(&row.server);
        rpc::Server {
            url,
            api_key: None,
            model,
        }
    });
    let mut args = args(hub, &server, None);
    (args.native, args.priors, args.subagents) = (a.native, a.priors, a.subagents);
    args.context = a.context;
    let root = args.root_of(&row).map_err(not_started)?;
    let connected = args
        .connect(root.clone(), Some(&row))
        .await
        .map_err(not_started)?;
    open(
        hub,
        &args.url,
        connected,
        root,
        Some(row),
        a.mode.unwrap_or_default(),
    )
    .await
}

/// `thread/mode`: the open thread's mode from its agent's next call on, and an event that
/// tells every client.
pub fn set_mode(hub: &Hub, a: rpc::SetMode) -> Reply {
    let set = lock(&hub.threads)
        .get(&a.thread.0)
        .map(|h| h.mode.set(core_mode(a.mode)))
        .is_some();
    if !set {
        return Err(no_thread(a.thread));
    }
    hub.emit(
        a.thread,
        vec![(a.thread, p::EventKind::ModeChanged { mode: a.mode })],
    );
    done()
}

fn core_mode(mode: p::Mode) -> Mode {
    match mode {
        p::Mode::Manual => Mode::Manual,
        p::Mode::EditAutomatically => Mode::EditAutomatically,
        p::Mode::Plan => Mode::Plan,
        p::Mode::Auto => Mode::Auto,
    }
}

fn wire_mode(mode: Mode) -> p::Mode {
    match mode {
        Mode::Manual => p::Mode::Manual,
        Mode::EditAutomatically => p::Mode::EditAutomatically,
        Mode::Plan => p::Mode::Plan,
        Mode::Auto => p::Mode::Auto,
    }
}

/// The thread of a connected session, new or stored: its agent on a task of its own.
async fn open(
    hub: &Arc<Hub>,
    url: &str,
    mut s: Connected,
    root: PathBuf,
    stored: Option<SessionRow>,
    mode: p::Mode,
) -> Reply {
    let mut store = session::open_store(&hub.data_dir).map_err(not_started)?;
    let mut notes = Vec::new();
    if let Some(pending) = s.pending.take() {
        let key = pending.key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let asked = rpc::TrustAsked {
                    project: root.display().to_string(),
                    rules: pending
                        .rules
                        .iter()
                        .map(|r| rpc::Rule {
                            text: r.text.clone(),
                            source: r.source.clone(),
                            any: matches!(r.text.as_str(), "Bash" | "Bash(*)"),
                        })
                        .collect(),
                };
                let answer = hub
                    .ask(rpc::RULES_TRUST, json!(asked))
                    .await
                    .and_then(|v| serde_json::from_value::<rpc::TrustAnswer>(v).ok());
                let yes = answer.is_some_and(|a| a.trust);
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    notes.push(format!("the answer about the rules is not kept: {e}"));
                }
                yes
            }
            Err(e) => {
                notes.push(format!("the session store: {e}"));
                false
            }
        };
        if trusted {
            s.opts.rules.trust(pending);
        } else {
            notes.push(
                "the project's allow rules are not used: you did not trust them (they are asked about again when they change)"
                    .into(),
            );
        }
    }
    if !s.mcp.pending.is_empty() {
        let key = s.mcp_key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let asked = rpc::McpTrustAsked {
                    project: root.display().to_string(),
                    servers: s
                        .mcp
                        .pending
                        .iter()
                        .map(|m| rpc::McpServer {
                            name: m.name.clone(),
                            command: m.shown(),
                        })
                        .collect(),
                };
                let answer = hub
                    .ask(rpc::MCP_TRUST, json!(asked))
                    .await
                    .and_then(|v| serde_json::from_value::<rpc::TrustAnswer>(v).ok());
                let yes = answer.is_some_and(|a| a.trust);
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    notes.push(format!("the answer about the MCP servers is not kept: {e}"));
                }
                yes
            }
            Err(e) => {
                notes.push(format!("the session store: {e}"));
                false
            }
        };
        if trusted {
            s.mcp.trust();
        } else {
            notes.push(
                "the project's MCP servers do not start: you did not trust them (they are asked about again when .mcp.json changes)"
                    .into(),
            );
        }
    }
    if !s.hooks.pending.is_empty() {
        let key = s.hooks_key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let asked = rpc::HooksTrustAsked {
                    project: root.display().to_string(),
                    hooks: s
                        .hooks
                        .pending
                        .iter()
                        .map(|h| rpc::HookShown {
                            hook: h.shown(),
                            source: h.source.clone(),
                        })
                        .collect(),
                };
                let answer = hub
                    .ask(rpc::HOOKS_TRUST, json!(asked))
                    .await
                    .and_then(|v| serde_json::from_value::<rpc::TrustAnswer>(v).ok());
                let yes = answer.is_some_and(|a| a.trust);
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    notes.push(format!("the answer about the hooks is not kept: {e}"));
                }
                yes
            }
            Err(e) => {
                notes.push(format!("the session store: {e}"));
                false
            }
        };
        if trusted {
            s.hooks.trust();
        } else {
            notes.push(
                "the project's hooks do not run: you did not trust them (they are asked about again when they change)"
                    .into(),
            );
        }
    }
    notes.extend(s.hooks.problems.iter().map(|p| format!("hook {p}")));
    notes.extend(s.skill_problems.iter().map(|p| format!("skill {p}")));
    let mut started: Vec<String> = s
        .start_mcp()
        .await
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    // a remote server that wants a sign-in: in the browser now, if the user says so
    let mut signed = false;
    for (server, challenge) in s.mcp_wanting_sign_in() {
        let asked = rpc::McpSignInAsked {
            project: root.display().to_string(),
            server: rpc::McpServer {
                name: server.name.clone(),
                command: server.shown(),
            },
        };
        let yes = hub
            .ask(rpc::MCP_SIGN_IN, json!(asked))
            .await
            .and_then(|v| serde_json::from_value::<rpc::SignInAnswer>(v).ok())
            .is_some_and(|a| a.sign_in);
        if !yes {
            continue;
        }
        match s
            .sign_in_mcp(&server, &challenge, crate::session::open_in_browser)
            .await
        {
            Ok(()) => signed = true,
            Err(why) => notes.push(format!("mcp {}: {why}", server.name)),
        }
    }
    if signed {
        started = s
            .start_mcp()
            .await
            .into_iter()
            .map(|(line, _)| line)
            .collect();
    }
    notes.extend(started);
    let mcp_view = super::mcp::McpView {
        gateway: s.opts.mcp.clone(),
        servers: s.mcp.servers.clone(),
        untrusted: s.mcp.pending.clone(),
        problems: s.mcp.problems.clone(),
        signed: s.mcp_signed().map(std::path::Path::to_path_buf),
    };
    let (context_note, context_warning) = s.context_notes();
    let context = rpc::Context {
        working: s.opts.context.working_max,
        compacts_at: s.compacts_at(),
        window: s.opts.n_ctx,
    };
    let answer_now = s.answer_now.take();
    let vision = s.opts.vision;
    s.opts.mode = ModeSwitch::new(core_mode(mode));
    let switch = s.opts.mode.clone();
    let rule_files = std::mem::take(&mut s.rule_files);
    let rule_problems = std::mem::take(&mut s.rule_problems);
    let info = p::ThreadInfo {
        kind: "chat".into(),
        project: root.display().to_string(),
        profile: s.profile.clone(),
        level: s.level.clone(),
        server: url.to_owned(),
        model: s.model.clone(),
        parent: None,
    };
    // the approver finds a question's item in the thread's projector, numbered once the
    // session has its id
    let shared = Arc::new(Mutex::new(Projector::new(p::ThreadId(0), info.clone())));
    let approver = Box::new(Remote {
        hub: hub.clone(),
        projector: shared.clone(),
        always: HashSet::new(),
    });
    let agent = match &stored {
        Some(row) => s.resumed_agent(row, store, approver),
        None => s.new_agent("chat", url, store, approver),
    };
    let mut agent = agent.map_err(not_started)?;
    let Some(id) = agent.session_id() else {
        return Err(RpcError::new(
            rpc::error::NOT_STARTED,
            "the session store failed",
        ));
    };
    let thread = p::ThreadId(id);
    let mut conversation = Vec::new();
    *lock(&shared) = match &stored {
        Some(row) => {
            let n = numbers(hub, id, row.turns);
            conversation = n.conversation;
            Projector::resumed(thread, info.clone(), n.turns, n.items, n.generations)
        }
        None => Projector::new(thread, info.clone()),
    };
    match &stored {
        Some(row) => {
            notes.push(format!(
                "resumed after {} turn{}",
                row.turns,
                if row.turns == 1 { "" } else { "s" }
            ));
            if agent.inherited_changes() {
                notes.push("the project may hold changes ognivo did not make: builds and tests ask until you accept them".into());
            }
        }
        None if agent.unreviewed_changes() => notes
            .push("not a clean git tree: builds and tests ask until you accept the changes".into()),
        None => {}
    }
    if let Some(line) = unreviewed(&mut agent) {
        notes.push(line);
    }
    if !rule_files.is_empty() {
        notes.push(format!("rules {}", printable(&rule_files.join(", "))));
    }
    for problem in &rule_problems {
        notes.push(format!("not a rule: {}", printable(problem)));
    }
    notes.push(context_note);
    notes.extend(context_warning);
    let first = instructions(&agent);
    if let Some(AgentEvent::Instructions { files, skipped }) = &first {
        if !files.is_empty() {
            notes.push(format!("instructions {}", given(files)));
        }
        notes.extend(skipped.iter().map(not_read));
    }
    // the stream says the mode from the thread's start on: its last word is the mode now
    let mut start = lock(&shared).started();
    start.extend(lock(&shared).mode_changed(mode));
    hub.emit(thread, start);
    let (commands, received) = mpsc::unbounded_channel();
    let running = Arc::new(AtomicBool::new(false));
    let can_answer_now = answer_now.is_some();
    lock(&hub.threads).insert(
        id,
        ThreadHandle {
            commands,
            running: running.clone(),
            answer_now: can_answer_now,
            context,
            mode: switch,
            vision,
            mcp: mcp_view,
        },
    );
    let actor = Actor {
        hub: hub.clone(),
        agent,
        projector: shared,
        thread,
        root,
        first,
        answer_now,
        running,
        conversation,
        vision,
    };
    tokio::spawn(actor.run(received));
    reply(rpc::Opened {
        thread,
        info,
        notes,
        answer_now: can_answer_now,
        context,
        mode,
        vision,
    })
}

/// Where a thread stands in the kept stream.
struct Numbers {
    /// Its last turn, item and generation.
    turns: u32,
    items: u32,
    generations: u32,
    /// The turns that have not been taken back, oldest first.
    conversation: Vec<u32>,
}

/// Where a thread's numbers stand in the kept stream. A thread from before the core kept
/// its events, or with turns in `chat` since, goes on from the turns its session counted.
fn numbers(hub: &Hub, id: i64, turns: u32) -> Numbers {
    let rows = lock(&hub.reader).wire(id, 0).unwrap_or_default();
    let (mut turn, mut item, mut generation) = (0, 0, 0);
    let mut conversation = Vec::new();
    for row in rows {
        let Ok(e) = serde_json::from_str::<p::Event>(&row) else {
            continue;
        };
        if e.thread.0 != id {
            continue;
        }
        match e.kind {
            p::EventKind::TurnStarted { turn: t, .. } => {
                turn = turn.max(t.0);
                conversation.push(t.0);
            }
            p::EventKind::ThreadRewound { turn: t } => conversation.retain(|&c| c < t.0),
            p::EventKind::ItemStarted { item: i } => item = item.max(i.id.0),
            p::EventKind::Generation { generation: g } => generation = generation.max(g.index),
            _ => {}
        }
    }
    // turns in `chat` since
    conversation.extend(turn + 1..=turns);
    Numbers {
        turns: turn.max(turns),
        items: item,
        generations: generation,
        conversation,
    }
}

/// The conversation's instruction files, for the first item of its next turn.
fn instructions(agent: &Agent) -> Option<AgentEvent> {
    let load = agent.instructions();
    (!load.files.is_empty() || !load.skipped.is_empty()).then(|| AgentEvent::Instructions {
        files: load.files.clone(),
        skipped: load.skipped.clone(),
    })
}

/// What waits for review, as the line under a turn of `chat` says it.
fn unreviewed(agent: &mut Agent) -> Option<String> {
    let mut what: Vec<String> = agent
        .changes()
        .iter()
        .map(|c| format!("{} ({})", c.shown, counts(c.created, &c.hunks)))
        .collect();
    match agent.side_effects().len() {
        0 => {}
        1 => what.push("1 command with side effects".to_owned()),
        n => what.push(format!("{n} commands with side effects")),
    }
    (!what.is_empty()).then(|| printable(&format!("unreviewed: {}", what.join(", "))))
}

/// A thread's agent and what goes with it, on the thread's task.
struct Actor {
    hub: Arc<Hub>,
    agent: Agent,
    projector: Arc<Mutex<Projector>>,
    thread: p::ThreadId,
    root: PathBuf,
    /// The conversation's instruction files, the first item of its next turn.
    first: Option<AgentEvent>,
    answer_now: Option<AnswerNow>,
    running: Arc<AtomicBool>,
    /// The thread's turns that have not been taken back, oldest first: the last of them are
    /// those of the agent's conversation, which a `/clear` or a compaction starts anew.
    conversation: Vec<u32>,
    /// Whether its model takes images.
    vision: bool,
}

/// How a turn came out.
enum Ended {
    Done(Result<TurnEnd, DriverError>),
    /// From outside: the model reads why with the next message.
    Stopped(&'static str),
}

impl Actor {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let closed = loop {
            let Some(command) = commands.recv().await else {
                break None;
            };
            let answer = match command {
                Command::Turn(text, images, reply) => {
                    if let Err(e) = self.sees(&images) {
                        let _ = reply.send(Err(e));
                        continue;
                    }
                    let _ = reply.send(done());
                    match self.turn(&text, images, &mut commands).await {
                        Some(close) => break Some(close),
                        None => continue,
                    }
                }
                Command::Edit(turn, text, images, reply) => {
                    if let Err(e) = self.sees(&images).and_then(|()| self.rewind(turn)) {
                        let _ = reply.send(Err(e));
                        continue;
                    }
                    let _ = reply.send(done());
                    match self.turn(&text, images, &mut commands).await {
                        Some(close) => break Some(close),
                        None => continue,
                    }
                }
                // no turn to stop
                Command::Interrupt(reply) | Command::AnswerNow(reply) => (reply, done()),
                Command::Changes(reply) => (reply, self.changes()),
                Command::Accept(file, reply) => (reply, self.accept(file)),
                Command::Revert(file, force, reply) => (reply, self.revert(&file, force)),
                Command::Clear(reply) => {
                    self.agent.clear();
                    self.first = instructions(&self.agent);
                    (reply, done())
                }
                Command::Close(reply) => break Some(reply),
            };
            let _ = answer.0.send(answer.1);
        };
        lock(&self.hub.threads).remove(&self.thread.0);
        self.agent.end_session("other").await;
        self.agent.close();
        if let Some(reply) = closed {
            let _ = reply.send(done());
        }
    }

    /// Images go to a model that takes them; for one that does not, the turn does not start.
    fn sees(&self, images: &[Image]) -> Result<(), RpcError> {
        if images.is_empty() || self.vision {
            return Ok(());
        }
        Err(RpcError::new(
            rpc::error::UNSUPPORTED,
            "the model of this thread reads no images: its llama-server runs without a projector (--mmproj)",
        ))
    }

    /// Runs a turn to its end, or until a client stops it; returns the answer of a close
    /// that came meanwhile.
    async fn turn(
        &mut self,
        text: &str,
        images: Vec<Image>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> Option<oneshot::Sender<Reply>> {
        self.running.store(true, Ordering::SeqCst);
        self.hub.running(1);
        if let Some(a) = &self.answer_now {
            a.clear();
        }
        self.conversation.push(lock(&self.projector).turns() + 1);
        let (hub, projector, thread) = (self.hub.clone(), self.projector.clone(), self.thread);
        let mut first = self.first.take();
        let mut closing = None;
        let ended = {
            let mut on = |e: AgentEvent| {
                let started = matches!(e, AgentEvent::TurnStarted { .. });
                let mut projector = lock(&projector);
                let mut out = projector.event(e);
                if started && let Some(e) = first.take() {
                    out.extend(projector.event(e));
                }
                drop(projector);
                hub.emit(thread, out);
            };
            let turn = self.agent.turn_with(text, images, &mut on);
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    end = &mut turn => break Ended::Done(end),
                    command = commands.recv() => match command {
                        Some(Command::Interrupt(reply)) => {
                            let _ = reply.send(done());
                            break Ended::Stopped(STOPPED);
                        }
                        Some(Command::AnswerNow(reply)) => {
                            let answer = match &self.answer_now {
                                Some(a) => {
                                    a.press();
                                    done()
                                }
                                None => Err(RpcError::new(
                                    rpc::error::UNSUPPORTED,
                                    "only the Native path ends the model's reasoning",
                                )),
                            };
                            let _ = reply.send(answer);
                        }
                        Some(Command::Close(reply)) => {
                            closing = Some(reply);
                            break Ended::Stopped(CORE_STOPPED);
                        }
                        Some(other) => other.busy(),
                        None => break Ended::Stopped(CORE_STOPPED),
                    },
                }
            }
        };
        match ended {
            Ended::Done(Ok(_)) => {}
            Ended::Done(Err(e)) => self
                .agent
                .abandon_turn(&format!("The last turn ended on a server error: {e}")),
            Ended::Stopped(why) => {
                self.agent.abandon_turn(why);
                let out = lock(&self.projector).interrupted();
                self.hub.emit(self.thread, out);
            }
        }
        if let Some(e) = first {
            self.first = Some(e);
        }
        if let Some(e) = self.agent.store_failure() {
            eprintln!(
                "ognivo core: thread {}: the session store failed: {e}; the session is no longer saved",
                self.thread.0
            );
        }
        self.running.store(false, Ordering::SeqCst);
        self.hub.running(-1);
        closing
    }

    /// The conversation back to before `turn`: the agent's history, and an event that says
    /// so. Each turn of the conversation has one message of the user's: the last of them
    /// are those of the turns not taken back.
    fn rewind(&mut self, turn: u32) -> Result<(), RpcError> {
        let starts = self.agent.turn_starts();
        let turns = &self.conversation[self.conversation.len().saturating_sub(starts.len())..];
        let at = turns
            .iter()
            .position(|&t| t == turn)
            .map(|i| starts[starts.len() - turns.len() + i]);
        let Some(at) = at else {
            return Err(RpcError::new(
                rpc::error::NO_TURN,
                format!(
                    "turn {turn} is not in the conversation any more: a /clear or a compaction came after it"
                ),
            ));
        };
        self.agent.rewind(at);
        self.conversation.retain(|&t| t < turn);
        let out = lock(&self.projector).rewound(turn);
        self.hub.emit(self.thread, out);
        Ok(())
    }

    fn changes(&mut self) -> Reply {
        let files = self
            .agent
            .changes()
            .into_iter()
            .map(|c| rpc::ChangedFile {
                path: c.shown,
                created: c.created,
                deleted: c.deleted,
                drifted: c.drifted,
                hunks: c
                    .hunks
                    .into_iter()
                    .map(|h| p::Hunk {
                        old_start: h.old_start as u32,
                        new_start: h.new_start as u32,
                        removed: h.removed,
                        added: h.added,
                    })
                    .collect(),
            })
            .collect();
        reply(rpc::Changes {
            files,
            commands: self.agent.side_effects().to_vec(),
            inherited: self.agent.inherited_changes(),
        })
    }

    /// As `/accept` in `chat`: one file, or everything.
    fn accept(&mut self, file: Option<String>) -> Reply {
        let path = file.as_deref().map(|f| files::resolve(&self.root, f));
        let commands = self.agent.side_effects().len();
        let inherited = self.agent.inherited_changes();
        let mut accepted = self
            .agent
            .accept(path.as_deref())
            .map_err(|e| RpcError::new(rpc::error::JOURNAL, e.to_string()))?;
        if file.is_none() {
            match commands {
                0 => {}
                1 => accepted.push("1 command with side effects".to_owned()),
                n => accepted.push(format!("{n} commands with side effects")),
            }
            if inherited {
                accepted.push("what the project had before this session".to_owned());
            }
        }
        reply(rpc::Accepted {
            accepted,
            unreviewed: self.agent.unreviewed_changes(),
        })
    }

    /// As `/revert` in `chat`, for one file; a file something else changed too only with
    /// `force`.
    fn revert(&mut self, file: &str, force: bool) -> Reply {
        let path = files::resolve(&self.root, file);
        let how = self
            .agent
            .revert(&path, force)
            .map_err(|e| RpcError::new(rpc::error::JOURNAL, e.to_string()))?;
        reply(rpc::Reverted {
            removed: how == Reverted::Removed,
        })
    }
}

/// Asks the clients, the subagents' questions too; `a` allows exactly that action again for
/// the rest of the session, as in `chat`.
struct Remote {
    hub: Arc<Hub>,
    projector: Arc<Mutex<Projector>>,
    always: HashSet<(String, String)>,
}

impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remote").finish_non_exhaustive()
    }
}

impl Approver for Remote {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            let key = (req.tool.clone(), req.action.clone());
            if self.always.contains(&key) {
                return Verdict::Allow;
            }
            let (thread, item) = {
                let projector = lock(&self.projector);
                let at = projector.asking(req.agent.as_deref(), &req.tool, &req.action);
                (projector.thread(), at)
            };
            let asked = rpc::ApprovalAsked {
                thread,
                item: item.map(|(thread, item)| rpc::ItemRef { thread, item }),
                tool: req.tool.clone(),
                action: req.action.clone(),
                reason: req.reason.clone(),
                unreviewed: req.unreviewed.clone(),
                agent: req.agent.clone(),
            };
            let answer = self
                .hub
                .ask(rpc::APPROVAL_ASK, json!(asked))
                .await
                .and_then(|v| serde_json::from_value::<rpc::ApprovalAnswer>(v).ok());
            match answer {
                Some(rpc::ApprovalAnswer {
                    decision: rpc::Decision::Allow,
                    ..
                }) => Verdict::Allow,
                Some(rpc::ApprovalAnswer {
                    decision: rpc::Decision::AllowSession,
                    ..
                }) => {
                    self.always.insert(key);
                    Verdict::Allow
                }
                Some(rpc::ApprovalAnswer { instead, .. }) => {
                    Verdict::Refuse(declined(&req.action, instead.as_deref()))
                }
                None => Verdict::Refuse(declined(&req.action, None)),
            }
        })
    }

    fn can_ask(&self) -> bool {
        true
    }
}
