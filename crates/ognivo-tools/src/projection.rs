//! What each model sees of the tools, and the way back (ToolProjector, docs/architecture.md
//! §5.1 item 3 and §5.5). The agent works with [`Call`]; every family gets names and
//! argument shapes it handles best:
//! - gpt-oss: snake_case `functions.*` with one-line descriptions;
//! - Gemma 4: lowercase names and flat string arguments whose alphabetical order is the
//!   order they should be generated in, because its template sorts keys. M0: all 62 of its
//!   edits came as `newString, oldString, path`, the replacement before its anchor; here
//!   `file, find, replace`;
//! - Qwen: the Claude Code shapes (`Read`, `Edit` with `file_path`, `old_string`, ...).
//!
//! Arguments are checked against the declared schema before anything runs: required
//! keys, types, no unknown keys. Numbers and booleans may come as strings (Gemma's call
//! syntax has only strings).
//!
//! gpt-oss may also get the tools of its training instead ([`Layout::Priors`]):
//! `container.exec` with an argument list, and `repo_browser.open_file`, `search`,
//! `print_tree` and `apply_patch`, the layout that reproduced OpenAI's SWE-bench score
//! (docs/research/2026-10-29/gpt-oss.md §D, layout A). They map to the same calls.
//!
//! An agent gets all of a layout's tools or some of them ([`Toolset`]): the user's agent
//! with subagents also gets `delegate`, and a subagent that only reads none that changes
//! files.

use std::time::Duration;

use ognivo_models::{Family, ToolDecl};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ToolId;

/// A tool call in ognivo's own terms.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Call {
    Read {
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Edit {
        path: String,
        old: String,
        new: String,
        replace_all: bool,
    },
    Write {
        path: String,
        content: String,
    },
    /// An apply_patch (V4A) patch.
    Patch {
        patch: String,
    },
    Shell {
        command: String,
        timeout: Option<Duration>,
    },
    Grep {
        pattern: String,
        path: Option<String>,
        glob: Option<String>,
    },
    Glob {
        pattern: String,
        path: Option<String>,
    },
    /// Tasks for subagents, which work on them at the same time.
    Delegate {
        tasks: Vec<Task>,
    },
    /// A tool of an MCP server, by the server's names; the arguments as the model wrote
    /// them, a JSON object.
    Mcp {
        server: String,
        tool: String,
        arguments: String,
    },
    /// The MCP servers' tools that fit a query, when there are too many to declare.
    ToolSearch {
        query: String,
    },
    /// A skill of the user or the project, loaded for the model (Claude Code's `Skill`).
    Skill {
        name: String,
        /// What it should work on, as the words after `/name` would be.
        arguments: Option<String>,
    },
}

/// One task of `delegate`, for one subagent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Task {
    pub title: String,
    /// What to find out or do, and what the report must say.
    pub goal: String,
    /// What the subagent needs to know, which it does not see otherwise.
    pub context: Option<String>,
    /// It may change files: those in `files`, and no others.
    pub write: bool,
    /// For a write task the files it may change; for a read task where to start.
    pub files: Vec<String>,
    /// The subagent of `.claude/agents` to give it to; `None`: the general-purpose one.
    pub agent: Option<String>,
}

impl Call {
    pub fn id(&self) -> ToolId {
        match self {
            Call::Read { .. } => ToolId::Read,
            Call::Edit { .. } => ToolId::Edit,
            Call::Write { .. } => ToolId::Write,
            Call::Patch { .. } => ToolId::Patch,
            Call::Shell { .. } => ToolId::Bash,
            Call::Grep { .. } => ToolId::Grep,
            Call::Glob { .. } => ToolId::Glob,
            Call::Delegate { .. } => ToolId::Delegate,
            Call::Mcp { server, tool, .. } => ToolId::Mcp {
                server: server.clone(),
                tool: tool.clone(),
            },
            Call::ToolSearch { .. } => ToolId::ToolSearch,
            Call::Skill { .. } => ToolId::Skill,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgError {
    #[error("unknown tool {0:?}; the tools are: {1}")]
    UnknownTool(String, String),
    #[error("{tool}: arguments are not a JSON object")]
    NotObject { tool: String },
    #[error("{tool}: missing required argument `{arg}`")]
    Missing { tool: String, arg: String },
    #[error("{tool}: unknown argument `{arg}`; the arguments are: {known}")]
    Unknown {
        tool: String,
        arg: String,
        known: String,
    },
    #[error("{tool}: `{arg}` must be {want}")]
    Type {
        tool: String,
        arg: String,
        want: &'static str,
    },
    #[error("{tool}: {why}")]
    Invalid { tool: String, why: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Bool,
    /// A list of strings: a command and its arguments.
    StrList,
}

impl Kind {
    fn schema(self) -> &'static str {
        match self {
            Kind::Str => "string",
            Kind::Int => "integer",
            Kind::Bool => "boolean",
            Kind::StrList => "array",
        }
    }

    fn json(self) -> Value {
        match self {
            Kind::StrList => json!({"type": "array", "items": {"type": "string"}}),
            other => json!({"type": other.schema()}),
        }
    }
}

/// Which tools a model gets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// ognivo's own tools, shaped per family.
    #[default]
    Standard,
    /// gpt-oss's own: `container.exec` and `repo_browser.*`, apply_patch for edits. A name
    /// with a namespace is one of the model's built-in tools, which the Native path declares
    /// in the system message; gpt-oss only, and the Native path only.
    Priors,
}

/// Which of a layout's tools an agent gets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Toolset {
    /// Every tool: the user's agent, or a subagent that may change files.
    #[default]
    All,
    /// Every tool and `delegate`: the user's agent with subagents.
    Delegating,
    /// None that changes files: a subagent that only reads. Its commands still run, under
    /// the same approvals.
    Reading,
}

impl Toolset {
    fn has(self, id: &ToolId) -> bool {
        match self {
            Toolset::All | Toolset::Delegating => true,
            Toolset::Reading => !matches!(id, ToolId::Edit | ToolId::Write | ToolId::Patch),
        }
    }
}

/// Tasks one `delegate` call may give.
pub const MAX_TASKS: usize = 6;

const DELEGATE_DOC: &str = "Give tasks to subagents. They work at the same time, each in a fresh context with your tools but this one, and each returns a report of what it found or did; you get the reports together. Worth it for independent pieces of work: finding things in different parts of the project, reviewing, running tests. A subagent knows only what its task says, so give it the goal and everything it needs. A read task changes no file; a write task may change only the files it lists, and tasks that list the same file run one after another.";

/// `delegate`'s parameters, in the order the model writes them.
fn delegate_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "description": format!("The tasks, at most {MAX_TASKS}."),
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string", "description": "A few words naming the task."},
                        "goal": {"type": "string", "description": "What to find out or do, and what the report must say."},
                        "context": {"type": "string", "description": "What the subagent needs to know: file paths, findings so far, constraints."},
                        "mode": {"type": "string", "enum": ["read", "write"], "description": "read: it reads and runs commands but changes no file; write: it may also change the files listed."},
                        "files": {"type": "array", "items": {"type": "string"}, "description": "write: the only files it may change; read: where to start looking."}
                    },
                    "required": ["title", "goal", "mode"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["tasks"],
        "additionalProperties": false
    })
}

