//! Subagent templates of `.claude/agents` (docs/architecture.md §9, decision 18), read as
//! Claude Code reads them: a Markdown file whose frontmatter names the subagent and says
//! what to give it (`name`, `description`), and whose body is its role.
//!
//! - The project's (`.claude/agents/*.md` of the root and of the directories above it up to
//!   the top of its git work tree, the nearest first) win over the user's
//!   (`~/.claude/agents`, or Claude Code's `CLAUDE_CONFIG_DIR`); a template whose name a
//!   weightier one has is named among the problems.
//! - `tools` (a list, or names between commas) are the tools it gets of those its task
//!   allows, and `disallowedTools` take tools away; without `tools`, it gets them all.
//!   `maxTurns` bounds its steps.
//! - The main agent gives a task to one with `delegate`'s `agent`. The subagent gets the
//!   template's body before its task: the system prompt stays the one every subagent shares,
//!   so the server's cache serves it.
//! - Not read: `model` (a session has one model), `permissionMode` (the approvals stay the
//!   session's), `skills`, `mcpServers`, `hooks`, `memory`, `isolation` and the others.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ognivo_tools::ToolId;

use crate::frontmatter::Document;
use crate::rules::Places;

/// The tools a template may name, as ognivo has them.
const ALL: [ToolId; 8] = [
    ToolId::Read,
    ToolId::Grep,
    ToolId::Glob,
    ToolId::Bash,
    ToolId::Edit,
    ToolId::Write,
    ToolId::Patch,
    ToolId::Skill,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    /// When to give it a task.
    pub description: String,
    /// The tools it may have.
    pub tools: Vec<ToolId>,
    pub max_turns: Option<u32>,
    /// Its role, which it gets before its task.
    pub body: String,
    /// The file as the user names it.
    pub shown: String,
}

impl Template {
    /// Whether it may change files, given a task that lets it.
    pub fn edits(&self) -> bool {
        self.tools
            .iter()
            .any(|t| matches!(t, ToolId::Edit | ToolId::Write | ToolId::Patch))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Templates {
    pub all: Vec<Template>,
    /// Files that could not be used, and templates another one's name hides.
    pub problems: Vec<String>,
}

impl Templates {
    pub fn get(&self, name: &str) -> Option<&Template> {
        self.all.iter().find(|t| t.name == name)
    }
}

/// Finds the templates of the project and the user.
pub fn load(places: &Places) -> Templates {
    let top = places
        .root
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&places.root)
        .to_path_buf();
    let mut dirs: Vec<PathBuf> = places
        .root
        .ancestors()
        .take_while(|d| d.starts_with(&top))
        .map(|d| d.join(".claude/agents"))
        .collect();
    if let Some(claude) = places
        .claude_dir
        .clone()
        .or_else(|| places.home.as_ref().map(|h| h.join(".claude")))
    {
        dirs.push(claude.join("agents"));
    }
    let mut found = Templates::default();
    let mut read = HashSet::new();
    for dir in dirs {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "md") && p.is_file())
                .collect(),
            Err(_) => continue,
        };
        files.sort();
        for file in files {
            let shown = shown(&file, places);
            let Ok(real) = ognivo_os::canonicalize(&file) else {
                continue;
            };
            if !read.insert(real.clone()) {
                continue;
            }
            if file.starts_with(&places.root) && !real.starts_with(&top) {
                found
                    .problems
                    .push(format!("{shown}: not used, it points outside the project"));
                continue;
            }
            match template(&file, &shown) {
                Ok(t) => match found.get(&t.name) {
                    Some(k) => found.problems.push(format!(
                        "{shown}: not used, {} has the name {}",
                        k.shown, t.name
                    )),
                    None => found.all.push(t),
                },
                Err(why) => found.problems.push(format!("{shown}: not used, {why}")),
            }
        }
    }
    found
}

fn template(file: &Path, shown: &str) -> Result<Template, String> {
    let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let doc = Document::parse(&text);
    if let Some(p) = doc.problem {
        return Err(p);
    }
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let name = doc.text("name").unwrap_or_else(|| stem.into_owned());
    if name.chars().count() > 256
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "the name {name:?} may hold only letters, digits and - _ ."
        ));
    }
    let description = doc
        .text("description")
        .ok_or("it has no description of what to give it")?;
    let mut tools = if doc.has("tools") {
        ids(&doc.list("tools"))
    } else {
        ALL.to_vec()
    };
    let taken = ids(&doc.list("disallowedTools"));
    tools.retain(|t| !taken.contains(t));
    let body = doc.body.trim().to_owned();
    Ok(Template {
        name,
        description,
        tools,
        max_turns: doc
            .text("maxTurns")
            .and_then(|n| n.parse().ok())
            .filter(|n| *n > 0),
        body,
        shown: shown.to_owned(),
    })
}

/// The tools Claude Code's names stand for here; `Bash(git *)` is Bash.
fn ids(names: &[String]) -> Vec<ToolId> {
    let mut ids = Vec::new();
    for n in names {
        let base = n.split('(').next().unwrap_or(n).trim();
        let these: &[ToolId] = match base {
            "Read" => &[ToolId::Read],
            "Grep" => &[ToolId::Grep],
            "Glob" | "LS" => &[ToolId::Glob],
            "Bash" => &[ToolId::Bash],
            "Edit" | "MultiEdit" | "NotebookEdit" => &[ToolId::Edit, ToolId::Patch],
            "Write" => &[ToolId::Write, ToolId::Patch],
            "Skill" => &[ToolId::Skill],
            _ => &[],
        };
        for id in these {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
    }
    ids
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn the_projects_templates_win_and_name_their_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &root.join(".claude/agents/reviewer.md"),
            "---\nname: code-reviewer\ndescription: Reviews a change for bugs.\ntools: Read, Grep, Glob, Bash(git diff *)\nmaxTurns: 12\n---\nYou review code. Report bugs with file:line.\n",
        );
        write(
            &root.join(".claude/agents/writer.md"),
            "---\nname: doc-writer\ndescription: Writes docs.\ndisallowedTools: [Bash]\n---\nYou write docs.\n",
        );
        write(&root.join(".claude/agents/bare.md"), "No frontmatter.\n");
        write(
            &home.join(".claude/agents/reviewer.md"),
            "---\nname: code-reviewer\ndescription: The user's reviewer.\n---\n",
        );
        let t = load(&Places {
            root: root.clone(),
            home: Some(home),
            claude_dir: None,
        });
        let names: Vec<&str> = t.all.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["code-reviewer", "doc-writer"]);
        assert_eq!(t.problems.len(), 2, "{:?}", t.problems);
        assert!(t.problems.iter().any(|p| p.starts_with(
            "~/.claude/agents/reviewer.md: not used, .claude/agents/reviewer.md has the name code-reviewer"
        )));
        let reviewer = t.get("code-reviewer").unwrap();
        assert_eq!(
            reviewer.tools,
            [ToolId::Read, ToolId::Grep, ToolId::Glob, ToolId::Bash]
        );
        assert!(!reviewer.edits());
        assert_eq!(reviewer.max_turns, Some(12));
        assert_eq!(
            reviewer.body,
            "You review code. Report bugs with file:line."
        );
        let writer = t.get("doc-writer").unwrap();
        assert!(writer.edits() && !writer.tools.contains(&ToolId::Bash));
    }
}
