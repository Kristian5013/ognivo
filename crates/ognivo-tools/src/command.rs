//! Shell command classification for the approval policy (docs/architecture.md §8).
//!
//! tree-sitter-bash parses the command line into plain commands ([`simple_commands`]). A
//! syntax error, or anything this does not follow (substitutions, variable expansions,
//! subshells and braces, control flow, functions, here-documents, background jobs), makes
//! the whole line `SideEffects`, which asks. The line is `ReadOnly` or `ExecutesProjectCode`
//! only when each plain command in it is; the worst one decides. A plain command is
//! recognised when:
//! - its words are plain once quotes are removed: no glob, braces or `~` the shell would
//!   expand, no variables set in front of it, no redirection but to `/dev/null` or between
//!   descriptors (`2>&1`);
//! - it runs with its working directory inside the project root; `cd` into a directory of
//!   the project is followed, and the commands after it are checked from every directory
//!   they may run in;
//! - wrappers are seen through: `timeout`, `nice`, `time`, `env` without settings, and
//!   `bash -c` or `sh -c` with a script that is classified the same way;
//! - each option is on the tool's allowlist (getopt tools also take short flags together,
//!   `-la`, `-rn`), and each path argument resolves, symlinks included, to a location inside
//!   the project root;
//! - for `cat`/`head`/`tail`/`wc` and the hex dumpers, each path is an existing regular
//!   file (no FIFOs, devices or links to `/dev/zero`);
//! - a script of the project (`bash x.sh`, `python3 x.py`, `./x.sh`) is project code when it
//!   is a regular file inside the root and its arguments that are not options stay inside.
//!
//! The classifier only decides whether to ask. The executor must also run commands in a
//! hermetic form: git with `-c core.fsmonitor=false --no-pager` (plus `--no-ext-diff
//! --no-textconv` for diff/log/show), rg with `--no-config` and no `RIPGREP_CONFIG_PATH`,
//! no `*_API_KEY`/`*_TOKEN` in the child environment, and a hard time and output cap for
//! `ReadOnly` commands. Checks here are not atomic with execution (the filesystem can
//! change in between); the agent's own Read/Grep/Glob tools use `openat2` confinement.

use std::path::{Component, Path, PathBuf};

use tree_sitter::{Node, Parser};

use crate::Effect;

/// `bash -c` inside `bash -c`, at most.
const MAX_NESTING: usize = 3;
/// Programs whose first argument is a script for them to run.
const INTERPRETERS: [&str; 7] = ["bash", "sh", "python", "python3", "node", "ruby", "perl"];

/// Where a command would run.
#[derive(Debug, Clone)]
pub struct ExecContext {
    /// Project root, already canonical.
    pub root: PathBuf,
    /// Current working directory of the agent's shell; persists between commands.
    pub cwd: PathBuf,
}

impl ExecContext {
    /// Resolves `arg` against the cwd; through symlinks when the path exists.
    fn resolve(&self, arg: &str) -> PathBuf {
        resolve(&self.cwd, arg)
    }

    fn inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

/// Lexical normalisation for paths that do not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Allowed options of one read-only or project-code tool.
struct Spec {
    /// Options that take no value.
    flags: &'static [&'static str],
    /// Options whose value is the next word and is not a path (counts, patterns, types).
    with_value: &'static [&'static str],
    /// Short options that may carry a number glued on (`-n5`, `-U3`, `-5`).
    numeric_prefixes: &'static [&'static str],
    /// Positional arguments must be existing regular files.
    files_only: bool,
    /// Short flags may come together (`-la`): the tool parses options with getopt.
    clusters: bool,
    /// Positional arguments at most.
    max_positional: usize,
}

const NONE: &[&str] = &[];

fn s(
    flags: &'static [&'static str],
    with_value: &'static [&'static str],
    numeric_prefixes: &'static [&'static str],
    files_only: bool,
) -> Spec {
    Spec {
        flags,
        with_value,
        numeric_prefixes,
        files_only,
        clusters: false,
        max_positional: usize::MAX,
    }
}

impl Spec {
    fn clustered(mut self) -> Self {
        self.clusters = true;
        self
    }

    fn at_most(mut self, n: usize) -> Self {
        self.max_positional = n;
        self
    }
}

