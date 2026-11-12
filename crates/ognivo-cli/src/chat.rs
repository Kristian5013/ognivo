//! `ognivo chat`: the line-mode REPL of docs/architecture.md §10. A turn streams to the
//! terminal (`view.rs` shows it), actions that need the user ask y/n/a, and ^C stops a turn
//! without ending the session. Between turns `/changes`, `/accept` and `/revert` review
//! what the agent changed (the edit journal, §8). The session is kept in the store as it
//! goes: `--resume ID` or `--continue` picks it up again, edit journal included. There is no
//! compaction yet, so `/clear` starts over when the context fills up.
//!
//! Exit codes: 0 `/exit` or ^D; 2 bad usage; 3 the server failed at the start; 143 SIGTERM.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::Args;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use ognivo_core::agent::{Agent, AgentEvent, ApprovalRequest, Approver, TurnEnd, Verdict};
use ognivo_core::approval::Mode;
use ognivo_core::instructions::Load;
use ognivo_core::journal::{FileChange, JournalError, Reverted};
use ognivo_core::rules::Pending;
use ognivo_core::verify::{self, Verification};
use ognivo_models::DriverError;
use ognivo_models::image::{self, Image};
use ognivo_tools::files;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use tokio::sync::{mpsc, oneshot};

use ognivo_store::{SessionRow, Store};

use crate::session::{Failure, NO_VISION, SessionArgs, image_file};
use crate::view::{self, Paint, Screen, Style, View, duration, kilo, printable};

#[derive(Debug, Args)]
pub struct ChatArgs {
    #[command(flatten)]
    session: SessionArgs,
    /// Go on with a stored session (see ognivo sessions)
    #[arg(long, value_name = "ID", conflicts_with = "go_on")]
    resume: Option<i64>,
    /// Go on with the last chat session of this project
    #[arg(long = "continue", id = "go_on")]
    go_on: bool,
    /// Stream the model's reasoning too, dimmed
    #[arg(long)]
    show_reasoning: bool,
}

const HELP: &str = "\
/help            this help
/changes [file]  what the agent changed and nobody reviewed yet; with a file, all its diff
/accept [file]   mark it reviewed; with nothing left, builds and tests run without asking
/revert [file]   put the files back as they were; commands with side effects stay done
/clear           start a new conversation; what the session changed stays unreviewed
/image [file]    an image for your next message, which the model sees before its text:
                 PNG, JPEG, GIF or BMP, for a model whose server runs --mmproj; without a
                 file, what goes with it; /image clear takes them back
/mode [mode]     how far the agent goes without asking: manual (every edit and command
                 asks), edit (edits apply at once, the default), plan (it reads and plans,
                 nothing changes) or auto (no questions but protected files and rules)
/exit            quit; so does ^D
A line that ends in \\ goes on in the next one; \"\"\" on a line of its own starts and ends a block.";

/// [`HELP`], and the keys that stop a turn and answer now: `^C` and `^\`, or on Windows
/// Ctrl+C and Ctrl+Break.
fn help() -> String {
    let (stop, answer) = ognivo_os::signal::KEYS;
    format!(
        "{HELP}\n{stop} stops the running turn, and the conversation goes on.\n{answer} ends the model's reasoning where it stands and asks for the answer (with --native)."
    )
}

/// Items named in a list of what is unreviewed; the rest are counted.
const SUMMARY_FILES: usize = 3;

fn fail(f: Failure) -> anyhow::Result<ExitCode> {
    eprintln!("ognivo chat: {}", f.message);
    Ok(ExitCode::from(f.code))
}

