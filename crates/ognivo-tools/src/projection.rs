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

use std::time::Duration;

use ognivo_models::{Family, ToolDecl};
use serde_json::{Map, Value, json};

use crate::ToolId;

/// A tool call in ognivo's own terms.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl Call {
    pub fn id(&self) -> ToolId {
        match self {
            Call::Read { .. } => ToolId::Read,
            Call::Edit { .. } => ToolId::Edit,
            Call::Write { .. } => ToolId::Write,
            Call::Shell { .. } => ToolId::Bash,
            Call::Grep { .. } => ToolId::Grep,
            Call::Glob { .. } => ToolId::Glob,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Str,
    Int,
    Bool,
}

impl Kind {
    fn schema(self) -> &'static str {
        match self {
            Kind::Str => "string",
            Kind::Int => "integer",
            Kind::Bool => "boolean",
        }
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

use Kind::{Bool, Int, Str};

const READ_DOC: &str = "Read a text file with line numbers; the first line gives its size. Pages of up to 2000 lines: continue with offset.";
const EDIT_DOC: &str = "Replace one exact, unique occurrence of a text in a file with new text. Copy the text from read output without the line numbers.";
const WRITE_DOC: &str =
    "Create a file, or replace a whole file you have read, with the given content.";
const SHELL_DOC: &str = "Run a bash command in the project and return its exit code and output. The working directory persists between calls.";
const GREP_DOC: &str = "Search file contents line by line with a regular expression (Rust syntax: escape [ ] ( ) { } . * + ? | \\ to match them literally); returns path:line:text for each match.";
const GLOB_DOC: &str =
    "List files whose path matches a glob pattern such as **/*.rs, newest first.";

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

fn table(family: Family) -> &'static [Tool] {
    match family {
        Family::Harmony => HARMONY,
        Family::Gemma4 => GEMMA,
        Family::QwenXml => QWEN,
    }
}

/// The tool declarations for `family`, in declaration order (sent with serde_json's
/// `preserve_order`, so the order on the wire is this order).
pub fn declarations(family: Family) -> Vec<ToolDecl> {
    table(family)
        .iter()
        .map(|t| {
            let mut props = Map::new();
            for a in t.args {
                props.insert(
                    a.name.into(),
                    json!({"type": a.kind.schema(), "description": a.doc}),
                );
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
        .collect()
}

/// The model-facing name of a tool, for messages about it.
pub fn name_of(family: Family, id: &ToolId) -> Option<&'static str> {
    table(family).iter().find(|t| &t.id == id).map(|t| t.name)
}

/// Checks a call against its declaration and maps it to a [`Call`].
pub fn parse(family: Family, name: &str, arguments: &str) -> Result<Call, ArgError> {
    let tools = table(family);
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        let known = tools.iter().map(|t| t.name).collect::<Vec<_>>().join(", ");
        return Err(ArgError::UnknownTool(name.into(), known));
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
        ToolId::Read => Call::Read {
            path: req("path"),
            offset: n("offset"),
            limit: n("limit"),
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
        ToolId::Bash => Call::Shell {
            command: req("command"),
            timeout: n("timeout_ms")
                .map(|ms| Duration::from_millis(ms as u64))
                .or_else(|| n("timeout_s").map(|s| Duration::from_secs(s as u64))),
        },
        ToolId::Grep => Call::Grep {
            pattern: req("pattern"),
            path: s("path"),
            glob: s("glob"),
        },
        ToolId::Glob => Call::Glob {
            pattern: req("pattern"),
            path: s("path"),
        },
        ref other => unreachable!("no projection declares {other:?}"),
    })
}

/// Strings stay strings; integers and booleans may arrive as their string forms.
fn coerce(v: &Value, kind: Kind) -> Option<Value> {
    match (kind, v) {
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
    fn gemma_keys_sort_into_generation_order() {
        for d in declarations(Family::Gemma4) {
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
        let edit = &declarations(Family::Gemma4)[1];
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
            assert_eq!(parse(family, name, args).unwrap(), want, "{family}");
        }
        let shell = parse(
            Family::QwenXml,
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
        let g = parse(Family::Gemma4, "bash", r#"{"command":"ls","timeout_s":7}"#).unwrap();
        assert_eq!(
            g,
            Call::Shell {
                command: "ls".into(),
                timeout: Some(Duration::from_secs(7))
            }
        );
    }

    #[test]
    fn bad_arguments_are_explained() {
        let h = Family::Harmony;
        assert!(matches!(
            parse(h, "Read", "{}"),
            Err(ArgError::UnknownTool(..))
        ));
        assert!(matches!(
            parse(h, "read", "[]"),
            Err(ArgError::NotObject { .. })
        ));
        assert!(
            matches!(parse(h, "read", "{}"), Err(ArgError::Missing { arg, .. }) if arg == "path")
        );
        // OpenCode's camelCase from M0 is not ours: say which names exist
        let e = parse(h, "edit", r#"{"path":"a","oldString":"x","newString":"y"}"#).unwrap_err();
        assert!(e.to_string().contains("old_string, new_string"), "{e}");
        assert!(matches!(
            parse(h, "read", r#"{"path":"a","limit":"ten"}"#),
            Err(ArgError::Type { .. })
        ));
    }

    #[test]
    fn declarations_are_strict_schemas() {
        for f in [Family::Harmony, Family::Gemma4, Family::QwenXml] {
            let decls = declarations(f);
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
}
