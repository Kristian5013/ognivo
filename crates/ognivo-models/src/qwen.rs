//! Qwen3.8's format with XML tool calls (docs/architecture.md §5.5), rendered the way its chat
//! template renders it (profiles/templates/Qwen3.8-27B/canonical.jinja), for the Native path
//! (M3).
//!
//! The golden test holds the rendering to the reference's, HF transformers' Jinja2, of
//! `evals/templates/scenarios/seq.json` (profiles/templates/Qwen3.8-27B/golden). Every past
//! reasoning block stays, as the template has it by default (`preserve_thinking`).
//!
//! A prompt is a list of pieces: special tokens by id, and text. The reference splits the
//! rendered text at its added tokens and tokenizes what lies between two of them as one; so
//! texts next to each other make one piece here, the template's and the content's alike. The
//! template's own text holds special tokens too (`<tool_call>` in the format it describes),
//! and they are pieces of their own. Content is never read for special tokens (§5.3): the
//! driver cuts it at the guarded strings before it is tokenized.
//!
//! What the model writes back is read by [`parse`], token by token and tolerantly.

pub mod parse;

use serde_json::{Map, Value};

use crate::engine::{Constraint, TokenId, Trigger};
use crate::generic::ToolDecl;
use crate::grammar::{Rules, literal};

/// Ids from here on are Qwen3.8's special and added tokens, and unused ones; below are text.
pub(crate) const CONTROL: TokenId = 248_044;

/// The special tokens of the format, by their ids in Qwen3.8's vocabulary (tokenizer.ggml.tokens
/// of its GGUF). `<tool_call>` and the others in angle brackets are added tokens that llama.cpp
/// reads in text even without special parsing, so content is cut at them (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Special {
    EndOfText,
    ImStart,
    ImEnd,
    ToolCall,
    ToolCallEnd,
    ToolResponse,
    ToolResponseEnd,
    Think,
    ThinkEnd,
}

impl Special {
    pub const ALL: [Special; 9] = [
        Special::EndOfText,
        Special::ImStart,
        Special::ImEnd,
        Special::ToolCall,
        Special::ToolCallEnd,
        Special::ToolResponse,
        Special::ToolResponseEnd,
        Special::Think,
        Special::ThinkEnd,
    ];

    pub fn id(self) -> TokenId {
        match self {
            Special::EndOfText => 248_044,
            Special::ImStart => 248_045,
            Special::ImEnd => 248_046,
            Special::ToolCall => 248_058,
            Special::ToolCallEnd => 248_059,
            Special::ToolResponse => 248_066,
            Special::ToolResponseEnd => 248_067,
            Special::Think => 248_068,
            Special::ThinkEnd => 248_069,
        }
    }

    pub fn from_id(id: TokenId) -> Option<Special> {
        Special::ALL.into_iter().find(|s| s.id() == id)
    }

    /// The token's text, put together here so that none stands in the source in one piece
    /// (§5.3).
    pub fn text(self) -> String {
        match self {
            Special::EndOfText => format!("<|{}|>", "endoftext"),
            Special::ImStart => format!("<|{}|>", "im_start"),
            Special::ImEnd => format!("<|{}|>", "im_end"),
            Special::ToolCall => format!("<{}>", "tool_call"),
            Special::ToolCallEnd => format!("</{}>", "tool_call"),
            Special::ToolResponse => format!("<{}>", "tool_response"),
            Special::ToolResponseEnd => format!("</{}>", "tool_response"),
            Special::Think => format!("<{}>", "think"),
            Special::ThinkEnd => format!("</{}>", "think"),
        }
    }

    /// The tokens that end a generation: the template's end of a message, and the end of
    /// text.
    pub fn ends_generation(id: TokenId) -> bool {
        id == Special::ImEnd.id() || id == Special::EndOfText.id()
    }
}

/// How much the model reasons: the template's `reasoning_effort`, or no reasoning at all
/// (`enable_thinking` false).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    XHigh,
    Medium,
    Low,
    Off,
}