pub async fn chat(a: ChatArgs) -> anyhow::Result<ExitCode> {
    let store = match a.session.store() {
        Ok(store) => store,
        Err(f) => return fail(f),
    };
    let stored = match stored(&a, &store) {
        Ok(stored) => stored,
        Err(f) => return fail(f),
    };
    let root = match &stored {
        Some(row) => a.session.root_of(row),
        None => a.session.root(),
    };
    let root = match root {
        Ok(root) => root,
        Err(f) => return fail(f),
    };
    let mut s = match a.session.connect(root.clone(), stored.as_ref()).await {
        Ok(s) => s,
        Err(f) => return fail(f),
    };
    let mut store = store;
    let n_ctx = s.opts.n_ctx;
    let (context_note, context_warning) = s.context_notes();
    let screen = Screen::new(Style::detect());
    let terminal = ognivo_os::TerminalMode::save();
    let input = Input::spawn(s.opts.data_dir.join("chat_history"));
    let (stop, mut stopped) = mpsc::unbounded_channel();
    let approver = Ask {
        input: input.clone(),
        always: HashSet::new(),
        stop,
        screen: screen.clone(),
    };
    let max_steps = s.opts.max_steps;
    screen.line(
        &format!(
            "ognivo chat · {} · {}{}{}",
            s.profile,
            a.session.url,
            n_ctx.map(|n| format!(" · n_ctx {n}")).unwrap_or_default(),
            s.model
                .as_deref()
                .map(|m| format!(" · {m}"))
                .unwrap_or_default()
        ),
        Paint::Plain,
    );
    screen.line(&format!("· {context_note}"), Paint::Dim);
    if let Some(w) = &context_warning {
        screen.line(&format!("· {w}"), Paint::Yellow);
    }
    let answer_now = s.answer_now.take();
    let rule_files = std::mem::take(&mut s.rule_files);
    let rule_problems = std::mem::take(&mut s.rule_problems);
    let skill_problems = std::mem::take(&mut s.skill_problems);
    let skills_line = crate::session::skills_line(&s.opts.skills);
    let mut untrusted = false;
    if let Some(pending) = s.pending.take() {
        let key = pending.key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let yes = trust(&screen, &input, &pending).await;
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    let line = format!("· the answer is not kept: {e}");
                    screen.line(&line, Paint::Yellow);
                }
                yes
            }
            Err(e) => {
                screen.line(&format!("· the session store: {e}"), Paint::Yellow);
                false
            }
        };
        if trusted {
            s.opts.rules.trust(pending);
        } else {
            untrusted = true;
        }
    }
    // the project's MCP servers run its commands: they start once trusted
    let mut mcp_untrusted = false;
    if !s.mcp.pending.is_empty() {
        let key = s.mcp_key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let yes = trust_mcp(&screen, &input, &s.mcp.pending).await;
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    screen.line(&format!("· the answer is not kept: {e}"), Paint::Yellow);
                }
                yes
            }
            Err(e) => {
                screen.line(&format!("· the session store: {e}"), Paint::Yellow);
                false
            }
        };
        if trusted {
            s.mcp.trust();
        } else {
            mcp_untrusted = true;
        }
    }
    // the project's hooks run its commands too
    let mut hooks_untrusted = false;
    if !s.hooks.pending.is_empty() {
        let key = s.hooks_key();
        let trusted = match store.trusted(&root, &key) {
            Ok(Some(answer)) => answer,
            Ok(None) => {
                let yes = trust_hooks(&screen, &input, &s.hooks.pending).await;
                if let Err(e) = store.set_trusted(&root, &key, yes) {
                    screen.line(&format!("· the answer is not kept: {e}"), Paint::Yellow);
                }
                yes
            }
            Err(e) => {
                screen.line(&format!("· the session store: {e}"), Paint::Yellow);
                false
            }
        };
        if trusted {
            s.hooks.trust();
        } else {
            hooks_untrusted = true;
        }
    }
    let hook_files = s.hooks.files.clone();
    let hook_problems = s.hooks.problems.clone();
    let mut mcp_lines = s.start_mcp().await;
    // a remote server that wants a sign-in: now, in the browser, if the user says so
    let mut signed = false;
    for (server, challenge) in s.mcp_wanting_sign_in() {
        if !sign_in_now(&screen, &input, &server).await {
            continue;
        }
        let opened = |url: &str| {
            screen.line(&format!("  sign in on this page: {url}"), Paint::Plain);
            crate::session::open_in_browser(url);
        };
        match s.sign_in_mcp(&server, &challenge, opened).await {
            Ok(()) => {
                screen.line(
                    &format!("· signed in to {}", printable(&server.name)),
                    Paint::Plain,
                );
                signed = true;
            }
            Err(why) => screen.line(
                &format!("· {}: {}", printable(&server.name), printable(&why)),
                Paint::Yellow,
            ),
        }
    }
    if signed {
        mcp_lines = s.start_mcp().await;
    }
    let vision = s.opts.vision;
    let agent = match &stored {
        Some(row) => s.resumed_agent(row, store, Box::new(approver)),
        None => s.new_agent("chat", &a.session.url, store, Box::new(approver)),
    };
    let mut agent = match agent {
        Ok(agent) => agent,
        Err(f) => return fail(f),
    };
    let id = agent.session_id().unwrap_or_default();
    match &stored {
        Some(row) => {
            screen.line(
                &format!(
                    "project {} · session {id}, resumed after {} turn{}",
                    root.display(),
                    row.turns,
                    if row.turns == 1 { "" } else { "s" }
                ),
                Paint::Plain,
            );
            instructions(&screen, agent.instructions());
            if let Some(line) = last_message(&agent) {
                screen.line(&format!("· last message: {line}"), Paint::Dim);
            }
            unreviewed(&mut agent, &screen);
            if agent.inherited_changes() {
                screen.line(
                    "· the project may hold changes ognivo did not make: builds and tests ask until /accept",
                    Paint::Dim,
                );
            }
        }
        None => {
            screen.line(
                &format!(
                    "project {} · session {id}{}",
                    root.display(),
                    if agent.unreviewed_changes() {
                        " · not a clean git tree: builds and tests ask until /accept"
                    } else {
                        ""
                    }
                ),
                Paint::Plain,
            );
            instructions(&screen, agent.instructions());
        }
    }
    if !rule_files.is_empty() {
        let line = format!("rules {}", printable(&rule_files.join(", ")));
        screen.line(&line, Paint::Plain);
    }
    for p in &rule_problems {
        screen.line(&format!("· not a rule: {}", printable(p)), Paint::Yellow);
    }
    if let Some(line) = &skills_line {
        screen.line(&printable(line), Paint::Plain);
    }
    for p in &skill_problems {
        screen.line(&format!("· skill {}", printable(p)), Paint::Yellow);
    }
    if !hook_files.is_empty() {
        let line = format!("hooks {}", printable(&hook_files.join(", ")));
        screen.line(&line, Paint::Plain);
    }
    for p in &hook_problems {
        screen.line(&format!("· hook {}", printable(p)), Paint::Yellow);
    }
    if hooks_untrusted {
        screen.line(
            "· the project's hooks do not run: you did not trust them (they are asked about again when they change)",
            Paint::Dim,
        );
    }
    if untrusted {
        screen.line(
            "· the project's allow rules are not used: you did not trust them (they are asked about again when they change)",
            Paint::Dim,
        );
    }
    for (line, warn) in &mcp_lines {
        let paint = if *warn { Paint::Yellow } else { Paint::Plain };
        screen.line(&printable(line), paint);
    }
    if mcp_untrusted {
        screen.line(
            "· the project's MCP servers do not start: you did not trust them (they are asked about again when .mcp.json changes)",
            Paint::Dim,
        );
    }
    let (stop_key, answer_key) = ognivo_os::signal::KEYS;
    screen.line(
        &if answer_now.is_some() {
            format!("/help · {stop_key} stops a turn · {answer_key} answers now · ^D quits")
        } else {
            format!("/help · {stop_key} stops a turn · ^D quits")
        },
        Paint::Dim,
    );
    let mut int = ognivo_os::signal::interrupt()?;
    let mut term = ognivo_os::signal::terminate()?;
    let term_name = term.name();
    // ^\ in a line-mode terminal, Ctrl+Break on Windows: the reasoning ends, and the
    // answer comes (Native path)
    let mut quit = ognivo_os::signal::quit()?;
    // the images `/image` attached, which go with the next message
    let mut images: Vec<Image> = Vec::new();
    loop {
        if let Some(e) = agent.store_failure() {
            screen.line(
                &format!("· the session store failed: {e}; the session is no longer saved"),
                Paint::Yellow,
            );
        }
        let entry = tokio::select! {
            entry = read_entry(&input, &screen) => entry,
            _ = term.recv() => {
                agent.close();
                return Ok(terminated(terminal.as_ref(), term_name));
            }
        };
        let text = match entry {
            Entry::Message(text) => text,
            Entry::Help => {
                screen.line(&help(), Paint::Plain);
                for line in commands_help(&agent) {
                    screen.line(&printable(&line), Paint::Plain);
                }
                continue;
            }
            Entry::Clear => {
                agent.clear();
                screen.line("· new conversation", Paint::Dim);
                instructions(&screen, agent.instructions());
                continue;
            }
            Entry::Changes(file) => {
                changes(&mut agent, &screen, &root, file.as_deref());
                continue;
            }
            Entry::Accept(file) => {
                accept(&mut agent, &screen, &root, file.as_deref());
                continue;
            }
            Entry::Revert(file) => {
                revert(&mut agent, &screen, &input, &root, file.as_deref()).await;
                continue;
            }
            Entry::Mode(name) => {
                switch_mode(&agent, &screen, name.as_deref());
                continue;
            }
            Entry::Image(file) => {
                attach(&mut images, vision, &screen, file.as_deref());
                continue;
            }
            Entry::Exit => {
                agent.end_session("prompt_input_exit").await;
                if let Some(id) = agent.close() {
                    screen.line(
                        &format!("· session {id} is kept: ognivo chat --resume {id}"),
                        Paint::Dim,
                    );
                }
                return Ok(ExitCode::SUCCESS);
            }
        };
        // a ^C, a stop or a ^\ left from before must not end this turn or its reasoning
        while int.recv().now_or_never().is_some() {}
        while stopped.try_recv().is_ok() {}
        while quit.recv().now_or_never().is_some() {}
        if let Some(a) = &answer_now {
            a.clear();
        }
        let (steps, generated, started) = (
            agent.stats.steps,
            agent.stats.completion_tokens,
            Instant::now(),
        );
        let mut view = View::new(screen.clone(), root.clone(), a.show_reasoning);
        let outcome = {
            let mut on = |e: AgentEvent| view.show(e);
            let turn = agent.turn_with(&text, std::mem::take(&mut images), &mut on);
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    end = &mut turn => break Outcome::Ended(end),
                    _ = int.recv() => break Outcome::Stopped,
                    _ = stopped.recv() => break Outcome::Stopped,
                    _ = term.recv() => break Outcome::Terminated,
                    _ = quit.recv() => {
                        if let Some(a) = &answer_now {
                            a.press();
                        }
                    }
                }
            }
        };
        view.finish();
        let unanswered = match outcome {
            Outcome::Terminated => {
                agent.abandon_turn(&format!("ognivo was stopped by {term_name}."));
                agent.close();
                return Ok(terminated(terminal.as_ref(), term_name));
            }
            Outcome::Stopped => {
                agent.abandon_turn("The user stopped the last turn.");
                screen.line("· stopped", Paint::Dim);
                true
            }
            Outcome::Ended(Ok(TurnEnd::Answer(answer))) => {
                verification(&screen, &answer.verification);
                if answer.unsupported_claim {
                    screen.line(
                        "[ognivo] the answer above claims a check that did not happen",
                        Paint::Yellow,
                    );
                }
                false
            }
            Outcome::Ended(Ok(end)) => {
                screen.line(
                    &format!("[ognivo] stopped: {}", why(&end, max_steps)),
                    Paint::Yellow,
                );
                true
            }
            Outcome::Ended(Err(e)) => {
                agent.abandon_turn(&format!("The last turn ended on a server error: {e}"));
                screen.line(&format!("[ognivo] server error: {e}"), Paint::Red);
                true
            }
        };
        if unanswered {
            verification(&screen, &agent.verification());
        }
        unreviewed(&mut agent, &screen);
        let taken = agent.stats.steps - steps;
        screen.line(
            &format!(
                "({taken} step{} · {} · {} generated · context {}{})",
                if taken == 1 { "" } else { "s" },
                duration(started.elapsed()),
                kilo(agent.stats.completion_tokens - generated),
                kilo(agent.context_tokens()),
                n_ctx
                    .map(|n| format!(" of {}", kilo(n.into())))
                    .unwrap_or_default()
            ),
            Paint::Dim,
        );
    }
}

