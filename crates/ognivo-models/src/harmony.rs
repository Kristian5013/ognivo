//! Harmony, the format of gpt-oss (docs/architecture.md §5.5), rendered the way its
//! reference implementation renders it, for the Native path (M2).
//!
//! The rendering follows openai/harmony 0.0.8 (`src/encoding.rs`, Apache-2.0) to the
//! character, the conversion of JSON Schemas to its TypeScript form included: that is
//! the form the model was trained on. The golden test holds it to the reference's
//! rendering of `evals/templates/scenarios/seq.json` (profiles/templates/gpt-oss-20b/
//! golden, made by docs/research/2026-10-29/template-tools/harmony_ref.py).
//!
//! A prompt is a list of pieces: control tokens by id, and text that is tokenized as
//! content, never with special-token parsing (§5.3). Pieces are not merged: the reference
//! tokenizes each piece on its own, and the ids at a piece's edges depend on it.
//!
//! What the model writes back is read by [`parse`], token by token and tolerantly.

pub mod parse;

use std::collections::HashSet;

use serde_json::Value;

use crate::engine::{Constraint, TokenId, Trigger};

/// The control tokens of Harmony and their ids in o200k_harmony.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Special {
    Start,
    End,
    Message,
    Channel,
    Constrain,
    Call,
    Return,
}

impl Special {
    pub fn id(self) -> u32 {
        match self {
            Special::Start => 200_006,
            Special::End => 200_007,
            Special::Message => 200_008,
            Special::Channel => 200_005,
            Special::Constrain => 200_003,
            Special::Call => 200_012,
            Special::Return => 200_002,
        }
    }

    pub const ALL: [Special; 7] = [
        Special::Start,
        Special::End,
        Special::Message,
        Special::Channel,
        Special::Constrain,
        Special::Call,
        Special::Return,
    ];

    pub fn from_id(id: TokenId) -> Option<Special> {
        Special::ALL.into_iter().find(|s| s.id() == id)
    }

    /// The token's text, put together here so that no control token stands in the
    /// source in one piece (§5.3).
    pub fn text(self) -> String {
        let name = match self {
            Special::Start => "start",
            Special::End => "end",
            Special::Message => "message",
            Special::Channel => "channel",
            Special::Constrain => "constrain",
            Special::Call => "call",
            Special::Return => "return",
        };
        format!("<|{name}|>")
    }
}