impl Level {
    /// The profile's name of a level.
    pub fn named(name: &str) -> Option<Level> {
        match name {
            "xhigh" => Some(Level::XHigh),
            "medium" => Some(Level::Medium),
            "low" => Some(Level::Low),
            "off" => Some(Level::Off),
            _ => None,
        }
    }

    /// What the template puts at the start of the system message for the level.
    fn instructions(self) -> Option<&'static str> {
        match self {
            Level::XHigh => Some(
                "Reasoning effort is set to xhigh. Please think carefully through the task, validate key assumptions, consider plausible alternatives, and prioritize correctness, consistency, and clarity in the final answer.",
            ),
            Level::Low => Some(
                "Reasoning effort is set to low. Keep your thinking brief and focused, moving directly to the conclusion without unnecessary elaboration.",
            ),
            Level::Medium | Level::Off => None,
        }
    }
}

/// A piece of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Special(Special),
    /// Text tokenized as one, never with special-token parsing.
    Text(String),
    /// Ids the model generated, sent back as they came (§5.4).
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

    /// Text goes on the text before it: the reference tokenizes them as one.
    pub(crate) fn text(&mut self, t: &str) {
        if t.is_empty() {
            return;
        }
        match self.pieces.last_mut() {
            Some(Piece::Text(before)) => before.push_str(t),
            _ => self.pieces.push(Piece::Text(t.to_owned())),
        }
    }

    pub(crate) fn tokens(&mut self, ids: Vec<TokenId>) {
        if !ids.is_empty() {
            self.pieces.push(Piece::Tokens(ids));
        }
    }

    /// The start of the assistant's next message, where generation begins: with reasoning
    /// off, an empty reasoning block, as the template writes it.
    pub(crate) fn assistant(&mut self, level: Level) {
        self.special(Special::ImStart);
        self.text("assistant\n");
        self.special(Special::Think);
        if level == Level::Off {
            self.text("\n\n");
            self.special(Special::ThinkEnd);
            self.text("\n\n");
        } else {
            self.text("\n");
        }
    }

    /// The prompt as text with its special tokens written out, as the golden files hold it;
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
}

/// A call in an assistant message of the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Call<'a> {
    pub name: &'a str,
    pub arguments: Map<String, Value>,
}

