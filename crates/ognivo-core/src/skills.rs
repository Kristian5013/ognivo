//! Skills and custom slash commands, read as Claude Code reads them (docs/architecture.md
//! §9, decision 18), so that a project set up for it works as it is.
//!
//! - A skill is `.claude/skills/<name>/SKILL.md`, with whatever files its directory holds;
//!   a command, `.claude/commands/<name>.md`, is the same thing without a directory of its
//!   own (one in a subdirectory is named with it: `frontend/component.md` is
//!   `frontend:component`). Both are the user's (in
//!   `~/.claude`, or Claude Code's `CLAUDE_CONFIG_DIR`) or the project's: those of the root
//!   and of every directory above it up to the top of its git work tree, the nearest first,
//!   and those of directories below the root, which the conversation hears of with the
//!   first call that reads or changes a file under them.
//! - On a name clash the user's wins over the project's, a nearer directory's over a
//!   farther one's, and a skill over a command; the others are named among the problems.
//!   A project's skill file must stay inside the project, as its instruction files must.
//! - The model gets each skill's name and description (and `when_to_use`) in the project
//!   message, within a budget, and loads a skill with the skill tool: the body of its file,
//!   rendered. `disable-model-invocation` keeps a skill for the user, `user-invocable:
//!   false` for the model.
//! - The user runs one as `/name arguments`; the model gets that line and the rendered body.
//! - Rendering fills `$ARGUMENTS`, `$ARGUMENTS[N]` and `$N` (from 0: `$0` is the first
//!   argument; quotes keep words together), the names of `arguments`,
//!   `${CLAUDE_SKILL_DIR}`, `${CLAUDE_PROJECT_DIR}` and `${CLAUDE_SESSION_ID}`; arguments
//!   that no placeholder takes are added as `ARGUMENTS: …`, and `\$` is a plain `$`. Then
//!   the commands of `` !`command` `` (at a line's start or after a blank) and of ```` ```! ````
//!   blocks run, and their output takes their place ([`Part`]): nothing asks while a skill
//!   renders, so a command that would ask stops it.
//! - `allowed-tools` allow what they name, as rules, for the rest of the turn that runs the
//!   skill; the project's count once the user has trusted them with the project's other
//!   allow rules ([`crate::rules`]).
//! - Not read yet: `model`, `effort`, `context: fork` and `agent` (the skill runs in the
//!   conversation), `hooks`, `paths`, `shell` and `disallowed-tools`.

use std::collections::HashSet;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::frontmatter::Document;
use crate::rules::Places;

/// Characters of the skills' list in a message: past them a skill is named without its
/// description, as Claude Code cuts descriptions to its budget.
pub const LIST_CHARS: usize = 8_000;
/// Characters of one skill's description in the list (Claude Code: 1,536).
const ENTRY_CHARS: usize = 1_536;
/// Bytes read of a skill's file.
const MAX_READ: u64 = 1 << 20;
const NAME_MAX: usize = 256;
/// How deep below the root, and through how many directories, skills are looked for.
const NESTED_DEPTH: usize = 8;
const NESTED_DIRS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    User,
    Project,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// What it does and when to use it: `description` (else the body's first paragraph),
    /// then `when_to_use`.
    pub description: String,
    pub argument_hint: Option<String>,
    /// Names that `$name` takes the arguments by, in their order.
    pub arguments: Vec<String>,
    pub model_invocable: bool,
    pub user_invocable: bool,
    /// The rules of `allowed-tools`, as written.
    pub allowed_tools: Vec<String>,
    /// SKILL.md, or the command's file.
    pub file: PathBuf,
    /// The skill's directory; a command's is its file's.
    pub dir: PathBuf,
    pub scope: Scope,
    /// A command of `.claude/commands`.
    pub command: bool,
    /// The file as the user names it: `.claude/skills/pdf/SKILL.md`, `~/.claude/…`.
    pub shown: String,
    /// For a skill of a directory below the root: that directory, whose files bring it.
    pub under: Option<PathBuf>,
}

impl Skill {
    /// Whose it is and what, for messages: "the project's command".
    pub fn kind(&self) -> String {
        format!(
            "{} {}",
            match self.scope {
                Scope::User => "the user's",
                Scope::Project => "the project's",
            },
            if self.command { "command" } else { "skill" }
        )
    }

