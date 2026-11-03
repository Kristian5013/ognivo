//! Tool identities and effects.
//!
//! Canonical ids use Claude Code's tool names (`Bash`, `Read`, `Edit`, ...) because hooks,
//! permission rules and skills under `.claude/` refer to them (docs/architecture.md §9).
//! The names, shapes and placement the *model* sees are a per-model projection
//! (ToolProjector, §5.1) and are unrelated to these ids.

pub mod command;
pub mod diff;
pub mod edit;
pub mod exec;
pub mod files;
pub mod projection;
pub mod search;

pub use command::{ExecContext, classify_command};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolId {
    Bash,
    Read,
    Edit,
    Write,
    /// apply_patch (V4A), OpenAI's edit format: changes several files at once.
    Patch,
    Grep,
    Glob,
    WebFetch,
    UpdatePlan,
    AskUser,
    Skill,
    ToolSearch,
    Delegate,
    Mcp {
        server: String,
        tool: String,
    },
}

impl ToolId {
    /// The name hooks and permission rules match against.
    pub fn canonical_name(&self) -> String {
        match self {
            ToolId::Bash => "Bash".into(),
            ToolId::Read => "Read".into(),
            ToolId::Edit => "Edit".into(),
            ToolId::Write => "Write".into(),
            // Claude Code has none; rules on paths govern it as they do edits
            ToolId::Patch => "ApplyPatch".into(),
            ToolId::Grep => "Grep".into(),
            ToolId::Glob => "Glob".into(),
            ToolId::WebFetch => "WebFetch".into(),
            ToolId::UpdatePlan => "TodoWrite".into(),
            ToolId::AskUser => "AskUserQuestion".into(),
            ToolId::Skill => "Skill".into(),
            ToolId::ToolSearch => "ToolSearch".into(),
            ToolId::Delegate => "Task".into(),
            ToolId::Mcp { server, tool } => format!("mcp__{server}__{tool}"),
        }
    }

    /// Effect before looking at the arguments. Exhaustive on purpose: a new tool does not
    /// compile until someone decides what it may do. `Bash` is refined by
    /// [`classify_command`], edits by the approval policy's path checks.
    pub fn baseline_effect(&self) -> Effect {
        match self {
            ToolId::Read | ToolId::Grep | ToolId::Glob => Effect::ReadOnly,
            // no effect of their own: the calls they lead to are checked one by one
            ToolId::UpdatePlan
            | ToolId::AskUser
            | ToolId::Skill
            | ToolId::ToolSearch
            | ToolId::Delegate => Effect::ReadOnly,
            ToolId::Edit | ToolId::Write | ToolId::Patch => Effect::EditsFiles,
            // MCP annotations such as readOnlyHint are untrusted input
            ToolId::WebFetch | ToolId::Mcp { .. } => Effect::Network,
            ToolId::Bash => Effect::SideEffects,
        }
    }
}

/// What running a tool call can do to the outside world; drives approvals (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    /// Reads and searches inside the project.
    ReadOnly,
    /// Changes files through the agent's own edit/write tools; journaled, so reversible.
    /// Which paths may change silently is decided by the approval policy.
    EditsFiles,
    /// Runs the project's own code (tests, builds, scripts) inside the project.
    ExecutesProjectCode,
    /// Talks to the network: a channel for exfiltration, so never silent by default.
    Network,
    /// Anything else a shell command may do; not reversible by the edit journal.
    SideEffects,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_match_claude_code() {
        assert_eq!(ToolId::Bash.canonical_name(), "Bash");
        assert_eq!(ToolId::Delegate.canonical_name(), "Task");
        let mcp = ToolId::Mcp {
            server: "github".into(),
            tool: "create_issue".into(),
        };
        assert_eq!(mcp.canonical_name(), "mcp__github__create_issue");
        assert_eq!(mcp.baseline_effect(), Effect::Network);
    }
}
