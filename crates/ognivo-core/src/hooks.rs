//! Claude Code's hooks (docs/architecture.md §9, decision 18): commands that the user's and
//! the project's settings run on the agent's events, read as Claude Code reads them.
//!
//! - Sources: Claude Code's `~/.claude/settings.json` (or `CLAUDE_CONFIG_DIR`), the
//!   project's `.claude/settings.json` and `.claude/settings.local.json`. Their entries add
//!   up, and the same command in several files runs once. `disableAllHooks` counts as the
//!   nearest file that sets it says: local, then project, then user.
//! - The project's hooks run its commands, so they run only once the user has trusted
//!   exactly that set of them, as the project's MCP servers ([`Loaded::pending`]).
//! - Events: PreToolUse, PostToolUse, PostToolUseFailure, UserPromptSubmit, Stop,
//!   SubagentStop, SessionStart, SessionEnd, PreCompact, Notification. A matcher of
//!   letters, digits, `_`, `-`, blanks, `,` and `|` names what it matches exactly (a tool by
//!   Claude Code's name, a source, a reason, a trigger, a notification's type); any other
//!   is a regular expression; none, `""` or `*` matches everything.
//! - A hook gets the event as JSON on stdin (`session_id`, `transcript_path`, `cwd`,
//!   `permission_mode`, `hook_event_name` and the event's own fields) and runs in the project
//!   root with `CLAUDE_PROJECT_DIR` set: `bash -c` with its command, or the command itself
//!   with `args`. Its `timeout` (seconds) is 600 by default, 30 for UserPromptSubmit and 1.5
//!   for SessionEnd. The hooks of an event run at the same time; `async` ones are not
//!   waited for.
//! - Exit 0: the output may be JSON (`continue`, `stopReason`, `systemMessage`,
//!   `decision`, `reason`, `hookSpecificOutput`); plain output of UserPromptSubmit and
//!   SessionStart is context for the model. Exit 2 blocks the call, the prompt, the stop or
//!   the compaction, and its stderr says why. Any other exit is a notice, and the action goes
//!   on.
//! - Only `command` hooks: `http`, `mcp_tool`, `prompt` and `agent` ones, events ognivo
//!   does not have, and `if` conditions other than `Bash(…)` are named among the problems.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::approval::Decision;
use crate::rules::{Places, Rules, Ruling};

/// Bytes of a hook's output read, and characters of context it may give the model
/// (Claude Code: 10,000).
const OUTPUT_MAX: u64 = 1 << 20;
const CONTEXT_MAX: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    UserPromptSubmit,
    Stop,
    SubagentStop,
    SessionStart,
    SessionEnd,
    PreCompact,
    Notification,
}

impl Event {
    const ALL: [Event; 10] = [
        Event::PreToolUse,
        Event::PostToolUse,
        Event::PostToolUseFailure,
        Event::UserPromptSubmit,
        Event::Stop,
        Event::SubagentStop,
        Event::SessionStart,
        Event::SessionEnd,
        Event::PreCompact,
        Event::Notification,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Event::PreToolUse => "PreToolUse",
            Event::PostToolUse => "PostToolUse",
            Event::PostToolUseFailure => "PostToolUseFailure",
            Event::UserPromptSubmit => "UserPromptSubmit",
            Event::Stop => "Stop",
            Event::SubagentStop => "SubagentStop",
            Event::SessionStart => "SessionStart",
            Event::SessionEnd => "SessionEnd",
            Event::PreCompact => "PreCompact",
            Event::Notification => "Notification",
        }
    }

    /// Whether a matcher chooses among its hooks; those of the others always run.
    fn matched(self) -> bool {
        !matches!(self, Event::UserPromptSubmit | Event::Stop)
    }

    fn timeout(self) -> Duration {
        match self {
            Event::UserPromptSubmit => Duration::from_secs(30),
            Event::SessionEnd => Duration::from_millis(1500),
            _ => Duration::from_secs(600),
        }
    }
}

#[derive(Debug, Clone)]
enum Matcher {
    All,
    Names(Vec<String>),
    Pattern(regex::Regex),
}