    /// The body rendered with `arguments`, cut where its commands are to run.
    pub fn render(&self, arguments: &str, vars: &Vars<'_>) -> Result<Vec<Part>, String> {
        let text = read(&self.file).map_err(|e| format!("{}: {e}", self.shown))?;
        let doc = Document::parse(&text);
        let dir = self.dir.display().to_string();
        let lookup = |name: &str| match name {
            "CLAUDE_SKILL_DIR" => Some(dir.clone()),
            "CLAUDE_PROJECT_DIR" => Some(vars.project_dir.display().to_string()),
            "CLAUDE_SESSION_ID" => Some(vars.session_id.to_owned()),
            _ => None,
        };
        Ok(commands(&fill(
            &doc.body,
            arguments,
            &self.arguments,
            lookup,
        )))
    }
}

/// What `${…}` gives in a skill's body besides the skill's own directory.
#[derive(Debug, Clone, Copy)]
pub struct Vars<'a> {
    pub project_dir: &'a Path,
    pub session_id: &'a str,
}

/// A rendered body: its text, and the commands whose output goes in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(String),
    /// `` !`command` `` (`block`: false) or a ```` ```! ```` block.
    Run {
        command: String,
        block: bool,
    },
}

impl Part {
    /// The placeholder as the body wrote it, for messages about it.
    pub fn pattern(command: &str, block: bool) -> String {
        if block {
            format!("```!\n{command}\n```")
        } else {
            format!("!`{command}`")
        }
    }
}

/// The skills a session has.
#[derive(Debug, Clone, Default)]
pub struct Skills {
    all: Vec<Skill>,
    /// Files that could not be read or used, and skills another one's name hides.
    pub problems: Vec<String>,
    /// Directories below the root whose skills the conversation has heard of.
    told: HashSet<PathBuf>,
}

/// Finds the skills and commands of the user and the project.
pub fn load(places: &Places) -> Skills {
    let mut found = Finder {
        places,
        top: places
            .root
            .ancestors()
            .find(|d| d.join(".git").exists())
            .unwrap_or(&places.root)
            .to_path_buf(),
        skills: Vec::new(),
        files: HashSet::new(),
        problems: Vec::new(),
    };
    let claude = places
        .claude_dir
        .clone()
        .or_else(|| places.home.as_ref().map(|h| h.join(".claude")));
    if let Some(claude) = &claude {
        found.skills_in(&claude.join("skills"), Scope::User, None);
        found.commands_in(&claude.join("commands"), Scope::User);
    }
    let above: Vec<PathBuf> = places
        .root
        .ancestors()
        .take_while(|d| d.starts_with(&found.top))
        .map(Path::to_path_buf)
        .collect();
    for dir in &above {
        found.skills_in(&dir.join(".claude/skills"), Scope::Project, None);
    }
    for dir in &above {
        found.commands_in(&dir.join(".claude/commands"), Scope::Project);
    }
    for dir in nested(&places.root) {
        found.skills_in(
            &dir.join(".claude/skills"),
            Scope::Project,
            Some(dir.clone()),
        );
    }
    let mut skills = Skills {
        problems: found.problems,
        ..Skills::default()
    };
    for s in found.skills {
        match skills.all.iter().find(|k| k.name == s.name) {
            Some(k) => skills.problems.push(format!(
                "{}: not used, {} {} has the name {}",
                s.shown,
                k.kind(),
                k.shown,
                s.name
            )),
            None => skills.all.push(s),
        }
    }
    skills
}

