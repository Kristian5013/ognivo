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
//! are layered on top by [`crate::rules`] (deny > ask > allow).
//!
//! The thread's [`Mode`], which the user switches at any time, makes the policy stricter
//! or looser before the rules are applied: it never lifts a rule's question or refusal,
//! and in no mode does an edit of a protected path or outside the project go silently.

use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use ognivo_tools::Effect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
}

/// How far the agent goes without the user (the GUI's modes, as Claude Code's permission
/// modes): what the policy decides, made stricter or looser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Every edit, and every command that does more than read, asks first.
    Manual,
    /// The policy as it is: the agent's edits inside the project apply at once and are
    /// reviewed in Changes; commands ask as the policy says.
    #[default]
    EditAutomatically,
    /// The agent reads and plans, and changes nothing: edits, and commands that do more
    /// than read, are refused without a question.
    Plan,
    /// What would ask goes ahead, except edits of protected paths and outside the project
    /// and what a rule asks for.
    Auto,
}

impl Mode {
    /// A read (read, grep, glob) the policy decided on.
    pub fn read(self, policy: Decision) -> Decision {
        match self {
            Mode::Auto => Decision::Allow,
            _ => policy,
        }
    }

    /// An edit the policy decided on; `None`: the mode refuses it.
    pub fn edit(self, policy: Decision) -> Option<Decision> {
        match self {
            Mode::Plan => None,
            Mode::Manual => Some(Decision::Ask),
            Mode::EditAutomatically | Mode::Auto => Some(policy),
        }
    }

    /// A command of `effect` the policy decided on; `None`: the mode refuses it.
    pub fn command(self, effect: Effect, policy: Decision) -> Option<Decision> {
        match self {
            _ if effect == Effect::ReadOnly => Some(policy),
            Mode::Plan => None,
            Mode::Manual => Some(Decision::Ask),
            Mode::Auto => Some(Decision::Allow),
            Mode::EditAutomatically => Some(policy),
        }
    }

    /// An MCP tool no rule names: a question, or in auto mode a yes.
    pub fn mcp(self) -> Decision {
        match self {
            Mode::Auto => Decision::Allow,
            _ => Decision::Ask,
        }
    }
}

/// The thread's mode as the user switches it, while a turn runs too: the agent and its
/// subagents read it at each call.
#[derive(Debug, Clone)]
pub struct ModeSwitch(Arc<AtomicU8>);

impl ModeSwitch {
    pub fn new(mode: Mode) -> Self {
        Self(Arc::new(AtomicU8::new(Self::number(mode))))
    }

    pub fn get(&self) -> Mode {
        match self.0.load(Ordering::SeqCst) {
            0 => Mode::Manual,
            2 => Mode::Plan,
            3 => Mode::Auto,
            _ => Mode::EditAutomatically,
        }
    }

    pub fn set(&self, mode: Mode) {
        self.0.store(Self::number(mode), Ordering::SeqCst);
    }

    fn number(mode: Mode) -> u8 {
        match mode {
            Mode::Manual => 0,
            Mode::EditAutomatically => 1,
            Mode::Plan => 2,
            Mode::Auto => 3,
        }
    }
}

impl Default for ModeSwitch {
    fn default() -> Self {
        Self::new(Mode::default())
    }
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
    // `.GIT` is `.git` where the OS's names match in any case
    let same = |a: &str, b: &str| {
        if ognivo_os::PATHS_IGNORE_CASE {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    let among = |n: &str, list: &[&str]| list.iter().any(|x| same(n, x));
    let protected = names.iter().any(|n| among(n, PROTECTED_DIRS))
        || names.last().is_some_and(|n| among(n, PROTECTED_FILES))
        || PROTECTED_PREFIXES
            .iter()
            .any(|p| names.len() >= p.len() && names.iter().zip(p.iter()).all(|(n, x)| same(n, x)));
    if protected {
        return false;
    }
    if names.last().is_some_and(|n| same(n, "pyproject.toml")) {
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
        let root = ognivo_os::canonicalize(std::env::current_dir().unwrap()).unwrap();
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

    #[test]
    fn modes_make_the_policy_stricter_or_looser() {
        use Decision::{Allow, Ask};
        let (manual, edits, plan, auto) = (
            Mode::Manual,
            Mode::EditAutomatically,
            Mode::Plan,
            Mode::Auto,
        );
        // edits: the policy, every one asking, none, or the policy: a protected path asks
        // even in auto mode
        assert_eq!(edits.edit(Allow), Some(Allow));
        assert_eq!(manual.edit(Allow), Some(Ask));
        assert_eq!(plan.edit(Allow), None);
        assert_eq!(auto.edit(Allow), Some(Allow));
        assert_eq!(auto.edit(Ask), Some(Ask));
        // a command that only reads goes as the policy says in every mode
        for mode in [manual, edits, plan, auto] {
            assert_eq!(mode.command(Effect::ReadOnly, Allow), Some(Allow));
        }
        assert_eq!(
            edits.command(Effect::ExecutesProjectCode, Allow),
            Some(Allow)
        );
        assert_eq!(
            manual.command(Effect::ExecutesProjectCode, Allow),
            Some(Ask)
        );
        assert_eq!(plan.command(Effect::ExecutesProjectCode, Allow), None);
        assert_eq!(auto.command(Effect::SideEffects, Ask), Some(Allow));
        assert_eq!(auto.command(Effect::Network, Ask), Some(Allow));
        assert_eq!(edits.command(Effect::Network, Ask), Some(Ask));
        // reads outside the project ask, but in auto mode
        assert_eq!(plan.read(Ask), Ask);
        assert_eq!(manual.read(Allow), Allow);
        assert_eq!(auto.read(Ask), Allow);
        assert_eq!(auto.mcp(), Allow);
        assert_eq!(edits.mcp(), Ask);
    }

    #[test]
    fn a_switch_is_one_mode_for_all_who_hold_it() {
        let switch = ModeSwitch::default();
        assert_eq!(switch.get(), Mode::EditAutomatically);
        let subagent = switch.clone();
        for mode in [
            Mode::Plan,
            Mode::Auto,
            Mode::Manual,
            Mode::EditAutomatically,
        ] {
            switch.set(mode);
            assert_eq!(subagent.get(), mode);
        }
    }
}