/// How a turn came out.
#[derive(Debug)]
enum Outcome {
    Ended(Result<TurnEnd, DriverError>),
    /// By ^C, here or at a question.
    Stopped,
    Terminated,
}

/// Whether the project's allow rules may count: they let commands run without a question.
async fn trust(screen: &Screen, input: &Input, pending: &Pending) -> bool {
    screen.line(
        "? the project's settings allow these without a question:",
        Paint::Bold,
    );
    for r in &pending.rules {
        let mut line = format!("    {} ({})", printable(&r.text), printable(&r.source));
        let any = matches!(r.text.as_str(), "Bash" | "Bash(*)");
        if any {
            line.push_str(": any command at all");
        }
        screen.line(&line, if any { Paint::Yellow } else { Paint::Plain });
    }
    matches!(
        input.read("  use them? [y]es [n]o > ").await,
        Read::Line(line) if answer(&line) == Some(Answer::Yes)
    )
}

/// Whether the project's MCP servers may start: each runs a command the project names.
async fn trust_mcp(screen: &Screen, input: &Input, servers: &[ognivo_mcp::ServerConfig]) -> bool {
    screen.line(
        "? the project's .mcp.json starts these MCP servers:",
        Paint::Bold,
    );
    for s in servers {
        let line = format!("    {}: {}", printable(&s.name), printable(&s.shown()));
        screen.line(&line, Paint::Plain);
    }
    matches!(
        input.read("  start them? [y]es [n]o > ").await,
        Read::Line(line) if answer(&line) == Some(Answer::Yes)
    )
}