impl Matcher {
    fn new(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text == "*" {
            return Ok(Matcher::All);
        }
        if text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|'))
        {
            return Ok(Matcher::Names(
                text.split(['|', ','])
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(String::from)
                    .collect(),
            ));
        }
        regex::Regex::new(text)
            .map(Matcher::Pattern)
            .map_err(|e| e.to_string())
    }

    fn matches(&self, target: &str) -> bool {
        match self {
            Matcher::All => true,
            Matcher::Names(names) => names.iter().any(|n| n == target),
            Matcher::Pattern(p) => p.is_match(target),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Hook {
    pub event: Event,
    matcher: Matcher,
    /// The matcher as written.
    pub matched: String,
    pub command: String,
    args: Option<Vec<String>>,
    /// `if`: a `Bash(…)` rule the command of a call must meet.
    condition: Option<Rules>,
    timeout: Duration,
    background: bool,
    /// The settings file, as the user names it.
    pub source: String,
}

impl Hook {
    /// The hook as the trust question and messages show it.
    pub fn shown(&self) -> String {
        let mut line = self.event.name().to_owned();
        if !self.matched.is_empty() {
            line.push_str(&format!(" {}", self.matched));
        }
        line.push_str(&format!(": {}", self.command));
        if let Some(args) = &self.args {
            line.push(' ');
            line.push_str(&args.join(" "));
        }
        line
    }

    fn same(&self, other: &Hook) -> bool {
        self.event == other.event && self.command == other.command && self.args == other.args
    }
}

/// The hooks in force.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    hooks: Vec<Hook>,
}

/// What [`load`] found.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub hooks: Hooks,
    /// The project's hooks, which run once the user trusts them.
    pub pending: Vec<Hook>,
    /// Files with hooks, in the order read: `.claude/settings.json (2)`.
    pub files: Vec<String>,
    /// Hooks and files that could not be used, with why.
    pub problems: Vec<String>,
}

impl Loaded {
    /// The project's hooks as one text, the key under which the user's answer is kept.
    pub fn pending_key(&self) -> String {
        let mut lines: Vec<String> = self
            .pending
            .iter()
            .map(|h| format!("{} ({})", h.shown(), h.source))
            .collect();
        lines.sort();
        format!("hooks\n{}", lines.join("\n"))
    }

    /// The user trusted the project's hooks: they run.
    pub fn trust(&mut self) {
        self.hooks.hooks.append(&mut self.pending);
    }
}

/// Reads the hooks of the user and the project.
pub fn load(places: &Places) -> Loaded {
    let mut loaded = Loaded::default();
    let mut sources: Vec<(PathBuf, bool)> = Vec::new();
    let claude = places
        .claude_dir
        .clone()
        .or_else(|| places.home.as_ref().map(|h| h.join(".claude")));
    if let Some(claude) = claude {
        sources.push((claude.join("settings.json"), false));
    }
    for name in [".claude/settings.json", ".claude/settings.local.json"] {
        sources.push((places.root.join(name), true));
    }
    let mut disabled = false;
    for (path, of_project) in sources {
        let shown = shown(&path, places);
        let settings = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(e) => {
                    loaded.problems.push(format!("{shown}: {e}"));
                    continue;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                loaded.problems.push(format!("{shown}: {e}"));
                continue;
            }
        };
        // the nearest file that says decides
        if let Some(d) = settings.get("disableAllHooks").and_then(Value::as_bool) {
            disabled = d;
        }
        let Some(events) = settings.get("hooks") else {
            continue;
        };
        let Some(events) = events.as_object() else {
            loaded
                .problems
                .push(format!("{shown}: `hooks` is not an object"));
            continue;
        };
        let mut count = 0;
        for (name, groups) in events {
            let Some(event) = Event::ALL.into_iter().find(|e| e.name() == name) else {
                loaded.problems.push(format!(
                    "{shown}: the {name} hooks do not run, ognivo has no such event"
                ));
                continue;
            };
            for group in groups.as_array().into_iter().flatten() {
                let matched = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let matcher = match Matcher::new(&matched) {
                    Ok(m) if event.matched() => m,
                    Ok(_) => Matcher::All,
                    Err(why) => {
                        loaded.problems.push(format!(
                            "{shown}: the {name} matcher {matched:?} is not a regular expression: {why}"
                        ));
                        continue;
                    }
                };
                for h in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match hook(h, event, &matcher, &matched, &shown) {
                        Ok(hook) => {
                            count += 1;
                            let into = if of_project {
                                &mut loaded.pending
                            } else {
                                &mut loaded.hooks.hooks
                            };
                            if !into.iter().any(|k| k.same(&hook)) {
                                into.push(hook);
                            }
                        }
                        Err(why) => loaded
                            .problems
                            .push(format!("{shown}: a {name} hook does not run: {why}")),
                    }
                }
            }
        }
        if count > 0 {
            loaded.files.push(format!("{shown} ({count})"));
        }
    }
    if disabled {
        loaded.hooks.hooks.clear();
        loaded.pending.clear();
        loaded.files.push("disableAllHooks: none runs".into());
    }
    loaded
}

