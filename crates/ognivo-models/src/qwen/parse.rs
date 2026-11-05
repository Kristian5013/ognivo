//! Qwen3.8's output read back token by token, tolerantly, for the Native path (M3).
//!
//! The generation starts where the template's generation prompt ends, inside the reasoning
//! (or, with reasoning off, after an empty one): the reasoning, `</think>`, the content, then
//! the calls, each `<tool_call>`, `<function=NAME>`, its `<parameter=KEY>` values, `</function>`
//! and `</tool_call>`, then the end of the message. Special tokens are read by id and everything
//! else as bytes, so text that only spells a special token stays text.
//!
//! The tolerance is llama.cpp's for this format on the Generic path (common/parsers/
//! qwen3-coder.cpp): the reasoning may end at `<tool_call>` without `</think>`, and so may it
//! here; a value is the text up to the first closing tag, a string when its schema allows one
//! and JSON when it does not. Beyond that, a call without its newlines or without
//! `</tool_call>` still reads, but is not as the template writes it, so its ids do not go back
//! as they came. Reading stops at the end of the message or of text, and where another message
//! begins.

use serde_json::{Map, Value};

use super::{CONTROL, Special, takes_string};
use crate::engine::TokenId;
use crate::generic::{ExecutableCalls, FinishReason, Outcome, ToolCall, ToolDecl};

/// What the reader is in, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Reasoning,
    Content,
    Call,
    /// Text after the calls, which the format has no place for: shown as reasoning, never kept.
    Junk,
}

/// What reading reports as it goes, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A part begins; a call's once its function's name is read.
    Open {
        part: Part,
        name: Option<String>,
    },
    /// More of the open part, in whole characters; of a call, the text after its name.
    Text(String),
    Close,
}

/// How reading ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// Still reading; after [`Reader::finish`], the stream stopped inside the message.
    Open,
    /// The end of the message or of text.
    Done,
    /// Another message began.
    RanOn,
}

/// One call as written: the bytes between `<tool_call>` and `</tool_call>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallText {
    pub body: Vec<u8>,
    /// `</tool_call>` came.
    pub closed: bool,
    /// Special tokens inside it.
    pub foreign: Vec<TokenId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Reasoning,
    Content,
    Call,
    /// After a call's `</tool_call>`.
    After,
    Done,
}

/// Reads one generation of the assistant.
#[derive(Debug)]
pub struct Reader {
    ids: Vec<TokenId>,
    reasoning: Vec<u8>,
    /// The generation began inside the reasoning, and whether that ended.
    began_reasoning: bool,
    reasoning_ended: bool,
    content: Vec<u8>,
    calls: Vec<CallText>,
    /// Text after a call that is not whitespace.
    junk: Vec<u8>,
    /// Special tokens in the reasoning, the content or between the calls.
    foreign: Vec<TokenId>,
    state: State,
    end: End,
    /// Bytes of a character the part being read has not finished.
    utf8: Vec<u8>,
    /// The part shown as open, and the content's whitespace not shown yet.
    shown: Option<Part>,
    held: String,
    /// The text of the call being read while its function's name is not read yet.
    call_head: Option<String>,
}

impl Reader {
    /// A reader for a generation that begins inside the reasoning, or after it when
    /// `reasoning` is false (reasoning off).
    pub fn new(reasoning: bool) -> Self {
        Reader {
            ids: Vec::new(),
            reasoning: Vec::new(),
            began_reasoning: reasoning,
            reasoning_ended: !reasoning,
            content: Vec::new(),
            calls: Vec::new(),
            junk: Vec::new(),
            foreign: Vec::new(),
            state: if reasoning {
                State::Reasoning
            } else {
                State::Content
            },
            end: End::Open,
            utf8: Vec::new(),
            shown: None,
            held: String::new(),
            call_head: None,
        }
    }

