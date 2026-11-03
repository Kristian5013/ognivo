//! T2-mini (docs/architecture.md §11): scenarios on small fixture projects
//! (`evals/t2/scenarios/*.toml`), each run as one headless turn by the agent on the Generic
//! driver against a live llama-server. Each run is judged by what it leaves (the files,
//! the answer, the tools it took) and counted by the M1 exit metrics: parse_ok, schema_ok,
//! first-try edit apply, truncated calls executed, loops, cache hits.
//!
//! Runs are headless (`NoUser`): reads, edits inside the project, and the project's own
//! commands while nothing is unreviewed go ahead without asking, and anything else is
//! refused, so the model never runs code it wrote.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ognivo_core::agent::{
    Agent, AgentEvent, AgentOptions, Layout, NoUser, SameModel, Stats, Subagents, Toolset, TurnEnd,
};
use ognivo_core::approval::WorkspaceState;
use ognivo_core::instructions::UserInstructions;
use ognivo_core::rules::Rules;
use ognivo_core::session::Session;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{
    ChatDriver, DriverError, Endpoint, ModelDriver, ModelProfile, NativeDriver, Priority, Router,
    Scheduled, native,
};
use ognivo_store::{NewSession, Store};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A turn's wall-clock limit unless the scenario sets one.
const TIME_LIMIT: Duration = Duration::from_secs(300);
/// Steps of a turn unless the scenario sets a number.
const MAX_STEPS: u32 = 15;
/// The tool names scenarios may expect: Claude Code's, as ognivo's tool ids name them.
const TOOLS: [&str; 6] = ["Read", "Edit", "Write", "Bash", "Grep", "Glob"];
/// Stand-ins for the profile's own control tokens, so no scenario file holds one (§5.3):
/// its first framing string, and its first guarded string that frames nothing.
const FRAMING: &str = "⟦framing⟧";
const SPECIAL: &str = "⟦special⟧";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// The right tool for a question about the project.
    Selection,
    /// No tool at all.
    Irrelevance,
    /// Exact text through the arguments: quotes, backslashes, Unicode, control tokens.
    Arguments,
    /// Changes to a small project.
    Edit,
    /// A task whose obvious first try fails.
    Recovery,
    /// A task that tempts the model to repeat itself.
    Loop,
    /// More than a read shows at once.
    Context,
    /// The project's own commands.
    Shell,
    /// A task of independent parts, which subagents can do at the same time.
    Parallel,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Selection => "selection",
            Category::Irrelevance => "irrelevance",
            Category::Arguments => "arguments",
            Category::Edit => "edit",
            Category::Recovery => "recovery",
            Category::Loop => "loop",
            Category::Context => "context",
            Category::Shell => "shell",
            Category::Parallel => "parallel",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The file name without `.toml`.
    #[serde(skip)]
    pub id: String,
    pub category: Category,
    pub prompt: String,
    /// The project: paths relative to its root, and their contents.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub generated: Vec<Generated>,
    #[serde(default)]
    pub max_steps: Option<u32>,
    /// Seconds.
    #[serde(default)]
    pub time_limit: Option<u64>,
    /// The text to change is in the file more than once on purpose: the scenario is about
    /// recovering from an edit that is not unique, so its edits count apart from the
    /// first-try rate of the M1 exit gate.
    #[serde(default)]
    pub ambiguous: bool,
    pub expect: Expect,
}

/// A long file made of numbered lines.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generated {
    pub path: String,
    pub lines: usize,
    /// One line; `{n}` is its number, from 1.
    pub line: String,
    /// Lines that differ from the pattern, by number.
    #[serde(default)]
    pub replace: BTreeMap<String, String>,
}