/// Whether the project's hooks may run: they are its commands.
async fn trust_hooks(screen: &Screen, input: &Input, hooks: &[ognivo_core::hooks::Hook]) -> bool {
    screen.line(
        "? the project's settings run these commands on the agent's events:",
        Paint::Bold,
    );
    for h in hooks {
        let line = format!("    {} ({})", printable(&h.shown()), printable(&h.source));
        screen.line(&line, Paint::Plain);
    }
    matches!(
        input.read("  run them? [y]es [n]o > ").await,
        Read::Line(line) if answer(&line) == Some(Answer::Yes)
    )
}

/// Whether to sign in to a remote MCP server that wants it, now, before the session starts.
async fn sign_in_now(screen: &Screen, input: &Input, server: &ognivo_mcp::ServerConfig) -> bool {
    screen.line(
        &format!(
            "? the MCP server {} ({}) wants you to sign in",
            printable(&server.name),
            printable(&server.shown())
        ),
        Paint::Bold,
    );
    matches!(
        input.read("  sign in now, in the browser? [y]es [n]o > ").await,
        Read::Line(line) if answer(&line) == Some(Answer::Yes)
    )
}

/// The session `--resume` or `--continue` asks for.
fn stored(a: &ChatArgs, store: &Store) -> Result<Option<SessionRow>, Failure> {
    let row = if let Some(id) = a.resume {
        store.session(id).map_err(Failure::usage)?.ok_or_else(|| {
            Failure::usage(format!("there is no session {id}; see ognivo sessions"))
        })?
    } else if a.go_on {
        store
            .latest(&a.session.root()?, "chat")
            .map_err(Failure::usage)?
            .ok_or_else(|| Failure::usage("this project has no chat session to go on with"))?
    } else {
        return Ok(None);
    };
    Ok(Some(row))
}

/// The first line of the user's last message, for a resumed session.
fn last_message(agent: &Agent) -> Option<String> {
    agent.messages().iter().rev().find_map(|m| match m {
        ognivo_models::ChatMessage::User { text, .. } if !text.starts_with("[ognivo]") => {
            text.lines().next().map(printable)
        }
        _ => None,
    })
}

/// The instruction files of the system prompt, and those it could not take.
fn instructions(screen: &Screen, load: &Load) {
    if !load.files.is_empty() {
        let line = format!("instructions {}", view::given(&load.files));
        screen.line(&line, Paint::Plain);
    }
    for s in &load.skipped {
        screen.line(&view::not_read(s), Paint::Yellow);
    }
}

/// The line under a turn that says whether its changes were checked: green when a build
/// or test passed after the last change.
fn verification(screen: &Screen, v: &Verification) {
    if let Some(line) = verify::summary(v) {
        let paint = match v {
            Verification::Passed { .. } => Paint::Green,
            _ => Paint::Yellow,
        };
        screen.line(&format!("[ognivo] {line}"), paint);
    }
}