    /// Reads one token: its id, and its bytes unless it is a special token.
    pub fn push(&mut self, id: TokenId, piece: &[u8]) -> Vec<Event> {
        let mut ev = Vec::new();
        if self.state == State::Done {
            return ev;
        }
        self.ids.push(id);
        let special = Special::from_id(id);
        if Special::ends_generation(id) {
            self.close(&mut ev);
            self.stop(End::Done);
            return ev;
        }
        if special == Some(Special::ImStart) {
            // the token that began the next message is not the generation's
            self.ids.pop();
            self.close(&mut ev);
            self.stop(End::RanOn);
            return ev;
        }
        match (self.state, special) {
            (State::Reasoning, Some(Special::ThinkEnd)) => {
                self.close(&mut ev);
                self.reasoning_ended = true;
                self.state = State::Content;
            }
            // the reasoning may end at a call, as llama.cpp reads it
            (
                State::Reasoning | State::Content | State::Call | State::After,
                Some(Special::ToolCall),
            ) => {
                self.close(&mut ev);
                self.reasoning_ended = true;
                self.calls.push(CallText::default());
                self.call_head = Some(String::new());
                self.state = State::Call;
            }
            (State::Call, Some(Special::ToolCallEnd)) => {
                self.close(&mut ev);
                if let Some(c) = self.calls.last_mut() {
                    c.closed = true;
                }
                self.state = State::After;
            }
            (State::Call, _) if id >= CONTROL => {
                if let Some(c) = self.calls.last_mut() {
                    c.foreign.push(id);
                }
            }
            (_, _) if id >= CONTROL => self.foreign.push(id),
            (State::Reasoning, None) => {
                self.reasoning.extend_from_slice(piece);
                let text = self.chars(piece);
                self.show(Part::Reasoning, None, text, &mut ev);
            }
            (State::Content, None) => {
                self.content.extend_from_slice(piece);
                let text = self.chars(piece);
                // the whitespace after the reasoning shows only before more content
                let text = format!("{}{text}", std::mem::take(&mut self.held));
                if self.shown.is_none() && text.trim().is_empty() {
                    self.held = text;
                } else {
                    let text = if self.shown.is_none() {
                        text.trim_start().to_owned()
                    } else {
                        text
                    };
                    self.show(Part::Content, None, text, &mut ev);
                }
            }
            (State::Call, None) => {
                if let Some(c) = self.calls.last_mut() {
                    c.body.extend_from_slice(piece);
                }
                let text = self.chars(piece);
                self.show_call(text, &mut ev);
            }
            (State::After, None) => {
                let text = self.chars(piece);
                if !self.junk.is_empty() || !text.trim().is_empty() {
                    self.junk.extend_from_slice(piece);
                    self.show(Part::Junk, None, text, &mut ev);
                }
            }
            (State::Done, _) | (_, Some(_)) => {}
        }
        ev
    }

    /// The server ended the generation at an end of generation the stream did not bring: the
    /// message is complete.
    pub fn end_of_generation(&mut self) {
        if self.state != State::Done {
            self.end = End::Done;
        }
    }

    /// Whether the format ended the generation: nothing more is read.
    pub fn stopped(&self) -> bool {
        self.state == State::Done
    }