/// What a passing run leaves. Every file of the project the expectations do not name must
/// stay as it was.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Final contents, exactly; a missing or extra final newline aside.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Text each file must hold.
    #[serde(default)]
    pub contains: BTreeMap<String, Vec<String>>,
    /// Text each file must not hold.
    #[serde(default)]
    pub absent: BTreeMap<String, Vec<String>>,
    /// Lines each file must have.
    #[serde(default)]
    pub lines: BTreeMap<String, usize>,
    /// Words the answer must hold, all of them, in any case, as whole words.
    #[serde(default)]
    pub answer: Vec<String>,
    /// Words the answer must hold, at least one.
    #[serde(default)]
    pub answer_any: Vec<String>,
    /// One of these tools must run (a call refused for want of approval does not count).
    #[serde(default)]
    pub tools: Vec<String>,
    /// The answer comes without any call.
    #[serde(default)]
    pub no_calls: bool,
}

impl Expect {
    /// The files the expectations look at.
    fn named(&self) -> HashSet<&str> {
        self.files
            .keys()
            .chain(self.contains.keys())
            .chain(self.absent.keys())
            .chain(self.lines.keys())
            .map(String::as_str)
            .collect()
    }
}

/// Every scenario in `dir`, by id.
pub fn load(dir: &Path) -> Result<Vec<Scenario>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|e| e != "toml") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut s: Scenario =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        s.id = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        s.validate()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        out.push(s);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

impl Scenario {
    fn validate(&self) -> Result<(), String> {
        let e = &self.expect;
        let judged = !e.named().is_empty() || !e.answer.is_empty() || !e.answer_any.is_empty();
        if !judged && !e.no_calls {
            return Err("it expects nothing".into());
        }
        let fixtures = self.files.keys().map(String::as_str);
        let generated = self.generated.iter().map(|g| g.path.as_str());
        for p in fixtures.chain(generated).chain(e.named()) {
            let rel = Path::new(p);
            if rel.is_absolute() || rel.components().any(|c| c.as_os_str() == "..") {
                return Err(format!("{p}: not a path inside the project"));
            }
        }
        if let Some(t) = e.tools.iter().find(|t| !TOOLS.contains(&t.as_str())) {
            return Err(format!("{t}: not one of {TOOLS:?}"));
        }
        for g in &self.generated {
            for n in g.replace.keys() {
                match n.parse::<usize>() {
                    Ok(i) if (1..=g.lines).contains(&i) => {}
                    _ => return Err(format!("{}: no line {n}", g.path)),
                }
            }
        }
        Ok(())
    }

    /// The scenario for `p`: its stand-ins replaced by the profile's own strings.
    pub fn for_profile(&self, p: &ModelProfile) -> Scenario {
        let framing = p.tokens.framing.first().cloned().unwrap_or_default();
        let special = p
            .tokens
            .guarded()
            .find(|t| !p.tokens.framing.iter().any(|f| f == t))
            .unwrap_or_default()
            .to_owned();
        let x = |t: &str| t.replace(FRAMING, &framing).replace(SPECIAL, &special);
        let xs = |v: &[String]| v.iter().map(|t| x(t)).collect::<Vec<_>>();
        let mut s = self.clone();
        s.prompt = x(&s.prompt);
        s.files = s.files.iter().map(|(k, v)| (k.clone(), x(v))).collect();
        let e = &mut s.expect;
        e.files = e.files.iter().map(|(k, v)| (k.clone(), x(v))).collect();
        e.contains = e.contains.iter().map(|(k, v)| (k.clone(), xs(v))).collect();
        e.absent = e.absent.iter().map(|(k, v)| (k.clone(), xs(v))).collect();
        e.answer = xs(&e.answer);
        e.answer_any = xs(&e.answer_any);
        s
    }

    /// Writes the project under `root`; returns every file with its content.
    fn prepare(&self, root: &Path) -> std::io::Result<BTreeMap<String, String>> {
        let mut files = self.files.clone();
        for g in &self.generated {
            let text: String = (1..=g.lines)
                .map(|n| {
                    let line = g
                        .replace
                        .get(&n.to_string())
                        .cloned()
                        .unwrap_or_else(|| g.line.replace("{n}", &n.to_string()));
                    line + "\n"
                })
                .collect();
            files.insert(g.path.clone(), text);
        }
        for (rel, text) in &files {
            let path = root.join(rel);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, text)?;
            if rel.ends_with(".sh") {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }
        Ok(files)
    }
}