/// The directories below `root` that have `.claude/skills`, as git sees the tree: neither
/// ignored nor hidden ones.
fn nested(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let walk = ignore::WalkBuilder::new(root)
        .max_depth(Some(NESTED_DEPTH))
        .filter_entry(|e| e.file_type().is_some_and(|t| t.is_dir()))
        .build();
    for entry in walk.flatten().take(NESTED_DIRS) {
        let dir = entry.path();
        if dir != root && dir.join(".claude/skills").is_dir() {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs.sort();
    dirs
}

struct Finder<'a> {
    places: &'a Places,
    /// The project's files must stay inside this: its git work tree, or the root.
    top: PathBuf,
    skills: Vec<Skill>,
    /// Real paths already read: a link or the home as the project adds nothing.
    files: HashSet<PathBuf>,
    problems: Vec<String>,
}

impl Finder<'_> {
    fn skills_in(&mut self, dir: &Path, scope: Scope, under: Option<PathBuf>) {
        let mut dirs: Vec<PathBuf> = match fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("SKILL.md").is_file())
                .collect(),
            Err(_) => return,
        };
        dirs.sort();
        for d in dirs {
            let name = d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            self.read(&d.join("SKILL.md"), &d, name, scope, false, under.clone());
        }
    }

    fn commands_in(&mut self, dir: &Path, scope: Scope) {
        let mut files = Vec::new();
        let mut dirs = vec![dir.to_path_buf()];
        while let Some(d) = dirs.pop() {
            let Ok(entries) = fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "md") {
                    files.push(p);
                }
            }
        }
        files.sort();
        for f in files {
            // `frontend/component.md` is `frontend:component`
            let rel = f.strip_prefix(dir).unwrap_or(&f).with_extension("");
            let name = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join(":");
            let parent = f.parent().unwrap_or(dir).to_path_buf();
            self.read(&f, &parent, name, scope, true, None);
        }
    }

    fn read(
        &mut self,
        file: &Path,
        dir: &Path,
        default_name: String,
        scope: Scope,
        command: bool,
        under: Option<PathBuf>,
    ) {
        let shown = shown(file, self.places);
        let real = match file.canonicalize() {
            Ok(real) => real,
            Err(e) => {
                self.problems.push(format!("{shown}: {e}"));
                return;
            }
        };
        if scope == Scope::Project && !real.starts_with(&self.top) {
            self.problems
                .push(format!("{shown}: not used, it points outside the project"));
            return;
        }
        if !self.files.insert(real) {
            return;
        }
        let text = match read(file) {
            Ok(text) => text,
            Err(e) => {
                self.problems.push(format!("{shown}: {e}"));
                return;
            }
        };
        let doc = Document::parse(&text);
        if let Some(p) = &doc.problem {
            self.problems.push(format!("{shown}: {p}"));
        }
        // a command is named by its file
        let name = match doc.text("name") {
            Some(name) if !command => name,
            _ => default_name,
        };
        if let Err(why) = check_name(&name) {
            self.problems
                .push(format!("{shown}: not used, the name {name:?} {why}"));
            return;
        }
        let mut description = doc
            .text("description")
            .unwrap_or_else(|| first_paragraph(&doc.body));
        if let Some(when) = doc.text("when_to_use") {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(&when);
        }
        self.skills.push(Skill {
            name,
            description,
            argument_hint: doc.text("argument-hint"),
            arguments: doc.list("arguments"),
            model_invocable: !doc.flag("disable-model-invocation").unwrap_or(false),
            user_invocable: doc.flag("user-invocable").unwrap_or(true),
            allowed_tools: doc.list("allowed-tools"),
            file: file.to_path_buf(),
            dir: dir.to_path_buf(),
            scope,
            command,
            shown,
            under,
        });
    }
}

/// Claude Code refuses names with control, invisible or direction-changing characters and
/// `<` or `>`; a name here is also a word after `/` and in the skill tool's schema.
fn check_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("is empty");
    }
    if name.chars().count() > NAME_MAX {
        return Err("is too long");
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err("may hold only letters, digits and - _ . :");
    }
    Ok(())
}

impl Skills {
    pub fn all(&self) -> &[Skill] {
        &self.all
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.all.iter().find(|s| s.name == name)
    }

    /// The skills the model may load, those of directories below the root among them: the
    /// skill tool's names.
    pub fn for_model(&self) -> Vec<&Skill> {
        self.all.iter().filter(|s| s.model_invocable).collect()
    }

    /// The list for the project message: the skills the model may load, but those of
    /// directories below the root, which come with their files. `tool` is the skill tool's
    /// name in the model's terms.
    pub fn note(&self, tool: &str) -> Option<String> {
        let skills: Vec<&Skill> = self
            .all
            .iter()
            .filter(|s| s.model_invocable && s.under.is_none())
            .collect();
        if skills.is_empty() {
            return None;
        }
        // T2, 2026-11-06: told only to load the skill "before you start", gpt-oss searched
        // the project's files for it and never called the tool, or loaded it and wrote a
        // heading of its own
        Some(format!(
            "Skills of the user and the project: instructions for kinds of tasks, which no file of the project shows. When the task fits a skill's description, call the {tool} tool with its name before anything else, then do what it says exactly, its formats too.\n{}",
            list(&skills)
        ))
    }