fn hook(
    h: &Value,
    event: Event,
    matcher: &Matcher,
    matched: &str,
    source: &str,
) -> Result<Hook, String> {
    let kind = h.get("type").and_then(Value::as_str).unwrap_or("command");
    if kind != "command" {
        return Err(format!("ognivo runs command hooks only, not {kind}"));
    }
    let command = h
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty())
        .ok_or("it has no command")?
        .to_owned();
    let args = match h.get("args") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or("`args` holds a non-text")
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Some(_) => return Err("`args` is not a list".into()),
    };
    let condition = match h.get("if").and_then(Value::as_str) {
        None => None,
        // as an ask rule it meets a command when it meets any part of it
        Some(rule) if rule.trim_start().starts_with("Bash") => {
            Some(Rules::asking(&[rule], source)?)
        }
        Some(rule) => return Err(format!("ognivo meets only Bash(…) in `if`, not {rule}")),
    };
    let timeout = match h.get("timeout").and_then(Value::as_f64) {
        Some(s) if s > 0.0 => Duration::from_secs_f64(s.min(86_400.0)),
        _ => event.timeout(),
    };
    Ok(Hook {
        event,
        matcher: matcher.clone(),
        matched: matched.to_owned(),
        command,
        args,
        condition,
        timeout,
        background: h.get("async").and_then(Value::as_bool).unwrap_or(false),
        source: source.to_owned(),
    })
}

fn shown(path: &Path, places: &Places) -> String {
    if let Ok(rel) = path.strip_prefix(&places.root) {
        return ognivo_os::slashed(rel);
    }
    match places.home.as_deref().map(|h| path.strip_prefix(h)) {
        Some(Ok(rel)) => format!("~/{}", ognivo_os::slashed(rel)),
        _ => ognivo_os::slashed(path),
    }
}

/// Where a hook runs and what it is told of the session.
#[derive(Debug, Clone)]
pub struct Context {
    pub session_id: String,
    /// The agent's working directory, `cwd` of the input.
    pub cwd: PathBuf,
    /// Where the hook runs: the project root, `CLAUDE_PROJECT_DIR`.
    pub root: PathBuf,
}

/// What a PreToolUse hook decided besides blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Allow,
    Ask,
}

/// What the hooks of one event said, together.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// Exit 2, `decision: "block"` or `permissionDecision: "deny"`, with why.
    pub blocked: Option<String>,
    /// `permissionDecision` of PreToolUse: ask wins over allow, and a block over both.
    pub permission: Option<Permission>,
    /// `updatedInput` of PreToolUse: the call's input in its place.
    pub updated_input: Option<Value>,
    /// For the model: `additionalContext`, plain output of UserPromptSubmit and
    /// SessionStart, and why a PostToolUse hook objects.
    pub context: Vec<String>,
    /// For the user: `systemMessage`, and hooks that failed.
    pub notices: Vec<String>,
    /// `continue: false`: nothing goes on; `stopReason`.
    pub stop: Option<String>,
}