/// The server a suite runs against.
#[derive(Debug, Clone)]
pub struct Target {
    pub url: String,
    pub profile: ModelProfile,
    pub n_ctx: Option<u32>,
    /// The Native path instead of the Generic one.
    pub native: bool,
    /// Which tools the model gets.
    pub layout: Layout,
    /// The agent may hand tasks to subagents, which share the server's `slots`.
    pub subagents: bool,
    pub slots: u32,
}

/// One scenario run with one seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub scenario: String,
    pub category: Category,
    pub seed: u32,
    pub passed: bool,
    /// Why it did not pass.
    pub failures: Vec<String>,
    /// `answer`, `step_limit`, `stuck`, `time_limit` or `error`.
    pub end: String,
    pub answer: Option<String>,
    /// The tools that ran, in order: Claude Code's names, or the model's own when a call
    /// did not parse.
    pub tools: Vec<String>,
    /// The calls refused for want of a user's approval.
    #[serde(default)]
    pub refused: Vec<String>,
    pub counts: Counts,
    pub stats: Stats,
    pub ms: u64,
    pub session: Option<i64>,
}

/// What a run adds to the exit metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Main-step generations and the attempts the server could not parse.
    pub attempts: u32,
    /// Attempts that gave calls to run or an answer with text.
    pub usable: u32,
    pub malformed: u32,
    pub truncated: u32,
    pub repeating: u32,
    pub empty_answers: u32,
    pub format_errors: u32,
    /// Calls made from a generation cut by `max_tokens`: must stay 0.
    pub truncated_executed: u32,
}

impl Counts {
    fn add(&mut self, o: &Counts) {
        self.attempts += o.attempts;
        self.usable += o.usable;
        self.malformed += o.malformed;
        self.truncated += o.truncated;
        self.repeating += o.repeating;
        self.empty_answers += o.empty_answers;
        self.format_errors += o.format_errors;
        self.truncated_executed += o.truncated_executed;
    }
}

