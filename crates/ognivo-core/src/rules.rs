//! Permission rules (docs/architecture.md §8): deny > ask > allow, on top of the policy of
//! [`crate::approval`], in Claude Code's syntax, so that a project set up for it works as
//! it is.
//!
//! - Sources: the user's `~/.ognivo/config.toml` (`[permissions]`) and Claude Code's
//!   `~/.claude/settings.json` (`permissions`); the project's `.ognivo/config.toml`,
//!   `.claude/settings.json` and `.claude/settings.local.json`. Deny and ask rules count
//!   wherever they come from. The project's allow rules count only once the user has
//!   trusted exactly that set of them ([`Pending`]): a cloned repository must not let
//!   itself run commands.
//! - `Bash(cmd)` matches the command exactly, `Bash(cmd:*)` (or `Bash(cmd *)`) the command
//!   and any words after it, `Bash` every command. A compound command (`&&`, `||`, `;`,
//!   `|`) is allowed by rules only when each part is allowed by a rule or only reads; a
//!   part that sets variables, redirects to a file, or holds shell syntax this does not
//!   parse is never allowed by a rule of words. Deny and ask rules match any part.
//! - `Read(path)` covers read, grep and glob; `Edit(path)` covers edit and write. Paths are
//!   globs: `//abs`, `~/home`, `/in-root` and `in-root` (relative to the project root);
//!   `**` crosses directories, and a path without wildcards covers what is under it too.
//!   Files a deny or ask rule names are left out of grep and glob results, and a file
//!   closed to reading is closed to editing too (an edit's result shows its lines). An
//!   allow rule can lift the question about a read outside the project, never one about an
//!   edit: edits outside the project and of protected paths always ask.
//! - A command's arguments are checked as paths by the deny and ask path rules: against
//!   its cwd, `~/…` against the home, the value of `--opt=PATH`; `cat .env` meets
//!   `Read(./.env)`. Paths a shell builds (globs, `grep -r .`) are
//!   not seen; a command that should never run needs a Bash rule of its own.
//! - `mcp__server` and `mcp__server__*` match every tool of an MCP server, `mcp__server__tool`
//!   one; a tool of an MCP server no rule allows asks.
//! - Rules for tools ognivo does not have yet (WebFetch) are kept for later.

use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use ognivo_tools::command::{Simple, simple_commands};
use serde::Deserialize;

use crate::approval::Decision;

/// What a rule does with what it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Deny,
    Ask,
    Allow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bash,
    Read,
    Edit,
    Mcp,
}

#[derive(Debug, Clone)]
enum Pattern {
    Any,
    /// The words of a command, quotes removed; `prefix`: more words may follow.
    Words {
        words: Vec<String>,
        prefix: bool,
    },
    Path(Vec<GlobMatcher>),
    /// A server's tools: all of them, or one.
    Mcp {
        server: String,
        tool: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Rule {
    action: Action,
    kind: Kind,
    pattern: Pattern,
    /// As written: `Bash(cargo test:*)`.
    pub text: String,
    /// The file it came from, as the user names it: `~/.ognivo/config.toml`.
    pub source: String,
}

impl Rule {
    fn named(&self) -> String {
        format!("{} in {}", self.text, self.source)
    }
}

/// The rules in force.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    rules: Vec<Rule>,
    /// For `~/` in a command's arguments.
    home: Option<PathBuf>,
}

/// The project's allow rules, waiting for the user's trust.
#[derive(Debug, Clone)]
pub struct Pending {
    pub rules: Vec<Rule>,
}

impl Pending {
    /// The rules as one text, the key under which the user's answer is kept.
    pub fn key(&self) -> String {
        let mut lines: Vec<String> = self.rules.iter().map(Rule::named).collect();
        lines.sort();
        lines.join("\n")
    }
}

/// What the rules make of an action the policy decided on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ruling {
    Allow,
    Ask,
    /// Refused without a question; the text names the rule.
    Deny(String),
}

