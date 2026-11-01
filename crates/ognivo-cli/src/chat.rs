//! `ognivo chat`: the line-mode REPL of docs/architecture.md §10. The model's words stream to
//! the terminal, actions that need the user ask y/n/a, and ^C stops a turn without ending
//! the session. The conversation lives as long as the process: there is no store yet and no
//! compaction, so `/clear` starts over when the context fills up.
//!
//! Exit codes: 0 `/exit` or ^D; 2 bad usage; 3 the server failed at the start; 143 SIGTERM.

use std::collections::HashSet;
use std::io::{IsTerminal as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Args;
use futures::FutureExt as _;
use futures::future::BoxFuture;
use ognivo_core::agent::{Agent, AgentEvent, ApprovalRequest, Approver, TurnEnd, Verdict};
use ognivo_core::verify;
use ognivo_models::DriverError;
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};

use crate::session::{Failure, SessionArgs};

#[derive(Debug, Args)]
pub struct ChatArgs {
    #[command(flatten)]
    session: SessionArgs,
    /// Stream the model's reasoning too, dimmed
    #[arg(long)]
    show_reasoning: bool,
}

const HELP: &str = "\
/help   this help
/clear  start a new conversation; what the session changed stays unreviewed
/exit   quit; so does ^D
A line that ends in \\ goes on in the next one; \"\"\" on a line of its own starts and ends a block.
^C stops the running turn, and the conversation goes on.";

fn fail(f: Failure) -> anyhow::Result<ExitCode> {
    eprintln!("ognivo chat: {}", f.message);
    Ok(ExitCode::from(f.code))
}