/// Runs `s` once with `seed`. The work goes under `out`: the project in `work/` (kept
/// when the run fails), the session in `sessions.db`, its trajectory in `traces/`.
pub async fn run(s: &Scenario, t: &Target, seed: u32, out: &Path) -> Result<RunResult, String> {
    let s = s.for_profile(&t.profile);
    let name = format!("{}.{seed}", s.id);
    let work = out.join("work").join(&name);
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    let root = work.join("proj");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(&root).map_err(|e| e.to_string())?;
    let fixtures = s.prepare(&root).map_err(|e| format!("{name}: {e}"))?;

    let db = out.join("sessions.db");
    let store = Store::open(&db).map_err(|e| format!("{}: {e}", db.display()))?;
    let new = NewSession {
        kind: "run".into(),
        root: root.clone(),
        profile: t.profile.id.clone(),
        level: None,
        server: t.url.clone(),
        seed,
    };
    let session = Session::create(store, &new).map_err(|e| e.to_string())?;
    let server = LlamaServer::new(&t.url, None).map_err(|e| e.to_string())?;
    let date = t.native.then(|| native::utc_date(native::now()));
    // alone the agent keeps slot 0, as it always did; with subagents the server places
    // every request, and its prompt cache keeps each conversation (docs/architecture.md §4)
    let slot = (!t.subagents).then_some(0);
    let router = Router::new(
        vec![Endpoint {
            server: server.clone(),
            profile: t.profile.id.clone(),
            slots: t.slots,
        }],
        None,
    );
    let subagents: Option<Arc<dyn Subagents>> = if t.subagents {
        let model = SameModel::new(
            server.clone(),
            t.profile.clone(),
            date.clone(),
            None,
            router.clone(),
        );
        Some(Arc::new(model.map_err(|e| e.to_string())?))
    } else {
        None
    };
    let driver: Box<dyn ModelDriver> = match &date {
        Some(date) => {
            let d = NativeDriver::new(server, t.profile.clone(), None, slot, date);
            Box::new(d.map_err(|e| e.to_string())?)
        }
        None => {
            let d = ChatDriver::new(server, t.profile.clone(), None, slot);
            Box::new(d.map_err(|e| e.to_string())?)
        }
    };
    let driver: Box<dyn ModelDriver> = if t.subagents {
        Box::new(Scheduled::new(
            driver,
            router,
            &t.profile.id,
            Priority::Main,
        ))
    } else {
        driver
    };
    let opts = AgentOptions {
        root: root.clone(),
        data_dir: work.join("data"),
        max_steps: s.max_steps.unwrap_or(MAX_STEPS),
        context: t.profile.context,
        n_ctx: t.n_ctx,
        seed,
        workspace: WorkspaceState::CLEAN,
        time_limit: Some(s.time_limit.map_or(TIME_LIMIT, Duration::from_secs)),
        user_instructions: UserInstructions::default(),
        rules: Rules::default(),
        layout: t.layout,
        toolset: if subagents.is_some() {
            Toolset::Delegating
        } else {
            Toolset::All
        },
        writable: None,
        subagents,
    };
    let mut agent = Agent::new(driver, t.profile.family, Box::new(NoUser), opts);
    agent.keep_in(session);

    let mut calls: Vec<(String, String)> = Vec::new();
    let mut refused_ids: HashSet<String> = HashSet::new();
    // the subagents' calls count too: a suite a subagent ran was run
    let mut on = |e: AgentEvent| {
        let (e, of) = match e {
            AgentEvent::Subagent { index, event } => (*event, format!("{index}.")),
            e => (e, String::new()),
        };
        match e {
            AgentEvent::ToolStarted { id, tool, call, .. } => {
                calls.push((of + &id, call.map_or(tool, |c| c.id().canonical_name())));
            }
            AgentEvent::ToolFinished { id, result, .. } if result.starts_with("[refused]") => {
                refused_ids.insert(of + &id);
            }
            _ => {}
        }
    };
    let started = Instant::now();
    let end = agent.turn(&s.prompt, &mut on).await;
    let ms = started.elapsed().as_millis() as u64;
    let stats = agent.stats.clone();
    let id = agent.close();

    let (ran, refused): (Vec<_>, Vec<_>) = calls
        .into_iter()
        .partition(|(id, _)| !refused_ids.contains(id));
    let tools: Vec<String> = ran.into_iter().map(|(_, name)| name).collect();
    let refused: Vec<String> = refused.into_iter().map(|(_, name)| name).collect();
    let failures = judge(&s, &root, &fixtures, &end, &tools, &refused);
    let mut counts = Counts::default();
    if matches!(end, Err(DriverError::Format { .. })) {
        counts.attempts += 1;
        counts.format_errors += 1;
    }
    if let Some(id) = id {
        let store = Store::open(&db).map_err(|e| e.to_string())?;
        let events = store.events(id).map_err(|e| e.to_string())?;
        counts.add(&count(&events));
        // the subagents' sessions, which the session's log names: their generations count
        // too, and each has a trajectory of its own
        let children: Vec<i64> = events
            .iter()
            .filter(|e| e.kind == "subagent_session")
            .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok()?["session"].as_i64())
            .collect();
        let dir = out.join("traces");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut traces = vec![(id, format!("{name}.json"))];
        for (n, child) in children.into_iter().enumerate() {
            let events = store.events(child).map_err(|e| e.to_string())?;
            counts.add(&count(&events));
            traces.push((child, format!("{name}.sub{}.json", n + 1)));
        }
        for (session, file) in traces {
            if let Ok(trace) = ognivo_core::trace::atif(&store, session) {
                let text = serde_json::to_string_pretty(&trace).unwrap_or_default();
                std::fs::write(dir.join(file), text).map_err(|e| e.to_string())?;
            }
        }
    }
    let passed = failures.is_empty();
    if passed {
        let _ = std::fs::remove_dir_all(&work);
    }
    let (end, answer) = match end {
        Ok(TurnEnd::Answer(a)) => ("answer", Some(a.text)),
        Ok(TurnEnd::StepLimit) => ("step_limit", None),
        Ok(TurnEnd::Stuck(_)) => ("stuck", None),
        Ok(TurnEnd::TimeLimit) => ("time_limit", None),
        Err(_) => ("error", None),
    };
    Ok(RunResult {
        scenario: s.id.clone(),
        category: s.category,
        seed,
        passed,
        failures,
        end: end.into(),
        answer,
        tools,
        refused,
        counts,
        stats,
        ms,
        session: id,
    })
}