/// What [`load`] found.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub rules: Rules,
    /// The project's allow rules, if it has any; they count after [`Rules::trust`].
    pub pending: Option<Pending>,
    /// Files with rules, in the order read: `~/.ognivo/config.toml (3)`.
    pub files: Vec<String>,
    /// Rules or files that could not be used, with why.
    pub problems: Vec<String>,
}

/// Where rules are looked for.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// Canonical.
    pub root: PathBuf,
    pub home: Option<PathBuf>,
    /// Claude Code's directory, `~/.claude` unless `CLAUDE_CONFIG_DIR` moves it.
    pub claude_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lists {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// `~/.ognivo/config.toml`: only `[permissions]` is read here.
#[derive(Debug, Default, Deserialize)]
struct OgnivoConfig {
    #[serde(default)]
    permissions: Lists,
}

/// Claude Code's settings: only the rule lists of `permissions` are read.
#[derive(Debug, Default, Deserialize)]
struct ClaudeSettings {
    #[serde(default)]
    permissions: ClaudePermissions,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudePermissions {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// Reads the rules of the user and the project.
pub fn load(places: &Places) -> Loaded {
    let mut loaded = Loaded::default();
    loaded.rules.home = places.home.clone();
    let mut pending = Vec::new();
    let home = places.home.as_deref();
    let mut sources: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(home) = home {
        sources.push((home.join(".ognivo/config.toml"), false));
        let claude = places
            .claude_dir
            .clone()
            .unwrap_or_else(|| home.join(".claude"));
        sources.push((claude.join("settings.json"), false));
    }
    for name in [
        ".ognivo/config.toml",
        ".claude/settings.json",
        ".claude/settings.local.json",
    ] {
        sources.push((places.root.join(name), true));
    }
    for (path, of_project) in sources {
        let shown = shown(&path, places);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                loaded.problems.push(format!("{shown}: {e}"));
                continue;
            }
        };
        let lists = if path.extension().is_some_and(|x| x == "toml") {
            toml::from_str::<OgnivoConfig>(&text)
                .map(|c| c.permissions)
                .map_err(|e| e.message().to_owned())
        } else {
            serde_json::from_str::<ClaudeSettings>(&text)
                .map(|s| Lists {
                    allow: s.permissions.allow,
                    ask: s.permissions.ask,
                    deny: s.permissions.deny,
                })
                .map_err(|e| e.to_string())
        };
        let lists = match lists {
            Ok(lists) => lists,
            Err(e) => {
                loaded.problems.push(format!("{shown}: {e}"));
                continue;
            }
        };
        let mut count = 0;
        for (action, texts) in [
            (Action::Deny, &lists.deny),
            (Action::Ask, &lists.ask),
            (Action::Allow, &lists.allow),
        ] {
            for text in texts {
                match parse(text, action, &shown, places) {
                    Ok(Some(rule)) if of_project && action == Action::Allow => {
                        pending.push(rule);
                        count += 1;
                    }
                    Ok(Some(rule)) => {
                        loaded.rules.rules.push(rule);
                        count += 1;
                    }
                    Ok(None) => count += 1,
                    Err(why) => loaded.problems.push(format!("{text} in {shown}: {why}")),
                }
            }
        }
        if count > 0 {
            loaded.files.push(format!("{shown} ({count})"));
        }
    }
    loaded.rules.order();
    if !pending.is_empty() {
        loaded.pending = Some(Pending { rules: pending });
    }
    loaded
}

/// One rule; `None` for a tool ognivo does not have yet.
fn parse(
    text: &str,
    action: Action,
    source: &str,
    places: &Places,
) -> Result<Option<Rule>, String> {
    let text = text.trim();
    let (tool, spec) = match text.split_once('(') {
        Some((tool, rest)) => {
            let spec = rest
                .strip_suffix(')')
                .ok_or("a rule in parentheses ends with `)`")?;
            (tool, Some(spec))
        }
        None => (text, None),
    };
    let kind = match tool {
        "Bash" => Kind::Bash,
        "Read" | "Grep" | "Glob" | "LS" => Kind::Read,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Kind::Edit,
        "WebFetch" | "WebSearch" | "Task" | "TodoWrite" | "NotebookRead" => return Ok(None),
        t if t.starts_with("mcp__") => {
            if spec.is_some() {
                return Err("an MCP rule has no parentheses".into());
            }
            let rest = &t["mcp__".len()..];
            let (server, tool) = match rest.split_once("__") {
                Some((s, "*")) => (s, None),
                Some((s, t)) => (s, Some(t.to_owned())),
                None => (rest, None),
            };
            if server.is_empty() {
                return Err("no server".into());
            }
            return Ok(Some(Rule {
                action,
                kind: Kind::Mcp,
                pattern: Pattern::Mcp {
                    server: server.to_owned(),
                    tool,
                },
                text: text.to_owned(),
                source: source.to_owned(),
            }));
        }
        _ => return Err(format!("no tool {tool:?}")),
    };
    let pattern = match (kind, spec.map(str::trim)) {
        (_, None) | (Kind::Bash, Some("*")) => Pattern::Any,
        (_, Some("")) => return Err("empty parentheses".into()),
        (Kind::Mcp, Some(_)) => return Err("an MCP rule has no parentheses".into()),
        (Kind::Bash, Some(spec)) => {
            let (spec, prefix) = match spec.strip_suffix(":*").or_else(|| spec.strip_suffix(" *")) {
                Some(head) => (head.trim_end(), true),
                None => (spec, false),
            };
            match simple_commands(spec).as_deref() {
                Some([c]) if !c.assigns && !c.redirects && !c.words.is_empty() => Pattern::Words {
                    words: c.words.clone(),
                    prefix,
                },
                _ => return Err("not one plain command".into()),
            }
        }
        (Kind::Read | Kind::Edit, Some(spec)) => Pattern::Path(path_globs(spec, places)?),
    };
    Ok(Some(Rule {
        action,
        kind,
        pattern,
        text: text.to_owned(),
        source: source.to_owned(),
    }))
}

/// The globs a path rule matches: the path, and what is under it when it has no wildcards.
fn path_globs(spec: &str, places: &Places) -> Result<Vec<GlobMatcher>, String> {
    let escaped = |p: &Path| globset::escape(&p.display().to_string());
    let full = if let Some(rest) = spec.strip_prefix("//") {
        format!("/{rest}")
    } else if let Some(rest) = spec.strip_prefix("~/") {
        let home = places.home.as_deref().ok_or("no home directory for `~/`")?;
        format!("{}/{rest}", escaped(home))
    } else {
        let rest = spec.strip_prefix('/').unwrap_or(spec);
        let rest = rest.strip_prefix("./").unwrap_or(rest);
        format!("{}/{rest}", escaped(&places.root))
    };
    let full = full.trim_end_matches('/').to_owned();
    let mut globs = vec![full.clone()];
    if !spec.contains(['*', '?', '[', '{']) {
        globs.push(format!("{full}/**"));
    }
    globs
        .iter()
        .map(|g| {
            GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .map(|g| g.compile_matcher())
                .map_err(|e| e.kind().to_string())
        })
        .collect()
}

fn shown(path: &Path, places: &Places) -> String {
    if let Ok(rel) = path.strip_prefix(&places.root) {
        return rel.display().to_string();
    }
    match places.home.as_deref().map(|h| path.strip_prefix(h)) {
        Some(Ok(rel)) => format!("~/{}", rel.display()),
        _ => path.display().to_string(),
    }
}

impl Rules {
    /// Adds the project's allow rules the user trusted.
    pub fn trust(&mut self, pending: Pending) {
        self.rules.extend(pending.rules);
        self.order();
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Deny first, then ask, then allow.
    fn order(&mut self) {
        self.rules.sort_by_key(|r| match r.action {
            Action::Deny => 0,
            Action::Ask => 1,
            Action::Allow => 2,
        });
    }

    fn of(&self, kind: Kind, action: Action) -> impl Iterator<Item = &Rule> {
        self.rules
            .iter()
            .filter(move |r| r.kind == kind && r.action == action)
    }

    /// A tool of an MCP server: what a rule says, or a question.
    pub fn mcp(&self, server: &str, tool: &str) -> Ruling {
        let hit = |action| {
            self.of(Kind::Mcp, action).find(|r| match &r.pattern {
                Pattern::Mcp { server: s, tool: t } => {
                    s == server && t.as_deref().is_none_or(|t| t == tool)
                }
                _ => false,
            })
        };
        if let Some(r) = hit(Action::Deny) {
            return Ruling::Deny(r.named());
        }
        if hit(Action::Ask).is_some() {
            return Ruling::Ask;
        }
        if hit(Action::Allow).is_some() {
            return Ruling::Allow;
        }
        Ruling::Ask
    }

    /// A shell command run in `cwd`, given the policy's decision on it. `read_only` says
    /// whether a plain part of a compound command only reads, as the classifier sees it.
    pub fn command(
        &self,
        command: &str,
        cwd: &Path,
        policy: Decision,
        read_only: impl Fn(&str) -> bool,
    ) -> Ruling {
        let split = simple_commands(command);
        // a command this cannot split is cut roughly, for deny and ask only
        let words: Vec<Vec<String>> = match &split {
            Some(parts) => parts.iter().map(|p| p.words.clone()).collect(),
            None => rough(command),
        };
        let hits = |action| {
            self.of(Kind::Bash, action)
                .find(|r| words.iter().any(|w| r.matches_words(w)))
        };
        // the arguments that may be paths, as the command would open them
        let paths: Vec<PathBuf> = words
            .iter()
            .flat_map(|w| w.iter().skip(1))
            .filter_map(|a| match a.strip_prefix('-') {
                // `--file=PATH`
                Some(option) => option.split_once('=').map(|(_, v)| v),
                None => Some(a.as_str()),
            })
            .map(|a| match (a, a.strip_prefix("~/"), self.home.as_deref()) {
                ("~", _, Some(home)) => home.to_owned(),
                (_, Some(rest), Some(home)) => ognivo_tools::files::resolve(home, rest),
                _ => ognivo_tools::files::resolve(cwd, a),
            })
            .collect();
        let named = |action| {
            self.rules.iter().find(|r| {
                r.kind != Kind::Bash
                    && r.action == action
                    && paths.iter().any(|p| r.matches_path(p))
            })
        };
        if let Some(r) = hits(Action::Deny) {
            return Ruling::Deny(r.named());
        }
        if let Some(r) = named(Action::Deny) {
            return Ruling::Deny(r.named());
        }
        if hits(Action::Ask).is_some() || named(Action::Ask).is_some() {
            return Ruling::Ask;
        }
        if policy == Decision::Allow {
            return Ruling::Allow;
        }
        let allow: Vec<&Rule> = self.of(Kind::Bash, Action::Allow).collect();
        if allow.iter().any(|r| matches!(r.pattern, Pattern::Any)) {
            return Ruling::Allow;
        }
        let Some(parts) = split else {
            return Ruling::Ask;
        };
        let covered = |p: &Simple| {
            !p.assigns && !p.redirects && allow.iter().any(|r| r.matches_words(&p.words))
                || read_only(&p.text)
        };
        if parts.iter().all(covered) {
            Ruling::Allow
        } else {
            Ruling::Ask
        }
    }

    /// A read of `path` (read, grep, glob), given the policy's decision.
    pub fn read(&self, path: &Path, policy: Decision) -> Ruling {
        self.path(Kind::Read, path, policy, true)
    }

    /// An edit or write of `path`, given the policy's decision; no rule lifts its question,
    /// and a path closed to reading is closed to editing.
    pub fn edit(&self, path: &Path, policy: Decision) -> Ruling {
        match self.path(Kind::Read, path, Decision::Allow, false) {
            Ruling::Allow => self.path(Kind::Edit, path, policy, false),
            closed => closed,
        }
    }

    fn path(&self, kind: Kind, path: &Path, policy: Decision, liftable: bool) -> Ruling {
        if let Some(r) = self.of(kind, Action::Deny).find(|r| r.matches_path(path)) {
            return Ruling::Deny(r.named());
        }
        if self.of(kind, Action::Ask).any(|r| r.matches_path(path)) {
            return Ruling::Ask;
        }
        let lifted = liftable && self.of(kind, Action::Allow).any(|r| r.matches_path(path));
        if policy == Decision::Allow || lifted {
            Ruling::Allow
        } else {
            Ruling::Ask
        }
    }

    /// Whether grep and glob leave out this file: a deny or ask rule names it.
    pub fn hides(&self, path: &Path) -> bool {
        self.rules
            .iter()
            .any(|r| r.kind == Kind::Read && r.action != Action::Allow && r.matches_path(path))
    }
}

impl Rule {
    fn matches_words(&self, words: &[String]) -> bool {
        match &self.pattern {
            Pattern::Any => true,
            Pattern::Words {
                words: rule,
                prefix: true,
            } => words.starts_with(rule),
            Pattern::Words {
                words: rule,
                prefix: false,
            } => words == rule.as_slice(),
            Pattern::Path(_) | Pattern::Mcp { .. } => false,
        }
    }

    fn matches_path(&self, path: &Path) -> bool {
        match &self.pattern {
            Pattern::Any => true,
            Pattern::Path(globs) => globs.iter().any(|g| g.is_match(path)),
            Pattern::Words { .. } | Pattern::Mcp { .. } => false,
        }
    }
}

/// The words of a command line cut at its separators, quotes and all: what deny and ask
/// rules see of a command [`simple_commands`] cannot split.
fn rough(command: &str) -> Vec<Vec<String>> {
    command
        .split(['\n', ';', '|', '&'])
        .map(|piece| {
            piece
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Rules {
        /// [`Rules::command`] run in the project root of these tests.
        fn command_in(&self, command: &str, policy: Decision, ro: impl Fn(&str) -> bool) -> Ruling {
            self.command(command, Path::new("/p"), policy, ro)
        }
    }

    fn rules(allow: &[&str], ask: &[&str], deny: &[&str]) -> Rules {
        let places = Places {
            root: PathBuf::from("/p"),
            home: Some(PathBuf::from("/h")),
            claude_dir: None,
        };
        let mut rules = Rules {
            home: places.home.clone(),
            ..Rules::default()
        };
        for (action, list) in [
            (Action::Allow, allow),
            (Action::Ask, ask),
            (Action::Deny, deny),
        ] {
            for text in list {
                rules
                    .rules
                    .push(parse(text, action, "test", &places).unwrap().unwrap());
            }
        }
        rules.order();
        rules
    }

    #[test]
    fn commands_meet_deny_then_ask_then_allow() {
        let r = rules(
            &["Bash(cargo test:*)", "Bash(git status)", "Bash(npm run *)"],
            &["Bash(git push:*)"],
            &["Bash(rm:*)"],
        );
        let ro = |t: &str| t == "tail -n 20";
        let asks = Decision::Ask;
        assert_eq!(r.command_in("cargo test", asks, ro), Ruling::Allow);
        assert_eq!(
            r.command_in("cargo test -p x --offline", asks, ro),
            Ruling::Allow
        );
        assert_eq!(
            r.command_in("cargo test 2>&1 | tail -n 20", asks, ro),
            Ruling::Allow
        );
        assert_eq!(r.command_in("npm run lint", asks, ro), Ruling::Allow);
        // whole words only, and exact means exact
        assert_eq!(r.command_in("cargo tests", asks, ro), Ruling::Ask);
        assert_eq!(r.command_in("git status -s", asks, ro), Ruling::Ask);
        // every part must be covered
        assert_eq!(r.command_in("cargo test && curl x", asks, ro), Ruling::Ask);
        // variables and redirections to files are never allowed by rules of words
        assert_eq!(
            r.command_in("RUSTC_WRAPPER=x cargo test", asks, ro),
            Ruling::Ask
        );
        assert_eq!(r.command_in("cargo test > out", asks, ro), Ruling::Ask);
        // deny matches any part, and the first words of what does not split
        assert_eq!(
            r.command_in("cargo test && rm -rf target", asks, ro),
            Ruling::Deny("Bash(rm:*) in test".into())
        );
        assert_eq!(
            r.command_in("rm -rf $(pwd)", Decision::Allow, ro),
            Ruling::Deny("Bash(rm:*) in test".into())
        );
        assert_eq!(
            r.command_in("echo hi; rm -rf $(pwd)", Decision::Allow, ro),
            Ruling::Deny("Bash(rm:*) in test".into())
        );
        // ask wins over the policy and over allow
        assert_eq!(
            r.command_in("git push origin", Decision::Allow, ro),
            Ruling::Ask
        );
        assert_eq!(r.command_in("ls", Decision::Allow, ro), Ruling::Allow);
        assert_eq!(r.command_in("echo $(id)", asks, ro), Ruling::Ask);
        let all = rules(&["Bash"], &[], &[]);
        assert_eq!(all.command_in("echo $(id)", asks, ro), Ruling::Allow);
    }

    #[test]
    fn an_mcp_tool_meets_its_servers_rules_and_asks_without_one() {
        let r = rules(
            &["mcp__github", "mcp__docs__search"],
            &["mcp__github__merge_pr"],
            &["mcp__github__delete_repo", "mcp__shell__*"],
        );
        assert_eq!(r.mcp("github", "list_issues"), Ruling::Allow);
        assert_eq!(r.mcp("github", "merge_pr"), Ruling::Ask);
        assert!(
            matches!(r.mcp("github", "delete_repo"), Ruling::Deny(why) if why.starts_with("mcp__github__delete_repo"))
        );
        assert!(matches!(r.mcp("shell", "run"), Ruling::Deny(_)));
        assert_eq!(r.mcp("docs", "search"), Ruling::Allow);
        assert_eq!(r.mcp("docs", "fetch"), Ruling::Ask);
        assert_eq!(r.mcp("other", "x"), Ruling::Ask);
        let places = Places {
            root: PathBuf::from("/p"),
            home: None,
            claude_dir: None,
        };
        assert!(parse("mcp__github(x)", Action::Allow, "t", &places).is_err());
        assert!(parse("mcp__", Action::Allow, "t", &places).is_err());
    }

    #[test]
    fn paths_are_globs_from_the_root_the_home_or_the_top() {
        let r = rules(
            &["Read(~/notes/**)", "Edit(//tmp/**)"],
            &["Read(./.env)"],
            &["Read(secrets)", "Edit(/docs/*.md)"],
        );
        let asks = Decision::Ask;
        let p = |s: &str| PathBuf::from(s);
        assert_eq!(r.read(&p("/p/.env"), Decision::Allow), Ruling::Ask);
        // a path without wildcards covers what is under it
        assert_eq!(
            r.read(&p("/p/secrets/key.pem"), Decision::Allow),
            Ruling::Deny("Read(secrets) in test".into())
        );
        assert_eq!(r.read(&p("/p/src/lib.rs"), Decision::Allow), Ruling::Allow);
        // an allow rule lifts a read's question outside the project
        assert_eq!(r.read(&p("/h/notes/a/b.md"), asks), Ruling::Allow);
        assert_eq!(r.read(&p("/h/other.md"), asks), Ruling::Ask);
        // `*` stays within a directory
        assert_eq!(
            r.edit(&p("/p/docs/a.md"), Decision::Allow),
            Ruling::Deny("Edit(/docs/*.md) in test".into())
        );
        assert_eq!(r.edit(&p("/p/docs/x/a.md"), Decision::Allow), Ruling::Allow);
        // closed to reading, closed to editing
        assert_eq!(r.edit(&p("/p/.env"), Decision::Allow), Ruling::Ask);
        assert!(matches!(
            r.edit(&p("/p/secrets/k"), Decision::Allow),
            Ruling::Deny(_)
        ));
        // a command naming such a path meets the same rules
        let ro = |_: &str| true;
        assert_eq!(
            r.command_in("cat secrets/key.pem", Decision::Allow, ro),
            Ruling::Deny("Read(secrets) in test".into())
        );
        assert_eq!(
            r.command_in("head -n 3 ./.env", Decision::Allow, ro),
            Ruling::Ask
        );
        assert_eq!(
            r.command_in("tool --input=.env", Decision::Allow, ro),
            Ruling::Ask
        );
        let home = rules(&[], &[], &["Read(~/.ssh/**)"]);
        assert!(matches!(
            home.command_in("cat ~/.ssh/id_rsa", Decision::Allow, ro),
            Ruling::Deny(_)
        ));
        assert_eq!(
            r.command_in("cat src/lib.rs", Decision::Allow, ro),
            Ruling::Allow
        );
        // no rule lifts an edit's question
        assert_eq!(r.edit(&p("/tmp/scratch"), asks), Ruling::Ask);
        assert!(r.hides(&p("/p/.env")) && r.hides(&p("/p/secrets/x")));
        assert!(!r.hides(&p("/h/notes/a.md")));
    }

    #[test]
    fn rules_come_from_the_user_and_the_project_whose_allow_rules_wait_for_trust() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(dir.path()).unwrap();
        let root = home.join("p[1]");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(home.join(".ognivo")).unwrap();
        std::fs::write(
            home.join(".ognivo/config.toml"),
            "[permissions]\nallow = [\"Bash(cargo test:*)\"]\ndeny = [\"Bash(git push:*)\"]\n\n[servers]\nmain = 1\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".claude/settings.json"),
            r#"{"permissions": {"allow": ["Bash(npm run lint)", "WebFetch", "Bash(foo"], "deny": ["Read(./.env)"]}, "model": "x"}"#,
        )
        .unwrap();
        std::fs::write(root.join(".claude/settings.local.json"), "{ not json").unwrap();
        let places = Places {
            root: root.clone(),
            home: Some(home.clone()),
            claude_dir: None,
        };
        let loaded = load(&places);
        assert_eq!(
            loaded.files,
            ["~/.ognivo/config.toml (2)", ".claude/settings.json (3)"]
        );
        assert_eq!(loaded.problems.len(), 2, "{:?}", loaded.problems);
        assert!(loaded.problems[0].starts_with("Bash(foo in .claude/settings.json: "));
        assert!(loaded.problems[1].starts_with(".claude/settings.local.json: "));
        let mut rules = loaded.rules;
        // the project's deny counts at once (and the root's brackets are literal)
        assert!(matches!(
            rules.read(&root.join(".env"), Decision::Allow),
            Ruling::Deny(_)
        ));
        assert_eq!(
            rules.command_in("cargo test", Decision::Ask, |_| false),
            Ruling::Allow
        );
        // its allow rules only once trusted
        assert_eq!(
            rules.command_in("npm run lint", Decision::Ask, |_| false),
            Ruling::Ask
        );
        let pending = loaded.pending.unwrap();
        assert_eq!(pending.key(), "Bash(npm run lint) in .claude/settings.json");
        rules.trust(pending);
        assert_eq!(
            rules.command_in("npm run lint", Decision::Ask, |_| false),
            Ruling::Allow
        );
        assert!(matches!(
            rules.command_in("git push", Decision::Ask, |_| false),
            Ruling::Deny(_)
        ));
    }
}