impl Outcome {
    fn block(&mut self, why: &str) {
        let why = if why.trim().is_empty() {
            "no reason given"
        } else {
            why.trim()
        };
        match &mut self.blocked {
            Some(all) => {
                all.push('\n');
                all.push_str(why);
            }
            None => self.blocked = Some(why.to_owned()),
        }
    }
}

impl Hooks {
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn has(&self, event: Event) -> bool {
        self.hooks.iter().any(|h| h.event == event)
    }

    /// Runs the hooks of `event` whose matcher fits `target` (a tool's name, a source, a
    /// reason…) and, for a call of Bash, whose `if` its `command` meets, all at once, and
    /// reads what they said. `fields` are the event's own.
    pub async fn run(
        &self,
        event: Event,
        target: &str,
        command: Option<&str>,
        fields: Map<String, Value>,
        ctx: &Context,
    ) -> Outcome {
        let chosen: Vec<&Hook> = self
            .hooks
            .iter()
            .filter(|h| h.event == event && h.matcher.matches(target))
            .filter(|h| match (&h.condition, command) {
                (None, _) => true,
                (Some(rules), Some(c)) => {
                    rules.command(c, &ctx.cwd, Decision::Allow, |_| true) == Ruling::Ask
                }
                (Some(_), None) => false,
            })
            .collect();
        let mut out = Outcome::default();
        if chosen.is_empty() {
            return out;
        }
        let mut input = Map::new();
        input.insert("session_id".into(), json!(ctx.session_id));
        input.insert("transcript_path".into(), json!(""));
        input.insert("cwd".into(), json!(ctx.cwd));
        input.insert("permission_mode".into(), json!("default"));
        input.insert("hook_event_name".into(), json!(event.name()));
        input.extend(fields);
        let input = Value::Object(input).to_string().into_bytes();
        let (background, waited): (Vec<&Hook>, Vec<&Hook>) =
            chosen.into_iter().partition(|h| h.background);
        for h in background {
            let (h, input, ctx) = (h.clone(), input.clone(), ctx.clone());
            tokio::spawn(async move {
                let _ = run_one(&h, &input, &ctx).await;
            });
        }
        let ran = futures::future::join_all(waited.iter().map(|h| run_one(h, &input, ctx))).await;
        for (h, ran) in waited.into_iter().zip(ran) {
            read(h, ran, &mut out);
        }
        out
    }
}

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

async fn run_one(hook: &Hook, input: &[u8], ctx: &Context) -> Result<Ran, String> {
    let mut cmd = match &hook.args {
        Some(args) => {
            let mut c = ognivo_os::async_command(&hook.command);
            c.args(args);
            c
        }
        None => {
            let bash = ognivo_os::shell::bash().map_err(|e| e.to_string())?;
            let mut c = ognivo_os::async_command(bash);
            c.arg("-c").arg(&hook.command);
            c
        }
    };
    cmd.current_dir(&ctx.root)
        .env("CLAUDE_PROJECT_DIR", &ctx.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // a hook and what it starts end together, at its timeout or when it is done
    let mut tree = ognivo_os::Tree::spawn(cmd).map_err(|e| e.to_string())?;
    let mut stdin = tree.stdin().ok_or("no stdin")?;
    let input = input.to_vec();
    // a hook that reads nothing must not hold up the others
    tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });
    let mut stdout = tree.stdout().ok_or("no stdout")?;
    let mut stderr = tree.stderr().ok_or("no stderr")?;
    let done = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut o, mut e) = (
            (&mut stdout).take(OUTPUT_MAX),
            (&mut stderr).take(OUTPUT_MAX),
        );
        let _ = tokio::join!(o.read_to_end(&mut out), e.read_to_end(&mut err));
        (tree.wait().await, out, err)
    };
    match tokio::time::timeout(hook.timeout, done).await {
        Ok((status, out, err)) => Ok(Ran {
            code: status.ok().and_then(|s| s.code()),
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            timed_out: false,
        }),
        Err(_) => {
            tree.stop(ognivo_os::Stop::Kill);
            Ok(Ran {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            })
        }
    }
}

