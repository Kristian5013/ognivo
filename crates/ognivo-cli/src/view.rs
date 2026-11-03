//! How `ognivo chat` shows a turn (docs/architecture.md §10): the model's words as they
//! stream, one line per call with what came of it, the diff of each change, and a line for
//! each long stretch of thinking. Subagents work at the same time, so each of their calls
//! is one line, numbered by its task, once it is done. Text from the model and the tools
//! goes through [`printable`] first.

use std::collections::HashMap;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ognivo_core::agent::AgentEvent;
use ognivo_core::instructions::{Given, Skipped};
use ognivo_core::journal::FileChange;
use ognivo_tools::diff::Hunk;
use ognivo_tools::projection::Call;

/// Reasoning shorter than this leaves no line.
const THINKING_SHOWN: Duration = Duration::from_secs(3);
/// Characters of a call's line, a result or a notice.
const LINE_CHARS: usize = 160;
/// Characters of a line of code or output.
const CODE_CHARS: usize = 200;
/// Diff lines shown per change; the rest are counted.
const DIFF_LINES: usize = 12;
/// Output lines shown under a command that failed.
const FAILED_TAIL: usize = 3;

/// Shows a turn's events as they come.
#[derive(Debug)]
pub struct View {
    screen: Screen,
    root: PathBuf,
    show_reasoning: bool,
    /// When the current stretch of reasoning began, and the screen's writes once its
    /// line was printed.
    thinking: Option<(Instant, Option<u64>)>,
    /// The call that is running.
    call: Option<Running>,
    /// The subagents of the running `delegate`, by their tasks' numbers.
    subagents: HashMap<usize, Subagent>,
}

#[derive(Debug, Default)]
struct Subagent {
    title: String,
    /// The tool and the line of its running call.
    call: Option<(String, String)>,
}

#[derive(Debug)]
struct Running {
    /// ognivo's name for the tool, or the model's for a call that did not parse.
    name: String,
    /// The screen's writes once the call's line was printed: the line is open for the
    /// result while nothing else was written.
    open: Option<u64>,
    /// Its change was shown.
    changed: bool,
}

impl View {
    pub fn new(screen: Screen, root: PathBuf, show_reasoning: bool) -> Self {
        Self {
            screen,
            root,
            show_reasoning,
            thinking: None,
            call: None,
            subagents: HashMap::new(),
        }
    }

