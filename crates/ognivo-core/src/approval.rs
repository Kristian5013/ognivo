//! When a tool call may run without asking the user (owner's decisions 7, 12, 13 and the
//! follow-ups of 2026-10-30).
//!
//! v1 has no sandbox, so this policy is the security boundary (docs/architecture.md §8):
//! - Reads and searches inside the project run silently.
//! - The agent's own edits and writes inside the project are applied at once: the edit
//!   journal makes them reversible and they are reviewed in Changes. Edits outside the
//!   project or to a protected path always ask: those files make git, CI, the agent, an
//!   editor, a build tool or a shell run programs, so "reversible later" is not enough.
//! - Commands that run project code (`cargo test`, `npm test`, ...) run silently only
//!   while the workspace has no unreviewed changes; otherwise they ask, because an
//!   auto-applied edit plus an auto-run test would execute arbitrary code unprompted.
//! - Network access and everything else asks.
//!
//! User and project rules (`~/.ognivo/config.toml`, `.claude/settings.json` permissions)
//! are layered on top in M1 (deny > ask > allow).

use std::path::{Component, Path};

use ognivo_tools::Effect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
}

/// Review state of the workspace. It turns dirty on the first change by any agent or
/// subagent (edits, writes, approved commands with side effects, MCP calls) and turns
/// clean only when the user accepts or reverts the changes in Changes; it is not reset
/// per turn and persists with the session (SQLite, M1). No `Default`: the state must be
/// stated, never assumed clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceState {
    pub unreviewed_changes: bool,
}

impl WorkspaceState {
    pub const CLEAN: Self = Self {
        unreviewed_changes: false,
    };
    pub const DIRTY: Self = Self {
        unreviewed_changes: true,
    };
}

/// Decides by effect alone. Edits are not decided here: without their paths and contents
/// the only safe answer is Ask, so callers decide edits with [`decide_edit`].
pub fn decide(effect: Effect, ws: WorkspaceState) -> Decision {
    match effect {
        Effect::ReadOnly => Decision::Allow,
        Effect::EditsFiles => Decision::Ask,
        Effect::ExecutesProjectCode if !ws.unreviewed_changes => Decision::Allow,
        Effect::ExecutesProjectCode | Effect::Network | Effect::SideEffects => Decision::Ask,
    }
}

/// Reads and searches: silent inside the project, a question anywhere else, since other
/// projects, dotfiles and secrets live outside it. `path` must be resolved the way the
/// tool opens it (symlinks included); `root` must be canonical.
pub fn decide_read(path: &Path, root: &Path) -> Decision {
    if path.starts_with(root) {
        Decision::Allow
    } else {
        Decision::Ask
    }
}

/// One file an edit changes. The path must be canonical (symlinks resolved, as the edit
/// tools open it); `before` is `None` when the file is created, `after` when deleted.
#[derive(Debug, Clone, Copy)]
pub struct FileEdit<'a> {
    pub path: &'a Path,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
}

/// Directories anywhere in the path whose contents make other programs run code.
const PROTECTED_DIRS: &[&str] = &[
    ".git",
    ".claude",
    ".ognivo",
    ".vscode",
    ".idea",
    ".devcontainer",
    ".direnv",
    ".cargo",
];
/// File names, at any depth, with the same property.
const PROTECTED_FILES: &[&str] = &[
    ".gitattributes",
    ".gitmodules",
    ".mcp.json",
    "AGENTS.md",
    "CLAUDE.md",
    "CLAUDE.local.md",
    ".envrc",
    "mise.toml",
    ".mise.toml",
    ".tool-versions",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
    ".pnpmfile.cjs",
    ".bashrc",
    ".zshrc",
    ".profile",
    "Makefile",
    "makefile",
    "GNUmakefile",
];
/// Path prefixes relative to the project root: CI workflows run code with repository secrets.
const PROTECTED_PREFIXES: &[&[&str]] = &[&[".github", "workflows"]];

/// Whether the agent may apply these file changes without asking. `root` must be canonical.
pub fn decide_edit(edits: &[FileEdit<'_>], root: &Path) -> Decision {
    if edits.iter().all(|e| silent(e, root)) {
        Decision::Allow
    } else {
        Decision::Ask
    }
}

fn silent(edit: &FileEdit<'_>, root: &Path) -> bool {
    let Ok(rel) = edit.path.strip_prefix(root) else {
        return false;
    };
    let mut names = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => match n.to_str() {
                Some(s) => names.push(s),
                None => return false,
            },
            _ => return false,
        }
    }
    let protected = names.iter().any(|n| PROTECTED_DIRS.contains(n))
        || names.last().is_some_and(|n| PROTECTED_FILES.contains(n))
        || PROTECTED_PREFIXES.iter().any(|p| names.starts_with(p));
    if protected {
        return false;
    }
    if names.last() == Some(&"pyproject.toml") {
        return !tool_tables_change(edit.before, edit.after);
    }
    true
}