/// `delegate` by the name each family knows such a tool by: Claude Code's for Qwen.
pub fn delegate_name(family: Family) -> &'static str {
    match family {
        Family::QwenXml => "Task",
        Family::Harmony | Family::Gemma4 => "delegate",
    }
}

/// One argument: the model-facing name, the canonical field it fills, and its doc line.
struct Arg {
    name: &'static str,
    field: &'static str,
    kind: Kind,
    required: bool,
    doc: &'static str,
}

struct Tool {
    name: &'static str,
    id: ToolId,
    doc: &'static str,
    args: &'static [Arg],
}

const fn arg(
    name: &'static str,
    field: &'static str,
    kind: Kind,
    required: bool,
    doc: &'static str,
) -> Arg {
    Arg {
        name,
        field,
        kind,
        required,
        doc,
    }
}

use Kind::{Bool, Int, Str, StrList};

const READ_DOC: &str = "Read a text file with line numbers; the first line gives its size. Pages of up to 2000 lines: continue with offset.";
const EDIT_DOC: &str = "Replace one exact, unique occurrence of a text in a file with new text. Copy the text from read output without the line numbers.";
const WRITE_DOC: &str =
    "Create a file, or replace a whole file you have read, with the given content.";
const SHELL_DOC: &str = "Run a bash command in the project and return its exit code and output. The working directory persists between calls.";
const GREP_DOC: &str = "Search file contents line by line with a regular expression (Rust syntax: escape [ ] ( ) { } . * + ? | \\ to match them literally); returns path:line:text for each match.";
const GLOB_DOC: &str =
    "List files whose path matches a glob pattern such as **/*.rs, newest first.";
const EXEC_DOC: &str = "Run a command in the project and return its exit code and output. Give the command as an argument list, such as [\"bash\", \"-lc\", \"cargo test\"]. The working directory persists between calls.";
const OPEN_FILE_DOC: &str = "Open a text file with line numbers; the first line gives its size. Pages of up to 2000 lines: continue from where a page ends.";
const SEARCH_DOC: &str = "Search the contents of the project's files for a text, line by line; returns path:line:text for each match.";
const PRINT_TREE_DOC: &str =
    "List the files under a directory, down to a depth (default 2), newest first.";
/// Codex's description of apply_patch for gpt-oss (OpenAI Codex, `create_apply_patch_json_tool`
/// at b127a3643, Apache-2.0), without its invocation through a shell, and with what this
/// one does not do.
const APPLY_PATCH_DOC: &str = "Use the `apply_patch` tool to edit files.
Your patch language is a stripped‑down, file‑oriented diff format designed to be easy to parse and safe to apply. You can think of it as a high‑level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of two headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Update File: <path> - patch an existing file in place.

Then one or more “hunks”, each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with ' ' (context), '-' (a line to remove) or '+' (a line to add).

For instructions on [context_before] and [context_after]:
- By default, show 3 lines of code immediately above and 3 lines immediately below each change. If a change is within 3 lines of a previous change, do NOT duplicate the first change’s [context_after] lines in the second change’s [context_before] lines.
- If 3 lines of context is insufficient to uniquely identify the snippet of code within the file, use the @@ operator to indicate the class or function to which the snippet belongs. For instance, we might have:
@@ class BaseClass
[3 lines of pre-context]
- [old_code]
+ [new_code]
[3 lines of post-context]

A full patch can combine several operations:

*** Begin Patch
*** Add File: hello.txt
+Hello world
*** Update File: src/app.py
@@ def greet():
-print(\"Hi\")
+print(\"Hello, world!\")
*** End Patch

It is important to remember:

- You must include a header with your intended action (Add/Update)
- You must prefix new lines with `+` even when creating a new file
- File references can only be relative, NEVER ABSOLUTE.
- Delete and move files with container.exec.";

/// gpt-oss's own tools, in its own shapes (the layout of docs/research/2026-10-29/gpt-oss.md
/// §D, layout A).
const HARMONY_PRIORS: &[Tool] = &[
    Tool {
        name: "container.exec",
        id: ToolId::Bash,
        doc: EXEC_DOC,
        args: &[
            arg(
                "command",
                "argv",
                StrList,
                true,
                "The command and its arguments.",
            ),
            arg(
                "timeout_ms",
                "timeout_ms",
                Int,
                false,
                "Time limit in milliseconds, at most 600000.",
            ),
        ],
    },
    Tool {
        name: "repo_browser.open_file",
        id: ToolId::Read,
        doc: OPEN_FILE_DOC,
        args: &[
            arg(
                "path",
                "path",
                Str,
                true,
                "File path, relative to the project root.",
            ),
            arg(
                "line_start",
                "offset",
                Int,
                false,
                "First line to show, 1-based.",
            ),
            arg("line_end", "end", Int, false, "Last line to show."),
        ],
    },
    Tool {
        name: "repo_browser.search",
        id: ToolId::Grep,
        doc: SEARCH_DOC,
        args: &[
            arg("query", "query", Str, true, "The text to find."),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory or file to search; default the project root.",
            ),
        ],
    },
    Tool {
        name: "repo_browser.print_tree",
        id: ToolId::Glob,
        doc: PRINT_TREE_DOC,
        args: &[
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory to list; default the project root.",
            ),
            arg("depth", "depth", Int, false, "How many levels down."),
        ],
    },
    Tool {
        name: "repo_browser.apply_patch",
        id: ToolId::Patch,
        doc: APPLY_PATCH_DOC,
        args: &[arg(
            "patch",
            "patch",
            Str,
            true,
            "The entire patch, from *** Begin Patch to *** End Patch.",
        )],
    },
];

const HARMONY: &[Tool] = &[
    Tool {
        name: "read",
        id: ToolId::Read,
        doc: READ_DOC,
        args: &[
            arg(
                "path",
                "path",
                Str,
                true,
                "File path, relative to the project root or absolute.",
            ),
            arg(
                "offset",
                "offset",
                Int,
                false,
                "First line to show, 1-based.",
            ),
            arg("limit", "limit", Int, false, "Number of lines to show."),
        ],
    },
    Tool {
        name: "edit",
        id: ToolId::Edit,
        doc: EDIT_DOC,
        args: &[
            arg("path", "path", Str, true, "File to change."),
            arg(
                "old_string",
                "old",
                Str,
                true,
                "Exact text to replace; must be unique in the file.",
            ),
            arg("new_string", "new", Str, true, "Replacement text."),
            arg(
                "replace_all",
                "replace_all",
                Bool,
                false,
                "Replace every occurrence.",
            ),
        ],
    },
    Tool {
        name: "write",
        id: ToolId::Write,
        doc: WRITE_DOC,
        args: &[
            arg("path", "path", Str, true, "File to write."),
            arg(
                "content",
                "content",
                Str,
                true,
                "The complete file content.",
            ),
        ],
    },
    Tool {
        name: "shell",
        id: ToolId::Bash,
        doc: SHELL_DOC,
        args: &[
            arg("command", "command", Str, true, "The bash command line."),
            arg(
                "timeout_ms",
                "timeout_ms",
                Int,
                false,
                "Time limit in milliseconds, at most 600000.",
            ),
        ],
    },
    Tool {
        name: "grep",
        id: ToolId::Grep,
        doc: GREP_DOC,
        args: &[
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Regular expression (Rust syntax).",
            ),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory or file to search; default the project root.",
            ),
            arg(
                "glob",
                "glob",
                Str,
                false,
                "Only files matching this glob, e.g. *.rs.",
            ),
        ],
    },
    Tool {
        name: "glob",
        id: ToolId::Glob,
        doc: GLOB_DOC,
        args: &[
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Glob pattern, e.g. src/**/*.rs.",
            ),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory to search; default the project root.",
            ),
        ],
    },
];

/// Gemma 4: alphabetical key order is generation order, so every anchor precedes its
/// replacement and every path precedes its content.
const GEMMA: &[Tool] = &[
    Tool {
        name: "read",
        id: ToolId::Read,
        doc: READ_DOC,
        args: &[
            arg(
                "file",
                "path",
                Str,
                true,
                "File path, relative to the project root or absolute.",
            ),
            arg("limit", "limit", Int, false, "Number of lines to show."),
            arg(
                "offset",
                "offset",
                Int,
                false,
                "First line to show, 1-based.",
            ),
        ],
    },
    Tool {
        name: "edit",
        id: ToolId::Edit,
        doc: EDIT_DOC,
        args: &[
            arg("file", "path", Str, true, "File to change."),
            arg(
                "find",
                "old",
                Str,
                true,
                "Exact text to replace; must be unique in the file.",
            ),
            arg("replace", "new", Str, true, "Replacement text."),
            arg(
                "replace_all",
                "replace_all",
                Bool,
                false,
                "Replace every occurrence.",
            ),
        ],
    },
    Tool {
        name: "write",
        id: ToolId::Write,
        doc: WRITE_DOC,
        args: &[
            arg("file", "path", Str, true, "File to write."),
            arg("text", "content", Str, true, "The complete file content."),
        ],
    },
    Tool {
        name: "bash",
        id: ToolId::Bash,
        doc: SHELL_DOC,
        args: &[
            arg("command", "command", Str, true, "The bash command line."),
            arg(
                "timeout_s",
                "timeout_s",
                Int,
                false,
                "Time limit in seconds, at most 600.",
            ),
        ],
    },
    Tool {
        name: "grep",
        id: ToolId::Grep,
        doc: GREP_DOC,
        args: &[
            arg(
                "glob",
                "glob",
                Str,
                false,
                "Only files matching this glob, e.g. *.rs.",
            ),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory or file to search; default the project root.",
            ),
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Regular expression (Rust syntax).",
            ),
        ],
    },
    Tool {
        name: "glob",
        id: ToolId::Glob,
        doc: GLOB_DOC,
        args: &[
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory to search; default the project root.",
            ),
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Glob pattern, e.g. src/**/*.rs.",
            ),
        ],
    },
];