pub async fn chat(a: ChatArgs) -> anyhow::Result<ExitCode> {
    let root = match a.session.root() {
        Ok(root) => root,
        Err(f) => return fail(f),
    };
    let s = match a.session.connect(root.clone()).await {
        Ok(s) => s,
        Err(f) => return fail(f),
    };
    let n_ctx = s.server.props().await.ok().and_then(|p| p.n_ctx);
    let style = Style::detect();
    let terminal = tcgetattr(std::io::stdin()).ok();
    let input = Input::spawn(s.opts.data_dir.join("chat_history"));
    let (stop, mut stopped) = mpsc::unbounded_channel();
    let approver = Ask {
        input: input.clone(),
        always: HashSet::new(),
        stop,
        style,
    };
    let max_steps = s.opts.max_steps;
    println!(
        "ognivo chat · {} · {}{}",
        s.profile,
        a.session.url,
        n_ctx.map(|n| format!(" · n_ctx {n}")).unwrap_or_default()
    );
    let mut agent = Agent::new(Box::new(s.driver), s.family, Box::new(approver), s.opts);
    println!(
        "project {}{}",
        root.display(),
        if agent.unreviewed_changes() {
            " · has unreviewed changes"
        } else {
            ""
        }
    );
    println!("{}", style.dim("/help · ^C stops a turn · ^D quits"));
    let mut int = signal(SignalKind::interrupt())?;
    let mut term = signal(SignalKind::terminate())?;
    loop {
        let entry = tokio::select! {
            entry = read_entry(&input, style) => entry,
            _ = term.recv() => return Ok(terminated(terminal.as_ref())),
        };
        let text = match entry {
            Entry::Message(text) => text,
            Entry::Help => {
                println!("{HELP}");
                continue;
            }
            Entry::Clear => {
                agent.clear();
                println!("{}", style.dim("· new conversation"));
                continue;
            }
            Entry::Exit => return Ok(ExitCode::SUCCESS),
        };
        // a ^C or a stop left from before must not end this turn
        while int.recv().now_or_never().is_some() {}
        while stopped.try_recv().is_ok() {}
        let (steps, generated, started) = (
            agent.stats.steps,
            agent.stats.completion_tokens,
            Instant::now(),
        );
        let mut view = View::new(style, a.show_reasoning);
        let outcome = {
            let mut on = |e: AgentEvent| view.show(e);
            let turn = agent.turn(&text, &mut on);
            tokio::pin!(turn);
            tokio::select! {
                end = &mut turn => Outcome::Ended(end),
                _ = int.recv() => Outcome::Stopped,
                _ = stopped.recv() => Outcome::Stopped,
                _ = term.recv() => Outcome::Terminated,
            }
        };
        view.finish();
        let unanswered = match outcome {
            Outcome::Terminated => return Ok(terminated(terminal.as_ref())),
            Outcome::Stopped => {
                agent.abandon_turn("The user stopped this turn.");
                println!("{}", style.dim("· stopped"));
                true
            }
            Outcome::Ended(Ok(TurnEnd::Answer(answer))) => {
                if let Some(line) = verify::summary(&answer.verification) {
                    println!("{}", style.dim(&format!("[ognivo] {line}")));
                }
                if answer.unsupported_claim {
                    println!("[ognivo] the answer above claims a check that did not happen");
                }
                false
            }
            Outcome::Ended(Ok(end)) => {
                println!("[ognivo] stopped: {}", why(&end, max_steps));
                true
            }
            Outcome::Ended(Err(e)) => {
                agent.abandon_turn(&format!("This turn ended on a server error: {e}"));
                println!("[ognivo] server error: {e}");
                true
            }
        };
        if unanswered && let Some(line) = verify::summary(&agent.verification()) {
            println!("{}", style.dim(&format!("[ognivo] {line}")));
        }
        println!(
            "{}",
            style.dim(&format!(
                "({} steps · {} · {} generated · context {}{})",
                agent.stats.steps - steps,
                duration(started.elapsed()),
                kilo(agent.stats.completion_tokens - generated),
                kilo(agent.context_tokens()),
                n_ctx
                    .map(|n| format!(" of {}", kilo(n.into())))
                    .unwrap_or_default()
            ))
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

fn why(end: &TurnEnd, max_steps: u32) -> String {
    match end {
        TurnEnd::StepLimit => format!("the step limit ({max_steps}) was reached"),
        TurnEnd::Stuck(why) => why.clone(),
        TurnEnd::TimeLimit => "the time limit ran out".to_owned(),
        TurnEnd::Answer(_) => "answered".to_owned(),
    }
}

/// SIGTERM: the terminal gets back the mode it had, even if the line editor holds it.
fn terminated(terminal: Option<&Termios>) -> ExitCode {
    if let Some(t) = terminal {
        let _ = tcsetattr(std::io::stdin(), OptionalActions::Now, t);
    }
    println!("\n· stopped by SIGTERM");
    ExitCode::from(143)
}

/// What the user typed at the prompt.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Message(String),
    Help,
    Clear,
    Exit,
}

/// One entry: a message of one or more lines, or a command.
async fn read_entry(input: &Input, style: Style) -> Entry {
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
                println!("{}", style.dim("(^C again or ^D quits)"));
            }
            Read::Eof => return Entry::Exit,
        }
    }
}