    pub fn show(&mut self, e: AgentEvent) {
        if let AgentEvent::Reasoning { text } = &e {
            self.reasoning(text);
            return;
        }
        self.end_thinking();
        match e {
            AgentEvent::Text { text } => self.screen.write(&printable(&text), Paint::Plain),
            AgentEvent::ToolStarted {
                tool,
                arguments,
                call,
                ..
            } => self.started(&tool, &arguments, call.as_ref()),
            AgentEvent::FileChanged {
                path,
                created,
                hunks,
            } => self.changed(&path, created, &hunks),
            AgentEvent::ToolFinished { result, .. } => self.finished(&result),
            AgentEvent::Instructions { files, skipped } => {
                if !files.is_empty() {
                    let line = format!("· instructions: {}", given(&files));
                    self.screen.line(&line, Paint::Dim);
                }
                for s in &skipped {
                    self.screen.line(&not_read(s), Paint::Yellow);
                }
            }
            AgentEvent::Refused { tool, reason } => {
                let name = self
                    .call
                    .as_ref()
                    .map_or(tool.as_str(), |c| c.name.as_str());
                let line = format!("✗ {name}: {}", printable(&reason));
                self.screen.line(&line, Paint::Yellow)
            }
            AgentEvent::Notice { text } => {
                let text = text.strip_prefix("[ognivo] ").unwrap_or(&text);
                self.screen
                    .line(&format!("· {}", short(text, LINE_CHARS)), Paint::Dim)
            }
            AgentEvent::Restarted { reason } => self
                .screen
                .line(&format!("· restarted: {}", printable(&reason)), Paint::Dim),
            AgentEvent::Compacting { tokens, limit } => self.screen.line(
                &format!(
                    "· compacting the conversation: about {} tokens, over the {} budget",
                    kilo(tokens),
                    kilo(limit)
                ),
                Paint::Dim,
            ),
            AgentEvent::Compacted {
                replaced,
                kept,
                summary,
            } => {
                let (line, paint) = compacted(replaced, kept, summary.as_deref());
                self.screen.line(&line, paint);
            }
            AgentEvent::SubagentStarted {
                index,
                title,
                write,
                ..
            } => {
                let mode = match &write {
                    Some(files) => format!("write {}", files.join(", ")),
                    None => "read".to_owned(),
                };
                let line = format!("⇉ {} «{}» · {mode}", index + 1, printable(&title));
                self.screen.line(&short(&line, LINE_CHARS), Paint::Plain);
                self.subagents.insert(index, Subagent { title, call: None });
            }
            AgentEvent::Subagent { index, event } => self.subagent(index, *event),
            AgentEvent::SubagentFinished {
                index,
                end,
                steps,
                changed,
            } => {
                let title = self
                    .subagents
                    .remove(&index)
                    .map(|s| s.title)
                    .unwrap_or_default();
                let how = match end.as_str() {
                    "answer" => format!("reported after {steps} steps"),
                    "step_limit" => format!("ran out of its {steps} steps"),
                    "time_limit" => "out of time".to_owned(),
                    "stuck" => format!("stopped after {steps} steps"),
                    _ => "failed".to_owned(),
                };
                let mut line = format!("⇇ {} «{}» · {how}", index + 1, printable(&title));
                if !changed.is_empty() {
                    line.push_str(&format!(" · changed {}", changed.join(", ")));
                }
                let paint = if end == "answer" {
                    Paint::Plain
                } else {
                    Paint::Yellow
                };
                self.screen.line(&short(&line, LINE_CHARS), paint);
            }
            AgentEvent::Reasoning { .. }
            | AgentEvent::TurnStarted { .. }
            | AgentEvent::TurnEnded { .. }
            | AgentEvent::Generation { .. }
            | AgentEvent::Approval { .. } => {}
        }
    }

    /// A subagent's event: a line for each call once it is done, its changes, refusals
    /// and notes; its words come in its report.
    fn subagent(&mut self, index: usize, e: AgentEvent) {
        let n = index + 1;
        let line = match e {
            AgentEvent::ToolStarted {
                tool,
                arguments,
                call,
                ..
            } => {
                let name = match &call {
                    Some(c) => name_of(c).to_owned(),
                    None => tool,
                };
                let line = self.title(&name, &arguments, call.as_ref());
                self.subagents.entry(index).or_default().call = Some((name, line));
                return;
            }
            AgentEvent::ToolFinished { result, .. } => {
                let Some((name, line)) = self.subagents.entry(index).or_default().call.take()
                else {
                    return;
                };
                let (status, paint) = match Status::of(&name, &result) {
                    Status::Refused => ("refused".to_owned(), Paint::Yellow),
                    Status::Error(e) => (format!("✗ {e}"), Paint::Yellow),
                    Status::Done(s) | Status::Note(s) => (s.to_owned(), Paint::Dim),
                    Status::Failed { status, .. } => (status.to_owned(), Paint::Yellow),
                };
                (format!("{line} · {status}"), paint)
            }
            AgentEvent::FileChanged {
                path,
                created,
                hunks,
            } => (
                format!("  {path} · {}", counts(created, &hunks)),
                Paint::Dim,
            ),
            AgentEvent::Refused { reason, .. } => (format!("✗ {reason}"), Paint::Yellow),
            AgentEvent::Notice { text } => (
                format!("· {}", text.strip_prefix("[ognivo] ").unwrap_or(&text)),
                Paint::Dim,
            ),
            AgentEvent::Compacting { .. } => ("· compacting".to_owned(), Paint::Dim),
            _ => return,
        };
        let (text, paint) = line;
        self.screen.line(
            &short(&format!("  {n}│ {}", printable(&text)), LINE_CHARS),
            paint,
        );
    }