const QWEN: &[Tool] = &[
    Tool {
        name: "Read",
        id: ToolId::Read,
        doc: READ_DOC,
        args: &[
            arg(
                "file_path",
                "path",
                Str,
                true,
                "File path, relative to the project root or absolute.",
            ),
            arg(
                "offset",
                "offset",
                Int,
                false,
                "First line to show, 1-based.",
            ),
            arg("limit", "limit", Int, false, "Number of lines to show."),
        ],
    },
    Tool {
        name: "Edit",
        id: ToolId::Edit,
        doc: EDIT_DOC,
        args: &[
            arg("file_path", "path", Str, true, "File to change."),
            arg(
                "old_string",
                "old",
                Str,
                true,
                "Exact text to replace; must be unique in the file.",
            ),
            arg("new_string", "new", Str, true, "Replacement text."),
            arg(
                "replace_all",
                "replace_all",
                Bool,
                false,
                "Replace every occurrence.",
            ),
        ],
    },
    Tool {
        name: "Write",
        id: ToolId::Write,
        doc: WRITE_DOC,
        args: &[
            arg("file_path", "path", Str, true, "File to write."),
            arg(
                "content",
                "content",
                Str,
                true,
                "The complete file content.",
            ),
        ],
    },
    Tool {
        name: "Bash",
        id: ToolId::Bash,
        doc: SHELL_DOC,
        args: &[
            arg("command", "command", Str, true, "The bash command line."),
            arg(
                "timeout",
                "timeout_ms",
                Int,
                false,
                "Time limit in milliseconds, at most 600000.",
            ),
        ],
    },
    Tool {
        name: "Grep",
        id: ToolId::Grep,
        doc: GREP_DOC,
        args: &[
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Regular expression (Rust syntax).",
            ),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory or file to search; default the project root.",
            ),
            arg(
                "glob",
                "glob",
                Str,
                false,
                "Only files matching this glob, e.g. *.rs.",
            ),
        ],
    },
    Tool {
        name: "Glob",
        id: ToolId::Glob,
        doc: GLOB_DOC,
        args: &[
            arg(
                "pattern",
                "pattern",
                Str,
                true,
                "Glob pattern, e.g. src/**/*.rs.",
            ),
            arg(
                "path",
                "path",
                Str,
                false,
                "Directory to search; default the project root.",
            ),
        ],
    },
];

fn table(family: Family, layout: Layout) -> &'static [Tool] {
    match (family, layout) {
        (Family::Harmony, Layout::Priors) => HARMONY_PRIORS,
        (Family::Harmony, Layout::Standard) => HARMONY,
        (Family::Gemma4, _) => GEMMA,
        (Family::QwenXml, _) => QWEN,
    }
}

/// The tool declarations for `family` in `layout`, in declaration order (sent with
/// serde_json's `preserve_order`, so the order on the wire is this order).
pub fn declarations(family: Family, layout: Layout) -> Vec<ToolDecl> {
    declarations_for(family, layout, Toolset::All)
}

/// The declarations of `toolset`'s tools, `delegate` last.
pub fn declarations_for(family: Family, layout: Layout, toolset: Toolset) -> Vec<ToolDecl> {
    let mut decls: Vec<ToolDecl> = table(family, layout)
        .iter()
        .filter(|t| toolset.has(&t.id))
        .map(|t| {
            let mut props = Map::new();
            for a in t.args {
                let mut schema = a.kind.json();
                schema["description"] = json!(a.doc);
                props.insert(a.name.into(), schema);
            }
            let required: Vec<&str> = t
                .args
                .iter()
                .filter(|a| a.required)
                .map(|a| a.name)
                .collect();
            ToolDecl {
                name: t.name.into(),
                description: t.doc.into(),
                parameters: json!({
                    "type": "object",
                    "properties": props,
                    "required": required,
                    "additionalProperties": false,
                }),
            }
        })
        .collect();
    if toolset == Toolset::Delegating {
        decls.push(ToolDecl {
            name: delegate_name(family).into(),
            description: DELEGATE_DOC.into(),
            parameters: delegate_parameters(),
        });
    }
    decls
}

/// The tool a model-facing name stands for in `family` and `layout`.
pub fn id_of(family: Family, layout: Layout, name: &str) -> Option<ToolId> {
    table(family, layout)
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.id.clone())
}

/// The model-facing name of a tool, for messages about it.
pub fn name_of(family: Family, layout: Layout, id: &ToolId) -> Option<&'static str> {
    table(family, layout)
        .iter()
        .find(|t| &t.id == id)
        .map(|t| t.name)
}