/// A command is a whole entry: a message may start with a path like `/etc/hosts`.
fn command(text: &str) -> Option<Entry> {
    match text.trim() {
        "/help" | "/?" => Some(Entry::Help),
        "/clear" | "/new" => Some(Entry::Clear),
        "/exit" | "/quit" => Some(Entry::Exit),
        _ => None,
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
    style: Style,
}

impl Approver for Ask {
    fn approve<'a>(&'a mut self, req: &'a ApprovalRequest) -> BoxFuture<'a, Verdict> {
        Box::pin(async move {
            let key = (req.tool.clone(), req.action.clone());
            if self.always.contains(&key) {
                let note = format!("· allowed for this session: {}", printable(&req.action));
                println!("{}", self.style.dim(&note));
                return Verdict::Allow;
            }
            println!("? {} {}", req.tool, printable(&req.action));
            println!("  {}", req.reason);
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
                        None => println!("  y, n, a, or n and what to do instead"),
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
fn declined(action: &str, why: Option<&str>) -> String {
    match why {
        Some(why) => format!("The user declined `{action}` and says: {why}"),
        None => format!("The user declined `{action}`. Do not retry it unchanged."),
    }
}

/// Shows a turn's events as they come: the model's words as they stream, a line for each
/// stretch of thinking, a line per call and per result.
#[derive(Debug)]
struct View {
    style: Style,
    show_reasoning: bool,
    /// The cursor is not at the start of a line.
    mid_line: bool,
    /// When the current stretch of reasoning began.
    thinking: Option<Instant>,
}

impl View {
    fn new(style: Style, show_reasoning: bool) -> Self {
        Self {
            style,
            show_reasoning,
            mid_line: false,
            thinking: None,
        }
    }

    fn show(&mut self, e: AgentEvent) {
        if let AgentEvent::Reasoning { text } = &e {
            if self.thinking.is_none() {
                self.thinking = Some(Instant::now());
                self.line_start();
                self.write("· thinking", true);
                if self.show_reasoning {
                    self.line_start();
                }
            }
            if self.show_reasoning {
                self.write(&printable(text), true);
            }
            return;
        }
        self.end_thinking();
        match e {
            AgentEvent::Text { text } => self.write(&printable(&text), false),
            AgentEvent::ToolStarted { tool, arguments } => {
                self.line(&format!("→ {tool} {}", short(&arguments, 160)), false)
            }
            AgentEvent::ToolFinished { result, .. } => {
                let first = result.lines().next().unwrap_or_default();
                self.line(&format!("← {}", short(first, 200)), true)
            }
            AgentEvent::Refused { tool, reason } => {
                self.line(&format!("✗ {tool}: {}", printable(&reason)), false)
            }
            AgentEvent::Notice { text } => self.line(&format!("· {}", printable(&text)), true),
            AgentEvent::Restarted { reason } => {
                self.line(&format!("· restarted: {}", printable(&reason)), true)
            }
            AgentEvent::Reasoning { .. } => {}
        }
    }

    /// Closes a stretch of thinking with how long it took.
    fn end_thinking(&mut self) {
        if let Some(t) = self.thinking.take() {
            let took = duration(t.elapsed());
            if self.show_reasoning {
                self.line(&format!("· thought for {took}"), true);
            } else {
                self.write(&format!(" {took}"), true);
                self.line_start();
            }
        }
    }

    fn finish(&mut self) {
        self.end_thinking();
        self.line_start();
    }

    fn write(&mut self, text: &str, dim: bool) {
        if text.is_empty() {
            return;
        }
        if dim {
            print!("{}", self.style.dim(text));
        } else {
            print!("{text}");
        }
        let _ = std::io::stdout().flush();
        self.mid_line = !text.ends_with('\n');
    }

    fn line_start(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn line(&mut self, text: &str, dim: bool) {
        self.line_start();
        self.write(text, dim);
        self.line_start();
    }
}

/// Text that came from the model or a tool, with control characters shown instead of
/// obeyed: an escape sequence in a file must not reach the terminal (§10).
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'),
            '\u{7f}' => '\u{2421}',
            '\u{80}'..='\u{9f}' => '\u{fffd}',
            _ => c,
        })
        .collect()
}

/// [`printable`], on one line, at most `max` characters.
fn short(text: &str, max: usize) -> String {
    let line: String = printable(text)
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .take(max)
        .collect();
    if line.chars().count() < text.chars().count() {
        format!("{line}…")
    } else {
        line
    }
}

/// `2.1K` above a thousand.
fn kilo(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `41 s` or `3 min 5 s`.
fn duration(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => format!("{s} s"),
        s => format!("{} min {} s", s / 60, s % 60),
    }
}

/// Dim meta lines on a terminal, unless NO_COLOR is set.
#[derive(Debug, Clone, Copy)]
struct Style {
    color: bool,
}

impl Style {
    fn detect() -> Self {
        Self {
            color: std::io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        }
    }

    fn dim(self, text: &str) -> String {
        if self.color {
            format!("\x1b[2m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

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
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
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

    #[test]
    fn control_characters_are_shown_not_obeyed() {
        // an OSC 52 clipboard write and a bell, as a file could carry them
        let text = "a\x1b]52;c;ZXZpbA==\x07b\n\tc\u{9b}d\u{7f}";
        assert_eq!(printable(text), "a␛]52;c;ZXZpbA==␇b\n\tc\u{fffd}d␡");
        assert_eq!(short("line one\nline two", 12), "line one lin…");
        assert_eq!(short("ok", 12), "ok");
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(kilo(999), "999");
        assert_eq!(kilo(98_304), "98.3K");
        assert_eq!(duration(Duration::from_secs(41)), "41 s");
        assert_eq!(duration(Duration::from_secs(185)), "3 min 5 s");
    }
}