fn capped(text: &str) -> String {
    let mut t: String = text.chars().take(CONTEXT_MAX).collect();
    if t.len() < text.len() {
        t.push_str(" … [cut]");
    }
    t
}

/// What one hook's run says, added to `out`.
fn read(hook: &Hook, ran: Result<Ran, String>, out: &mut Outcome) {
    let event = hook.event;
    let name = format!("the {} hook {}", event.name(), hook.source);
    let ran = match ran {
        Ok(ran) => ran,
        Err(e) => {
            out.notices.push(format!("{name} did not start: {e}"));
            return;
        }
    };
    if ran.timed_out {
        out.notices.push(format!(
            "{name} ran longer than its {} s and was stopped",
            hook.timeout.as_secs_f64()
        ));
        return;
    }
    match ran.code {
        Some(0) => {
            let stdout = ran.stdout.trim();
            if stdout.starts_with('{')
                && let Ok(Value::Object(o)) = serde_json::from_str::<Value>(stdout)
            {
                json_output(event, &o, out);
            } else if matches!(event, Event::UserPromptSubmit | Event::SessionStart)
                && !stdout.is_empty()
            {
                out.context.push(capped(stdout));
            }
        }
        Some(2) => match event {
            Event::PreToolUse
            | Event::UserPromptSubmit
            | Event::Stop
            | Event::SubagentStop
            | Event::PreCompact => out.block(&ran.stderr),
            Event::PostToolUse | Event::PostToolUseFailure => {
                out.context.push(capped(ran.stderr.trim()));
            }
            Event::SessionStart | Event::SessionEnd => {
                out.notices.push(format!("{name}: {}", ran.stderr.trim()))
            }
            Event::Notification => {}
        },
        code => {
            let first = ran.stderr.lines().next().unwrap_or_default();
            let code = code.map_or("a signal".to_owned(), |c| format!("exit {c}"));
            out.notices.push(format!("{name} failed ({code}): {first}"));
        }
    }
}