/// The line under a turn that lists what waits for review, if anything does.
fn unreviewed(agent: &mut Agent, screen: &Screen) {
    let files: Vec<String> = agent
        .changes()
        .iter()
        .map(|c| format!("{} ({})", c.shown, view::counts(c.created, &c.hunks)))
        .collect();
    let mut what = Vec::new();
    if !files.is_empty() {
        what.push(listed(&files));
    }
    match agent.side_effects().len() {
        0 => {}
        1 => what.push("1 command with side effects".to_owned()),
        n => what.push(format!("{n} commands with side effects")),
    }
    if !what.is_empty() {
        let line = format!(
            "· unreviewed: {} · /changes /accept /revert",
            what.join(", ")
        );
        screen.line(&printable(&line), Paint::Dim);
    }
}

/// `a, b, c and 2 more`.
fn listed(items: &[String]) -> String {
    let mut out = items[..items.len().min(SUMMARY_FILES)].join(", ");
    if items.len() > SUMMARY_FILES {
        out.push_str(&format!(" and {} more", items.len() - SUMMARY_FILES));
    }
    out
}

/// `/changes`: every unreviewed file with a short diff, or one file's whole diff.
fn changes(agent: &mut Agent, screen: &Screen, root: &Path, file: Option<&str>) {
    let all = agent.changes();
    let Some(file) = file else {
        if all.is_empty() && agent.side_effects().is_empty() && !agent.inherited_changes() {
            screen.line("· nothing unreviewed", Paint::Dim);
            return;
        }
        for c in &all {
            show_change(screen, c, false);
        }
        for command in agent.side_effects() {
            let line = format!(
                "· ran `{}`: its side effects are not in the journal",
                printable(command)
            );
            screen.line(&line, Paint::Yellow);
        }
        if agent.inherited_changes() {
            screen.line(
                "· and what the project had before this session (git status shows it)",
                Paint::Dim,
            );
        }
        screen.line(
            "· /accept or /revert all of it, or one file: /revert <file>",
            Paint::Dim,
        );
        return;
    };
    match find(&all, root, file) {
        Some(c) => show_change(screen, c, true),
        None => screen.line(
            &format!("· {file}: no unreviewed change by ognivo"),
            Paint::Dim,
        ),
    }
}

fn show_change(screen: &Screen, c: &FileChange, whole: bool) {
    for (line, paint) in view::change_lines(c, whole) {
        screen.line(&line, paint);
    }
}

/// The change of the file the user named, relative to the project root or absolute.
fn find<'a>(all: &'a [FileChange], root: &Path, file: &str) -> Option<&'a FileChange> {
    let path = files::resolve(root, file);
    all.iter().find(|c| c.path == path)
}

/// `/accept`: everything, or one file.
fn accept(agent: &mut Agent, screen: &Screen, root: &Path, file: Option<&str>) {
    let path = file.map(|f| files::resolve(root, f));
    let commands = agent.side_effects().len();
    let inherited = agent.inherited_changes();
    match agent.accept(path.as_deref()) {
        Ok(mut what) => {
            if file.is_none() {
                match commands {
                    0 => {}
                    1 => what.push("1 command with side effects".to_owned()),
                    n => what.push(format!("{n} commands with side effects")),
                }
                if inherited {
                    what.push("what the project had before this session".to_owned());
                }
            }
            if what.is_empty() {
                screen.line("· nothing to accept", Paint::Dim);
            } else {
                let line = format!("· accepted: {}", what.join(", "));
                screen.line(&printable(&line), Paint::Dim);
            }
            if !agent.unreviewed_changes() {
                screen.line(
                    "· nothing unreviewed: builds and tests run without asking",
                    Paint::Dim,
                );
            }
        }
        Err(JournalError::NotChanged(_)) => {
            let line = format!(
                "· {}: no unreviewed change by ognivo",
                file.unwrap_or_default()
            );
            screen.line(&line, Paint::Dim);
        }
        Err(e) => screen.line(&printable(&format!("· {e}")), Paint::Yellow),
    }
}

/// `/revert`: every unreviewed file, or one. A file that changed otherwise too is reverted
/// only after a yes; commands with side effects stay done.
async fn revert(
    agent: &mut Agent,
    screen: &Screen,
    input: &Input,
    root: &Path,
    file: Option<&str>,
) {
    let all = agent.changes();
    let targets: Vec<&FileChange> = match file {
        Some(f) => match find(&all, root, f) {
            Some(c) => vec![c],
            None => {
                screen.line(
                    &format!("· {f}: no unreviewed change by ognivo"),
                    Paint::Dim,
                );
                return;
            }
        },
        None => all.iter().collect(),
    };
    if targets.is_empty() {
        screen.line("· no file to revert", Paint::Dim);
    }
    for c in targets {
        let name = printable(&c.shown);
        if c.drifted && !confirmed(screen, input, &name).await {
            screen.line(&format!("· kept {name}"), Paint::Dim);
            continue;
        }
        match agent.revert(&c.path, c.drifted) {
            Ok(Reverted::Restored) => screen.line(&format!("· reverted {name}"), Paint::Dim),
            Ok(Reverted::Removed) => screen.line(
                &format!("· removed {name}: ognivo had created it"),
                Paint::Dim,
            ),
            Err(e) => screen.line(&printable(&format!("· {e}")), Paint::Yellow),
        }
    }
    if file.is_none() && !agent.side_effects().is_empty() {
        screen.line(
            "· commands with side effects are not undone; /accept once you have checked them",
            Paint::Yellow,
        );
    }
}