/// The field an argument fills, the tool and the argument by their model-facing names in
/// `family` and `layout`: what a call the model is still writing shows of it.
pub fn field_of(family: Family, layout: Layout, tool: &str, arg: &str) -> Option<&'static str> {
    table(family, layout)
        .iter()
        .find(|t| t.name == tool)?
        .args
        .iter()
        .find(|a| a.name == arg)
        .map(|a| a.field)
}

/// Checks a call against its declaration and maps it to a [`Call`].
pub fn parse(
    family: Family,
    layout: Layout,
    name: &str,
    arguments: &str,
) -> Result<Call, ArgError> {
    parse_for(family, layout, Toolset::All, name, arguments)
}

/// [`parse`] for an agent with `toolset`: a tool it does not have is unknown.
pub fn parse_for(
    family: Family,
    layout: Layout,
    toolset: Toolset,
    name: &str,
    arguments: &str,
) -> Result<Call, ArgError> {
    if toolset == Toolset::Delegating && name == delegate_name(family) {
        return delegate(name, arguments);
    }
    let tools: Vec<&Tool> = table(family, layout)
        .iter()
        .filter(|t| toolset.has(&t.id))
        .collect();
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        let mut known: Vec<&str> = tools.iter().map(|t| t.name).collect();
        if toolset == Toolset::Delegating {
            known.push(delegate_name(family));
        }
        return Err(ArgError::UnknownTool(name.into(), known.join(", ")));
    };
    let tname = || tool.name.to_owned();
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(arguments) else {
        return Err(ArgError::NotObject { tool: tname() });
    };
    for key in obj.keys() {
        if !tool.args.iter().any(|a| a.name == key) {
            return Err(ArgError::Unknown {
                tool: tname(),
                arg: key.clone(),
                known: tool
                    .args
                    .iter()
                    .map(|a| a.name)
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    }
    let mut fields = Map::new();
    for a in tool.args {
        match obj.get(a.name) {
            None | Some(Value::Null) if a.required => {
                return Err(ArgError::Missing {
                    tool: tname(),
                    arg: a.name.into(),
                });
            }
            None | Some(Value::Null) => {}
            Some(v) => {
                let value = coerce(v, a.kind).ok_or_else(|| ArgError::Type {
                    tool: tname(),
                    arg: a.name.into(),
                    want: a.kind.schema(),
                })?;
                fields.insert(a.field.into(), value);
            }
        }
    }
    let s = |k: &str| fields.get(k).and_then(Value::as_str).map(str::to_owned);
    let n = |k: &str| fields.get(k).and_then(Value::as_u64).map(|n| n as usize);
    let req = |k: &str| s(k).expect("required string checked above");
    Ok(match tool.id {
        // open_file gives the last line where read gives a count
        ToolId::Read => Call::Read {
            path: req("path"),
            offset: n("offset"),
            limit: n("limit").or_else(|| {
                n("end").map(|end| {
                    end.saturating_add(1)
                        .saturating_sub(n("offset").unwrap_or(1))
                        .max(1)
                })
            }),
        },
        ToolId::Edit => Call::Edit {
            path: req("path"),
            old: req("old"),
            new: req("new"),
            replace_all: fields
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        ToolId::Write => Call::Write {
            path: req("path"),
            content: req("content"),
        },
        ToolId::Bash => {
            let command = match fields.get("argv") {
                Some(Value::Array(argv)) => match argv.as_slice() {
                    // the patch as the argument, Codex's other way to call it
                    [Value::String(w), Value::String(patch)]
                        if PATCH_WORDS.contains(&w.as_str()) =>
                    {
                        return Ok(Call::Patch {
                            patch: patch.clone(),
                        });
                    }
                    _ => command_line(argv),
                },
                _ => req("command"),
            };
            if let Some(patch) = patch_in(&command) {
                return Ok(Call::Patch { patch });
            }
            Call::Shell {
                command,
                timeout: n("timeout_ms")
                    .map(|ms| Duration::from_millis(ms as u64))
                    .or_else(|| n("timeout_s").map(|s| Duration::from_secs(s as u64))),
            }
        }
        // search finds a text, grep a regular expression
        ToolId::Grep => Call::Grep {
            pattern: s("pattern").unwrap_or_else(|| regex::escape(&req("query"))),
            path: s("path"),
            // models fill optional fields with "": an empty filter is no filter, not one
            // that no file matches
            glob: s("glob").filter(|g| !g.trim().is_empty()),
        },
        ToolId::Glob => Call::Glob {
            pattern: s("pattern").unwrap_or_else(|| levels(n("depth").unwrap_or(2))),
            path: s("path"),
        },
        ToolId::Patch => Call::Patch {
            patch: req("patch"),
        },
        ref other => unreachable!("no projection declares {other:?}"),
    })
}

/// `delegate`'s tasks, checked: each has a title and a goal, a write task its files.
fn delegate(tool: &str, arguments: &str) -> Result<Call, ArgError> {
    let invalid = |why: String| ArgError::Invalid {
        tool: tool.to_owned(),
        why,
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(arguments) else {
        return Err(ArgError::NotObject { tool: tool.into() });
    };
    if let Some(key) = obj.keys().find(|k| *k != "tasks") {
        return Err(ArgError::Unknown {
            tool: tool.into(),
            arg: key.clone(),
            known: "tasks".into(),
        });
    }
    let Some(Value::Array(list)) = obj.get("tasks") else {
        return Err(ArgError::Missing {
            tool: tool.into(),
            arg: "tasks".into(),
        });
    };
    if list.is_empty() || list.len() > MAX_TASKS {
        return Err(invalid(format!(
            "give 1 to {MAX_TASKS} tasks, not {}",
            list.len()
        )));
    }
    let mut tasks = Vec::new();
    for (i, t) in list.iter().enumerate() {
        let n = i + 1;
        let Value::Object(t) = t else {
            return Err(invalid(format!("task {n} is not an object")));
        };
        let text = |key: &str| {
            t.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let (Some(title), Some(goal)) = (text("title"), text("goal")) else {
            return Err(invalid(format!("task {n} needs a title and a goal")));
        };
        let write = match t.get("mode").and_then(Value::as_str) {
            Some("read") | None => false,
            Some("write") => true,
            Some(other) => {
                return Err(invalid(format!(
                    "task {n}: mode is read or write, not {other:?}"
                )));
            }
        };
        let files: Vec<String> = match t.get("files") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect(),
            Some(_) => return Err(invalid(format!("task {n}: files is a list of paths"))),
        };
        if write && files.is_empty() {
            return Err(invalid(format!(
                "task {n} is a write task: list the files it may change"
            )));
        }
        tasks.push(Task {
            title,
            goal,
            context: text("context"),
            write,
            files,
            agent: text("agent"),
        });
    }
    Ok(Call::Delegate { tasks })
}

/// The words that call apply_patch in a command line.
const PATCH_WORDS: [&str; 3] = ["apply_patch", "applypatch", "repo_browser.apply_patch"];

/// The patch of a command line that is nothing but `apply_patch <<'EOF' … EOF`, Codex's
/// way of calling it, which gpt-oss learnt: given container.exec, it wrote 13 of 34 patches
/// so on the first run of the layout of its training, and 4 more without the closing
/// marker, which bash reads to the end. A shell has no apply_patch, so such a command is
/// the patch's call.
fn patch_in(command: &str) -> Option<String> {
    let rest = PATCH_WORDS
        .iter()
        .find_map(|w| command.trim_start().strip_prefix(w))?;
    let heredoc = rest.trim_start().strip_prefix("<<")?;
    let heredoc = heredoc.strip_prefix('-').unwrap_or(heredoc);
    let (marker, body) = heredoc.split_once('\n')?;
    let marker = marker.trim().trim_matches(['\'', '"']);
    if marker.is_empty()
        || !marker
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut lines = Vec::new();
    let mut tail = body.lines();
    for line in tail.by_ref() {
        if line.trim() == marker {
            // nothing may follow it: the command is the patch alone
            return tail.all(|l| l.trim().is_empty()).then(|| lines.join("\n"));
        }
        lines.push(line);
    }
    // no closing marker: bash reads to the end, and a patch it is
    let body = lines.join("\n");
    body.trim_start()
        .starts_with("*** Begin Patch")
        .then(|| body.trim_end().to_owned())
}

/// A command line from an argument list: the script of `bash -lc SCRIPT` as it is,
/// anything else quoted for the shell where it needs it.
fn command_line(argv: &[Value]) -> String {
    let args: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    if let [shell, flag, script] = args.as_slice()
        && matches!(*shell, "bash" | "sh" | "zsh" | "/bin/bash" | "/bin/sh")
        && matches!(*flag, "-c" | "-lc" | "-cl")
    {
        return (*script).to_owned();
    }
    args.iter()
        .map(|a| {
            let plain = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-./=:,+@%".contains(c));
            if plain {
                (*a).to_owned()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A glob for the files `depth` levels down, at most 6.
fn levels(depth: usize) -> String {
    let all: Vec<String> = (1..=depth.clamp(1, 6))
        .map(|i| vec!["*"; i].join("/"))
        .collect();
    format!("{{{}}}", all.join(","))
}

/// Strings stay strings; integers and booleans may arrive as their string forms.
fn coerce(v: &Value, kind: Kind) -> Option<Value> {
    match (kind, v) {
        (Kind::StrList, Value::Array(a)) if a.iter().all(Value::is_string) && !a.is_empty() => {
            Some(v.clone())
        }
        // a command line where a list belongs
        (Kind::StrList, Value::String(line)) => Some(json!(["bash", "-lc", line])),
        (Kind::Str, Value::String(_)) => Some(v.clone()),
        (Kind::Int, Value::Number(n)) => n.as_u64().map(Value::from),
        (Kind::Int, Value::String(s)) => s.trim().parse::<u64>().ok().map(Value::from),
        (Kind::Bool, Value::Bool(_)) => Some(v.clone()),
        (Kind::Bool, Value::String(s)) => match s.trim() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_being_written_has_ognivos_names() {
        let (h, q, g) = (Family::Harmony, Family::QwenXml, Family::Gemma4);
        let s = Layout::Standard;
        // the file and its content, whatever each family calls them
        for (family, tool) in [(h, "write"), (q, "Write"), (g, "write")] {
            let path = ["path", "file", "file_path"]
                .into_iter()
                .find_map(|a| field_of(family, s, tool, a));
            assert_eq!(path, Some("path"), "{family:?}");
        }
        assert_eq!(field_of(q, s, "Write", "content"), Some("content"));
        assert_eq!(
            field_of(h, Layout::Priors, "repo_browser.apply_patch", "patch"),
            Some("patch")
        );
        assert_eq!(
            field_of(h, Layout::Priors, "container.exec", "command"),
            Some("argv")
        );
        assert_eq!(field_of(h, s, "write", "nope"), None);
        assert_eq!(field_of(h, s, "nope", "path"), None);
    }

    #[test]
    fn delegate_comes_only_with_subagents_and_its_tasks_are_checked() {
        let h = Family::Harmony;
        let names = |t: Toolset| -> Vec<String> {
            declarations_for(h, Layout::Standard, t)
                .into_iter()
                .map(|d| d.name)
                .collect()
        };
        assert_eq!(names(Toolset::Delegating).last().unwrap(), "delegate");
        assert!(!names(Toolset::All).iter().any(|n| n == "delegate"));
        // a subagent that only reads has no tool that changes files
        assert_eq!(names(Toolset::Reading), ["read", "shell", "grep", "glob"]);
        assert!(matches!(
            parse_for(h, Layout::Standard, Toolset::Reading, "edit", "{}"),
            Err(ArgError::UnknownTool(..))
        ));
        assert!(matches!(
            parse(h, Layout::Standard, "delegate", r#"{"tasks":[]}"#),
            Err(ArgError::UnknownTool(..))
        ));
        let args = r#"{"tasks":[
            {"title":"Find the loader","goal":"Where is the config read?","mode":"read","files":["src"]},
            {"title":"Rename","goal":"Rename x to y","context":"x is in a.rs","mode":"write","files":["src/a.rs"]}
        ]}"#;
        let call = parse_for(h, Layout::Standard, Toolset::Delegating, "delegate", args).unwrap();
        assert_eq!(
            call,
            Call::Delegate {
                tasks: vec![
                    Task {
                        title: "Find the loader".into(),
                        goal: "Where is the config read?".into(),
                        context: None,
                        write: false,
                        files: vec!["src".into()],
                        agent: None,
                    },
                    Task {
                        title: "Rename".into(),
                        goal: "Rename x to y".into(),
                        context: Some("x is in a.rs".into()),
                        write: true,
                        files: vec!["src/a.rs".into()],
                        agent: None,
                    },
                ]
            }
        );
        for bad in [
            r#"{"tasks":[]}"#,
            r#"{"tasks":[{"title":"t","goal":"g","mode":"write"}]}"#,
            r#"{"tasks":[{"title":" ","goal":"g","mode":"read"}]}"#,
            r#"{"tasks":[{"title":"t","goal":"g","mode":"fast"}]}"#,
        ] {
            let e = parse_for(h, Layout::Standard, Toolset::Delegating, "delegate", bad);
            assert!(matches!(e, Err(ArgError::Invalid { .. })), "{bad}: {e:?}");
        }
        let many = format!(
            r#"{{"tasks":[{}]}}"#,
            [r#"{"title":"t","goal":"g","mode":"read"}"#; MAX_TASKS + 1].join(",")
        );
        assert!(parse_for(h, Layout::Standard, Toolset::Delegating, "delegate", &many).is_err());
        // Qwen knows the tool by Claude Code's name
        assert_eq!(
            declarations_for(Family::QwenXml, Layout::Standard, Toolset::Delegating)
                .last()
                .unwrap()
                .name,
            "Task"
        );
    }

    #[test]
    fn gemma_keys_sort_into_generation_order() {
        for d in declarations(Family::Gemma4, Layout::Standard) {
            let keys: Vec<&String> = d.parameters["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(
                keys, sorted,
                "{}: declared order must be alphabetical",
                d.name
            );
        }
        let edit = &declarations(Family::Gemma4, Layout::Standard)[1];
        let keys: Vec<_> = edit.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["file", "find", "replace", "replace_all"]);
    }

    #[test]
    fn each_family_maps_to_the_same_call() {
        let want = Call::Edit {
            path: "src/a.rs".into(),
            old: "x".into(),
            new: "y".into(),
            replace_all: false,
        };
        let cases = [
            (
                Family::Harmony,
                "edit",
                r#"{"path":"src/a.rs","old_string":"x","new_string":"y"}"#,
            ),
            (
                Family::Gemma4,
                "edit",
                r#"{"file":"src/a.rs","find":"x","replace":"y","replace_all":"false"}"#,
            ),
            (
                Family::QwenXml,
                "Edit",
                r#"{"file_path":"src/a.rs","old_string":"x","new_string":"y"}"#,
            ),
        ];
        for (family, name, args) in cases {
            assert_eq!(
                parse(family, Layout::Standard, name, args).unwrap(),
                want,
                "{family}"
            );
        }
        let shell = parse(
            Family::QwenXml,
            Layout::Standard,
            "Bash",
            r#"{"command":"ls","timeout":"5000"}"#,
        )
        .unwrap();
        assert_eq!(
            shell,
            Call::Shell {
                command: "ls".into(),
                timeout: Some(Duration::from_secs(5))
            }
        );
        let g = parse(
            Family::Gemma4,
            Layout::Standard,
            "bash",
            r#"{"command":"ls","timeout_s":7}"#,
        )
        .unwrap();
        assert_eq!(
            g,
            Call::Shell {
                command: "ls".into(),
                timeout: Some(Duration::from_secs(7))
            }
        );
    }

    #[test]
    fn an_empty_file_filter_filters_nothing() {
        // the second three-model run of 2026-11-02: gpt-oss sent `"glob": ""`, grep found
        // nothing in files that had the name, and the model searched again until the limit
        for (family, name) in [
            (Family::Harmony, "grep"),
            (Family::Gemma4, "grep"),
            (Family::QwenXml, "Grep"),
        ] {
            let g = parse(
                family,
                Layout::Standard,
                name,
                r#"{"pattern":"calc_total","glob":" "}"#,
            )
            .unwrap();
            assert_eq!(
                g,
                Call::Grep {
                    pattern: "calc_total".into(),
                    path: None,
                    glob: None
                },
                "{family}"
            );
        }
    }

    #[test]
    fn bad_arguments_are_explained() {
        let h = Family::Harmony;
        assert!(matches!(
            parse(h, Layout::Standard, "Read", "{}"),
            Err(ArgError::UnknownTool(..))
        ));
        assert!(matches!(
            parse(h, Layout::Standard, "read", "[]"),
            Err(ArgError::NotObject { .. })
        ));
        assert!(
            matches!(parse(h, Layout::Standard, "read", "{}"), Err(ArgError::Missing { arg, .. }) if arg == "path")
        );
        // OpenCode's camelCase from M0 is not ours: say which names exist
        let e = parse(
            h,
            Layout::Standard,
            "edit",
            r#"{"path":"a","oldString":"x","newString":"y"}"#,
        )
        .unwrap_err();
        assert!(e.to_string().contains("old_string, new_string"), "{e}");
        assert!(matches!(
            parse(h, Layout::Standard, "read", r#"{"path":"a","limit":"ten"}"#),
            Err(ArgError::Type { .. })
        ));
    }

    #[test]
    fn declarations_are_strict_schemas() {
        for f in [Family::Harmony, Family::Gemma4, Family::QwenXml] {
            let decls = declarations(f, Layout::Standard);
            assert_eq!(decls.len(), 6, "{f}");
            for d in decls {
                assert_eq!(d.parameters["additionalProperties"], json!(false));
                assert!(
                    !d.description.contains('\n'),
                    "one-line descriptions for gpt-oss"
                );
            }
        }
    }

    #[test]
    fn gpt_oss_tools_of_its_training_map_to_ognivo_calls() {
        let h = Family::Harmony;
        let p = |name: &str, args: &str| parse(h, Layout::Priors, name, args).unwrap();
        // the script of bash -lc as it is, other lists quoted where they need it
        assert_eq!(
            p(
                "container.exec",
                r#"{"command":["bash","-lc","cargo test 2>&1 | tail -5"]}"#
            ),
            Call::Shell {
                command: "cargo test 2>&1 | tail -5".into(),
                timeout: None
            }
        );
        assert_eq!(
            p(
                "container.exec",
                r#"{"command":["grep","-n","fn main","src/main.rs"]}"#
            ),
            Call::Shell {
                command: "grep -n 'fn main' src/main.rs".into(),
                timeout: None
            }
        );
        // the last line to show is a count of lines for read
        assert_eq!(
            p(
                "repo_browser.open_file",
                r#"{"path":"a.rs","line_start":10,"line_end":19}"#
            ),
            Call::Read {
                path: "a.rs".into(),
                offset: Some(10),
                limit: Some(10)
            }
        );
        // search finds a text, so it is a regular expression of that text
        assert_eq!(
            p("repo_browser.search", r#"{"query":"foo(bar"}"#),
            Call::Grep {
                pattern: r"foo\(bar".into(),
                path: None,
                glob: None
            }
        );
        assert_eq!(
            p("repo_browser.print_tree", r#"{"depth":2}"#),
            Call::Glob {
                pattern: "{*,*/*}".into(),
                path: None
            }
        );
        assert_eq!(
            p(
                "repo_browser.apply_patch",
                r#"{"patch":"*** Begin Patch\n*** End Patch"}"#
            ),
            Call::Patch {
                patch: "*** Begin Patch\n*** End Patch".into()
            }
        );
        // the names are the model's own, and edit is not among them
        let names: Vec<String> = declarations(h, Layout::Priors)
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            names,
            [
                "container.exec",
                "repo_browser.open_file",
                "repo_browser.search",
                "repo_browser.print_tree",
                "repo_browser.apply_patch"
            ]
        );
        assert!(parse(h, Layout::Priors, "edit", "{}").is_err());
    }

    #[test]
    fn apply_patch_through_a_shell_is_the_patch() {
        let body = "*** Begin Patch\n*** Update File: a.rs\n@@\n-1\n+2\n*** End Patch";
        let patch = Call::Patch { patch: body.into() };
        let exec = |argv: Value| {
            parse(
                Family::Harmony,
                Layout::Priors,
                "container.exec",
                &json!({ "command": argv }).to_string(),
            )
            .unwrap()
        };
        // a heredoc, as gpt-oss writes it after Codex
        assert_eq!(
            exec(json!([
                "bash",
                "-lc",
                format!("apply_patch <<'PATCH'\n{body}\nPATCH")
            ])),
            patch
        );
        assert_eq!(
            exec(json!([
                "bash",
                "-lc",
                format!("repo_browser.apply_patch <<EOF\n{body}\nEOF\n")
            ])),
            patch
        );
        // the patch as the argument
        assert_eq!(exec(json!(["apply_patch", body])), patch);
        // without its closing marker, as bash reads it
        assert_eq!(
            exec(json!([
                "bash",
                "-lc",
                format!("apply_patch <<'PATCH'\n{body}")
            ])),
            patch
        );
        // more after it is a shell's command, and stays one
        assert!(matches!(
            exec(json!([
                "bash",
                "-lc",
                format!("apply_patch <<'PATCH'\n{body}\nPATCH\ncargo test")
            ])),
            Call::Shell { .. }
        ));
        // the same in ognivo's own shell tool
        let shell = parse(
            Family::Harmony,
            Layout::Standard,
            "shell",
            &json!({ "command": format!("apply_patch <<'EOF'\n{body}\nEOF") }).to_string(),
        )
        .unwrap();
        assert_eq!(shell, patch);
    }
}