fn json_output(event: Event, o: &Map<String, Value>, out: &mut Outcome) {
    let text = |k: &str| o.get(k).and_then(Value::as_str);
    if o.get("continue") == Some(&Value::Bool(false)) {
        out.stop = Some(text("stopReason").unwrap_or("a hook stopped it").to_owned());
    }
    if let Some(m) = text("systemMessage") {
        out.notices.push(m.to_owned());
    }
    if text("decision") == Some("block") {
        let why = text("reason").unwrap_or_default();
        match event {
            Event::PostToolUse | Event::PostToolUseFailure => {
                out.context.push(capped(why.trim()));
            }
            _ => out.block(why),
        }
    }
    let Some(h) = o.get("hookSpecificOutput").and_then(Value::as_object) else {
        return;
    };
    if h.get("hookEventName").and_then(Value::as_str) != Some(event.name()) {
        return;
    }
    if event == Event::PreToolUse {
        let why = h
            .get("permissionDecisionReason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match h.get("permissionDecision").and_then(Value::as_str) {
            Some("deny") => out.block(why),
            Some("ask") => out.permission = Some(Permission::Ask),
            Some("allow") if out.permission.is_none() => out.permission = Some(Permission::Allow),
            _ => {}
        }
        if let Some(v) = h.get("updatedInput").filter(|v| v.is_object()) {
            out.updated_input = Some(v.clone());
        }
    }
    if let Some(c) = h.get("additionalContext").and_then(Value::as_str)
        && !c.trim().is_empty()
    {
        out.context.push(capped(c.trim()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn fields(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[tokio::test]
    async fn hooks_load_wait_for_trust_and_say_what_they_decide() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let root = tmp.path().join("proj");
        write(
            &home.join(".claude/settings.json"),
            r#"{"hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [
                        {"type": "command", "command": "cat > \"$CLAUDE_PROJECT_DIR/in.json\"; echo 'no rm here' >&2; exit 2", "if": "Bash(rm *)"},
                        {"type": "command", "command": "echo '{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"ask\"}, \"systemMessage\": \"checked\"}'"}
                    ]},
                    {"matcher": "mcp__.*", "hooks": [{"type": "command", "command": "exit 2"}]}
                ],
                "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "echo 'Today is Friday.'"}]}],
                "Teleport": [{"hooks": [{"type": "command", "command": "true"}]}],
                "Stop": [{"hooks": [{"type": "prompt", "prompt": "Is it done?"}]}]
            }}"#,
        );
        write(
            &root.join(".claude/settings.json"),
            r#"{"hooks": {"PostToolUse": [{"matcher": "Edit|Write", "hooks": [
                {"type": "command", "command": "echo '{\"decision\": \"block\", \"reason\": \"lint: unused import\"}'"}
            ]}]}}"#,
        );
        let places = Places {
            root: root.clone(),
            home: Some(home.clone()),
            claude_dir: None,
        };
        let mut loaded = load(&places);
        assert_eq!(loaded.problems.len(), 2, "{:?}", loaded.problems);
        assert!(loaded.problems[0].contains("Teleport") || loaded.problems[1].contains("Teleport"));
        assert_eq!(loaded.pending.len(), 1);
        assert!(
            loaded
                .pending_key()
                .contains("PostToolUse Edit|Write: echo")
        );
        let ctx = Context {
            session_id: "s1".into(),
            cwd: root.clone(),
            root: root.clone(),
        };
        let hooks = &loaded.hooks;
        let rm = hooks
            .run(
                Event::PreToolUse,
                "Bash",
                Some("rm -rf build"),
                fields(json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf build"}})),
                &ctx,
            )
            .await;
        assert_eq!(rm.blocked.as_deref(), Some("no rm here"));
        assert_eq!(rm.permission, Some(Permission::Ask));
        assert_eq!(rm.notices, ["checked"]);
        let input: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("in.json")).unwrap()).unwrap();
        assert_eq!(input["hook_event_name"], "PreToolUse");
        assert_eq!(input["session_id"], "s1");
        assert_eq!(input["tool_input"]["command"], "rm -rf build");
        // the `if` keeps the first hook from other commands
        let ls = hooks
            .run(Event::PreToolUse, "Bash", Some("ls"), Map::new(), &ctx)
            .await;
        assert_eq!(ls.blocked, None);
        let mcp = hooks
            .run(
                Event::PreToolUse,
                "mcp__docs__search",
                None,
                Map::new(),
                &ctx,
            )
            .await;
        assert_eq!(mcp.blocked.as_deref(), Some("no reason given"));
        let prompt = hooks
            .run(Event::UserPromptSubmit, "", None, Map::new(), &ctx)
            .await;
        assert_eq!(prompt.context, ["Today is Friday."]);

        // the project's hook runs once trusted
        assert!(!hooks.has(Event::PostToolUse));
        loaded.trust();
        let edit = loaded
            .hooks
            .run(Event::PostToolUse, "Edit", None, Map::new(), &ctx)
            .await;
        assert_eq!(edit.context, ["lint: unused import"]);
        let read = loaded
            .hooks
            .run(Event::PostToolUse, "Read", None, Map::new(), &ctx)
            .await;
        assert_eq!(read, Outcome::default());
    }

    #[tokio::test]
    async fn a_hook_past_its_timeout_is_stopped_and_disable_all_hooks_turns_them_off() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        write(
            &root.join(".claude/settings.json"),
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "sleep 5", "timeout": 0.2}]}]}}"#,
        );
        let places = Places {
            root: root.clone(),
            home: None,
            claude_dir: None,
        };
        let mut loaded = load(&places);
        loaded.trust();
        let ctx = Context {
            session_id: String::new(),
            cwd: root.clone(),
            root: root.clone(),
        };
        let started = std::time::Instant::now();
        let out = loaded
            .hooks
            .run(Event::Stop, "", None, Map::new(), &ctx)
            .await;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(out.blocked, None);
        assert!(out.notices[0].contains("was stopped"), "{:?}", out.notices);

        write(
            &root.join(".claude/settings.local.json"),
            r#"{"disableAllHooks": true}"#,
        );
        let loaded = load(&places);
        assert!(loaded.hooks.is_empty() && loaded.pending.is_empty());
    }
}