/// Asks before a revert that would lose what something else changed in the file.
async fn confirmed(screen: &Screen, input: &Input, name: &str) -> bool {
    let question = format!(
        "? {name} also changed by a command or outside ognivo since its edits; a revert loses that"
    );
    screen.line(&question, Paint::Bold);
    matches!(
        input.read("  revert it anyway? [y]es [n]o > ").await,
        Read::Line(line) if answer(&line) == Some(Answer::Yes)
    )
}

/// The user's commands and skills, for `/help`: `/name hint  what it does`.
fn commands_help(agent: &Agent) -> Vec<String> {
    let mut lines: Vec<String> = agent
        .skills()
        .all()
        .iter()
        .filter(|s| s.user_invocable)
        .map(|s| {
            let mut line = format!("  /{}", s.name);
            if let Some(hint) = &s.argument_hint {
                line.push(' ');
                line.push_str(hint);
            }
            let about: String = s.description.chars().take(80).collect();
            if !about.is_empty() {
                line.push_str("  ");
                line.push_str(&about);
            }
            line
        })
        .collect();
    // the MCP servers' prompts run the same way
    for p in agent.mcp().map(|g| g.prompts()).unwrap_or_default() {
        let mut line = format!("  /{}", p.name);
        for a in &p.arguments {
            line.push_str(&if a.required {
                format!(" <{}>", a.name)
            } else {
                format!(" [{}]", a.name)
            });
        }
        let about: String = p.description.chars().take(80).collect();
        if !about.is_empty() {
            line.push_str("  ");
            line.push_str(&about);
        }
        lines.push(line);
    }
    if !lines.is_empty() {
        lines.insert(
            0,
            "commands, skills and MCP prompts of the user and the project:".into(),
        );
    }
    lines
}

fn why(end: &TurnEnd, max_steps: u32) -> String {
    match end {
        TurnEnd::StepLimit(_) => format!("the step limit ({max_steps}) was reached"),
        TurnEnd::Stuck(why) => why.clone(),
        TurnEnd::TimeLimit => "the time limit ran out".to_owned(),
        TurnEnd::NotRun(why) => why.clone(),
        TurnEnd::Answer(_) => "answered".to_owned(),
    }
}

/// SIGTERM: the terminal gets back the mode it had, even if the line editor holds it.
fn terminated(terminal: Option<&ognivo_os::TerminalMode>, by: &str) -> ExitCode {
    if let Some(t) = terminal {
        t.restore();
    }
    println!("\n· stopped by {by}");
    ExitCode::from(143)
}

/// What the user typed at the prompt.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Message(String),
    Help,
    Clear,
    /// A file, if one was named.
    Changes(Option<String>),
    Accept(Option<String>),
    Revert(Option<String>),
    /// A mode, if one was named.
    Mode(Option<String>),
    /// An image file, if one was named.
    Image(Option<String>),
    Exit,
}

/// One entry: a message of one or more lines, or a command.
async fn read_entry(input: &Input, screen: &Screen) -> Entry {
    let mut composer = Composer::default();
    let mut interrupted = false;
    loop {
        let prompt = if composer.pending() { "… " } else { "> " };
        match input.read(prompt).await {
            Read::Line(line) => {
                interrupted = false;
                let Some(text) = composer.feed(&line) else {
                    continue;
                };
                if let Some(c) = command(&text) {
                    return c;
                }
                if !text.trim().is_empty() {
                    input.remember(&text);
                    return Entry::Message(text);
                }
            }
            Read::Interrupted if composer.pending() => composer = Composer::default(),
            Read::Interrupted if interrupted => return Entry::Exit,
            Read::Interrupted => {
                interrupted = true;
                let again = format!("({} again or ^D quits)", ognivo_os::signal::KEYS.0);
                screen.line(&again, Paint::Dim);
            }
            Read::Eof => return Entry::Exit,
        }
    }
}

/// A command is a whole entry of one line: a message may start with a path like
/// `/etc/hosts`.
fn command(text: &str) -> Option<Entry> {
    let text = text.trim();
    if text.contains('\n') {
        return None;
    }
    let (word, arg) = match text.split_once(char::is_whitespace) {
        Some((word, arg)) => (word, Some(arg.trim().to_owned())),
        None => (text, None),
    };
    match (word, arg) {
        ("/help" | "/?", None) => Some(Entry::Help),
        ("/clear" | "/new", None) => Some(Entry::Clear),
        ("/exit" | "/quit", None) => Some(Entry::Exit),
        ("/changes" | "/diff", arg) => Some(Entry::Changes(arg)),
        ("/accept", arg) => Some(Entry::Accept(arg)),
        ("/revert", arg) => Some(Entry::Revert(arg)),
        ("/mode", arg) => Some(Entry::Mode(arg)),
        ("/image", arg) => Some(Entry::Image(arg)),
        _ => None,
    }
}