    /// Ends reading where the stream ended.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut ev = Vec::new();
        if self.state != State::Done {
            self.close(&mut ev);
            self.state = State::Done;
        }
        ev
    }

    /// The ids read: the generation up to where it stopped.
    pub fn ids(&self) -> &[TokenId] {
        &self.ids
    }

    /// Whether the reasoning is being read.
    pub fn in_reasoning(&self) -> bool {
        self.state == State::Reasoning
    }

    /// Whether the generation ended inside its reasoning: neither a call nor an answer.
    pub fn bare(&self) -> bool {
        !self.reasoning_ended && self.calls.is_empty()
    }

    /// Whether reading stopped at a token that ends a generation, which the server stops at
    /// itself.
    pub fn at_end_of_generation(&self) -> bool {
        self.ids
            .last()
            .is_some_and(|id| Special::ends_generation(*id))
    }

    /// The texts as the agent keeps them, mended from `released` where a character lost a byte;
    /// whether each mended text lies whole in `released`.
    pub fn mendable(&self, released: &str) -> bool {
        self.texts().iter().all(|t| match std::str::from_utf8(t) {
            Ok(_) => true,
            Err(_) => released.contains(&*mended_text(t, Some(released))),
        })
    }

    /// The step as the agent keeps it: the reasoning and the content trimmed as the template
    /// trims them, and the calls, their values typed by the tools' schemas. `limit` when the
    /// server cut the generation; `released`, the server's text, mends what lost bytes.
    pub(crate) fn kept(
        &self,
        limit: bool,
        call_id: &str,
        tools: &[ToolDecl],
        released: Option<&str>,
    ) -> crate::native::Kept {
        let valid = self.texts().iter().all(|t| std::str::from_utf8(t).is_ok());
        let reasoning = mended_text(&self.reasoning, released);
        let content = mended_text(&self.content, released);
        let mut clean = valid
            && self.end == End::Done
            && self.foreign.is_empty()
            && self.junk.is_empty()
            && (!self.began_reasoning || self.reasoning_ended);
        let mut calls = Vec::new();
        let mut problem = None;
        for (k, c) in self.calls.iter().enumerate() {
            let id = if k == 0 {
                call_id.to_owned()
            } else {
                format!("{call_id}_{k}")
            };
            let body = mended_text(&c.body, released);
            clean &= c.closed && c.foreign.is_empty();
            match call(&body, tools) {
                Ok((name, arguments, exact)) => {
                    clean &= exact;
                    if !c.foreign.is_empty() && problem.is_none() {
                        problem = Some(format!("{name}: a special token stands in the call"));
                    }
                    calls.push(ToolCall {
                        id,
                        name,
                        arguments: Value::Object(arguments).to_string(),
                    });
                }
                Err((name, why)) => {
                    clean = false;
                    problem.get_or_insert(why);
                    calls.push(ToolCall {
                        id,
                        name: name.unwrap_or_default(),
                        arguments: body.into_owned(),
                    });
                }
            }
        }
        let cut = self.end == End::Open;
        let (outcome, finish) = if !calls.is_empty() {
            if cut && !self.calls.last().is_some_and(|c| c.closed) {
                (
                    Outcome::Truncated { partial: calls },
                    Some(FinishReason::Length),
                )
            } else if let Some(reason) = problem {
                (Outcome::Malformed { calls, reason }, None)
            } else {
                (
                    Outcome::Calls(ExecutableCalls::checked(calls)),
                    Some(FinishReason::ToolCalls),
                )
            }
        } else if cut && limit {
            (
                Outcome::Truncated {
                    partial: Vec::new(),
                },
                Some(FinishReason::Length),
            )
        } else {
            (Outcome::Answer, Some(FinishReason::Stop))
        };
        crate::native::Kept {
            content: super::py_strip(&content).to_owned(),
            reasoning: super::py_strip(&reasoning).to_owned(),
            outcome,
            finish,
            clean,
        }
    }

    fn texts(&self) -> Vec<&[u8]> {
        let mut t: Vec<&[u8]> = vec![&self.reasoning, &self.content];
        t.extend(self.calls.iter().map(|c| c.body.as_slice()));
        t
    }

    fn stop(&mut self, end: End) {
        self.end = end;
        self.state = State::Done;
    }

    /// The whole characters `piece` completes, with the bytes held before it.
    fn chars(&mut self, piece: &[u8]) -> String {
        self.utf8.extend_from_slice(piece);
        crate::harmony::parse::whole_chars(&mut self.utf8)
    }

    fn show(&mut self, part: Part, name: Option<String>, text: String, ev: &mut Vec<Event>) {
        if self.shown != Some(part) {
            self.close(ev);
            ev.push(Event::Open { part, name });
            self.shown = Some(part);
        }
        if !text.is_empty() {
            ev.push(Event::Text(text));
        }
    }

    /// A call shows once its function's name is read; then its text as it comes.
    fn show_call(&mut self, text: String, ev: &mut Vec<Event>) {
        let Some(head) = self.call_head.as_mut() else {
            if !text.is_empty() {
                ev.push(Event::Text(text));
            }
            return;
        };
        head.push_str(&text);
        const OPENER: &str = "<function=";
        let start = head.trim_start();
        let (name, rest) = match start.strip_prefix(OPENER) {
            Some(r) => match r.find('>') {
                Some(i) => (Some(r[..i].trim().to_owned()), r[i + 1..].to_owned()),
                None => return,
            },
            // it may still become the opener
            None if start.len() < OPENER.len() && OPENER.starts_with(start) => return,
            // not the format: shown as it comes, with no name
            None => (None, start.to_owned()),
        };
        self.call_head = None;
        self.show(Part::Call, name, rest, ev);
    }

    /// Closes the part shown as open; a character it never finished shows as U+FFFD.
    fn close(&mut self, ev: &mut Vec<Event>) {
        if !self.utf8.is_empty() {
            self.utf8.clear();
            if self.shown.is_some() {
                ev.push(Event::Text('\u{FFFD}'.into()));
            }
        }
        self.held.clear();
        if self.shown.take().is_some() {
            ev.push(Event::Close);
        }
    }
}