    fn reasoning(&mut self, text: &str) {
        if self.thinking.is_none() {
            self.screen.line_start();
            let mark = if self.show_reasoning {
                self.screen.line("· thinking", Paint::Dim);
                None
            } else if self.screen.style.live {
                // erased when it ends
                self.screen.write("· thinking…", Paint::Dim);
                Some(self.screen.writes())
            } else {
                None
            };
            self.thinking = Some((Instant::now(), mark));
        }
        if self.show_reasoning {
            self.screen.write(&printable(text), Paint::Dim);
        }
    }

    /// Closes a stretch of thinking: a line with how long it took, unless it was short.
    fn end_thinking(&mut self) {
        let Some((began, mark)) = self.thinking.take() else {
            return;
        };
        let took = began.elapsed();
        if self.show_reasoning {
            self.screen
                .line(&format!("· thought for {}", duration(took)), Paint::Dim);
            return;
        }
        if mark == Some(self.screen.writes()) {
            self.screen.erase_line();
        }
        if took >= THINKING_SHOWN {
            self.screen
                .line(&format!("· thinking {}", duration(took)), Paint::Dim);
        }
    }

    fn started(&mut self, tool: &str, arguments: &str, call: Option<&Call>) {
        let name = match call {
            Some(c) => name_of(c),
            None => tool,
        }
        .to_owned();
        self.screen.line_start();
        self.screen
            .write(&self.title(&name, arguments, call), Paint::Plain);
        self.call = Some(Running {
            name,
            open: Some(self.screen.writes()),
            changed: false,
        });
    }

    /// `→ read src/lib.rs`, `→ $ cargo test`: what the call works on.
    fn title(&self, name: &str, arguments: &str, call: Option<&Call>) -> String {
        let what = match call {
            Some(Call::Shell { command, .. }) => return format!("→ $ {}", first_line(command)),
            Some(Call::Read { path, .. } | Call::Edit { path, .. } | Call::Write { path, .. }) => {
                self.path(path)
            }
            Some(Call::Grep {
                pattern,
                path,
                glob,
            }) => {
                let mut what = pattern.clone();
                if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
                    what.push_str(&format!(" in {}", self.path(p)));
                }
                if let Some(g) = glob {
                    what.push_str(&format!(" ({g})"));
                }
                what
            }
            Some(Call::Glob { pattern, path }) => match path.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => format!("{pattern} in {}", self.path(p)),
                None => pattern.clone(),
            },
            Some(Call::Delegate { tasks }) => tasks
                .iter()
                .map(|t| format!("«{}»", t.title))
                .collect::<Vec<_>>()
                .join(", "),
            // the files a patch names, from its headers
            Some(Call::Patch { patch }) => patch
                .lines()
                .filter_map(|l| {
                    ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                        .iter()
                        .find_map(|m| l.trim().strip_prefix(m))
                })
                .map(|p| self.path(p))
                .collect::<Vec<_>>()
                .join(", "),
            None => arguments.to_owned(),
        };
        format!("→ {name} {}", short(&what, LINE_CHARS))
    }

    /// A path the model gave, relative to the project root when it points inside it; an
    /// empty one is the directory the tools resolve it to.
    fn path(&self, raw: &str) -> String {
        if raw.is_empty() {
            return ".".to_owned();
        }
        match Path::new(raw).strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_owned(),
            Ok(rel) => rel.display().to_string(),
            Err(_) => raw.to_owned(),
        }
    }

    fn changed(&mut self, path: &str, created: bool, hunks: &[Hunk]) {
        if self.call.is_none() {
            self.screen
                .line(&format!("→ {}", short(path, LINE_CHARS)), Paint::Plain);
        }
        self.after(&counts(created, hunks), Paint::Dim);
        for (line, paint) in diff_lines(hunks, DIFF_LINES) {
            self.screen.line(&line, paint);
        }
        if let Some(c) = self.call.as_mut() {
            c.changed = true;
        }
    }

    fn finished(&mut self, result: &str) {
        let Some(call) = &self.call else {
            return;
        };
        if !call.changed {
            match Status::of(&call.name, result) {
                Status::Refused => {}
                Status::Error(e) => {
                    self.after(&format!("✗ {}", short(e, LINE_CHARS)), Paint::Yellow)
                }
                Status::Done(s) => self.after(&short(s, LINE_CHARS), Paint::Dim),
                Status::Note(s) => self.after(&short(s, LINE_CHARS), Paint::Yellow),
                Status::Failed { status, tail } => {
                    self.after(&short(status, LINE_CHARS), Paint::Yellow);
                    for line in tail {
                        self.screen
                            .line(&format!("    {}", short(line, CODE_CHARS)), Paint::Dim);
                    }
                }
            }
        }
        self.call = None;
        self.screen.line_start();
    }

    /// What came of the running call: after its line while that is open, otherwise on
    /// a line of its own.
    fn after(&mut self, status: &str, paint: Paint) {
        let open = self.call.as_mut().and_then(|c| c.open.take());
        if open == Some(self.screen.writes()) {
            self.screen.write(" · ", Paint::Dim);
            self.screen.write(status, paint);
            self.screen.line_start();
        } else {
            self.screen.line(&format!("← {status}"), paint);
        }
    }

    pub fn finish(&mut self) {
        self.end_thinking();
        self.call = None;
        self.screen.line_start();
    }
}