/// A piece of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Special(Special),
    /// Text tokenized on its own, as content.
    Text(String),
    /// Ids the model generated, sent back as they came (§5.4, `seal()`).
    Tokens(Vec<TokenId>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompt {
    pub pieces: Vec<Piece>,
}

impl Prompt {
    pub(crate) fn special(&mut self, s: Special) {
        self.pieces.push(Piece::Special(s));
    }

    /// The reference encodes an empty text as nothing.
    pub(crate) fn text(&mut self, t: impl Into<String>) {
        let t = t.into();
        if !t.is_empty() {
            self.pieces.push(Piece::Text(t));
        }
    }

    pub(crate) fn tokens(&mut self, ids: Vec<TokenId>) {
        if !ids.is_empty() {
            self.pieces.push(Piece::Tokens(ids));
        }
    }

    /// The start of the assistant's next message, where generation begins.
    pub(crate) fn assistant(&mut self) {
        self.special(Special::Start);
        self.text(Role::Assistant.as_str());
    }

    /// The prompt as text with its control tokens written out, as the golden files hold it;
    /// ids sent back as they came show as their number.
    pub fn display(&self) -> String {
        self.pieces
            .iter()
            .map(|p| match p {
                Piece::Special(s) => s.text(),
                Piece::Text(t) => t.clone(),
                Piece::Tokens(ids) => format!("⟨{} ids⟩", ids.len()),
            })
            .collect()
    }

    /// `m`, rendered as the reference renders it; `functions` tells whether the developer
    /// message declares functions, which the system message then names.
    pub(crate) fn message(&mut self, m: &Message, functions: bool) {
        self.special(Special::Start);
        if m.role == Role::Tool {
            // a tool's name stands for its role
            self.text(m.name.clone().unwrap_or_default());
        } else {
            self.text(m.role.as_str());
            if let Some(name) = &m.name {
                self.text(format!(":{name}"));
            }
        }
        if let Some(r) = m.recipient.as_deref().filter(|r| *r != "all") {
            self.text(format!(" to={r}"));
        }
        if let Some(c) = &m.channel {
            self.special(Special::Channel);
            self.text(c.clone());
        }
        if let Some(t) = &m.content_type {
            match t.strip_prefix(&Special::Constrain.text()) {
                Some(rest) => {
                    self.text(" ");
                    self.special(Special::Constrain);
                    self.text(rest);
                }
                None => self.text(format!(" {t}")),
            }
        }
        self.special(Special::Message);
        match &m.content {
            Content::Text(t) => self.text(t.clone()),
            Content::System(s) => self.text(system_text(s, functions)),
            Content::Developer {
                instructions,
                namespaces,
            } => {
                let mut sections = Vec::new();
                if let Some(i) = instructions {
                    sections.push("# Instructions".to_owned());
                    sections.push(i.clone());
                }
                if namespaces
                    .iter()
                    .any(|n| !n.tools.is_empty() || n.description.is_some())
                {
                    sections.push(tools_section(namespaces));
                }
                self.text(sections.join("\n\n"));
            }
        }
        self.special(if m.role == Role::Assistant && m.recipient.is_some() {
            Special::Call
        } else {
            Special::End
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// Where a call to the tool `name` goes: a name with a namespace is that built-in tool
/// itself (`container.exec`), any other a function (`functions.read`).
pub fn recipient(name: &str) -> String {
    if name.contains('.') {
        name.to_owned()
    } else {
        format!("functions.{name}")
    }
}

/// The grammar of a call to the tools in `tools` (§5.7): from `to=` and a namespace on, a
/// declared tool's name ([`recipient`]), the rest of its header in either order, and its
/// arguments as its schema has them ([`grammar`](crate::grammar)), then only the end of the
/// generation. llama.cpp's own grammar for gpt-oss keeps calls so on the Generic path:
/// without it, the first runs of the Native path had gpt-oss call the tools of its training
/// (`search`, 10 times in 432 calls) and give one tool's arguments to another (`glob` to
/// glob, 8 times).
///
/// It is lazy. It wakes at `to=` and a namespace of the tools in a header, at the start of
/// the generation or of an assistant message, as llama.cpp's triggers for gpt-oss do:
/// llama.cpp searches what the generation wrote so far, control tokens as their text, and
/// feeds the grammar from the first group of the match. Reasoning stays free, and the reader
/// takes the header as it comes ([`parse`]). A control token inside a string it lets
/// through, since it sees its text; the reader keeps such a call from running.
pub fn arguments_constraint(tools: &[crate::generic::ToolDecl]) -> Constraint {
    let mut namespaces: Vec<&str> = Vec::new();
    for t in tools {
        let ns = t.name.split_once('.').map_or("functions", |(ns, _)| ns);
        if !namespaces.contains(&ns) {
            namespaces.push(ns);
        }
    }
    let e = |s: Special| s.text().replace('|', r"\|");
    let trigger = format!(
        r"(?:^|{start}assistant)(?:{channel}(?:commentary|analysis))?(\s*to=(?:{namespaces}))",
        start = e(Special::Start),
        channel = e(Special::Channel),
        namespaces = namespaces.join("|"),
    );
    let lit = |s: Special| crate::grammar::literal(&s.text());
    let header = format!(
        "( {channel} ( \"commentary\" | \"analysis\" ) )? \" \"? ( {constrain} \"json\" | \"json\" )? {message} ws",
        channel = lit(Special::Channel),
        constrain = lit(Special::Constrain),
        message = lit(Special::Message),
    );
    let mut rules = crate::grammar::Rules::default();
    let header = rules.add("header", header);
    let calls: Vec<String> = tools
        .iter()
        .map(|t| {
            let args = rules.schema(&t.parameters, &format!("args-{}", t.name));
            format!(
                "{} {header} {args}",
                crate::grammar::literal(&recipient(&t.name))
            )
        })
        .collect();
    let call = rules.add("call", calls.join(" | "));
    let gbnf = rules.grammar(&format!("[ \\t\\n]* \"to=\" {call}"));
    Constraint::new(gbnf, vec![Trigger::Pattern(trigger)])
}

/// A tool as Harmony declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema of its argument object; `None` declares a tool without arguments.
    pub parameters: Option<Value>,
}

/// Tools under one name: `functions` in the developer message, or a namespace of the
/// model's own in the system message (`container`, `repo_browser`).
#[derive(Debug, Clone, PartialEq)]
pub struct Namespace {
    pub name: String,
    pub description: Option<String>,
    pub tools: Vec<Tool>,
}

/// What the system message says.
#[derive(Debug, Clone, PartialEq)]
pub struct System {
    pub model_identity: String,
    pub knowledge_cutoff: String,
    /// The date the session started, `YYYY-MM-DD`: fixed for the session, or the KV cache
    /// goes at midnight.
    pub date: String,
    pub effort: Effort,
    /// Namespaces of the model's own tools.
    pub namespaces: Vec<Namespace>,
}

impl System {
    /// What gpt-oss's own harness says, on a given date and effort.
    pub fn new(date: &str, effort: Effort) -> Self {
        System {
            model_identity: "You are ChatGPT, a large language model trained by OpenAI.".into(),
            knowledge_cutoff: "2024-06".into(),
            date: date.into(),
            effort,
            namespaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::Developer => "developer",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    System(System),
    Developer {
        instructions: Option<String>,
        namespaces: Vec<Namespace>,
    },
}

/// A Harmony message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub role: Role,
    /// The author's name: a tool's `functions.read_file`, which stands for its role.
    pub name: Option<String>,
    pub recipient: Option<String>,
    pub channel: Option<String>,
    /// The constrain token's text followed by `json`, or plain text such as `json`.
    pub content_type: Option<String>,
    pub content: Content,
}

impl Message {
    fn of(role: Role, content: Content) -> Self {
        Message {
            role,
            name: None,
            recipient: None,
            channel: None,
            content_type: None,
            content,
        }
    }

    pub fn system(system: System) -> Self {
        Message::of(Role::System, Content::System(system))
    }

    pub fn developer(instructions: Option<String>, namespaces: Vec<Namespace>) -> Self {
        Message::of(
            Role::Developer,
            Content::Developer {
                instructions,
                namespaces,
            },
        )
    }

    pub fn user(text: &str) -> Self {
        Message::of(Role::User, Content::Text(text.into()))
    }

    /// An assistant message on `channel`: `analysis`, `commentary` or `final`.
    pub fn assistant(channel: &str, text: &str) -> Self {
        Message {
            channel: Some(channel.into()),
            ..Message::of(Role::Assistant, Content::Text(text.into()))
        }
    }

    /// A call of `recipient` (`functions.read_file`) with JSON arguments.
    pub fn call(recipient: &str, arguments: &str) -> Self {
        Message {
            recipient: Some(recipient.into()),
            channel: Some("commentary".into()),
            content_type: Some(format!("{}json", Special::Constrain.text())),
            ..Message::of(Role::Assistant, Content::Text(arguments.into()))
        }
    }

    /// The result of a call of `tool` (`functions.read_file`), on the call's channel.
    pub fn result(tool: &str, channel: &str, text: &str) -> Self {
        Message {
            name: Some(tool.into()),
            recipient: Some("assistant".into()),
            channel: Some(channel.into()),
            ..Message::of(Role::Tool, Content::Text(text.into()))
        }
    }
}

/// `messages`, then the start of the assistant's next message.
pub fn render_for_completion(messages: &[Message]) -> Prompt {
    let mut p = render(messages);
    p.assistant();
    p
}

pub fn render(messages: &[Message]) -> Prompt {
    let functions = messages.iter().any(|m| match &m.content {
        Content::Developer { namespaces, .. } => namespaces
            .iter()
            .any(|n| n.name == "functions" && !n.tools.is_empty()),
        _ => false,
    });
    let mut p = Prompt::default();
    for m in messages {
        p.message(m, functions);
    }
    p
}

fn system_text(s: &System, functions: bool) -> String {
    let effort = match s.effort {
        Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    };
    let mut sections = vec![
        [
            s.model_identity.clone(),
            format!("Knowledge cutoff: {}", s.knowledge_cutoff),
            format!("Current date: {}", s.date),
        ]
        .join("\n"),
        format!("Reasoning: {effort}"),
    ];
    if !s.namespaces.is_empty() {
        sections.push(tools_section(&s.namespaces));
    }
    let mut channels = "# Valid channels: analysis, commentary, final. Channel must be included for every message.".to_owned();
    if functions {
        channels.push_str("\nCalls to these tools must go to the commentary channel: 'functions'.");
    }
    sections.push(channels);
    sections.join("\n\n")
}

/// `# Tools` and each namespace, in the order of their names (the reference keeps them in a
/// BTreeMap).
fn tools_section(namespaces: &[Namespace]) -> String {
    let mut sorted: Vec<&Namespace> = namespaces.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut sections = vec!["# Tools".to_owned()];
    for ns in sorted {
        let mut lines = vec![format!("## {}\n", ns.name)];
        if let Some(desc) = &ns.description {
            for line in desc.lines() {
                lines.push(if ns.tools.is_empty() {
                    line.to_owned()
                } else {
                    format!("// {line}")
                });
            }
        }
        if !ns.tools.is_empty() {
            lines.push(format!("namespace {} {{\n", ns.name));
            for tool in &ns.tools {
                for line in tool.description.lines() {
                    lines.push(format!("// {line}"));
                }
                match &tool.parameters {
                    Some(params) => lines.push(format!(
                        "type {} = (_: {}) => any;\n",
                        tool.name,
                        typescript(params, "")
                    )),
                    None => lines.push(format!("type {} = () => any;\n", tool.name)),
                }
            }
            lines.push(format!("}} // namespace {}", ns.name));
        }
        sections.push(lines.join("\n"));
    }
    sections.join("\n\n")
}

fn is_enum(schema: &Value) -> bool {
    schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
}

fn nullable(schema: &Value) -> bool {
    schema
        .get("nullable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `t`, made nullable when the schema says so and it is not already.
fn with_null(t: String, schema: &Value) -> String {
    if nullable(schema) && !t.contains("null") {
        format!("{t} | null")
    } else {
        t
    }
}

/// A default as the reference writes it in a comment: a string in quotes unless it is an
/// enum's, where `bare` drops them; anything else as JSON.
fn default_note(default: &Value, schema: &Value, bare_enum: bool) -> String {
    match default.as_str() {
        Some(s) if !is_enum(schema) => format!("default: \"{s}\""),
        Some(s) if bare_enum => format!("default: {s}"),
        _ => format!("default: {default}"),
    }
}

/// A JSON Schema as Harmony's TypeScript type, the reference's `json_schema_to_typescript`.
pub fn typescript(schema: &Value, indent: &str) -> String {
    if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
        let mut out = String::new();
        for variant in variants {
            out.push_str(&format!("\n{indent} | "));
            out.push_str(&with_null(
                typescript(variant, &format!("{indent}   ")),
                variant,
            ));
            let mut trailing = Vec::new();
            if let Some(d) = variant.get("description").and_then(Value::as_str) {
                trailing.push(d.to_owned());
            }
            if let Some(default) = variant.get("default") {
                trailing.push(default_note(default, variant, false));
            }
            if !trailing.is_empty() {
                out.push_str(&format!(" // {}", trailing.join(" ")));
            }
        }
        return out;
    }
    if let Some(types) = schema.get("type").and_then(Value::as_array) {
        let names: Vec<&str> = types
            .iter()
            .filter_map(Value::as_str)
            .map(|t| if t == "integer" { "number" } else { t })
            .collect();
        if !names.is_empty() {
            return names.join(" | ");
        }
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => object(schema, indent),
        Some("string") => {
            let values: Vec<String> = schema
                .get("enum")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|s| format!("\"{s}\""))
                        .collect()
                })
                .unwrap_or_default();
            if values.is_empty() {
                "string".into()
            } else {
                values.join(" | ")
            }
        }
        Some("number" | "integer") => "number".into(),
        Some("boolean") => "boolean".into(),
        Some("array") => match schema.get("items") {
            Some(items) => format!("{}[]", typescript(items, indent)),
            None => "Array<any>".into(),
        },
        _ => "any".into(),
    }
}

fn object(schema: &Value, indent: &str) -> String {
    let mut out = String::new();
    if let Some(d) = schema.get("description").and_then(Value::as_str) {
        out.push_str(&format!("{indent}// {d}\n"));
    }
    out.push_str("{\n");
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        let required: HashSet<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for (key, val) in props {
            let mark = if required.contains(key.as_str()) {
                ""
            } else {
                "?"
            };
            if let Some(t) = val.get("title").and_then(Value::as_str) {
                out.push_str(&format!("{indent}// {t}\n{indent}//\n"));
            }
            if val.get("oneOf").is_none()
                && let Some(d) = val.get("description").and_then(Value::as_str)
            {
                out.push_str(&format!("{indent}// {d}\n"));
            }
            if let Some(examples) = val.get("examples").and_then(Value::as_array)
                && !examples.is_empty()
            {
                out.push_str(&format!("{indent}// Examples:\n"));
                for ex in examples.iter().filter_map(Value::as_str) {
                    out.push_str(&format!("{indent}// - \"{ex}\"\n"));
                }
            }
            if let Some(variants) = val.get("oneOf").and_then(Value::as_array) {
                property_one_of(&mut out, key, mark, val, variants, indent);
                continue;
            }
            out.push_str(&format!("{indent}{key}{mark}: "));
            out.push_str(&with_null(typescript(val, &format!("{indent}    ")), val));
            out.push(',');
            if let Some(default) = val.get("default") {
                out.push_str(&format!(" // {}", default_note(default, val, true)));
            }
            out.push('\n');
        }
    }
    out.push_str(&format!("{indent}}}"));
    out
}

/// A property whose schema is a `oneOf`: one line a variant, under the property's name.
fn property_one_of(
    out: &mut String,
    key: &str,
    mark: &str,
    val: &Value,
    variants: &[Value],
    indent: &str,
) {
    let property_desc = val.get("description").and_then(Value::as_str);
    // the description goes above unless the first variant says the same
    let first_says_it = property_desc.is_some()
        && variants
            .first()
            .and_then(|v| v.get("description"))
            .and_then(Value::as_str)
            == property_desc;
    let mut above = false;
    if !first_says_it && let Some(d) = property_desc {
        out.push_str(&format!("{indent}// {d}\n"));
        above = true;
    }
    if let Some(default) = val.get("default") {
        out.push_str(&format!(
            "{indent}// {}\n",
            default_note(default, val, true)
        ));
    }
    out.push_str(&format!("{indent}{key}{mark}:\n"));
    for (i, variant) in variants.iter().enumerate() {
        out.push_str(&format!("{indent} | "));
        out.push_str(&with_null(
            typescript(variant, &format!("{indent}   ")),
            variant,
        ));
        let mut trailing = Vec::new();
        if !(i == 0 && above)
            && let Some(d) = variant.get("description").and_then(Value::as_str)
            && Some(d) != property_desc
        {
            trailing.push(d.to_owned());
        }
        if let Some(default) = variant.get("default") {
            trailing.push(default_note(default, variant, true));
        }
        if !trailing.is_empty() {
            out.push_str(&format!(" // {}", trailing.join(" ")));
        }
        out.push('\n');
    }
    out.push_str(&format!("{indent},\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// JSON as Python's `json.dumps(v, ensure_ascii=False)` writes it, which made the golden's
    /// call arguments.
    fn python_json(v: &Value) -> String {
        match v {
            Value::Object(m) => format!(
                "{{{}}}",
                m.iter()
                    .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), python_json(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Array(a) => format!(
                "[{}]",
                a.iter().map(python_json).collect::<Vec<_>>().join(", ")
            ),
            other => other.to_string(),
        }
    }

    fn repo() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The scenario as harmony_ref.py turned it into Harmony: its system message becomes the
    /// developer's instructions, its tools the `functions` namespace, and the analysis
    /// before the most recent final is dropped (OpenAI's rule).
    fn seq() -> Vec<Message> {
        let text =
            std::fs::read_to_string(repo().join("evals/templates/scenarios/seq.json")).unwrap();
        let sc: Value = serde_json::from_str(&text).unwrap();
        let tools = sc["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| Tool {
                name: t["function"]["name"].as_str().unwrap().into(),
                description: t["function"]["description"].as_str().unwrap().into(),
                parameters: Some(t["function"]["parameters"].clone()),
            })
            .collect();
        let m = sc["messages"].as_array().unwrap();
        let mut out = vec![
            Message::system(System::new("2026-10-29", Effort::High)),
            Message::developer(
                Some(m[0]["content"].as_str().unwrap().into()),
                vec![Namespace {
                    name: "functions".into(),
                    description: None,
                    tools,
                }],
            ),
        ];
        let last_final = m
            .iter()
            .rposition(|x| x["role"] == "assistant" && x.get("tool_calls").is_none())
            .unwrap();
        let mut names = std::collections::HashMap::new();
        for (i, x) in m.iter().enumerate().skip(1) {
            match (x["role"].as_str().unwrap(), x.get("tool_calls")) {
                ("user", _) => out.push(Message::user(x["content"].as_str().unwrap())),
                ("assistant", Some(calls)) => {
                    if let Some(r) = x["reasoning_content"].as_str()
                        && i > last_final
                    {
                        out.push(Message::assistant("analysis", r));
                    }
                    let call = &calls[0];
                    let name = call["function"]["name"].as_str().unwrap();
                    names.insert(call["id"].as_str().unwrap().to_owned(), name.to_owned());
                    out.push(Message::call(
                        &format!("functions.{name}"),
                        &python_json(&call["function"]["arguments"]),
                    ));
                }
                ("assistant", None) => {
                    out.push(Message::assistant("final", x["content"].as_str().unwrap()))
                }
                ("tool", _) => out.push(Message::result(
                    &format!("functions.{}", names[x["tool_call_id"].as_str().unwrap()]),
                    "commentary",
                    x["content"].as_str().unwrap(),
                )),
                (role, _) => panic!("role {role}"),
            }
        }
        out
    }

    #[test]
    fn renders_the_scenario_as_the_reference_does() {
        let golden = repo().join("profiles/templates/gpt-oss-20b/golden");
        // the golden escapes < and > so no control token stands in the repository whole
        let want =
            std::fs::read_to_string(golden.join("seq.effort-high.harmony-reference-0.0.8.txt"))
                .unwrap()
                .replace("\\u003c", "<")
                .replace("\\u003e", ">");
        let prompt = render_for_completion(&seq());
        let got = prompt.display();
        if got != want {
            let at = got
                .chars()
                .zip(want.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let around = |s: &str| {
                s.chars()
                    .skip(at.saturating_sub(60))
                    .take(120)
                    .collect::<String>()
            };
            panic!(
                "differs at char {at}:\n got: {:?}\nwant: {:?}",
                around(&got),
                around(&want)
            );
        }
        // and the control tokens are the reference's, id for id, in order
        let ids: Vec<u32> = serde_json::from_str(
            &std::fs::read_to_string(
                golden.join("seq.effort-high.harmony-reference-0.0.8.tokens.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let specials: Vec<u32> = prompt
            .pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Special(s) => Some(s.id()),
                Piece::Text(_) | Piece::Tokens(_) => None,
            })
            .collect();
        let golden_specials: Vec<u32> = ids.into_iter().filter(|&id| id >= 199_998).collect();
        assert_eq!(specials, golden_specials);
    }

    #[test]
    fn the_grammar_wakes_at_a_call_header_and_nowhere_else() {
        let read = crate::generic::ToolDecl {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        };
        let c = arguments_constraint(&[read]);
        let [Trigger::Pattern(p)] = c.triggers.as_slice() else {
            panic!("{:?}", c.triggers)
        };
        // llama.cpp searches what the generation wrote so far, control tokens as their text
        let re = regex::Regex::new(p).unwrap();
        let text = |f: &str| {
            let mut out = f.to_owned();
            for s in Special::ALL {
                let name = s.text();
                out = out.replace(&format!("«{}»", &name[2..name.len() - 2]), &name);
            }
            out
        };
        let wakes = [
            // the first message, the recipient after the channel or after the role
            "«channel»commentary to=functions",
            " to=functions",
            "«channel»analysis to=functions",
            // a later message, both orders
            "«channel»analysis«message»Need it.«end»«start»assistant«channel»commentary to=functions",
            "«channel»analysis«message»Need it.«end»«start»assistant to=functions",
            // after a junk header (llama.cpp #27720)
            "«channel»commentary 2? Ask.«end»«start»assistant«channel»commentary to=functions",
        ];
        for w in wakes {
            let t = text(w);
            let m = re.captures(&t).unwrap_or_else(|| panic!("{w}"));
            // the grammar starts at the recipient
            assert!(
                m.get(1).unwrap().as_str().trim_start() == "to=functions",
                "{w}"
            );
        }
        let sleeps = [
            "«channel»analysis«message»",
            // the text of a header inside a thought
            "«channel»analysis«message»Call it as to=functions.read «constrain»json«message»",
            // no function: the stray header, a namespace of the model's own
            "«channel»commentary to=assistant«channel»analysis«message»",
            "«channel»commentary to=container.exec «constrain»json«message»",
            "«channel»final«message»Use to=functions there.",
        ];
        for s in sleeps {
            assert!(!re.is_match(&text(s)), "{s}");
        }
        // from there the grammar takes the function's name and the rest of the header
        assert!(c.gbnf.starts_with("root ::= [ \\t\\n]* \"to=\" call\n"));
        assert!(
            c.gbnf
                .contains("call ::= \"functions.read_file\" header args-read-file\n")
        );
        // the model's own tools wake it under their namespaces
        let exec = crate::generic::ToolDecl {
            name: "container.exec".into(),
            description: "Run a command".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "array", "items": {"type": "string"}}}, "required": ["command"]}),
        };
        let c = arguments_constraint(&[exec]);
        let [Trigger::Pattern(p)] = c.triggers.as_slice() else {
            panic!("{:?}", c.triggers)
        };
        let re = regex::Regex::new(p).unwrap();
        assert!(re.is_match(&text(
            "«channel»analysis to=container.exec «constrain»json«message»"
        )));
        assert!(!re.is_match(&text("«channel»commentary to=functions.read«message»")));
        assert!(
            c.gbnf
                .contains("call ::= \"container.exec\" header args-container-exec\n")
        );
    }

    #[test]
    fn converts_schemas_as_the_reference_does() {
        // enums, arrays, nullable, titles, examples and defaults of every kind
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["fast", "slow"], "default": "fast"},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files"},
                "any": {"type": "array"},
                "limit": {"type": "integer", "nullable": true, "title": "Limit", "examples": ["10"]},
                "name": {"type": "string", "default": "x"}
            },
            "required": ["paths"]
        });
        assert_eq!(
            typescript(&schema, ""),
            "{\n\
             mode?: \"fast\" | \"slow\", // default: fast\n\
             // Files\n\
             paths: string[],\n\
             any?: Array<any>,\n\
             // Limit\n//\n// Examples:\n// - \"10\"\n\
             limit?: number | null,\n\
             name?: string, // default: \"x\"\n\
             }"
        );
        // a property of variants: one line each, the description above, defaults after
        let one_of = json!({
            "type": "object",
            "properties": {
                "target": {
                    "description": "Where to look",
                    "oneOf": [
                        {"type": "string", "description": "A path"},
                        {"type": "integer", "description": "A line", "default": 1}
                    ]
                }
            }
        });
        assert_eq!(
            typescript(&one_of, ""),
            "{\n// Where to look\ntarget?:\n | string\n | number // A line default: 1\n,\n}"
        );
        assert_eq!(
            typescript(&json!({"type": ["string", "integer", "null"]}), ""),
            "string | number | null"
        );
        assert_eq!(typescript(&json!({}), ""), "any");
    }
}