/// `bytes` as text, mended from `released` where it is not UTF-8.
fn mended_text<'a>(bytes: &'a [u8], released: Option<&str>) -> std::borrow::Cow<'a, str> {
    let text = String::from_utf8_lossy(bytes);
    match (released, std::str::from_utf8(bytes)) {
        (Some(r), Err(_)) => crate::native::mended(&text, r).into(),
        _ => text,
    }
}

/// A call read: its function, its arguments, and whether it was written exactly as the
/// template writes a call.
type Read = (String, Map<String, Value>, bool);
/// Why a call cannot run, with its function's name if that was read.
type Unread = (Option<String>, String);
/// A call's body as written: its function, its parameters' raw values, and whether it is
/// exactly as the template writes a call.
type Written = (String, Vec<(String, String)>, bool);

/// A call's body read: its function, a declared tool's, and its arguments, typed by the
/// tool's schema; whether it was written exactly as the template writes a call. The function's
/// name, if it was read, and what is wrong when the call cannot run.
fn call(body: &str, tools: &[ToolDecl]) -> Result<Read, Unread> {
    let (name, params, mut exact) = written(body).map_err(|why| (None, why))?;
    let Some(tool) = tools.iter().find(|t| t.name == name) else {
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        return Err((
            Some(name.clone()),
            format!(
                "{name}: there is no such tool; the tools are {}",
                names.join(", ")
            ),
        ));
    };
    let props = tool.parameters.get("properties").and_then(Value::as_object);
    let mut args = Map::new();
    for (key, raw) in params {
        let schema = props.and_then(|p| p.get(&key));
        let value = typed(&raw, schema).map_err(|why| {
            (
                Some(name.clone()),
                format!("{name}: parameter {key}: {why}"),
            )
        })?;
        exact &= !args.contains_key(&key);
        args.insert(key, value);
    }
    Ok((name, args, exact))
}

/// A value as its schema has it: a string when the schema allows only strings, JSON when it
/// allows none, and else JSON of one of its other types if it reads as that, or the string.
fn typed(raw: &str, schema: Option<&Value>) -> Result<Value, String> {
    let Some(schema) = schema else {
        return Ok(Value::String(raw.to_owned()));
    };
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let json = || serde_json::from_str::<Value>(raw.trim());
    if takes_string(schema) {
        let only_string = types == ["string"] || (types.is_empty() && schema.get("enum").is_some());
        if !only_string && let Ok(v) = json() {
            let fits = match &v {
                Value::Object(_) => types.is_empty() || types.contains(&"object"),
                Value::Array(_) => types.is_empty() || types.contains(&"array"),
                Value::Number(n) => {
                    types.is_empty()
                        || types.contains(&"number")
                        || (types.contains(&"integer") && (n.is_i64() || n.is_u64()))
                }
                Value::Bool(_) => types.is_empty() || types.contains(&"boolean"),
                Value::Null => types.is_empty() || types.contains(&"null"),
                Value::String(_) => false,
            };
            if fits {
                return Ok(v);
            }
        }
        return Ok(Value::String(raw.to_owned()));
    }
    json().map_err(|e| format!("{raw:?} is not JSON ({e})"))
}