/// A message of the conversation after the system message, as the template renders it.
#[derive(Debug, Clone, PartialEq)]
pub enum Turn<'a> {
    User(&'a str),
    Assistant {
        reasoning: &'a str,
        content: &'a str,
        calls: Vec<Call<'a>>,
    },
    /// An assistant message whose generated ids go back as they came: they follow the start
    /// of the message the model was given ([`Prompt::assistant`]).
    Replay(&'a [TokenId]),
    Tool(&'a str),
}

/// The conversation as the template renders it, ending where the model's next message
/// begins.
pub fn render(tools: &[ToolDecl], system: &str, turns: &[Turn<'_>], level: Level) -> Prompt {
    let mut p = Prompt::default();
    let system = py_strip(system);
    let instructions = level.instructions();
    if !tools.is_empty() {
        p.special(Special::ImStart);
        p.text("system\n");
        if let Some(i) = instructions {
            p.text(i);
            p.text("\n\n");
        }
        p.text("# Tools\n\nYou have access to the following functions:\n\n<tools>");
        for t in tools {
            p.text("\n");
            p.text(&python_json(&tool_json(t)));
        }
        p.text("\n</tools>");
        format_instructions(&mut p);
        if !system.is_empty() {
            p.text("\n\n");
            p.text(system);
        }
        p.special(Special::ImEnd);
        p.text("\n");
    } else if !system.is_empty() || instructions.is_some() {
        p.special(Special::ImStart);
        p.text("system\n");
        match instructions {
            Some(i) if !system.is_empty() => {
                p.text(i);
                p.text("\n\n");
            }
            Some(i) => p.text(i),
            None => {}
        }
        p.text(system);
        p.special(Special::ImEnd);
        p.text("\n");
    }
    for (i, turn) in turns.iter().enumerate() {
        match turn {
            Turn::User(text) => {
                p.special(Special::ImStart);
                p.text("user\n");
                p.text(py_strip(text));
                p.special(Special::ImEnd);
                p.text("\n");
            }
            Turn::Assistant {
                reasoning,
                content,
                calls,
            } => {
                let content = py_strip(content);
                p.special(Special::ImStart);
                p.text("assistant\n");
                p.special(Special::Think);
                p.text("\n");
                p.text(py_strip(reasoning));
                p.text("\n");
                p.special(Special::ThinkEnd);
                p.text("\n\n");
                p.text(content);
                for (k, c) in calls.iter().enumerate() {
                    if k > 0 {
                        p.text("\n");
                    } else if !content.is_empty() {
                        p.text("\n\n");
                    }
                    p.special(Special::ToolCall);
                    p.text(&format!("\n<function={}>\n", c.name));
                    for (key, value) in &c.arguments {
                        p.text(&format!("<parameter={key}>\n"));
                        match value {
                            Value::String(s) => p.text(s),
                            other => p.text(&python_json(other)),
                        }
                        p.text("\n</parameter>\n");
                    }
                    p.text("</function>\n");
                    p.special(Special::ToolCallEnd);
                }
                p.special(Special::ImEnd);
                p.text("\n");
            }
            Turn::Replay(ids) => {
                p.assistant(level);
                p.tokens(sealed(ids));
                p.text("\n");
            }
            Turn::Tool(text) => {
                let first = !matches!(i.checked_sub(1).map(|j| &turns[j]), Some(Turn::Tool(_)));
                if first {
                    p.special(Special::ImStart);
                    p.text("user");
                }
                p.text("\n");
                p.special(Special::ToolResponse);
                p.text("\n");
                p.text(py_strip(text));
                p.text("\n");
                p.special(Special::ToolResponseEnd);
                if !matches!(turns.get(i + 1), Some(Turn::Tool(_))) {
                    p.special(Special::ImEnd);
                    p.text("\n");
                }
            }
        }
    }
    p.assistant(level);
    p
}

/// What the template tells the model about the format of a call, with its special tokens.
fn format_instructions(p: &mut Prompt) {
    p.text("\n\nIf you choose to call a function ONLY reply in the following format with NO suffix:\n\n");
    p.special(Special::ToolCall);
    p.text("\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n<parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\nmultiple lines\n</parameter>\n</function>\n");
    p.special(Special::ToolCallEnd);
    p.text("\n\n<IMPORTANT>\nReminder:\n- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within ");
    p.special(Special::ToolCall);
    p.special(Special::ToolCallEnd);
    p.text(" XML tags\n- Required parameters MUST be specified\n- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after\n- If there is no function call available, answer the question like normal with your current knowledge and do not tell the user about function calls\n</IMPORTANT>");
}

/// A generation's ids as they go back: they end with the end of a message, as the template
/// writes it, whichever token ended the generation, or none.
pub(crate) fn sealed(ids: &[TokenId]) -> Vec<TokenId> {
    let mut ids = ids.to_vec();
    if ids.last().is_some_and(|id| Special::ends_generation(*id)) {
        ids.pop();
    }
    ids.push(Special::ImEnd.id());
    ids
}

/// A tool as the template's `tools` holds it, in the OpenAI form.
/// `tools` as the system message declares them, the `<tools>` block: for a tool result that
/// declares tools the prompt does not (`tool_search`).
pub fn tools_text(tools: &[ToolDecl]) -> String {
    let lines: Vec<String> = tools.iter().map(|t| python_json(&tool_json(t))).collect();
    format!("<tools>\n{}\n</tools>", lines.join("\n"))
}

fn tool_json(t: &ToolDecl) -> Value {
    let mut function = Map::new();
    function.insert("name".into(), Value::String(t.name.clone()));
    function.insert("description".into(), Value::String(t.description.clone()));
    function.insert("parameters".into(), t.parameters.clone());
    let mut tool = Map::new();
    tool.insert("type".into(), Value::String("function".into()));
    tool.insert("function".into(), Value::Object(function));
    Value::Object(tool)
}

/// `json.dumps(v, ensure_ascii=False)`, as transformers' `tojson` writes it: `", "` between
/// items, `": "` after a key, keys in their order.
pub(crate) fn python_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let items: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", Value::String(k.clone()), python_json(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(python_json).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

/// Python's `str.strip()`, which Jinja's `trim` is: Rust's whitespace and the four
/// separators U+001C to U+001F.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// The closing of a parameter's value, which no string value holds.
pub(crate) const PARAMETER_END: &str = "\n</parameter>\n";

/// The grammar of calls to the tools in `tools` (§5.7), as llama.cpp builds it for this
/// format on the Generic path (common/parsers/qwen3-coder.cpp): from `<tool_call>` on, a
/// declared function, its required parameters in any order and then its optional ones, each
/// value a string up to the closing tag or JSON of its type, then more calls or none. It is
/// lazy, woken by the `<tool_call>` token: reasoning and content stay free.
pub fn calls_constraint(tools: &[ToolDecl]) -> Constraint {
    let mut rules = Rules::default();
    let string_value = until(&mut rules, "xml-string", PARAMETER_END);
    let mut functions = Vec::new();
    for (i, t) in tools.iter().enumerate() {
        let args = arguments(&mut rules, t, i, &string_value);
        let body = format!(
            "{} {args} {}",
            literal(&format!("<function={}>\n", t.name)),
            literal("</function>\n")
        );
        functions.push(rules.add(&format!("tool-{i}"), body));
    }
    let call = rules.add(
        "tool-call",
        format!(
            "{} \"\\n\" ( {} ) {} [ \\t\\n]*",
            literal(&Special::ToolCall.text()),
            functions.join(" | "),
            literal(&Special::ToolCallEnd.text())
        ),
    );
    let gbnf = rules.grammar(&format!("{call} ( {call} )*"));
    Constraint::new(
        gbnf,
        vec![Trigger::Token {
            id: Special::ToolCall.id(),
            text: Special::ToolCall.text(),
        }],
    )
}

/// The parameters of `tool`: the required ones in any order (up to three; more go in the
/// schema's order), then the optional ones, any of them, as often as the model writes them.
fn arguments(rules: &mut Rules, tool: &ToolDecl, i: usize, string_value: &str) -> String {
    let Some(props) = tool.parameters.get("properties").and_then(Value::as_object) else {
        let any = rules.add(
            &format!("tool-{i}-any"),
            format!("\"<parameter=\" [a-zA-Z0-9_.-]+ \">\\n\" {string_value}"),
        );
        return format!("{any}*");
    };
    let required: Vec<&str> = tool
        .parameters
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let (mut req, mut opt) = (Vec::new(), Vec::new());
    for (k, (key, schema)) in props.iter().enumerate() {
        let value = if takes_string(schema) {
            string_value.to_owned()
        } else {
            let json = rules.schema(schema, &format!("tool-{i}-arg-{k}-schema"));
            format!("{json} {}", literal(PARAMETER_END))
        };
        let arg = rules.add(
            &format!("tool-{i}-arg-{k}"),
            format!("{} {value}", literal(&format!("<parameter={key}>\n"))),
        );
        if required.contains(&key.as_str()) {
            req.push(arg);
        } else {
            opt.push(arg);
        }
    }
    let mut out = match req.len() {
        0 => String::new(),
        1..=3 => {
            let orders: Vec<String> = permutations(&req)
                .into_iter()
                .map(|p| p.join(" "))
                .collect();
            format!("( {} )", orders.join(" | "))
        }
        _ => req.join(" "),
    };
    if !opt.is_empty() {
        out.push_str(&format!(" ( {} )*", opt.join(" | ")));
    }
    out
}

/// Whether a parameter of this schema is written as a string: it may be one. A value that may
/// also be JSON of another type is the string rule in the grammar, as llama.cpp has it, and
/// the reader types it ([`parse`]).
pub(crate) fn takes_string(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(t)) => t == "string",
        Some(Value::Array(types)) => types.iter().any(|t| t == "string"),
        // an enum of strings, or no type: text
        _ => schema
            .get("enum")
            .and_then(Value::as_array)
            .is_none_or(|values| values.iter().all(Value::is_string)),
    }
}

fn permutations(items: &[String]) -> Vec<Vec<String>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, first.clone());
            out.push(p);
        }
    }
    out
}