/// `/image`: an image for the next message, what goes with it, or nothing again.
fn attach(images: &mut Vec<Image>, vision: bool, screen: &Screen, file: Option<&str>) {
    let listed = |images: &[Image]| {
        let what: Vec<String> = images.iter().map(Image::describe).collect();
        what.join("; ")
    };
    match file {
        None if images.is_empty() => screen.line(
            "· no image goes with your next message: /image FILE attaches one",
            Paint::Dim,
        ),
        None => screen.line(
            &format!("· with your next message: {}", listed(images)),
            Paint::Dim,
        ),
        Some("clear") => {
            images.clear();
            screen.line("· no image goes with your next message", Paint::Dim);
        }
        Some(_) if !vision => screen.line(&format!("· {NO_VISION}"), Paint::Yellow),
        Some(_) if images.len() >= image::MAX_PER_MESSAGE => screen.line(
            &format!(
                "· a message may hold at most {} images",
                image::MAX_PER_MESSAGE
            ),
            Paint::Yellow,
        ),
        Some(file) => match image_file(Path::new(file)).and_then(|i| {
            let mut all = images.clone();
            all.push(i.clone());
            image::too_many(&all).map_or(Ok(i), Err)
        }) {
            Ok(i) => {
                screen.line(
                    &format!(
                        "· {} goes with your next message: {}",
                        printable(file),
                        i.describe()
                    ),
                    Paint::Dim,
                );
                images.push(i);
            }
            Err(why) => screen.line(&format!("· {}", printable(&why)), Paint::Yellow),
        },
    }
}

/// `/mode`'s names for the modes.
const MODES: [(&str, Mode); 4] = [
    ("manual", Mode::Manual),
    ("edit", Mode::EditAutomatically),
    ("plan", Mode::Plan),
    ("auto", Mode::Auto),
];

fn mode_name(mode: Mode) -> &'static str {
    MODES
        .iter()
        .find(|(_, m)| *m == mode)
        .map_or("edit", |(name, _)| name)
}

/// `/mode`: the mode, or the mode switched to.
fn switch_mode(agent: &Agent, screen: &Screen, name: Option<&str>) {
    let names = MODES.map(|(n, _)| n).join(", ");
    match name.map(|n| MODES.iter().find(|(m, _)| *m == n.to_lowercase())) {
        None => screen.line(
            &format!(
                "· mode: {} · /mode {}",
                mode_name(agent.mode()),
                names.replace(", ", " | ")
            ),
            Paint::Dim,
        ),
        Some(Some((name, mode))) => {
            agent.set_mode(*mode);
            screen.line(&format!("· mode: {name}"), Paint::Dim);
        }
        Some(None) => screen.line(&format!("· the modes are {names}"), Paint::Yellow),
    }
}

/// Joins the lines of one message: a line that ends in `\` goes on in the next one, and a
/// `"""` line opens a block that the next `"""` line closes.
#[derive(Debug, Default)]
struct Composer {
    lines: Vec<String>,
    block: bool,
}

impl Composer {
    /// The message, once this line completes it.
    fn feed(&mut self, line: &str) -> Option<String> {
        let fence = line.trim() == "\"\"\"";
        if self.block {
            if fence {
                self.block = false;
                return Some(self.take());
            }
            self.lines.push(line.to_owned());
            return None;
        }
        if fence && self.lines.is_empty() {
            self.block = true;
            return None;
        }
        if let Some(head) = line.strip_suffix('\\') {
            self.lines.push(head.to_owned());
            return None;
        }
        self.lines.push(line.to_owned());
        Some(self.take())
    }

    fn pending(&self) -> bool {
        self.block || !self.lines.is_empty()
    }

    fn take(&mut self) -> String {
        std::mem::take(&mut self.lines).join("\n")
    }
}

/// Asks in the terminal. `a` allows exactly that action again for the rest of the session
/// (owner's decision of 2026-10-31), and `n` may carry what to do instead.
struct Ask {
    input: Input,
    always: HashSet<(String, String)>,
    /// ^C at a question stops the turn.
    stop: mpsc::UnboundedSender<()>,
    screen: Screen,
}

impl Approver for Ask {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            let key = (req.tool.clone(), req.action.clone());
            if self.always.contains(&key) {
                let note = format!("· allowed for this session: {}", printable(&req.action));
                self.screen.line(&note, Paint::Dim);
                return Verdict::Allow;
            }
            let who = req
                .agent
                .as_deref()
                .map(|title| format!("subagent «{}»: ", printable(title)))
                .unwrap_or_default();
            let question = format!("? {who}{} {}", req.tool, printable(&req.action));
            self.screen.line(&question, Paint::Bold);
            self.screen.line(&format!("  {}", req.reason), Paint::Plain);
            if !req.unreviewed.is_empty() {
                let line = format!("  unreviewed: {}", listed(&req.unreviewed));
                self.screen.line(&printable(&line), Paint::Plain);
            }
            loop {
                match self.input.read("  allow? [y]es [n]o [a]lways > ").await {
                    Read::Line(line) => match answer(&line) {
                        Some(Answer::Yes) => return Verdict::Allow,
                        Some(Answer::Always) => {
                            self.always.insert(key);
                            return Verdict::Allow;
                        }
                        Some(Answer::No(why)) => {
                            return Verdict::Refuse(declined(&req.action, why));
                        }
                        None => self
                            .screen
                            .line("  y, n, a, or n and what to do instead", Paint::Plain),
                    },
                    Read::Interrupted | Read::Eof => {
                        let _ = self.stop.send(());
                        return Verdict::Refuse("The user stopped the turn here.".to_owned());
                    }
                }
            }
        })
    }

    fn can_ask(&self) -> bool {
        true
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Answer<'a> {
    Yes,
    Always,
    /// What to do instead, if the user said.
    No(Option<&'a str>),
}

fn answer(line: &str) -> Option<Answer<'_>> {
    let line = line.trim();
    let (word, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(w, r)| (w, r.trim()));
    let word = word.trim_end_matches([',', ':', '.']).to_ascii_lowercase();
    match (word.as_str(), rest) {
        ("y" | "yes", "") => Some(Answer::Yes),
        ("a" | "always", "") => Some(Answer::Always),
        ("n" | "no", "") => Some(Answer::No(None)),
        ("n" | "no", why) => Some(Answer::No(Some(why))),
        _ => None,
    }
}