/// The names results use for the tools (`[read: …]`), whatever the model calls them.
fn name_of(call: &Call) -> &'static str {
    match call {
        Call::Read { .. } => "read",
        Call::Edit { .. } => "edit",
        Call::Write { .. } => "write",
        Call::Patch { .. } => "apply_patch",
        Call::Shell { .. } => "shell",
        Call::Grep { .. } => "grep",
        Call::Glob { .. } => "glob",
        Call::Delegate { .. } => "delegate",
    }
}

/// What came of a call, from the first line of its result. The formats are the tools'
/// own (ognivo-tools); anything else shows as it is.
#[derive(Debug, PartialEq, Eq)]
enum Status<'a> {
    /// A refusal, shown when it happened.
    Refused,
    Error(&'a str),
    Done(&'a str),
    /// ognivo's own result, such as a call it did not run.
    Note(&'a str),
    /// A command that did not exit 0, with the last lines of its output.
    Failed {
        status: &'a str,
        tail: Vec<&'a str>,
    },
}

impl<'a> Status<'a> {
    fn of(name: &str, result: &'a str) -> Self {
        let first = result.lines().next().unwrap_or_default();
        if first.starts_with("[refused]") {
            return Status::Refused;
        }
        if let Some(e) = first.strip_prefix("[error] ") {
            return Status::Error(e);
        }
        if first.starts_with("[edit: already applied]") {
            return Status::Done("already applied");
        }
        if let Some(done) = first.strip_prefix("[delegate] ") {
            return Status::Done(done.trim_end_matches(':'));
        }
        let Some(inside) = first
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.strip_prefix(name))
            .and_then(|s| s.strip_prefix(": "))
        else {
            return match first.strip_prefix("[ognivo] ") {
                Some(note) => Status::Note(note),
                None => Status::Done(first),
            };
        };
        match name {
            "shell" if !inside.starts_with("exit 0,") => Status::Failed {
                status: inside,
                tail: tail(result, FAILED_TAIL),
            },
            // the call's line names the file or the pattern; the rest is how much came
            "read" => Status::Done(inside.rsplit_once(" — ").map_or(inside, |(_, n)| n)),
            "grep" => Status::Done(inside.split_once(" for /").map_or(inside, |(n, _)| n)),
            "glob" => Status::Done(inside.split_once(" match ").map_or(inside, |(n, _)| n)),
            _ => Status::Done(inside),
        }
    }
}

/// The last `n` lines with text in a command's result, without ognivo's lines.
fn tail(result: &str, n: usize) -> Vec<&str> {
    let mut lines: Vec<&str> = result
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty() && !l.starts_with("[ognivo]") && !l.starts_with("[cwd: "))
        .collect();
    lines.drain(..lines.len().saturating_sub(n));
    lines
}

/// `+3 -1`, or `new, 25 lines` for a file that did not exist.
pub fn counts(created: bool, hunks: &[Hunk]) -> String {
    let removed: usize = hunks.iter().map(|h| h.removed.len()).sum();
    let added: usize = hunks.iter().map(|h| h.added.len()).sum();
    if created {
        format!("new, {added} line{}", if added == 1 { "" } else { "s" })
    } else if hunks.is_empty() {
        "unchanged".to_owned()
    } else {
        format!("+{added} -{removed}")
    }
}