    /// The skills of the directories between the root and `path` the conversation has not
    /// heard of yet, and the note that tells it of them.
    pub fn near(&mut self, path: &Path, tool: &str) -> Option<String> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let new: Vec<&Skill> = self
            .all
            .iter()
            .filter(|s| s.model_invocable)
            .filter(|s| {
                s.under
                    .as_ref()
                    .is_some_and(|d| path.starts_with(d) && !self.told.contains(d))
            })
            .collect();
        if new.is_empty() {
            return None;
        }
        for s in &new {
            if let Some(d) = &s.under
                && !dirs.contains(d)
            {
                dirs.push(d.clone());
            }
        }
        let text = format!(
            "[ognivo] Skills for the files here, loaded with {tool} as the others:\n{}",
            list(&new)
        );
        self.told.extend(dirs);
        Some(text)
    }

    /// A new conversation: it has heard of no skill of a directory below the root.
    pub fn forget(&mut self) {
        self.told.clear();
    }

    /// The skill `/name arguments` runs, when `name` is one the user may run, and the
    /// arguments.
    pub fn command(&self, text: &str) -> Option<(&Skill, String)> {
        let rest = text.strip_prefix('/')?;
        let (name, arguments) = match rest.split_once(char::is_whitespace) {
            Some((name, arguments)) => (name, arguments.trim()),
            None => (rest, ""),
        };
        let skill = self
            .all
            .iter()
            .find(|s| s.name == name && s.user_invocable)?;
        Some((skill, arguments.to_owned()))
    }
}

/// `- name: description` lines within [`LIST_CHARS`]; past them a name alone.
fn list(skills: &[&Skill]) -> String {
    let mut lines = Vec::new();
    let mut used = 0;
    let mut bare = 0;
    for s in skills {
        let mut about: String = s.description.chars().take(ENTRY_CHARS).collect();
        if about.len() < s.description.len() {
            about.push('…');
        }
        let line = if about.is_empty() {
            format!("- {}", s.name)
        } else {
            format!("- {}: {about}", s.name)
        };
        if used + line.len() <= LIST_CHARS {
            used += line.len() + 1;
            lines.push(line);
        } else {
            bare += 1;
            lines.push(format!("- {}", s.name));
        }
    }
    if bare > 0 {
        lines.push(format!(
            "({bare} descriptions did not fit; load such a skill to read what it is for.)"
        ));
    }
    lines.join("\n")
}

/// The body's first paragraph, for a skill without a description: its lines joined, the
/// marks of a heading taken off.
fn first_paragraph(body: &str) -> String {
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .skip_while(|l| l.is_empty())
        .take_while(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim())
        .collect();
    lines.join(" ")
}

fn read(path: &Path) -> std::io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(MAX_READ)
        .read_to_string(&mut text)?;
    Ok(text)
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