/// The function and the raw values of a call's body, and whether it was written exactly as
/// the template writes a call; what is wrong when it does not read.
fn written(body: &str) -> Result<Written, String> {
    fn newline(rest: &mut &str, exact: &mut bool) {
        match rest.strip_prefix('\n') {
            Some(r) => *rest = r,
            None => *exact = false,
        }
    }
    fn skip_space(rest: &mut &str, exact: &mut bool) {
        let t = rest.trim_start();
        if t.len() != rest.len() {
            *exact = false;
        }
        *rest = t;
    }
    let mut exact = true;
    let mut rest = body;
    newline(&mut rest, &mut exact);
    skip_space(&mut rest, &mut exact);
    let Some(r) = rest.strip_prefix("<function=") else {
        return Err(format!(
            "the call does not begin with <function=NAME>: {:?}",
            start_of(rest)
        ));
    };
    let Some(close) = r.find('>') else {
        return Err("the function's name has no closing >".into());
    };
    let name = r[..close].trim().to_owned();
    if name.len() != close {
        exact = false;
    }
    if name.is_empty() {
        return Err("the call names no function".into());
    }
    rest = &r[close + 1..];
    newline(&mut rest, &mut exact);
    let mut params = Vec::new();
    loop {
        skip_space(&mut rest, &mut exact);
        if let Some(r) = rest.strip_prefix("</function>") {
            rest = r;
            newline(&mut rest, &mut exact);
            if !rest.trim().is_empty() {
                return Err(format!(
                    "{name}: text after </function>: {:?}",
                    start_of(rest)
                ));
            }
            if !rest.is_empty() {
                exact = false;
            }
            break;
        }
        if rest.is_empty() {
            // no </function>: the call ends with the body
            exact = false;
            break;
        }
        let Some(r) = rest.strip_prefix("<parameter=") else {
            return Err(format!(
                "{name}: <parameter=KEY> or </function> was expected, not {:?}",
                start_of(rest)
            ));
        };
        let Some(close) = r.find('>') else {
            return Err(format!("{name}: a parameter's name has no closing >"));
        };
        let key = r[..close].trim().to_owned();
        if key.len() != close {
            exact = false;
        }
        let mut v = &r[close + 1..];
        newline(&mut v, &mut exact);
        let tag = "</parameter>";
        let (value, after) = match v.find(&format!("\n{tag}")) {
            Some(i) => (&v[..i], &v[i + 1 + tag.len()..]),
            None => match v.find(tag) {
                Some(i) => {
                    exact = false;
                    (&v[..i], &v[i + tag.len()..])
                }
                None => return Err(format!("{name}: parameter {key} has no </parameter>")),
            },
        };
        rest = after;
        newline(&mut rest, &mut exact);
        params.push((key, value.to_owned()));
    }
    Ok((name, params, exact))
}