/// The allowlist for a command and how many leading words name it.
fn spec_for(words: &[&str]) -> Option<(Spec, usize)> {
    Some(match words {
        ["git", "status", ..] => (
            s(
                &[
                    "-s",
                    "--short",
                    "-b",
                    "--branch",
                    "--porcelain",
                    "-v",
                    "--ignored",
                    "-u",
                    "--no-renames",
                ],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["git", "diff", ..] => (
            s(
                &[
                    "--stat",
                    "--cached",
                    "--staged",
                    "--name-only",
                    "--name-status",
                    "--shortstat",
                    "--numstat",
                    "-w",
                    "--ignore-all-space",
                    "--color",
                    "--no-color",
                    "--word-diff",
                    "-M",
                    "--summary",
                    "--",
                ],
                NONE,
                &["-U"],
                false,
            ),
            2,
        ),
        ["git", "log", ..] => (
            s(
                &[
                    "--oneline",
                    "--stat",
                    "--graph",
                    "--decorate",
                    "-p",
                    "--name-only",
                    "--name-status",
                    "--reverse",
                    "--first-parent",
                    "--",
                ],
                &["-n", "--max-count"],
                &["-n", "-"],
                false,
            ),
            2,
        ),
        ["git", "show", ..] => (
            s(
                &[
                    "--stat",
                    "--name-only",
                    "--name-status",
                    "-p",
                    "--oneline",
                    "--",
                ],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["git", "branch", ..] => (
            s(
                &["-a", "-r", "-v", "-vv", "--list", "--show-current"],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["ls", ..] => (
            s(
                &[
                    "-l", "-a", "-A", "-h", "-1", "-R", "-t", "-S", "-r", "-d", "-F", "-p", "-i",
                    "-s", "-n",
                ],
                NONE,
                NONE,
                false,
            )
            .clustered(),
            1,
        ),
        ["pwd"] => (s(NONE, NONE, NONE, false), 1),
        ["sort", ..] => (
            s(
                &["-n", "-r", "-u", "-h", "-V", "-f", "-b"],
                &["-k", "-t"],
                NONE,
                false,
            )
            .clustered(),
            1,
        ),
        ["uniq", ..] => (
            s(&["-c", "-d", "-u", "-i"], NONE, NONE, false).clustered(),
            1,
        ),
        ["cut", ..] => (s(NONE, &["-d", "-f", "-c", "-b"], NONE, false), 1),
        ["cat", ..] => (
            s(
                &["-n", "-b", "-A", "-v", "-e", "-E", "-t", "-T", "-s"],
                NONE,
                NONE,
                true,
            )
            .clustered(),
            1,
        ),
        ["head" | "tail", ..] => (s(NONE, &["-n", "-c"], &["-n", "-c", "-"], true), 1),
        ["wc", ..] => (
            s(&["-l", "-w", "-c", "-m"], NONE, NONE, true).clustered(),
            1,
        ),
        ["hexdump", ..] => (
            s(
                &["-C", "-c", "-b", "-d", "-o", "-x", "-v"],
                &["-n", "-s"],
                NONE,
                true,
            )
            .clustered(),
            1,
        ),
        ["od", ..] => (
            s(
                &["-c", "-a", "-b", "-d", "-o", "-x", "-v"],
                &["-A", "-t", "-N", "-j"],
                NONE,
                true,
            )
            .clustered(),
            1,
        ),
        // `xxd IN OUT` writes OUT, and `-r` turns a dump back into a file
        ["xxd", ..] => (
            s(
                &["-p", "-u", "-b", "-e", "-i"],
                &["-l", "-s", "-c", "-g"],
                NONE,
                true,
            )
            .at_most(1),
            1,
        ),
        ["rg", ..] => (
            s(
                &[
                    "-n",
                    "-i",
                    "-S",
                    "-s",
                    "-w",
                    "-l",
                    "-c",
                    "-F",
                    "-x",
                    "-v",
                    "--files",
                    "--hidden",
                    "--no-ignore",
                    "-u",
                    "-uu",
                    "--count",
                    "--line-number",
                    "--ignore-case",
                    "--smart-case",
                    "--word-regexp",
                    "--fixed-strings",
                    "--files-with-matches",
                    "--no-heading",
                    "--heading",
                    "--",
                ],
                &[
                    "-e",
                    "-t",
                    "-T",
                    "--type",
                    "-m",
                    "--max-count",
                    "-A",
                    "-B",
                    "-C",
                ],
                &["-A", "-B", "-C", "-m"],
                false,
            )
            .clustered(),
            1,
        ),
        // -r does not follow symlinks; -R does and is not listed
        ["grep", ..] => (
            s(
                &[
                    "-n", "-i", "-r", "-l", "-c", "-w", "-v", "-E", "-F", "-x", "-H", "-h", "-o",
                    "-s", "-q", "--",
                ],
                &["-e", "-m", "-A", "-B", "-C"],
                &["-A", "-B", "-C", "-m"],
                false,
            )
            .clustered(),
            1,
        ),
        ["find", ..] => (
            s(
                &["-print", "-empty", "-not", "-o", "-a"],
                &[
                    "-name",
                    "-iname",
                    "-type",
                    "-maxdepth",
                    "-mindepth",
                    "-path",
                    "-ipath",
                    "-size",
                    "-mtime",
                ],
                NONE,
                false,
            ),
            1,
        ),
        [
            "cargo",
            "test" | "build" | "check" | "run" | "clippy" | "bench",
            ..,
        ] => (
            s(
                &[
                    "--release",
                    "--all-targets",
                    "--workspace",
                    "--lib",
                    "--bins",
                    "--tests",
                    "--no-run",
                    "--quiet",
                    "-q",
                    "--offline",
                    "--locked",
                    "--",
                ],
                &[
                    "-p",
                    "--package",
                    "--bin",
                    "--test",
                    "--example",
                    "--bench",
                    "--features",
                    "-F",
                    "-j",
                    "--jobs",
                    "--profile",
                ],
                NONE,
                false,
            ),
            2,
        ),
        ["npm" | "pnpm" | "yarn", "test" | "run" | "start", ..] => {
            (s(&["--silent", "--"], NONE, NONE, false), 2)
        }
        ["pytest", ..] => (
            s(
                &["-q", "-x", "-v", "-vv", "-s", "--lf", "--ff", "--no-header"],
                &["-k", "-m", "--maxfail"],
                NONE,
                false,
            ),
            1,
        ),
        ["make" | "just", ..] => (s(&["-n", "-s", "-k"], &["-j"], &["-j"], false), 1),
        ["ctest", ..] => (
            s(
                &["--output-on-failure", "-V", "-N"],
                &["-R", "-E", "-j"],
                &["-j"],
                false,
            ),
            1,
        ),
        ["go", "test" | "build" | "vet", ..] => (
            s(
                &["-v", "-race", "-short"],
                &["-run", "-count", "-timeout"],
                NONE,
                false,
            ),
            2,
        ),
        _ => return None,
    })
}

fn effect_of(words: &[&str]) -> Effect {
    match words {
        [
            "cargo" | "npm" | "pnpm" | "yarn" | "pytest" | "make" | "just" | "ctest" | "go",
            ..,
        ] => Effect::ExecutesProjectCode,
        _ => Effect::ReadOnly,
    }
}

/// One plain command of a command line, as the shell runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simple {
    /// As written.
    pub text: String,
    /// Its words with quotes and escapes removed, its name first; the variables set in
    /// front of it are not among them.
    pub words: Vec<String>,
    /// It sets variables for itself.
    pub assigns: bool,
    /// It redirects to or from a file other than `/dev/null`.
    pub redirects: bool,
    /// One of its words has a glob, braces or `~` that the shell expands.
    pub expands: bool,
}

/// A command line as the shell runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Plain(Simple),
    /// `a; b` and line by line: each runs whatever the one before did.
    Seq(Vec<Line>),
    /// `a && b`: `b` runs when `a` succeeded.
    And(Box<Line>, Box<Line>),
    /// `a || b`: `b` runs when `a` failed.
    Or(Box<Line>, Box<Line>),
    /// `a | b`: each in a subshell of its own.
    Pipe(Vec<Line>),
    /// `! a`
    Not(Box<Line>),
}

impl Line {
    fn flatten(self, out: &mut Vec<Simple>) {
        match self {
            Line::Plain(c) => out.push(c),
            Line::Seq(items) | Line::Pipe(items) => items.into_iter().for_each(|l| l.flatten(out)),
            Line::And(a, b) | Line::Or(a, b) => {
                a.flatten(out);
                b.flatten(out);
            }
            Line::Not(a) => a.flatten(out),
        }
    }

    fn redirected(&mut self) {
        match self {
            Line::Plain(c) => c.redirects = true,
            Line::Seq(items) | Line::Pipe(items) => items.iter_mut().for_each(Line::redirected),
            Line::And(a, b) | Line::Or(a, b) => {
                a.redirected();
                b.redirected();
            }
            Line::Not(a) => a.redirected(),
        }
    }
}

/// The plain commands of a command line, in order: those of `&&` and `||` lists, of
/// pipelines (`|`, `|&`), of `;` and line breaks, and the one after `!`. `None` when the
/// line has a syntax error or anything else: substitutions, variable expansions,
/// subshells, braces, control flow, functions, here-documents, background jobs, or a line
/// continuation.
pub fn simple_commands(command: &str) -> Option<Vec<Simple>> {
    let mut out = Vec::new();
    parse(command)?.flatten(&mut out);
    Some(out)
}

fn parse(command: &str) -> Option<Line> {
    // `\` before a line break joins words in a way the tree does not show
    if command.contains("\\\n") {
        return None;
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(command, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut items = Vec::new();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match (child.is_named(), child.kind()) {
            (false, ";" | "\n") | (true, "comment") => {}
            // `&` puts a job in the background, out of the agent's sight
            (false, _) => return None,
            (true, _) => items.push(statement(child, command)?),
        }
    }
    Some(Line::Seq(items))
}

fn statement(node: Node<'_>, src: &str) -> Option<Line> {
    let mut cursor = node.walk();
    Some(match node.kind() {
        "command" => Line::Plain(simple(node, src)?),
        "list" => {
            let mut line: Option<Line> = None;
            let mut op = None;
            for child in node.children(&mut cursor) {
                if !child.is_named() {
                    op = Some(child.kind().to_owned());
                    continue;
                }
                let next = statement(child, src)?;
                line = Some(match (line.take(), op.take().as_deref()) {
                    (None, None) => next,
                    (Some(l), Some("&&")) => Line::And(Box::new(l), Box::new(next)),
                    (Some(l), Some("||")) => Line::Or(Box::new(l), Box::new(next)),
                    _ => return None,
                });
            }
            line?
        }
        "pipeline" => {
            let mut items = Vec::new();
            for child in node.children(&mut cursor) {
                match (child.is_named(), child.kind()) {
                    (false, "|" | "|&") => {}
                    (true, _) => items.push(statement(child, src)?),
                    (false, _) => return None,
                }
            }
            Line::Pipe(items)
        }
        "negated_command" => {
            let inner: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            let [inner] = inner.as_slice() else {
                return None;
            };
            Line::Not(Box::new(statement(*inner, src)?))
        }
        "redirected_statement" => {
            let mut harmful = false;
            for r in node.children_by_field_name("redirect", &mut cursor) {
                if r.kind() != "file_redirect" {
                    return None;
                }
                harmful |= !harmless(r, src)?;
            }
            let mut line = statement(node.child_by_field_name("body")?, src)?;
            if harmful {
                line.redirected();
            }
            line
        }
        _ => return None,
    })
}

fn simple(node: Node<'_>, src: &str) -> Option<Simple> {
    let mut c = Simple {
        text: src[node.byte_range()].trim().to_owned(),
        words: Vec::new(),
        assigns: false,
        redirects: false,
        expands: false,
    };
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // its value may run something (`X=$(cmd)`), so it must be plain too
            "variable_assignment" => {
                if let Some(value) = child.child_by_field_name("value") {
                    word(value, src)?;
                }
                c.assigns = true;
            }
            "command_name" => {
                let (w, expands) = word(child.named_child(0)?, src)?;
                c.words.push(w);
                c.expands |= expands;
            }
            "file_redirect" => c.redirects |= !harmless(child, src)?,
            _ => {
                let (w, expands) = word(child, src)?;
                c.words.push(w);
                c.expands |= expands;
            }
        }
    }
    Some(c)
}

/// A redirection that writes nowhere and reads nothing: to `/dev/null`, or from one
/// descriptor to another (`2>&1`, `>&2`, `2>&-`).
fn harmless(redirect: Node<'_>, src: &str) -> Option<bool> {
    let mut cursor = redirect.walk();
    let op = redirect
        .children(&mut cursor)
        .find(|c| !c.is_named())?
        .kind()
        .to_owned();
    // `2>&-` closes a descriptor: the operator holds it all
    let Some(dest) = redirect.child_by_field_name("destination") else {
        return Some(op.ends_with('-'));
    };
    let target = &src[dest.byte_range()];
    Some(match op.as_str() {
        ">&" | "<&" => dest.kind() == "number" || target == "-" || target == "/dev/null",
        ">" | ">>" | "&>" | "&>>" | ">|" => target == "/dev/null",
        _ => false,
    })
}

/// The text of a plain word, quotes and escapes removed, and whether the shell would
/// expand it; `None` for anything with a value only known when it runs.
fn word(node: Node<'_>, src: &str) -> Option<(String, bool)> {
    let text = &src[node.byte_range()];
    match node.kind() {
        "word" => Some(unquoted(text)),
        "number" => Some((text.to_owned(), false)),
        "raw_string" => Some((text.get(1..text.len() - 1)?.to_owned(), false)),
        "string" => {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .any(|c| c.kind() != "string_content")
            {
                return None;
            }
            Some((double_quoted(text.get(1..text.len() - 1)?), false))
        }
        "concatenation" => {
            let mut out = String::new();
            let mut expands = false;
            let mut cursor = node.walk();
            for piece in node.named_children(&mut cursor) {
                let (w, e) = word(piece, src)?;
                out.push_str(&w);
                expands |= e;
            }
            Some((out, expands))
        }
        _ => None,
    }
}

/// An unquoted word: `\x` is `x`; an unescaped glob character, brace or leading `~` means
/// the shell expands it.
fn unquoted(raw: &str) -> (String, bool) {
    let mut out = String::with_capacity(raw.len());
    let mut expands = raw.starts_with('~');
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '*' | '?' | '[' | '{' | '}' => {
                expands = true;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    (out, expands)
}

/// The inside of a `"…"` without expansions: `\` escapes only `$`, `` ` ``, `"` and `\`.
fn double_quoted(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some('$' | '`' | '"' | '\\')) => out.extend(chars.next()),
            _ => out.push(c),
        }
    }
    out
}

/// Classifies `command` as it would run in `ctx`.
pub fn classify_command(command: &str, ctx: &ExecContext) -> Effect {
    match parse(command) {
        Some(line) => run(&line, &ctx.root, std::slice::from_ref(&ctx.cwd), 0).effect,
        None => Effect::SideEffects,
    }
}

/// What a line does when run from any of some directories, and where the shell may be
/// after it succeeded and after it failed.
struct Outcome {
    effect: Effect,
    ok: Vec<PathBuf>,
    failed: Vec<PathBuf>,
}

fn run(line: &Line, root: &Path, cwds: &[PathBuf], depth: usize) -> Outcome {
    let same = |effect| Outcome {
        effect,
        ok: cwds.to_vec(),
        failed: cwds.to_vec(),
    };
    match line {
        Line::Plain(c) => {
            if c.assigns || c.redirects || c.expands {
                return same(Effect::SideEffects);
            }
            let words: Vec<&str> = c.words.iter().map(String::as_str).collect();
            match words.as_slice() {
                ["cd", dir] | ["cd", "--", dir] => {
                    let mut ok = Vec::new();
                    for cwd in cwds {
                        let to = resolve(cwd, dir);
                        if !to.starts_with(root) || !to.is_dir() {
                            return same(Effect::SideEffects);
                        }
                        if !ok.contains(&to) {
                            ok.push(to);
                        }
                    }
                    Outcome {
                        effect: Effect::ReadOnly,
                        ok,
                        failed: cwds.to_vec(),
                    }
                }
                words => same(plain(words, root, cwds, depth)),
            }
        }
        Line::Seq(items) => {
            let mut effect = Effect::ReadOnly;
            let mut now = cwds.to_vec();
            let mut last = same(Effect::ReadOnly);
            for item in items {
                last = run(item, root, &now, depth);
                effect = worse(effect, last.effect);
                now = union(&last.ok, &last.failed);
            }
            Outcome {
                effect,
                ok: last.ok,
                failed: last.failed,
            }
        }
        Line::And(a, b) => {
            let a = run(a, root, cwds, depth);
            let b = run(b, root, &a.ok, depth);
            Outcome {
                effect: worse(a.effect, b.effect),
                failed: union(&a.failed, &b.failed),
                ok: b.ok,
            }
        }
        Line::Or(a, b) => {
            let a = run(a, root, cwds, depth);
            let b = run(b, root, &a.failed, depth);
            Outcome {
                effect: worse(a.effect, b.effect),
                ok: union(&a.ok, &b.ok),
                failed: b.failed,
            }
        }
        // every part in a subshell: none moves the shell
        Line::Pipe(items) => same(
            items
                .iter()
                .map(|l| run(l, root, cwds, depth).effect)
                .fold(Effect::ReadOnly, worse),
        ),
        Line::Not(a) => {
            let a = run(a, root, cwds, depth);
            Outcome {
                effect: a.effect,
                ok: a.failed,
                failed: a.ok,
            }
        }
    }
}

fn union(a: &[PathBuf], b: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = a.to_vec();
    for p in b {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// One plain command, not `cd`, run from any of `cwds`.
fn plain(words: &[&str], root: &Path, cwds: &[PathBuf], depth: usize) -> Effect {
    match words {
        ["timeout", rest @ ..] => match after_timeout(rest) {
            Some(inner) => plain(inner, root, cwds, depth),
            None => Effect::SideEffects,
        },
        ["nice", "-n", n, rest @ ..] if n.parse::<i32>().is_ok() => plain(rest, root, cwds, depth),
        ["nice" | "time" | "env", rest @ ..]
            if rest
                .first()
                .is_some_and(|w| !w.starts_with('-') && !w.contains('=')) =>
        {
            plain(rest, root, cwds, depth)
        }
        // a script of its own: what it does with its directory stays in it
        ["bash" | "sh", "-c", script] if depth < MAX_NESTING => match parse(script) {
            Some(line) => cwds
                .iter()
                .map(|cwd| run(&line, root, std::slice::from_ref(cwd), depth + 1).effect)
                .fold(Effect::ReadOnly, worse),
            None => Effect::SideEffects,
        },
        // `cd` inside a wrapper moves nothing the agent sees
        ["cd", ..] => Effect::SideEffects,
        ["echo", ..] | ["true"] | ["false"] => Effect::ReadOnly,
        ["python" | "python3", "-m", "pytest", rest @ ..] => {
            let mut inner = vec!["pytest"];
            inner.extend_from_slice(rest);
            plain(&inner, root, cwds, depth)
        }
        [interpreter, file, args @ ..]
            if INTERPRETERS.contains(interpreter) && !file.starts_with('-') =>
        {
            script(file, args, root, cwds)
        }
        [path, args @ ..] if path.contains('/') => script(path, args, root, cwds),
        _ => cwds
            .iter()
            .map(|cwd| tool(words, root, cwd))
            .fold(Effect::ReadOnly, worse),
    }
}

/// A script of the project, run from each of `cwds`: project code when it is a regular file
/// inside the root and its arguments that are not options resolve inside it too.
fn script(file: &str, args: &[&str], root: &Path, cwds: &[PathBuf]) -> Effect {
    cwds.iter()
        .map(|cwd| {
            let ctx = ExecContext {
                root: root.to_owned(),
                cwd: cwd.to_owned(),
            };
            let path = ctx.resolve(file);
            let ours = ctx.inside(&ctx.resolve("."))
                && ctx.inside(&path)
                && std::fs::metadata(&path).is_ok_and(|m| m.is_file());
            let outside = args
                .iter()
                .any(|a| a.contains('@') || (!a.starts_with('-') && !ctx.inside(&ctx.resolve(a))));
            if ours && !outside {
                Effect::ExecutesProjectCode
            } else {
                Effect::SideEffects
            }
        })
        .fold(Effect::ReadOnly, worse)
}

/// The command `timeout [options] DURATION` runs.
fn after_timeout<'a>(rest: &'a [&'a str]) -> Option<&'a [&'a str]> {
    let mut i = 0;
    while let Some(&w) = rest.get(i) {
        match w {
            "-k" | "-s" | "--kill-after" | "--signal" => i += 2,
            "--preserve-status" | "--foreground" | "-v" | "--verbose" => i += 1,
            _ if w.starts_with("--kill-after=") || w.starts_with("--signal=") => i += 1,
            _ if w.starts_with('-') => return None,
            _ => break,
        }
    }
    let duration = rest.get(i)?;
    let number = duration.trim_end_matches(['s', 'm', 'h', 'd']);
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(&rest[i + 1..])
}

/// The worse of two effects for asking: the order is the order of risk.
fn worse(a: Effect, b: Effect) -> Effect {
    let rank = |e: Effect| match e {
        Effect::ReadOnly => 0,
        Effect::ExecutesProjectCode => 1,
        Effect::EditsFiles => 2,
        Effect::Network => 3,
        Effect::SideEffects => 4,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// A tool on the allowlist, run from `cwd`.
fn tool(words: &[&str], root: &Path, cwd: &Path) -> Effect {
    let ctx = ExecContext {
        root: root.to_owned(),
        cwd: cwd.to_owned(),
    };
    if !ctx.inside(&ctx.resolve(".")) {
        return Effect::SideEffects;
    }
    let Some((spec, skip)) = spec_for(words) else {
        return Effect::SideEffects;
    };
    let mut args = words[skip..].iter();
    let mut after_dashdash = false;
    let mut positional = 0;
    while let Some(&arg) = args.next() {
        if !after_dashdash && arg.starts_with('-') && arg.len() > 1 {
            if arg == "--" {
                after_dashdash = true;
            } else if spec.with_value.contains(&arg) {
                if args.next().is_none() {
                    return Effect::SideEffects;
                }
            } else if !spec.flags.contains(&arg)
                && !numeric_flag(arg, spec.numeric_prefixes)
                && !(spec.clusters && cluster(arg, spec.flags))
            {
                return Effect::SideEffects;
            }
            continue;
        }
        positional += 1;
        if positional > spec.max_positional {
            return Effect::SideEffects;
        }
        // positional: a path, a ref, a package or a pattern; anything resolving outside is refused
        if arg.contains('@') {
            return Effect::SideEffects; // `go run mod@latest`, scp-like remotes
        }
        let path = ctx.resolve(arg);
        if !ctx.inside(&path) {
            return Effect::SideEffects;
        }
        if spec.files_only && !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            return Effect::SideEffects;
        }
    }
    effect_of(words)
}

/// `arg` against `cwd`, through symlinks when the path exists.
fn resolve(cwd: &Path, arg: &str) -> PathBuf {
    let joined = cwd.join(arg);
    std::fs::canonicalize(&joined).unwrap_or_else(|_| normalize(&joined))
}

/// `-la`, `-rn`: allowed one-letter flags written together.
fn cluster(arg: &str, flags: &[&str]) -> bool {
    let Some(letters) = arg.strip_prefix('-') else {
        return false;
    };
    let short = |c: char| {
        flags
            .iter()
            .any(|f| f.len() == 2 && f.starts_with('-') && f.ends_with(c))
    };
    letters.len() > 1 && letters.chars().all(|c| c.is_ascii_alphabetic() && short(c))
}

/// `-n5`, `-U3`, `-20`: an allowed short option with only digits glued on.
fn numeric_flag(arg: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        arg.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: std::path::PathBuf,
        ctx: ExecContext,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A throwaway project: src/lib.rs, a symlink to /etc and a FIFO-like non-file.
    fn project(name: &str) -> Fixture {
        let base =
            std::env::temp_dir().join(format!("ognivo-cmd-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::os::unix::fs::symlink("/etc", base.join("etc-link")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", base.join("zero")).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        Fixture {
            dir: base,
            ctx: ExecContext {
                cwd: root.clone(),
                root,
            },
        }
    }

    #[test]
    fn reads_inside_the_project() {
        let f = project("reads");
        let c = |cmd| classify_command(cmd, &f.ctx);
        assert_eq!(c("git status"), Effect::ReadOnly);
        assert_eq!(c("git diff --stat"), Effect::ReadOnly);
        assert_eq!(c("git log --oneline -n 5"), Effect::ReadOnly);
        assert_eq!(c("git diff HEAD~1 -- src"), Effect::ReadOnly);
        assert_eq!(c("cat src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("head -n 20 src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("rg -n TODO src"), Effect::ReadOnly);
        assert_eq!(c("git branch"), Effect::ReadOnly);
        assert_eq!(c("cargo test -p ognivo-core"), Effect::ExecutesProjectCode);
    }

    #[test]
    fn short_flags_together_and_hex_dumps_only_read() {
        let f = project("clusters");
        let c = |cmd| classify_command(cmd, &f.ctx);
        // M1 T2-mini: gpt-oss asked for each of these and was refused
        for cmd in [
            "ls -A .",
            "ls -ltrA src",
            "grep -rn TODO src",
            "rg -in todo src",
            "cat -A src/lib.rs",
            "cat -An src/lib.rs",
            "sort -rn src/lib.rs",
            "wc -lc src/lib.rs",
            "hexdump -C src/lib.rs",
            "od -c src/lib.rs",
            "xxd -l 64 src/lib.rs",
        ] {
            assert_eq!(c(cmd), Effect::ReadOnly, "{cmd}");
        }
        for cmd in [
            "sort -rno out.txt src/lib.rs", // -o writes
            "grep -rR key .",               // -R follows symlinks
            "rg -iL key",
            "ls -lL etc-link",
            "find . -name x -delete",
            "xxd src/lib.rs src/lib.rs", // the second file is overwritten
            "xxd -r src/lib.rs",
            "head -n5x src/lib.rs",
        ] {
            assert_eq!(c(cmd), Effect::SideEffects, "{cmd}");
        }
    }

    #[test]
    fn the_projects_own_scripts_are_project_code() {
        let f = project("scripts");
        std::fs::create_dir_all(f.ctx.root.join("scripts")).unwrap();
        std::fs::write(f.ctx.root.join("scripts/run.sh"), "#!/bin/sh\necho ok\n").unwrap();
        std::fs::write(f.ctx.root.join("tool.py"), "print(1)\n").unwrap();
        let c = |cmd| classify_command(cmd, &f.ctx);
        for cmd in [
            "bash scripts/run.sh",
            "sh scripts/run.sh --verbose release",
            "./scripts/run.sh",
            "scripts/run.sh",
            "python3 tool.py src/lib.rs",
            "python3 -m pytest -q",
            "timeout 60 bash scripts/run.sh",
        ] {
            assert_eq!(c(cmd), Effect::ExecutesProjectCode, "{cmd}");
        }
        for cmd in [
            "bash scripts/missing.sh",
            "bash -x scripts/run.sh",
            "bash scripts/run.sh /etc/passwd",
            "bash etc-link/profile",
            "python3 -c 'print(1)'",
            "python3 -m http.server",
            "/usr/bin/cat src/lib.rs",
            "./zero",
            "node - < tool.py",
        ] {
            assert_eq!(c(cmd), Effect::SideEffects, "{cmd}");
        }
    }

    #[test]
    fn refuses_what_it_cannot_vouch_for() {
        let f = project("refuse");
        let c = |cmd| classify_command(cmd, &f.ctx);
        for cmd in [
            "git status && rm -rf target",
            "cat secrets > /tmp/x",
            "echo $(id)",
            "rm -rf build",
            "FOO=1 cargo test",
            "cat /etc/shadow",
            "cat ../other/secret",
            "cat etc-link/passwd", // symlink out of the project
            "cat zero",            // not a regular file
            "cat ~/.ssh/id_ed25519",
            "tail -f build.log",
            "git branch -D main",
            "git diff --output patch.txt",
            "git diff --ext-diff",
            "git diff --textconv",
            "git log --show-signature",
            "git diff --no-index a b",
            "rg --pre sh pattern",
            "rg -L secret",
            "rg --hostname-bin x y",
            "rg -z x",
            "grep -R key .",
            "grep -f/abs/patterns src",
            "find . -delete",
            "find . -fprint out.txt",
            "find -L . -name x",
            "wc --files0-from list",
            "go run example.com/tool@latest",
            "cargo test --manifest-path /tmp/x/Cargo.toml",
            "cargo clippy --fix",
            "make -C /",
            "npm run --prefix ../other x",
            "pytest --rootdir ..",
            "pytest /tmp/elsewhere",
        ] {
            assert_eq!(c(cmd), Effect::SideEffects, "{cmd}");
        }
    }

    fn words(command: &str) -> Option<Vec<Vec<String>>> {
        simple_commands(command).map(|cs| cs.into_iter().map(|c| c.words).collect())
    }

    fn w(list: &[&[&str]]) -> Option<Vec<Vec<String>>> {
        Some(
            list.iter()
                .map(|p| p.iter().map(|s| (*s).to_owned()).collect())
                .collect(),
        )
    }

    #[test]
    fn a_command_line_splits_into_plain_commands() {
        assert_eq!(words("cargo test"), w(&[&["cargo", "test"]]));
        assert_eq!(
            words("cargo test 2>&1 | tail -n 20"),
            w(&[&["cargo", "test"], &["tail", "-n", "20"]])
        );
        assert_eq!(
            words("git add -A && git commit -m \"fix the \\\"bug\\\"\"; git status"),
            w(&[
                &["git", "add", "-A"],
                &["git", "commit", "-m", "fix the \"bug\""],
                &["git", "status"]
            ])
        );
        assert_eq!(
            words("make 2>/dev/null || true\n! git diff --quiet"),
            w(&[&["make"], &["true"], &["git", "diff", "--quiet"]])
        );
        assert_eq!(
            words("echo 'a; b' >/dev/null 2>&1 # done"),
            w(&[&["echo", "a; b"]])
        );
        assert_eq!(
            words("rm a\\ b --name='x y'"),
            w(&[&["rm", "a b", "--name=x y"]])
        );
        assert_eq!(words("make |& tail"), w(&[&["make"], &["tail"]]));
        for bad in [
            "echo $(rm -rf x)",
            "echo `id`",
            "echo \"$HOME\"",
            "echo $'a\\tb'",
            "X=$(id) cmd",
            "(cd x && make)",
            "{ make; }",
            "if true; then ls; fi",
            "f() { ls; }",
            "x=1",
            "export X=1",
            "cat <<EOF\nhi\nEOF",
            "make &",
            "a && b &",
            "a && && b",
            "make &&",
            "make |",
            "echo 'open",
            "echo a\\\nb",
        ] {
            assert_eq!(simple_commands(bad), None, "{bad}");
        }
        let c = &simple_commands("FOO=1 BAR=\"2\" cargo test").unwrap()[0];
        assert!(c.assigns && c.words == ["cargo", "test"]);
        assert!(!simple_commands("echo FOO=1").unwrap()[0].assigns);
        for (cmd, redirects) in [
            ("make > out.log", true),
            ("make 2> err.log", true),
            ("cat < input", true),
            ("make >> /dev/null", false),
            ("make &> /dev/null", false),
            ("make 2>&-", false),
        ] {
            assert_eq!(
                simple_commands(cmd).unwrap()[0].redirects,
                redirects,
                "{cmd}"
            );
        }
        for (cmd, expands) in [
            ("ls *.rs", true),
            ("ls ~/x", true),
            ("echo {a,b}", true),
            ("ls \\*.rs", false),
            ("find . -name '*.rs'", false),
        ] {
            assert_eq!(simple_commands(cmd).unwrap()[0].expands, expands, "{cmd}");
        }
    }

    #[test]
    fn a_line_is_as_safe_as_its_worst_command() {
        let f = project("lines");
        let c = |cmd| classify_command(cmd, &f.ctx);
        assert_eq!(
            c("cargo test 2>&1 | tail -n 20"),
            Effect::ExecutesProjectCode
        );
        assert_eq!(
            c("cargo test -p x 2>&1 | grep FAILED | sort | uniq -c"),
            Effect::ExecutesProjectCode
        );
        assert_eq!(c("grep -n \"fn main\" src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("cat 'src/lib.rs' && echo done"), Effect::ReadOnly);
        assert_eq!(c("git status >/dev/null 2>&1 || true"), Effect::ReadOnly);
        // cd into the project is followed, out of it is not
        assert_eq!(c("cd src && cat lib.rs"), Effect::ReadOnly);
        assert_eq!(c("cd src && cat ../src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("cd .. && ls"), Effect::SideEffects);
        // after a cd that may not happen, a path must hold from both places
        assert_eq!(c("cd src || true; cat lib.rs"), Effect::SideEffects);
        // wrappers are seen through
        assert_eq!(c("timeout 60 cargo test"), Effect::ExecutesProjectCode);
        assert_eq!(c("timeout -k 5 1.5m git status"), Effect::ReadOnly);
        assert_eq!(c("time cargo build"), Effect::ExecutesProjectCode);
        assert_eq!(c("bash -c 'cat src/lib.rs | wc -l'"), Effect::ReadOnly);
        assert_eq!(c("sh -c \"cd src && ls\""), Effect::ReadOnly);
        for cmd in [
            "bash -c 'rm -rf src'",
            "timeout 60 rm -rf src",
            "timeout x cargo test",
            "env FOO=1 cargo test",
            "nice -n x make",
            "cat src/*.rs",
            "ls ~",
            "git status > status.txt",
            "cargo test &",
            "bash -c 'bash -c \"bash -c \\\"bash -c ls\\\"\"'",
        ] {
            assert_eq!(c(cmd), Effect::SideEffects, "{cmd}");
        }
    }

    #[test]
    fn cwd_outside_the_project_is_never_silent() {
        let f = project("cwd");
        let outside = ExecContext {
            root: f.ctx.root.clone(),
            cwd: f.ctx.root.parent().unwrap().to_path_buf(),
        };
        assert_eq!(
            classify_command("git status", &outside),
            Effect::SideEffects
        );
        assert_eq!(
            classify_command("cargo test", &outside),
            Effect::SideEffects
        );
    }
}