/// The words of the arguments, as a shell splits them: quotes keep blanks together.
fn words(arguments: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in arguments.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The body with its placeholders filled, as Claude Code fills them.
fn fill(
    body: &str,
    arguments: &str,
    names: &[String],
    var: impl Fn(&str) -> Option<String>,
) -> String {
    let arguments = arguments.trim();
    let words = words(arguments);
    let mut out = String::with_capacity(body.len());
    let mut took = false;
    let mut rest = body;
    while let Some(i) = rest.find(['$', '\\']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(after) = rest.strip_prefix("\\$") {
            out.push('$');
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('\\') {
            out.push('\\');
            rest = after;
            continue;
        }
        let after = &rest[1..];
        if let Some(inner) = after.strip_prefix('{')
            && let Some(end) = inner.find('}')
            && let Some(value) = var(&inner[..end])
        {
            out.push_str(&value);
            rest = &inner[end + 1..];
            continue;
        }
        let ident_len = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let ident = &after[..ident_len];
        if ident == "ARGUMENTS" {
            let tail = &after[ident_len..];
            if let Some(index) = tail.strip_prefix('[')
                && let Some(end) = index.find(']')
                && let Ok(n) = index[..end].parse::<usize>()
            {
                let whole = 1 + ident_len + 1 + end + 1;
                match words.get(n) {
                    Some(w) => {
                        out.push_str(w);
                        took = true;
                    }
                    None => out.push_str(&rest[..whole]),
                }
                rest = &rest[whole..];
                continue;
            }
            out.push_str(arguments);
            took |= !arguments.is_empty();
            rest = tail;
            continue;
        }
        if !ident.is_empty() && ident.bytes().all(|b| b.is_ascii_digit()) {
            match ident.parse::<usize>().ok().and_then(|n| words.get(n)) {
                Some(w) => {
                    out.push_str(w);
                    took = true;
                }
                None => out.push_str(&rest[..1 + ident_len]),
            }
            rest = &after[ident_len..];
            continue;
        }
        if let Some(n) = names.iter().position(|name| name == ident) {
            if let Some(w) = words.get(n) {
                out.push_str(w);
                took = true;
            }
            rest = &after[ident_len..];
            continue;
        }
        out.push('$');
        rest = after;
    }
    out.push_str(rest);
    if !arguments.is_empty() && !took {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("\nARGUMENTS: {arguments}\n"));
    }
    out
}

/// The filled body cut at its commands: `` !`command` `` where the `!` starts a line or
/// follows a blank, and blocks from a ```` ```! ```` line to a ```` ``` ```` line.
fn commands(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, parts: &mut Vec<Part>| {
        if !plain.is_empty() {
            parts.push(Part::Text(std::mem::take(plain)));
        }
    };
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("```!") {
            let mut command = String::new();
            let mut closed = false;
            for l in lines.by_ref() {
                if l.trim() == "```" {
                    closed = true;
                    break;
                }
                command.push_str(l);
            }
            if closed {
                flush(&mut plain, &mut parts);
                parts.push(Part::Run {
                    command: command.trim_end().to_owned(),
                    block: true,
                });
            } else {
                plain.push_str(line);
                plain.push_str(&command);
            }
            continue;
        }
        let mut rest = line;
        let mut at_start = true;
        loop {
            let found = rest.match_indices("!`").find(|(i, _)| {
                if *i == 0 {
                    at_start
                } else {
                    rest[..*i].ends_with(char::is_whitespace)
                }
            });
            let Some((i, _)) = found else {
                plain.push_str(rest);
                break;
            };
            let Some(len) = rest[i + 2..].find('`') else {
                plain.push_str(rest);
                break;
            };
            let command = &rest[i + 2..i + 2 + len];
            if command.trim().is_empty() {
                plain.push_str(&rest[..i + 2 + len + 1]);
                rest = &rest[i + 2 + len + 1..];
                at_start = false;
                continue;
            }
            plain.push_str(&rest[..i]);
            flush(&mut plain, &mut parts);
            parts.push(Part::Run {
                command: command.to_owned(),
                block: false,
            });
            rest = &rest[i + 2 + len + 1..];
            at_start = false;
        }
    }
    flush(&mut plain, &mut parts);
    parts
}