/// `· compacted: 46 messages became a summary of 310 words; 3 steps kept`.
pub fn compacted(replaced: usize, kept: usize, summary: Option<&str>) -> (String, Paint) {
    let steps = match kept {
        0 => "no step kept".to_owned(),
        1 => "1 step kept".to_owned(),
        n => format!("{n} steps kept"),
    };
    match summary {
        Some(text) => (
            format!(
                "· compacted: {replaced} messages became a summary of {} words; {steps}",
                text.split_whitespace().count()
            ),
            Paint::Dim,
        ),
        None => (
            format!(
                "· compacted without a summary, the model wrote none: {replaced} messages became ognivo's facts; {steps}"
            ),
            Paint::Yellow,
        ),
    }
}

/// `999`, `98.3K`.
pub fn kilo(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// `AGENTS.md, CLAUDE.md (cut)`: the instruction files the model got.
pub fn given(files: &[Given]) -> String {
    let names: Vec<String> = files
        .iter()
        .map(|f| {
            let name = short(&f.path, LINE_CHARS);
            if f.cut { format!("{name} (cut)") } else { name }
        })
        .collect();
    names.join(", ")
}

/// `· not read: ~/x.md (imported by CLAUDE.md from outside the project)`.
pub fn not_read(s: &Skipped) -> String {
    format!(
        "· not read: {} ({})",
        short(&s.path, LINE_CHARS),
        short(&s.why, LINE_CHARS)
    )
}

/// A file's unreviewed change for `/changes`: a line with its counts, then its diff, all
/// of it when `whole`.
pub fn change_lines(c: &FileChange, whole: bool) -> Vec<(String, Paint)> {
    let mut head = format!(
        "→ {} · {}",
        short(&c.shown, LINE_CHARS),
        counts(c.created, &c.hunks)
    );
    if c.deleted {
        head.push_str(", deleted");
    }
    let mut out = vec![(head, Paint::Plain)];
    if c.drifted {
        let note = "  also changed by a command or outside ognivo since its edits";
        out.push((note.to_owned(), Paint::Yellow));
    }
    out.extend(diff_lines(
        &c.hunks,
        if whole { usize::MAX } else { DIFF_LINES },
    ));
    out
}

/// A change as numbered `-` and `+` lines, at most `max` of them and a count of the rest.
fn diff_lines(hunks: &[Hunk], max: usize) -> Vec<(String, Paint)> {
    let width = hunks
        .iter()
        .map(|h| (h.old_start + h.removed.len()).max(h.new_start + h.added.len()))
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let total: usize = hunks.iter().map(|h| h.removed.len() + h.added.len()).sum();
    let mut out = Vec::new();
    let mut shown = 0;
    for (i, h) in hunks.iter().enumerate() {
        if shown == max {
            break;
        }
        if i > 0 {
            out.push((format!("  {:>width$}", "⋮"), Paint::Dim));
        }
        let removed = (h.old_start..)
            .zip(&h.removed)
            .map(|(n, l)| (n, '-', l, Paint::Red));
        let added = (h.new_start..)
            .zip(&h.added)
            .map(|(n, l)| (n, '+', l, Paint::Green));
        for (n, sign, line, paint) in removed.chain(added).take(max - shown) {
            out.push((
                format!("  {n:>width$} {sign} {}", short(line, CODE_CHARS)),
                paint,
            ));
            shown += 1;
        }
    }
    if total > shown {
        out.push((format!("  … {} more", total - shown), Paint::Dim));
    }
    out
}

/// Text that came from the model or a tool, with control characters shown instead of
/// obeyed: an escape sequence in a file must not reach the terminal (§10).
pub fn printable(text: &str) -> String {
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

/// The first line of `text`, at most [`LINE_CHARS`] characters; `…` marks what is left out.
fn first_line(text: &str) -> String {
    let text = text.trim();
    let line = text.lines().next().unwrap_or_default();
    let shown = short(line, LINE_CHARS);
    if line.len() < text.len() && !shown.ends_with('…') {
        format!("{shown} …")
    } else {
        shown
    }
}

/// `41 s` or `3 min 5 s`.
pub fn duration(d: Duration) -> String {
    match d.as_secs() {
        s if s < 60 => format!("{s} s"),
        s => format!("{} min {} s", s / 60, s % 60),
    }
}

/// How output looks: colors on a terminal unless NO_COLOR is set; erasing a line only
/// on a terminal.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
    live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Plain,
    Dim,
    Bold,
    Red,
    Green,
    Yellow,
}

impl Style {
    pub fn detect() -> Self {
        let live = std::io::stdout().is_terminal();
        Self {
            color: live && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
            live,
        }
    }

    fn paint(self, text: &str, paint: Paint) -> String {
        let code = match paint {
            Paint::Plain => return text.to_owned(),
            Paint::Dim => "2",
            Paint::Bold => "1",
            Paint::Red => "31",
            Paint::Green => "32",
            Paint::Yellow => "33",
        };
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

/// Standard output as the view and the questions share it. A call's line stays open
/// for its result; whatever is written in between closes it, and `writes` shows that.
#[derive(Debug, Clone)]
pub struct Screen {
    style: Style,
    cursor: Arc<Mutex<Cursor>>,
}

#[derive(Debug, Default)]
struct Cursor {
    /// Not at the start of a line.
    mid_line: bool,
    writes: u64,
}

impl Screen {
    pub fn new(style: Style) -> Self {
        Self {
            style,
            cursor: Arc::default(),
        }
    }

    fn write(&self, text: &str, paint: Paint) {
        if text.is_empty() {
            return;
        }
        let mut c = self.cursor();
        print!("{}", self.style.paint(text, paint));
        let _ = std::io::stdout().flush();
        c.mid_line = !text.ends_with('\n');
        c.writes += 1;
    }

    fn line_start(&self) {
        let mut c = self.cursor();
        if c.mid_line {
            println!();
            c.mid_line = false;
            c.writes += 1;
        }
    }

    pub fn line(&self, text: &str, paint: Paint) {
        self.line_start();
        self.write(text, paint);
        self.line_start();
    }

    /// Clears the line the cursor is on; for a terminal only.
    fn erase_line(&self) {
        let mut c = self.cursor();
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();
        c.mid_line = false;
        c.writes += 1;
    }

    fn writes(&self) -> u64 {
        self.cursor().writes
    }

    fn cursor(&self) -> MutexGuard<'_, Cursor> {
        self.cursor.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(kilo(999), "999");
        assert_eq!(kilo(98_304), "98.3K");
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
    fn durations() {
        assert_eq!(duration(Duration::from_secs(41)), "41 s");
        assert_eq!(duration(Duration::from_secs(185)), "3 min 5 s");
    }

    #[test]
    fn a_result_shows_as_one_status() {
        use ognivo_tools::files::{self, LineStyle, Target};
        use ognivo_tools::search;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
        // the tools' own results, so that a change of their format shows here
        let read = files::read(
            &Target::new(root.join("notes.txt"), root),
            None,
            None,
            LineStyle::Bar,
        )
        .unwrap();
        assert_eq!(
            Status::of("read", &read),
            Status::Done("lines 1-3 of 3 lines, 14 B, ~4 tokens")
        );
        let grep = search::grep(root, root, "t", None, &|_| false).unwrap();
        assert_eq!(
            Status::of("grep", &grep),
            Status::Done("2 matches in 1 files")
        );
        let glob = search::glob(root, root, "*.txt", &|_| false).unwrap();
        assert_eq!(Status::of("glob", &glob), Status::Done("1 files"));
        assert_eq!(
            Status::of("shell", "[shell: exit 0, 0.1 s]\nok"),
            Status::Done("exit 0, 0.1 s")
        );
        let failed = "[shell: exit 101, 2.0 s]\n[cwd: /tmp]\nrunning 2 tests\ntest a ... ok\n\
                      test b ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed\n\n\
                      [ognivo] Unchanged: this same call gave this same result before.";
        assert_eq!(
            Status::of("shell", failed),
            Status::Failed {
                status: "exit 101, 2.0 s",
                tail: vec![
                    "test a ... ok",
                    "test b ... FAILED",
                    "test result: FAILED. 1 passed; 1 failed"
                ],
            }
        );
        assert_eq!(
            Status::of("read", "[error] a.rs: no such file"),
            Status::Error("a.rs: no such file")
        );
        assert_eq!(
            Status::of("shell", "[refused] The user declined `rm -rf x`."),
            Status::Refused
        );
        assert_eq!(
            Status::of("edit", "[edit: already applied] a.rs: old_string is absent"),
            Status::Done("already applied")
        );
        assert_eq!(
            Status::of(
                "grep",
                "[ognivo] Not run: this same call gave the same result twice."
            ),
            Status::Note("Not run: this same call gave the same result twice.")
        );
        assert_eq!(
            Status::of("x", "something else"),
            Status::Done("something else")
        );
    }

    #[test]
    fn a_call_line_says_what_the_call_works_on() {
        let style = Style {
            color: false,
            live: false,
        };
        let view = View::new(Screen::new(style), PathBuf::from("/w/proj"), false);
        let title = |call: Call| view.title(name_of(&call), "", Some(&call));
        assert_eq!(
            title(Call::Shell {
                command: "\ncargo test -p a\n".into(),
                timeout: None
            }),
            "→ $ cargo test -p a"
        );
        assert_eq!(
            title(Call::Shell {
                command: "cat > a.rs <<'EOF'\nfn main() {}\nEOF".into(),
                timeout: None
            }),
            "→ $ cat > a.rs <<'EOF' …"
        );
        assert_eq!(
            title(Call::Read {
                path: "/w/proj/src/lib.rs".into(),
                offset: Some(5),
                limit: None
            }),
            "→ read src/lib.rs"
        );
        assert_eq!(
            title(Call::Grep {
                pattern: "fn main".into(),
                path: Some("/w/proj".into()),
                glob: Some("**/*.rs".into())
            }),
            "→ grep fn main in . (**/*.rs)"
        );
        assert_eq!(
            title(Call::Glob {
                pattern: "*.toml".into(),
                path: Some("/etc".into())
            }),
            "→ glob *.toml in /etc"
        );
        // gpt-oss sends empty paths at times: the tools read them as the current directory
        assert_eq!(
            title(Call::Read {
                path: String::new(),
                offset: None,
                limit: None
            }),
            "→ read ."
        );
        assert_eq!(
            title(Call::Grep {
                pattern: "timefmt".into(),
                path: Some(String::new()),
                glob: Some("*.rs".into())
            }),
            "→ grep timefmt (*.rs)"
        );
        // a call that did not parse shows as the model sent it
        assert_eq!(
            view.title("Bash", "{\"cmd\":\"ls\"}", None),
            "→ Bash {\"cmd\":\"ls\"}"
        );
    }

    #[test]
    fn a_change_shows_as_numbered_lines_up_to_a_limit() {
        let hunk = |old_start, new_start, removed: &[&str], added: &[&str]| Hunk {
            old_start,
            new_start,
            removed: removed.iter().map(|s| (*s).to_owned()).collect(),
            added: added.iter().map(|s| (*s).to_owned()).collect(),
        };
        let hunks = [
            hunk(9, 9, &["old\x1b[31m"], &["new"]),
            hunk(20, 20, &[], &["c", "d", "e"]),
        ];
        let lines: Vec<(String, Paint)> = [
            ("   9 - old␛[31m", Paint::Red),
            ("   9 + new", Paint::Green),
            ("   ⋮", Paint::Dim),
            ("  20 + c", Paint::Green),
            ("  21 + d", Paint::Green),
            ("  … 1 more", Paint::Dim),
        ]
        .into_iter()
        .map(|(l, p)| (l.to_owned(), p))
        .collect();
        assert_eq!(diff_lines(&hunks, 4), lines);
        assert_eq!(diff_lines(&hunks, 5).len(), 6);
        assert!(diff_lines(&[], 4).is_empty());
    }
}