/// `[tool.*]` in pyproject.toml configures test runners and build tools that run code
/// (pytest `addopts` and plugins, hatch scripts, ...): changing it asks, other tables of
/// the file stay silent. Unparsable TOML cannot be judged, so it asks too.
fn tool_tables_change(before: Option<&str>, after: Option<&str>) -> bool {
    fn tool(text: Option<&str>) -> Result<Option<toml::Value>, toml::de::Error> {
        let Some(text) = text else { return Ok(None) };
        Ok(text.parse::<toml::Table>()?.get("tool").cloned())
    }
    match (tool(before), tool(after)) {
        (Ok(a), Ok(b)) => a != b,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ognivo_tools::{ExecContext, classify_command};

    #[test]
    fn owner_rules() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let ctx = ExecContext {
            cwd: root.clone(),
            root,
        };
        let (clean, dirty) = (WorkspaceState::CLEAN, WorkspaceState::DIRTY);
        assert_eq!(
            decide(classify_command("git status", &ctx), dirty),
            Decision::Allow
        );
        assert_eq!(
            decide(classify_command("cargo test", &ctx), clean),
            Decision::Allow
        );
        assert_eq!(
            decide(classify_command("cargo test", &ctx), dirty),
            Decision::Ask
        );
        assert_eq!(decide(Effect::EditsFiles, clean), Decision::Ask);
        assert_eq!(decide(Effect::Network, clean), Decision::Ask);
        assert_eq!(
            decide(classify_command("rm -rf target", &ctx), clean),
            Decision::Ask
        );
    }

    fn edit(path: &str) -> FileEdit<'_> {
        FileEdit {
            path: Path::new(path),
            before: Some(""),
            after: Some("x"),
        }
    }

    #[test]
    fn protected_and_outside_paths_ask() {
        let root = Path::new("/work/proj");
        assert_eq!(
            decide_edit(
                &[edit("/work/proj/src/lib.rs"), edit("/work/proj/build.rs")],
                root
            ),
            Decision::Allow
        );
        assert_eq!(
            decide_edit(&[edit("/work/proj/.github/ISSUE_TEMPLATE/bug.md")], root),
            Decision::Allow
        );
        for p in [
            "/work/proj/.git/config",
            "/work/proj/.git/hooks/pre-commit",
            "/work/proj/sub/.gitattributes",
            "/work/proj/.gitmodules",
            "/work/proj/.claude/settings.json",
            "/work/proj/.mcp.json",
            "/work/proj/crates/x/AGENTS.md",
            "/work/proj/.cargo/config.toml",
            "/work/proj/.envrc",
            "/work/proj/Makefile",
            "/work/proj/sub/GNUmakefile",
            "/work/proj/.github/workflows/ci.yml",
            "/home/user/.bashrc",
            "/work/other/src/lib.rs",
        ] {
            assert_eq!(decide_edit(&[edit(p)], root), Decision::Ask, "{p}");
        }
        // one protected path in a multi-file patch is enough
        assert_eq!(
            decide_edit(
                &[edit("/work/proj/src/a.rs"), edit("/work/proj/.git/config")],
                root
            ),
            Decision::Ask
        );
    }

    #[test]
    fn pyproject_asks_only_for_tool_tables() {
        let root = Path::new("/work/proj");
        let path = Path::new("/work/proj/pyproject.toml");
        let base = "[project]\nname = \"x\"\ndependencies = []\n\n[tool.pytest.ini_options]\naddopts = \"-q\"\n";
        let deps = base.replace("dependencies = []", "dependencies = [\"requests\"]");
        let plugin = base.replace("addopts = \"-q\"", "addopts = \"-q -p evil\"");
        let check = |before: Option<&str>, after: Option<&str>| {
            decide_edit(
                &[FileEdit {
                    path,
                    before,
                    after,
                }],
                root,
            )
        };
        assert_eq!(check(Some(base), Some(&deps)), Decision::Allow);
        assert_eq!(check(Some(base), Some(&plugin)), Decision::Ask);
        assert_eq!(
            check(None, Some("[project]\nname = \"x\"\n")),
            Decision::Allow
        );
        assert_eq!(check(None, Some(base)), Decision::Ask);
        assert_eq!(check(Some(base), Some("[tool\nbroken")), Decision::Ask);
        assert_eq!(check(Some(base), None), Decision::Ask);
    }

    #[test]
    fn reads_outside_the_project_ask() {
        let root = Path::new("/home/u/proj");
        assert_eq!(
            decide_read(Path::new("/home/u/proj/src/a.rs"), root),
            Decision::Allow
        );
        assert_eq!(
            decide_read(Path::new("/home/u/.ssh/id_ed25519"), root),
            Decision::Ask
        );
        assert_eq!(
            decide_read(Path::new("/home/u/project2/a.rs"), root),
            Decision::Ask
        );
    }
}