/// Exit 1 of a search or a comparison means "none" or "they differ", not a failure.
pub fn tolerates_one(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or_default();
    matches!(
        first,
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "diff" | "cmp" | "test" | "["
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn places(root: &Path, home: &Path) -> Places {
        Places {
            root: root.to_path_buf(),
            home: Some(home.to_path_buf()),
            claude_dir: None,
        }
    }

    #[test]
    fn skills_and_commands_come_from_the_user_the_project_and_its_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let root = tmp.path().join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\ndescription: Reviews a change the user's way.\n---\nBody.\n",
        );
        write(
            &root.join(".claude/skills/review/SKILL.md"),
            "---\ndescription: The project's review.\n---\n",
        );
        write(
            &root.join(".claude/skills/pdf/SKILL.md"),
            "---\nname: pdf\ndescription: Fills PDF forms.\nwhen_to_use: When a form must be filled.\nallowed-tools: Bash(python3 *)\n---\nUse the script.\n",
        );
        write(
            &root.join(".claude/commands/git/commit.md"),
            "---\nargument-hint: '[message]'\ndisable-model-invocation: true\n---\n# Commit the change\n\nCommit with $ARGUMENTS.\n",
        );
        write(
            &root.join(".claude/commands/pdf.md"),
            "A command the skill pdf hides.\n",
        );
        write(
            &root.join(".claude/skills/bad name/SKILL.md"),
            "---\nname: bad name\n---\n",
        );
        write(
            &root.join("web/.claude/skills/deploy/SKILL.md"),
            "---\ndescription: Deploys the web app.\nuser-invocable: false\n---\n",
        );
        let skills = load(&places(&root, &home));
        let names: Vec<(&str, Scope, bool)> = skills
            .all()
            .iter()
            .map(|s| (s.name.as_str(), s.scope, s.command))
            .collect();
        assert_eq!(
            names,
            [
                ("review", Scope::User, false),
                ("pdf", Scope::Project, false),
                ("git:commit", Scope::Project, true),
                ("deploy", Scope::Project, false),
            ]
        );
        assert_eq!(skills.problems.len(), 3, "{:?}", skills.problems);
        assert!(skills.problems.iter().any(|p| p.starts_with(
            ".claude/skills/review/SKILL.md: not used, the user's skill ~/.claude/skills/review/SKILL.md"
        )));
        assert!(skills.problems.iter().any(|p| p.contains("bad name")));

        let pdf = skills.get("pdf").unwrap();
        assert_eq!(
            pdf.description,
            "Fills PDF forms. When a form must be filled."
        );
        assert_eq!(pdf.allowed_tools, ["Bash(python3 *)"]);
        let commit = skills.get("git:commit").unwrap();
        assert_eq!(commit.description, "Commit the change");
        assert!(!commit.model_invocable && commit.user_invocable);
        assert_eq!(commit.argument_hint.as_deref(), Some("[message]"));

        // the model's list: neither the user's command nor the subdirectory's skill
        let note = skills.note("skill").unwrap();
        assert!(
            note.contains("- review: Reviews a change the user's way."),
            "{note}"
        );
        assert!(note.contains("- pdf: Fills PDF forms."), "{note}");
        assert!(
            !note.contains("commit") && !note.contains("deploy"),
            "{note}"
        );
        let mut skills = skills;
        assert_eq!(skills.near(&root.join("src/lib.rs"), "skill"), None);
        let near = skills.near(&root.join("web/app.ts"), "skill").unwrap();
        assert!(near.contains("- deploy: Deploys the web app."), "{near}");
        assert_eq!(skills.near(&root.join("web/x.ts"), "skill"), None);
        skills.forget();
        assert!(skills.near(&root.join("web/x.ts"), "skill").is_some());

        // the user runs commands and skills, not those kept for the model
        let (s, args) = skills.command("/git:commit fix the typo").unwrap();
        assert_eq!(
            (s.name.as_str(), args.as_str()),
            ("git:commit", "fix the typo")
        );
        assert!(skills.command("/commit").is_none());
        assert!(skills.command("/deploy").is_none());
        assert!(skills.command("/etc/hosts is wrong").is_none());
    }

    #[test]
    fn a_project_skill_may_not_point_outside_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir_all(root.join(".claude/skills/x")).unwrap();
        write(&tmp.path().join("secret.md"), "secret\n");
        std::os::unix::fs::symlink(
            tmp.path().join("secret.md"),
            root.join(".claude/skills/x/SKILL.md"),
        )
        .unwrap();
        let skills = load(&places(&root, &tmp.path().join("home")));
        assert!(skills.all().is_empty());
        assert_eq!(
            skills.problems,
            [".claude/skills/x/SKILL.md: not used, it points outside the project"]
        );
    }

    #[test]
    fn placeholders_are_filled_as_claude_code_fills_them() {
        let none = |_: &str| None;
        assert_eq!(
            fill("Fix issue $ARGUMENTS.\n", "123", &[], none),
            "Fix issue 123.\n"
        );
        assert_eq!(
            fill(
                "From $0 to $1, then $ARGUMENTS[0]; $2 stays; costs \\$1.00.\n",
                "\"a b\" c",
                &[],
                none
            ),
            "From a b to c, then a b; $2 stays; costs $1.00.\n"
        );
        let names = ["issue".to_owned(), "branch".to_owned()];
        assert_eq!(
            fill("Issue $issue on $branch.", "42", &names, none),
            "Issue 42 on ."
        );
        assert_eq!(
            fill("No placeholder.\n", "do it now", &[], none),
            "No placeholder.\n\nARGUMENTS: do it now\n"
        );
        assert_eq!(
            fill("No placeholder.\n", "  ", &[], none),
            "No placeholder.\n"
        );
        let var = |name: &str| (name == "CLAUDE_SKILL_DIR").then(|| "/s".to_owned());
        assert_eq!(
            fill("Run ${CLAUDE_SKILL_DIR}/x.py ${HOME} $PATH", "", &[], var),
            "Run /s/x.py ${HOME} $PATH"
        );
    }

    #[test]
    fn commands_are_cut_out_where_they_run() {
        let parts = commands(
            "Status: !`git status --short`\nNot `!`this`, nor a!`that`.\n```!\ngit log -1\ngit diff\n```\nEnd.\n",
        );
        assert_eq!(
            parts,
            [
                Part::Text("Status: ".into()),
                Part::Run {
                    command: "git status --short".into(),
                    block: false
                },
                Part::Text("\nNot `!`this`, nor a!`that`.\n".into()),
                Part::Run {
                    command: "git log -1\ngit diff".into(),
                    block: true
                },
                Part::Text("End.\n".into()),
            ]
        );
        assert!(tolerates_one("grep -n x f") && !tolerates_one("git status"));
    }
}