fn start_of(s: &str) -> String {
    s.chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<ToolDecl> {
        vec![
            ToolDecl {
                name: "read".into(),
                description: String::new(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {"type": "string"},
                        "offset": {"type": "integer"},
                        "flags": {"type": ["array", "string"]}
                    },
                    "required": ["file_path"]
                }),
            },
            ToolDecl {
                name: "bash".into(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
        ]
    }

    /// A generation in the fake vocabulary of these tests: a byte is token 1000 + its value,
    /// and «name» is the special token of that name (§5.3: none stands in the source whole).
    fn read(text: &str, reasoning: bool) -> Reader {
        let mut r = Reader::new(reasoning);
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('«') {
                let end = after.find('»').unwrap();
                let s = match &after[..end] {
                    "/think" => Special::ThinkEnd,
                    "call" => Special::ToolCall,
                    "/call" => Special::ToolCallEnd,
                    "end" => Special::ImEnd,
                    "eot" => Special::EndOfText,
                    "start" => Special::ImStart,
                    "think" => Special::Think,
                    other => panic!("{other}"),
                };
                r.push(s.id(), b"");
                rest = &after[end + '»'.len_utf8()..];
            } else {
                let c = rest.chars().next().unwrap();
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    r.push(1000 + u32::from(b), &[b]);
                }
                rest = &rest[c.len_utf8()..];
            }
        }
        r
    }

    fn calls(k: &crate::native::Kept) -> Vec<(String, Value)> {
        match &k.outcome {
            Outcome::Calls(c) => c
                .calls()
                .iter()
                .map(|c| (c.name.clone(), serde_json::from_str(&c.arguments).unwrap()))
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reasoning_then_a_call_as_the_template_writes_it() {
        let r = read(
            "Read it first.\n«/think»\n\n«call»\n<function=read>\n<parameter=file_path>\nsrc/lib.rs\n</parameter>\n<parameter=offset>\n10\n</parameter>\n</function>\n«/call»«end»",
            true,
        );
        assert!(r.stopped() && r.at_end_of_generation());
        let k = r.kept(false, "c1", &tools(), None);
        assert_eq!(k.reasoning, "Read it first.");
        assert_eq!(k.content, "");
        assert!(k.clean);
        assert_eq!(
            calls(&k),
            [(
                "read".into(),
                json!({"file_path": "src/lib.rs", "offset": 10})
            )]
        );
    }

    #[test]
    fn several_calls_and_content_before_them() {
        let r = read(
            "x«/think»\n\nLet me look.\n\n«call»\n<function=bash>\n<parameter=command>\nls\n</parameter>\n</function>\n«/call»\n«call»\n<function=read>\n<parameter=file_path>\na b\n\n</parameter>\n</function>\n«/call»«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        assert_eq!(k.content, "Let me look.");
        assert!(k.clean);
        let got = calls(&k);
        assert_eq!(got[0], ("bash".into(), json!({"command": "ls"})));
        // a newline at the end of a value is the value's
        assert_eq!(got[1], ("read".into(), json!({"file_path": "a b\n"})));
        let Outcome::Calls(c) = &k.outcome else {
            panic!()
        };
        assert_eq!(c.calls()[1].id, "c_1");
    }

    #[test]
    fn an_answer_and_the_whitespace_around_it() {
        let r = read("Simple.\n«/think»\n\nIt is 391.«end»", true);
        let k = r.kept(false, "c", &tools(), None);
        assert!(matches!(k.outcome, Outcome::Answer) && k.clean);
        assert_eq!(
            (k.reasoning.as_str(), k.content.as_str()),
            ("Simple.", "It is 391.")
        );
        // without reasoning the generation begins with the content
        let r = read("Hi.«eot»", false);
        let k = r.kept(false, "c", &tools(), None);
        assert_eq!(k.content, "Hi.");
        assert!(k.clean);
    }

    #[test]
    fn values_take_their_schemas_types() {
        let r = read(
            "«/think»«call»\n<function=read>\n<parameter=file_path>\n42\n</parameter>\n<parameter=flags>\n[\"a\"]\n</parameter>\n</function>\n«/call»«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        // a string stays one even when it reads as JSON; a string-or-array is the array
        assert_eq!(calls(&k)[0].1, json!({"file_path": "42", "flags": ["a"]}));
        let r = read(
            "«/think»«call»\n<function=read>\n<parameter=file_path>\nx\n</parameter>\n<parameter=offset>\nten\n</parameter>\n</function>\n«/call»«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        let Outcome::Malformed { reason, .. } = &k.outcome else {
            panic!("{:?}", k.outcome)
        };
        assert!(
            reason.starts_with("read: parameter offset: \"ten\" is not JSON"),
            "{reason}"
        );
        assert!(!k.clean);
    }

    #[test]
    fn what_the_reader_tolerates_does_not_go_back_as_it_came() {
        // no newlines, no </tool_call>, the reasoning ended by the call
        let r = read(
            "think«call»<function=bash><parameter=command>ls</parameter></function>«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        assert_eq!(calls(&k), [("bash".into(), json!({"command": "ls"}))]);
        assert!(!k.clean);
        // an unknown tool cannot run
        let r = read(
            "«/think»«call»\n<function=search>\n</function>\n«/call»«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        let Outcome::Malformed { reason, calls } = &k.outcome else {
            panic!("{:?}", k.outcome)
        };
        assert_eq!(
            reason,
            "search: there is no such tool; the tools are read, bash"
        );
        assert_eq!(calls[0].name, "search");
        // text after the calls is junk: shown, never kept
        let r = read(
            "«/think»«call»\n<function=bash>\n<parameter=command>\nls\n</parameter>\n</function>\n«/call» and more«end»",
            true,
        );
        let k = r.kept(false, "c", &tools(), None);
        assert!(matches!(k.outcome, Outcome::Calls(_)) && !k.clean);
    }

    #[test]
    fn a_cut_generation_is_truncated() {
        let mut r = read(
            "«/think»«call»\n<function=bash>\n<parameter=command>\nls -",
            true,
        );
        r.finish();
        let k = r.kept(true, "c", &tools(), None);
        assert!(
            matches!(k.outcome, Outcome::Truncated { .. }),
            "{:?}",
            k.outcome
        );
        let mut r = read("still thinking", true);
        r.finish();
        assert!(r.bare());
        let k = r.kept(true, "c", &tools(), None);
        assert!(matches!(k.outcome, Outcome::Truncated { .. }));
    }

    #[test]
    fn another_message_stops_reading_before_its_start() {
        let r = read("x«/think»Done.«start»user", true);
        assert!(r.stopped() && !r.at_end_of_generation());
        assert_eq!(r.ids().last(), Some(&(1000 + u32::from(b'.'))));
        let k = r.kept(false, "c", &tools(), None);
        assert_eq!(k.content, "Done.");
        assert!(!k.clean);
    }

    #[test]
    fn display_shows_parts_and_a_call_by_its_name() {
        let mut r = Reader::new(true);
        let mut ev = Vec::new();
        let text = |r: &mut Reader, s: &str, ev: &mut Vec<Event>| {
            for b in s.bytes() {
                ev.extend(r.push(1000 + u32::from(b), &[b]));
            }
        };
        text(&mut r, "hm", &mut ev);
        ev.extend(r.push(Special::ThinkEnd.id(), b""));
        text(&mut r, "\n\n", &mut ev);
        ev.extend(r.push(Special::ToolCall.id(), b""));
        text(
            &mut r,
            "\n<function=bash>\n<parameter=command>\nls",
            &mut ev,
        );
        ev.extend(r.push(Special::ToolCallEnd.id(), b""));
        assert_eq!(
            ev,
            [
                Event::Open {
                    part: Part::Reasoning,
                    name: None
                },
                Event::Text("h".into()),
                Event::Text("m".into()),
                Event::Close,
                Event::Open {
                    part: Part::Call,
                    name: Some("bash".into())
                },
                Event::Text("\n".into()),
            ]
            .into_iter()
            .chain(
                "<parameter=command>\nls"
                    .chars()
                    .map(|c| Event::Text(c.to_string()))
            )
            .chain([Event::Close])
            .collect::<Vec<_>>()
        );
        // a special token in the content keeps the ids from going back
        let mut r = read("«/think»a", true);
        r.push(Special::Think.id(), b"");
        r.push(Special::ImEnd.id(), b"");
        assert!(!r.kept(false, "c", &tools(), None).clean);
    }
}