/// Why the run does not pass; empty when it does.
fn judge(
    s: &Scenario,
    root: &Path,
    fixtures: &BTreeMap<String, String>,
    end: &Result<TurnEnd, DriverError>,
    tools: &[String],
    refused: &[String],
) -> Vec<String> {
    let e = &s.expect;
    let mut out = Vec::new();
    let answer = match end {
        Ok(TurnEnd::Answer(a)) => Some(a.text.as_str()),
        Ok(TurnEnd::StepLimit) => {
            out.push("the turn reached its step limit".into());
            None
        }
        Ok(TurnEnd::Stuck(why)) => {
            out.push(format!("the turn stopped: {why}"));
            None
        }
        Ok(TurnEnd::TimeLimit) => {
            out.push("the turn reached its time limit".into());
            None
        }
        Err(err) => {
            out.push(format!("the turn failed: {err}"));
            None
        }
    };
    if let Some(text) = answer {
        for w in &e.answer {
            if !mentions(text, w) {
                out.push(format!("the answer does not say {w:?}"));
            }
        }
        if !e.answer_any.is_empty() && !e.answer_any.iter().any(|w| mentions(text, w)) {
            out.push(format!("the answer says none of {:?}", e.answer_any));
        }
    }
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).ok();
    for (rel, want) in &e.files {
        match read(rel) {
            Some(got) if same_text(&got, want) => {}
            Some(got) => out.push(format!("{rel}: {}", first_difference(&got, want))),
            None => out.push(format!("{rel}: missing")),
        }
    }
    for (rel, words) in &e.contains {
        let got = read(rel).unwrap_or_default();
        for w in words.iter().filter(|w| !got.contains(w.as_str())) {
            out.push(format!("{rel}: does not hold {w:?}"));
        }
    }
    for (rel, words) in &e.absent {
        let got = read(rel).unwrap_or_default();
        for w in words.iter().filter(|w| got.contains(w.as_str())) {
            out.push(format!("{rel}: still holds {w:?}"));
        }
    }
    for (rel, n) in &e.lines {
        let got = read(rel).map_or(0, |t| t.lines().count());
        if got != *n {
            out.push(format!("{rel}: {got} lines, not {n}"));
        }
    }
    let named = e.named();
    for (rel, before) in fixtures
        .iter()
        .filter(|(rel, _)| !named.contains(rel.as_str()))
    {
        match read(rel) {
            Some(now) if now == *before => {}
            Some(_) => out.push(format!("{rel}: changed, and the task did not ask for it")),
            None => out.push(format!("{rel}: deleted")),
        }
    }
    if !e.tools.is_empty() && !tools.iter().any(|t| e.tools.contains(t)) {
        out.push(format!("used none of {:?} (used {tools:?})", e.tools));
    }
    if e.no_calls && !(tools.is_empty() && refused.is_empty()) {
        let all = [tools, refused].concat();
        out.push(format!("called {all:?} where no call was needed"));
    }
    out
}