/// The automaton that reads text up to the first `end` in it (Knuth–Morris–Pratt): a state
/// for each character of `end` matched so far; `step(k, c)` is the state after `c`, and
/// `end.len()` (in characters) means `end` is complete.
struct Until {
    end: Vec<char>,
    fail: Vec<usize>,
}

impl Until {
    fn new(end: &str) -> Self {
        let d: Vec<char> = end.chars().collect();
        // the longest proper prefix of d[..=k] that is also its suffix
        let mut fail = vec![0usize; d.len()];
        for k in 1..d.len() {
            let mut j = fail[k - 1];
            while j > 0 && d[k] != d[j] {
                j = fail[j - 1];
            }
            if d[k] == d[j] {
                j += 1;
            }
            fail[k] = j;
        }
        Until { end: d, fail }
    }

    fn step(&self, mut k: usize, c: char) -> usize {
        loop {
            if self.end[k] == c {
                return k + 1;
            }
            if k == 0 {
                return 0;
            }
            k = self.fail[k - 1];
        }
    }
}

/// A rule for any text that does not hold `end`, followed by `end`: a rule for each state of
/// the automaton ([`Until`]), the last character of `end` ending it.
fn until(rules: &mut Rules, name: &str, end: &str) -> String {
    let a = Until::new(end);
    let n = a.end.len();
    let mut chars = a.end.clone();
    chars.sort_unstable();
    chars.dedup();
    let names: Vec<String> = (0..n).map(|k| format!("{name}-{k}")).collect();
    for k in 0..n {
        let mut alternatives = Vec::new();
        for &c in &chars {
            let to = a.step(k, c);
            let c = literal(&c.to_string());
            alternatives.push(if to == n {
                c
            } else {
                format!("{c} {}", names[to])
            });
        }
        let others: String = chars.iter().map(|c| class_char(*c)).collect();
        alternatives.push(format!("[^{others}] {}", names[0]));
        let got = rules.add(&names[k], alternatives.join(" | "));
        debug_assert_eq!(got, names[k]);
    }
    names[0].clone()
}