/// The refusal the model reads.
pub(crate) fn declined(action: &str, why: Option<&str>) -> String {
    match why {
        Some(why) => format!("The user declined `{action}` and says: {why}"),
        None => format!("The user declined `{action}`. Do not retry it unchanged."),
    }
}

/// `2.1K` above a thousand.
/// The line editor on a thread of its own: rustyline blocks, and the agent must not. Off a
/// terminal it reads plain lines, which is how the tests drive it.
#[derive(Debug, Clone)]
struct Input {
    requests: std::sync::mpsc::Sender<Request>,
}

#[derive(Debug)]
enum Request {
    Read {
        prompt: String,
        reply: oneshot::Sender<Read>,
    },
    /// A message for the history file.
    Remember(String),
}

#[derive(Debug)]
enum Read {
    Line(String),
    Interrupted,
    Eof,
}

impl Input {
    fn spawn(history: PathBuf) -> Self {
        let (requests, rx) = std::sync::mpsc::channel::<Request>();
        std::thread::spawn(move || {
            let mut editor = DefaultEditor::new().ok();
            if let Some(e) = editor.as_mut() {
                let _ = e.load_history(&history);
            }
            for req in rx {
                match req {
                    Request::Read { prompt, reply } => {
                        let read = match editor.as_mut().map(|e| e.readline(&prompt)) {
                            Some(Ok(line)) => Read::Line(line),
                            Some(Err(ReadlineError::Interrupted)) => Read::Interrupted,
                            Some(Err(_)) | None => Read::Eof,
                        };
                        let _ = reply.send(read);
                    }
                    Request::Remember(entry) => {
                        if let Some(e) = editor.as_mut()
                            && e.add_history_entry(entry.as_str()).unwrap_or(false)
                        {
                            let _ = save(e, &history);
                        }
                    }
                }
            }
        });
        Self { requests }
    }

    async fn read(&self, prompt: &str) -> Read {
        let (reply, rx) = oneshot::channel();
        let req = Request::Read {
            prompt: prompt.to_owned(),
            reply,
        };
        if self.requests.send(req).is_err() {
            return Read::Eof;
        }
        rx.await.unwrap_or(Read::Eof)
    }

    fn remember(&self, entry: &str) {
        let _ = self.requests.send(Request::Remember(entry.to_owned()));
    }
}

/// Writes the history readable by the user only: it holds what they typed.
fn save(editor: &mut DefaultEditor, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ognivo_os::private_file()
        .create(true)
        .append(true)
        .open(path)?;
    editor.save_history(path).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(lines: &[&str]) -> Vec<String> {
        let mut c = Composer::default();
        lines.iter().filter_map(|l| c.feed(l)).collect()
    }

    #[test]
    fn lines_join_into_messages() {
        assert_eq!(feed_all(&["one"]), ["one"]);
        assert_eq!(feed_all(&["fix \\", "this"]), ["fix \nthis"]);
        assert_eq!(
            feed_all(&["\"\"\"", "a", "", "  b\\", "\"\"\"", "next"]),
            ["a\n\n  b\\", "next"]
        );
        let mut c = Composer::default();
        assert_eq!(c.feed("\"\"\""), None);
        assert!(c.pending());
    }

    #[test]
    fn commands_are_whole_entries() {
        assert_eq!(command(" /exit "), Some(Entry::Exit));
        assert_eq!(command("/clear"), Some(Entry::Clear));
        assert_eq!(command("/etc/hosts is wrong"), None);
        assert_eq!(command("/exit now"), None);
        assert_eq!(command("/changes"), Some(Entry::Changes(None)));
        assert_eq!(
            command("/revert  src/lib.rs "),
            Some(Entry::Revert(Some("src/lib.rs".into())))
        );
        assert_eq!(command("/accept\nand more"), None);
        assert_eq!(command("/mode"), Some(Entry::Mode(None)));
        assert_eq!(
            command("/image shots/a b.png"),
            Some(Entry::Image(Some("shots/a b.png".into())))
        );
        assert_eq!(command("/image"), Some(Entry::Image(None)));
        assert_eq!(
            command("/mode plan"),
            Some(Entry::Mode(Some("plan".into())))
        );
    }

    #[test]
    fn answers() {
        assert_eq!(answer("y"), Some(Answer::Yes));
        assert_eq!(answer(" YES "), Some(Answer::Yes));
        assert_eq!(answer("a"), Some(Answer::Always));
        assert_eq!(answer("n"), Some(Answer::No(None)));
        assert_eq!(
            answer("n, run cargo check instead"),
            Some(Answer::No(Some("run cargo check instead")))
        );
        assert_eq!(answer("yy"), None);
        assert_eq!(answer("y please"), None);
        assert_eq!(answer(""), None);
    }
}