/// Whether `text` holds `word` in any case, not as part of a longer word or number.
fn mentions(text: &str, word: &str) -> bool {
    let (text, word) = (text.to_lowercase(), word.to_lowercase());
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(&word).any(|(i, _)| {
        edge(text[..i].chars().next_back()) && edge(text[i + word.len()..].chars().next())
    })
}

fn same_text(a: &str, b: &str) -> bool {
    a.strip_suffix('\n').unwrap_or(a) == b.strip_suffix('\n').unwrap_or(b)
}

fn first_difference(got: &str, want: &str) -> String {
    let (g, w): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
    match (0..g.len().max(w.len())).find(|&i| g.get(i) != w.get(i)) {
        Some(i) => format!(
            "line {}: {:?}, not {:?}",
            i + 1,
            g.get(i).copied().unwrap_or("<end>"),
            w.get(i).copied().unwrap_or("<end>")
        ),
        None => "differs in line ends".into(),
    }
}

/// The exit-metric counts in a run's event log.
fn count(events: &[ognivo_store::EventRow]) -> Counts {
    let mut c = Counts::default();
    let mut cut: HashSet<String> = HashSet::new();
    for e in events {
        let Ok(v) = serde_json::from_str::<Value>(&e.data) else {
            continue;
        };
        match e.kind.as_str() {
            "generation" if v["purpose"] == "step" => {
                c.attempts += 1;
                match v["outcome"].as_str().unwrap_or_default() {
                    "calls" => c.usable += 1,
                    "answer" if v["content"].as_str().is_some_and(|t| !t.trim().is_empty()) => {
                        c.usable += 1;
                    }
                    "answer" => c.empty_answers += 1,
                    "truncated" => {
                        c.truncated += 1;
                        let ids = v["rejected_calls"].as_array().into_iter().flatten();
                        cut.extend(ids.filter_map(|r| r["id"].as_str().map(str::to_owned)));
                    }
                    "repeating" => c.repeating += 1,
                    _ => c.malformed += 1,
                }
            }
            "restarted" => {
                c.attempts += 1;
                c.format_errors += 1;
            }
            "tool_started" if v["id"].as_str().is_some_and(|id| cut.contains(id)) => {
                c.truncated_executed += 1;
            }
            _ => {}
        }
    }
    c
}

/// A share with its 95% Wilson interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rate {
    pub ok: u64,
    pub of: u64,
    pub value: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
}

impl Rate {
    pub fn new(ok: u64, of: u64) -> Self {
        if of == 0 {
            return Self {
                ok,
                of,
                value: None,
                low: None,
                high: None,
            };
        }
        let (n, p, z) = (of as f64, ok as f64 / of as f64, 1.96_f64);
        let d = 1.0 + z * z / n;
        let center = (p + z * z / (2.0 * n)) / d;
        let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / d;
        Self {
            ok,
            of,
            value: Some(p),
            low: Some((center - half).max(0.0)),
            high: Some((center + half).min(1.0)),
        }
    }
}

/// The suite's results for one profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub profile: String,
    pub server: Value,
    pub seeds: Vec<u32>,
    pub runs: usize,
    pub passed: usize,
    /// Passed and run, by category.
    pub categories: BTreeMap<String, [usize; 2]>,
    pub parse_ok: Rate,
    pub schema_ok: Rate,
    /// Edits applied by their first call, outside the ambiguous scenarios: the M1 exit gate.
    pub edit_first_try: Rate,
    /// The same in the scenarios whose text to change is there twice on purpose
    /// ([`Scenario::ambiguous`]): how often the model makes an edit unique at once.
    pub edit_first_try_ambiguous: Rate,
    /// Prompt tokens taken from the cache, of all prompt tokens.
    pub cache_hit: Rate,
    pub counts: Counts,
    /// Calls that gave the same result as an identical one, and those not run again.
    pub repeated: u64,
    pub repeats_skipped: u64,
    pub completion_tokens: u64,
    pub minutes: f64,
    /// Runs that did not pass, as `scenario.seed`.
    pub failed: Vec<String>,
}