/// A character as it stands inside a GBNF character class.
fn class_char(c: char) -> String {
    match c {
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        '\t' => "\\t".into(),
        '\\' | ']' | '[' | '^' | '-' => format!("\\{c}"),
        c => c.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn special_tokens_are_named_by_their_ids() {
        for s in Special::ALL {
            assert_eq!(Special::from_id(s.id()), Some(s));
            assert!(s.id() >= CONTROL);
        }
        assert_eq!(Special::ToolCall.text().len(), 11);
        assert!(Special::ends_generation(Special::ImEnd.id()));
        assert!(!Special::ends_generation(Special::ToolCallEnd.id()));
    }

    #[test]
    fn texts_next_to_each_other_are_one_piece() {
        let mut p = Prompt::default();
        p.text("a");
        p.text("");
        p.text("b");
        p.special(Special::ImEnd);
        p.text("c");
        assert_eq!(
            p.pieces,
            [
                Piece::Text("ab".into()),
                Piece::Special(Special::ImEnd),
                Piece::Text("c".into())
            ]
        );
    }

    #[test]
    fn jinja_trim_is_pythons_strip() {
        assert_eq!(py_strip("\u{1f} a b\n\t"), "a b");
        assert_eq!(
            python_json(&json!({"a": [1, true, null], "b": "ü\n"})),
            "{\"a\": [1, true, null], \"b\": \"ü\\n\"}"
        );
    }

    #[test]
    fn a_generation_goes_back_ending_with_the_end_of_a_message() {
        let end = Special::ImEnd.id();
        assert_eq!(sealed(&[1, 2]), [1, 2, end]);
        assert_eq!(sealed(&[1, end]), [1, end]);
        assert_eq!(sealed(&[1, Special::EndOfText.id()]), [1, end]);
    }

    #[test]
    fn the_history_renders_as_the_reference_renders_the_scenario() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sc: Value = serde_json::from_str(
            &std::fs::read_to_string(repo.join("evals/templates/scenarios/seq.json")).unwrap(),
        )
        .unwrap();
        let tools: Vec<ToolDecl> = sc["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| ToolDecl {
                name: t["function"]["name"].as_str().unwrap().into(),
                description: t["function"]["description"].as_str().unwrap().into(),
                parameters: t["function"]["parameters"].clone(),
            })
            .collect();
        let messages = sc["messages"].as_array().unwrap();
        let system = messages[0]["content"].as_str().unwrap();
        let turns: Vec<Turn> = messages[1..]
            .iter()
            .map(|m| match m["role"].as_str().unwrap() {
                "user" => Turn::User(m["content"].as_str().unwrap()),
                "tool" => Turn::Tool(m["content"].as_str().unwrap()),
                _ => Turn::Assistant {
                    reasoning: m["reasoning_content"].as_str().unwrap_or_default(),
                    content: m["content"].as_str().unwrap_or_default(),
                    calls: m["tool_calls"]
                        .as_array()
                        .map(|calls| {
                            calls
                                .iter()
                                .map(|c| Call {
                                    name: c["function"]["name"].as_str().unwrap(),
                                    arguments: c["function"]["arguments"]
                                        .as_object()
                                        .unwrap()
                                        .clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                },
            })
            .collect();
        let got = render(&tools, system, &turns, Level::Medium).display();
        let golden = std::fs::read_to_string(repo.join(
            "profiles/templates/Qwen3.8-27B/golden/seq.think-medium-preserve.canonical.py-hf.txt",
        ))
        .unwrap();
        // the generation prompt ends with the newline after the start of the reasoning
        assert_eq!(got, golden);
        // the template's own text holds the call tokens, as their ids
        let p = render(&tools, system, &turns, Level::Medium);
        let calls = p
            .pieces
            .iter()
            .take_while(|x| **x != Piece::Special(Special::ImEnd))
            .filter(|x| **x == Piece::Special(Special::ToolCall))
            .count();
        assert_eq!(calls, 2);
    }

    #[test]
    fn levels_put_their_instructions_first_and_off_closes_the_reasoning() {
        let p = render(&[], "Be brief.", &[Turn::User("hi")], Level::XHigh).display();
        assert!(
            p.contains("system\nReasoning effort is set to xhigh."),
            "{p}"
        );
        assert!(p.contains("final answer.\n\nBe brief."), "{p}");
        let off = render(&[], "", &[Turn::User("hi")], Level::Off).display();
        let end = format!(
            "assistant\n{}\n\n{}\n\n",
            Special::Think.text(),
            Special::ThinkEnd.text()
        );
        assert!(off.ends_with(&end), "{off}");
        assert!(!off.contains("system"), "{off}");
    }

    #[test]
    fn tool_results_in_a_row_share_one_user_message() {
        let p = render(
            &[],
            "",
            &[
                Turn::User("go"),
                Turn::Tool("one"),
                Turn::Tool("two"),
                Turn::User("next"),
            ],
            Level::Medium,
        )
        .display();
        let tr = Special::ToolResponse.text();
        let tre = Special::ToolResponseEnd.text();
        let (s, e) = (Special::ImStart.text(), Special::ImEnd.text());
        assert!(
            p.contains(&format!(
                "{s}user\n{tr}\none\n{tre}\n{tr}\ntwo\n{tre}{e}\n{s}user\nnext{e}"
            )),
            "{p}"
        );
    }

    #[test]
    fn a_replayed_message_follows_the_start_it_was_generated_from() {
        let p = render(
            &[],
            "",
            &[Turn::User("go"), Turn::Replay(&[7, 8])],
            Level::Medium,
        );
        let replay = p
            .pieces
            .iter()
            .position(|x| matches!(x, Piece::Tokens(_)))
            .unwrap();
        assert_eq!(
            p.pieces[replay],
            Piece::Tokens(vec![7, 8, Special::ImEnd.id()])
        );
        assert_eq!(p.pieces[replay - 1], Piece::Text("\n".into()));
        assert_eq!(p.pieces[replay - 2], Piece::Special(Special::Think));
        assert_eq!(p.pieces[replay + 1], Piece::Text("\n".into()));
    }

    #[test]
    fn the_grammar_names_each_tool_and_its_parameters() {
        let tools = [ToolDecl {
            name: "read".into(),
            description: "Read a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer"}
                },
                "required": ["file_path"]
            }),
        }];
        let c = calls_constraint(&tools);
        let g = &c.gbnf;
        assert!(g.contains("tool-0 ::= \"<function=read>\\n\" ( tool-0-arg-0 ) ( tool-0-arg-1 | tool-0-arg-2 )* \"</function>\\n\"\n"), "{g}");
        assert!(
            g.contains("tool-0-arg-0 ::= \"<parameter=file_path>\\n\" xml-string-0\n"),
            "{g}"
        );
        assert!(
            g.contains(
                "tool-0-arg-1 ::= \"<parameter=offset>\\n\" integer \"\\n</parameter>\\n\"\n"
            ),
            "{g}"
        );
        assert_eq!(
            c.triggers,
            vec![Trigger::Token {
                id: Special::ToolCall.id(),
                text: Special::ToolCall.text()
            }]
        );
    }

    #[test]
    fn a_string_value_ends_at_the_first_closing_tag() {
        let a = Until::new(PARAMETER_END);
        // where the automaton says the value and its closing end
        let ends = |text: &str| -> Option<usize> {
            let mut k = 0;
            for (i, c) in text.char_indices() {
                k = a.step(k, c);
                if k == a.end.len() {
                    return Some(i + c.len_utf8());
                }
            }
            None
        };
        let close = PARAMETER_END;
        assert_eq!(ends(&format!("a{close}rest")), Some(1 + close.len()));
        // a newline before the closing tag is the value's
        assert_eq!(ends(&format!("a\n{close}")), Some(2 + close.len()));
        assert_eq!(ends(&format!("\n</param\n{close}")), Some(9 + close.len()));
        assert_eq!(ends("a\n</parameter>x"), None);
        assert_eq!(ends(close), Some(close.len()));
        // and the rules say the same: the last state ends the rule on the newline
        let mut rules = Rules::default();
        assert_eq!(until(&mut rules, "s", PARAMETER_END), "s-0");
        let g = rules.grammar("s-0");
        let last = g.lines().find(|l| l.starts_with("s-13 ::= ")).unwrap();
        assert!(last.starts_with("s-13 ::= \"\\n\" | \"/\" s-0 |"), "{last}");
        assert!(last.ends_with("| [^\\n/<>aemprt] s-0"), "{last}");
        let first = g.lines().find(|l| l.starts_with("s-0 ::= ")).unwrap();
        assert!(
            first.starts_with("s-0 ::= \"\\n\" s-1 | \"/\" s-0 |"),
            "{first}"
        );
    }

    #[test]
    fn which_parameters_are_strings() {
        assert!(takes_string(&json!({"type": "string"})));
        assert!(takes_string(&json!({"type": ["integer", "string"]})));
        assert!(!takes_string(&json!({"type": "integer"})));
        assert!(!takes_string(&json!({"type": ["integer", "null"]})));
        assert!(takes_string(&json!({"enum": ["a", "b"]})));
        assert!(!takes_string(&json!({"enum": [1, 2]})));
        assert!(takes_string(&json!({})));
    }
}