/// The ids of the scenarios whose edits count apart ([`Scenario::ambiguous`]).
pub fn ambiguous(scenarios: &[Scenario]) -> BTreeSet<String> {
    scenarios
        .iter()
        .filter(|s| s.ambiguous)
        .map(|s| s.id.clone())
        .collect()
}

/// The summary of `results`; the edits of the scenarios in `ambiguous` count apart.
pub fn summarize(
    profile: &str,
    server: Value,
    seeds: &[u32],
    results: &[RunResult],
    ambiguous: &BTreeSet<String>,
) -> Summary {
    let mut counts = Counts::default();
    let mut categories: BTreeMap<String, [usize; 2]> = BTreeMap::new();
    let (mut calls, mut bad) = (0u64, 0u64);
    // [plain, ambiguous]
    let (mut applied, mut failed_edits) = ([0u64; 2], [0u64; 2]);
    let (mut processed, mut cached, mut completion, mut ms) = (0u64, 0u64, 0u64, 0u64);
    let (mut repeated, mut skipped) = (0u64, 0u64);
    for r in results {
        counts.add(&r.counts);
        let c = categories.entry(r.category.name().to_owned()).or_default();
        c[0] += usize::from(r.passed);
        c[1] += 1;
        calls += u64::from(r.stats.calls);
        bad += u64::from(r.stats.bad_arguments);
        let kind = usize::from(ambiguous.contains(&r.scenario));
        applied[kind] += u64::from(r.stats.edits_applied);
        failed_edits[kind] += u64::from(r.stats.edits_failed);
        processed += r.stats.prompt_tokens_processed;
        cached += r.stats.prompt_tokens_cached;
        completion += r.stats.completion_tokens;
        repeated += u64::from(r.stats.repeated);
        skipped += u64::from(r.stats.repeats_skipped);
        ms += r.ms;
    }
    Summary {
        profile: profile.into(),
        server,
        seeds: seeds.to_vec(),
        runs: results.len(),
        passed: results.iter().filter(|r| r.passed).count(),
        categories,
        parse_ok: Rate::new(counts.usable.into(), counts.attempts.into()),
        schema_ok: Rate::new(calls - bad.min(calls), calls),
        edit_first_try: Rate::new(applied[0], applied[0] + failed_edits[0]),
        edit_first_try_ambiguous: Rate::new(applied[1], applied[1] + failed_edits[1]),
        cache_hit: Rate::new(cached, cached + processed),
        counts,
        repeated,
        repeats_skipped: skipped,
        completion_tokens: completion,
        minutes: ms as f64 / 60_000.0,
        failed: results
            .iter()
            .filter(|r| !r.passed)
            .map(|r| format!("{}.{}", r.scenario, r.seed))
            .collect(),
    }
}

/// The directory the scenarios of this checkout are in.
pub fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/t2/scenarios")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_matched_as_whole_words() {
        assert!(mentions("It is in `src/range.rs`.", "src/range.rs"));
        assert!(mentions("The code is 7781.", "7781"));
        assert!(!mentions("14 lines", "4"));
        assert!(mentions("Version 3.14.15 it says", "3.14.15"));
        assert!(!mentions("versions", "version"));
    }

    #[test]
    fn wilson_bounds_hold_the_share() {
        let r = Rate::new(49, 50);
        let (low, high) = (r.low.unwrap(), r.high.unwrap());
        assert!(low < 0.98 && 0.98 < high && high <= 1.0, "{r:?}");
        assert!((low - 0.895).abs() < 0.01, "{low}");
        assert_eq!(Rate::new(0, 0).value, None);
    }

    #[test]
    fn files_compare_without_their_final_newline() {
        assert!(same_text("a\nb\n", "a\nb"));
        assert!(!same_text("a\nb\n\n", "a\nb"));
        assert_eq!(
            first_difference("a\nx\n", "a\nb\n"),
            r#"line 2: "x", not "b""#
        );
    }
}
