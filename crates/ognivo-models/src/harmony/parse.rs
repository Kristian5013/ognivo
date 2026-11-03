//! gpt-oss's output read back token by token, tolerantly (docs/research/2026-10-29/gpt-oss.md
//! §E), for the Native path (M2).
//!
//! The generation starts after the start token and `assistant`, and runs over message
//! boundaries: llama.cpp takes the end token off gpt-oss's end-of-generation tokens, so only
//! call and return stop it. Control tokens are read by id and everything else as bytes, so
//! text that only spells a control token stays text. They go by their names here
//! ([`Special`]), so that none stands in the source whole (docs/architecture.md §5.3).
//!
//! The tolerance comes from real output that llama.cpp's own parser rejects, failing the whole
//! turn:
//! - the recipient after the role or after the channel, and the content type after the
//!   constrain token, glued to the recipient, or plain (` json`);
//! - junk headers: free text or `??` after the channel token, a doubled channel token, a
//!   header that ends at the end token and never reaches the message token (llama.cpp
//!   #27720: nine shapes from gpt-oss-20b at temperature 1). Such a message is
//!   [`Kind::Junk`]: shown as reasoning, never run, and the message after it reads as usual;
//! - a header without its start token;
//! - a message that runs into the next header without its end, as the content of a write did
//!   in M1 stamp A.
//!
//! Reading stops at call and return, and wherever a call or the final answer ends otherwise:
//! gpt-oss makes one call a generation, and what follows an answer is not wanted. Nothing
//! after the stop is read.

use std::borrow::Cow;
use std::ops::Range;

use serde_json::{Map, Value};

use super::Special;
use crate::engine::TokenId;

/// Ids from here on are control tokens of o200k_harmony: Harmony's seven, startoftext,
/// endoftext, endofprompt and the reserved ones.
pub(crate) const CONTROL: TokenId = 199_998;

const CHANNELS: [&str; 3] = ["analysis", "commentary", "final"];

/// Recipients that are no tool: `to=assistant` turns up in junk headers, `all` is everyone.
const NOT_TOOLS: [&str; 5] = ["all", "assistant", "user", "system", "developer"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// [`Special::End`].
    End,
    /// [`Special::Call`].
    Call,
    /// [`Special::Return`].
    Return,
    /// The next header began before the message ended.
    RanOn,
    /// The generation stopped inside the message.
    Cut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `analysis` without a recipient: the chain of thought.
    Analysis,
    /// `commentary` without a recipient: a preamble for the user.
    Preamble,
    /// `final`: the answer.
    Final,
    /// A message to a tool, on whatever channel: its content is the arguments.
    Call,
    /// A header that makes no sense, without a recipient: shown as reasoning, never run.
    Junk,
}

/// A header as the model wrote it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    /// When the header names one: the first message's role came with the prompt.
    pub role: Option<String>,
    pub channel: Option<String>,
    /// As written (`functions.read_file`); `to=assistant` and the like are no recipient.
    pub recipient: Option<String>,
    /// `json` or `code`, after the constrain token or plain.
    pub content_type: Option<String>,
    /// Whether the content type came after the constrain token.
    pub constrained: bool,
    /// What did not fit: an unknown channel, a doubled channel token, stray words.
    pub junk: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub header: Header,
    pub kind: Kind,
    /// The content's bytes; a junk header's own text when it never reached its message token.
    pub content: Vec<u8>,
    pub end: End,
    /// The message's ids in the generation, from its header to its end token.
    pub tokens: Range<usize>,
    /// Control tokens where none may stand: in the content, or among the header's words.
    pub foreign: Vec<TokenId>,
}

impl Message {
    /// The content, invalid UTF-8 replaced.
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.content)
    }

    pub fn valid_utf8(&self) -> bool {
        std::str::from_utf8(&self.content).is_ok()
    }
}

/// What reading reports as it goes, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A message's header is complete, and its content follows.
    Open {
        index: usize,
        kind: Kind,
        recipient: Option<String>,
    },
    /// More of the open message's content, in whole characters.
    Text {
        index: usize,
        text: String,
    },
    Close {
        index: usize,
        end: End,
    },
}

/// Reads one generation that follows the start token and `assistant`.
#[derive(Debug)]
pub struct Reader {
    ids: Vec<TokenId>,
    messages: Vec<Message>,
    state: State,
    /// The format ended the generation.
    stopped: bool,
}

#[derive(Debug)]
enum State {
    Header(Pending),
    /// The content of the last message, and the bytes of a character it has not finished.
    Content(Vec<u8>),
    /// After the end of a message that does not end the generation.
    Between,
    Done,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Part {
    #[default]
    Role,
    Channel,
    Constrain,
}

/// A header being read: the role part, a part for each channel token, the constrain part.
#[derive(Debug, Default)]
struct Pending {
    start: usize,
    role: Vec<u8>,
    channels: Vec<Vec<u8>>,
    constrain: Option<Vec<u8>>,
    constrains: usize,
    part: Part,
    foreign: Vec<TokenId>,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    pub fn new() -> Self {
        Reader {
            ids: Vec::new(),
            messages: Vec::new(),
            state: State::Header(Pending::at(0)),
            stopped: false,
        }
    }

    /// Reads one token: its id, and its bytes unless it is a control token.
    pub fn push(&mut self, id: TokenId, piece: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        let state = std::mem::replace(&mut self.state, State::Done);
        if !matches!(state, State::Done) {
            self.ids.push(id);
            self.state = self.read(state, id, piece, &mut events);
        }
        events
    }

    /// Whether the format ended the generation: nothing more is read, and the rest of the
    /// stream can go.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Ends reading where the stream ended: a message still open is [`End::Cut`].
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let at = self.ids.len();
        match std::mem::replace(&mut self.state, State::Done) {
            State::Content(utf8) => {
                self.close(utf8, at, End::Cut, &mut events);
            }
            State::Header(p) if !p.is_empty() => self.junk(p, at, End::Cut, &mut events),
            _ => {}
        }
        events
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The kind of the message whose content is being read, if one is.
    pub fn open(&self) -> Option<Kind> {
        match self.state {
            State::Content(_) => self.messages.last().map(|m| m.kind),
            _ => None,
        }
    }

    /// The ids read: the generation up to where it stopped.
    pub fn ids(&self) -> &[TokenId] {
        &self.ids
    }

    fn read(&mut self, state: State, id: TokenId, piece: &[u8], ev: &mut Vec<Event>) -> State {
        let at = self.ids.len() - 1;
        let special = Special::from_id(id);
        match state {
            State::Done => State::Done,
            State::Between => match special {
                Some(Special::Start) => State::Header(Pending::at(at)),
                // a stray end, or the stop with nothing open
                Some(Special::End) => State::Between,
                Some(Special::Call | Special::Return) => self.stop(),
                // a header without its start token
                _ => self.read(State::Header(Pending::at(at)), id, piece, ev),
            },
            State::Header(mut p) => match special {
                Some(Special::Channel) => {
                    p.channels.push(Vec::new());
                    p.part = Part::Channel;
                    State::Header(p)
                }
                Some(Special::Constrain) => {
                    p.constrain = Some(Vec::new());
                    p.constrains += 1;
                    p.part = Part::Constrain;
                    State::Header(p)
                }
                Some(Special::Message) => self.begin(p, ev),
                Some(Special::End) => {
                    self.junk(p, at + 1, End::End, ev);
                    State::Between
                }
                Some(Special::Start) => {
                    if !p.is_empty() {
                        self.junk(p, at, End::RanOn, ev);
                    }
                    State::Header(Pending::at(at))
                }
                Some(Special::Call) => {
                    self.junk(p, at + 1, End::Call, ev);
                    self.stop()
                }
                Some(Special::Return) => {
                    self.junk(p, at + 1, End::Return, ev);
                    self.stop()
                }
                None if id >= CONTROL => {
                    p.foreign.push(id);
                    State::Header(p)
                }
                None => {
                    p.current().extend_from_slice(piece);
                    State::Header(p)
                }
            },
            State::Content(mut utf8) => {
                let index = self.messages.len() - 1;
                match special {
                    None if id < CONTROL => {
                        self.messages[index].content.extend_from_slice(piece);
                        utf8.extend_from_slice(piece);
                        let text = whole_chars(&mut utf8);
                        if !text.is_empty() {
                            ev.push(Event::Text { index, text });
                        }
                        State::Content(utf8)
                    }
                    Some(Special::End) => {
                        if self.close(utf8, at + 1, End::End, ev) {
                            self.stop()
                        } else {
                            State::Between
                        }
                    }
                    Some(Special::Call) => {
                        self.close(utf8, at + 1, End::Call, ev);
                        self.stop()
                    }
                    Some(Special::Return) => {
                        self.close(utf8, at + 1, End::Return, ev);
                        self.stop()
                    }
                    Some(next @ (Special::Start | Special::Channel)) => {
                        if self.close(utf8, at, End::RanOn, ev) {
                            // the token that began the next header is not read
                            self.ids.pop();
                            return self.stop();
                        }
                        let mut p = Pending::at(at);
                        if next == Special::Channel {
                            p.channels.push(Vec::new());
                            p.part = Part::Channel;
                        }
                        State::Header(p)
                    }
                    // the message and constrain tokens have no place here, nor do the others
                    _ => {
                        self.messages[index].foreign.push(id);
                        State::Content(utf8)
                    }
                }
            }
        }
    }

    fn stop(&mut self) -> State {
        self.stopped = true;
        State::Done
    }

    fn begin(&mut self, p: Pending, ev: &mut Vec<Event>) -> State {
        let index = self.messages.len();
        let (header, kind) = p.read();
        ev.push(Event::Open {
            index,
            kind,
            recipient: header.recipient.clone(),
        });
        self.messages.push(Message {
            header,
            kind,
            content: Vec::new(),
            end: End::Cut,
            tokens: p.start..self.ids.len(),
            foreign: p.foreign,
        });
        State::Content(Vec::new())
    }

    /// Closes the open message; whether that ends the generation.
    fn close(&mut self, utf8: Vec<u8>, end_at: usize, end: End, ev: &mut Vec<Event>) -> bool {
        let index = self.messages.len() - 1;
        if !utf8.is_empty() {
            // a character the message never finished
            ev.push(Event::Text {
                index,
                text: '\u{FFFD}'.into(),
            });
        }
        let m = &mut self.messages[index];
        m.end = end;
        m.tokens.end = end_at;
        ev.push(Event::Close { index, end });
        matches!(m.kind, Kind::Call | Kind::Final)
    }

    /// A header that never reached its message token, as a message of its own text.
    fn junk(&mut self, p: Pending, end_at: usize, end: End, ev: &mut Vec<Event>) {
        let index = self.messages.len();
        let (mut header, _) = p.read();
        header.junk.push("the header has no message".into());
        let content = p.text();
        ev.push(Event::Open {
            index,
            kind: Kind::Junk,
            recipient: None,
        });
        if !content.is_empty() {
            ev.push(Event::Text {
                index,
                text: String::from_utf8_lossy(&content).into_owned(),
            });
        }
        ev.push(Event::Close { index, end });
        self.messages.push(Message {
            header,
            kind: Kind::Junk,
            content,
            end,
            tokens: p.start..end_at,
            foreign: p.foreign,
        });
    }
}

impl Pending {
    fn at(start: usize) -> Self {
        Pending {
            start,
            ..Pending::default()
        }
    }

    fn current(&mut self) -> &mut Vec<u8> {
        match (self.part, self.channels.last_mut(), &mut self.constrain) {
            (Part::Constrain, _, Some(c)) | (Part::Channel, Some(c), _) => c,
            _ => &mut self.role,
        }
    }

    /// Nothing but white space read.
    fn is_empty(&self) -> bool {
        self.channels.is_empty()
            && self.constrain.is_none()
            && self.foreign.is_empty()
            && self.role.iter().all(u8::is_ascii_whitespace)
    }

    /// The header's text, for a junk message.
    fn text(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let parts = std::iter::once(&self.role)
            .chain(&self.channels)
            .chain(&self.constrain);
        for part in parts.map(|p| p.trim_ascii()).filter(|p| !p.is_empty()) {
            if !out.is_empty() {
                out.push(b' ');
            }
            out.extend_from_slice(part);
        }
        out
    }

    fn read(&self) -> (Header, Kind) {
        let mut h = Header::default();
        let mut role = words(&self.role, &mut h).into_iter();
        h.role = role.next();
        if let Some(r) = h.role.as_deref().filter(|r| *r != "assistant") {
            h.junk.push(format!("role {r}"));
        }
        stray(role, &mut h);
        // the last channel counts: one before it is a stray header (llama.cpp #21286)
        if self.channels.len() > 1 {
            h.junk
                .push(format!("{} channel tokens", self.channels.len()));
        }
        for (i, part) in self.channels.iter().enumerate() {
            let mut ws = words(part, &mut h).into_iter();
            if i + 1 < self.channels.len() {
                stray(ws, &mut h);
                continue;
            }
            h.channel = ws.next();
            let mut rest = Vec::new();
            for w in ws {
                if matches!(w.as_str(), "json" | "code") && h.content_type.is_none() {
                    h.content_type = Some(w);
                } else {
                    rest.push(w);
                }
            }
            stray(rest.into_iter(), &mut h);
        }
        if let Some(c) = &self.constrain {
            if self.constrains > 1 {
                h.junk.push(format!("{} constrain tokens", self.constrains));
            }
            let mut ws = words(c, &mut h).into_iter();
            if let Some(t) = ws.next() {
                h.content_type = Some(t);
                h.constrained = true;
            }
            stray(ws, &mut h);
        }
        if let Some(c) = h.channel.as_deref().filter(|c| !CHANNELS.contains(c)) {
            h.junk.push(format!("channel {c}"));
        }
        let kind = match (&h.recipient, h.channel.as_deref()) {
            (Some(_), _) => Kind::Call,
            (None, Some("analysis")) => Kind::Analysis,
            (None, Some("commentary")) => Kind::Preamble,
            (None, Some("final")) => Kind::Final,
            (None, _) => Kind::Junk,
        };
        (h, kind)
    }
}

/// The words of a header part, with `to=X` taken out into the recipient.
fn words(part: &[u8], h: &mut Header) -> Vec<String> {
    let text = String::from_utf8_lossy(part);
    let mut out = Vec::new();
    for w in text.split_whitespace() {
        match w.strip_prefix("to=") {
            Some("") => h.junk.push("to= without a recipient".into()),
            Some("all") => {}
            Some(r) if NOT_TOOLS.contains(&r) => h.junk.push(w.to_owned()),
            Some(r) => match &h.recipient {
                Some(first) if first != r => h.junk.push(format!("a second recipient {r}")),
                _ => h.recipient = Some(r.to_owned()),
            },
            None => out.push(w.to_owned()),
        }
    }
    out
}

fn stray(words: impl Iterator<Item = String>, h: &mut Header) {
    let words: Vec<String> = words.collect();
    if !words.is_empty() {
        h.junk.push(words.join(" "));
    }
}

/// The whole characters at the start of `pending`, taken out of it. An incomplete character
/// at its end stays for the next piece; invalid bytes come out as U+FFFD.
fn whole_chars(pending: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut rest = pending.as_slice();
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                out.push_str(&String::from_utf8_lossy(valid));
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = &after[n..];
                    }
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    *pending = rest.to_vec();
    out
}

/// What reading a call's arguments repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
    /// The content was empty: no arguments.
    Empty,
    /// The JSON stood in a code fence.
    Fence,
    /// Text after the first JSON value was dropped.
    Trailing,
    /// The object stood alone in an array.
    Array,
    /// The object came as a JSON string.
    Quoted,
}

/// A call's arguments as gpt-oss writes them, as an object: the first JSON value of the
/// content, out of a code fence, a one-element array or a string.
pub fn arguments(content: &str) -> Result<(Map<String, Value>, Vec<Repair>), String> {
    let mut text = content.trim();
    if text.is_empty() {
        return Ok((Map::new(), vec![Repair::Empty]));
    }
    let mut repairs = Vec::new();
    if let Some(inner) = fenced(text) {
        repairs.push(Repair::Fence);
        text = inner;
    }
    let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let mut value = match values.next() {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(format!("the arguments are not JSON: {e}")),
        None => return Err("the arguments are empty".into()),
    };
    if !text[values.byte_offset()..].trim().is_empty() {
        repairs.push(Repair::Trailing);
    }
    loop {
        value = match value {
            Value::Object(m) => return Ok((m, repairs)),
            Value::Array(mut a) if a.len() == 1 && !repairs.contains(&Repair::Array) => {
                repairs.push(Repair::Array);
                a.remove(0)
            }
            Value::String(s) if !repairs.contains(&Repair::Quoted) => {
                match serde_json::from_str::<Value>(&s) {
                    Ok(v @ (Value::Object(_) | Value::Array(_))) => {
                        repairs.push(Repair::Quoted);
                        v
                    }
                    _ => return Err("the arguments are a string, not an object".into()),
                }
            }
            other => {
                let what = match other {
                    Value::Array(_) => "an array",
                    Value::String(_) => "a string",
                    Value::Number(_) => "a number",
                    Value::Bool(_) => "a boolean",
                    _ => "null",
                };
                return Err(format!("the arguments are {what}, not an object"));
            }
        }
    }
}

/// The inside of a code fence around all of `text`.
fn fenced(text: &str) -> Option<&str> {
    let inner = text.strip_prefix("```")?.strip_suffix("```")?;
    // the fence's info string, `json` for one
    Some(
        inner
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric())
            .trim(),
    )
}

/// The function a recipient names: `functions.NAME`, or a bare `NAME`, up to the first
/// character no name has (a glued constrain token written as text, a `?`). Other namespaces
/// (`container.exec`, `repo_browser.open_file`) are for the router of aliases.
pub fn function(recipient: &str) -> Option<&str> {
    let end = recipient
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        .unwrap_or(recipient.len());
    let name = &recipient[..end];
    let name = match name.strip_prefix("functions.") {
        Some(n) => n,
        None if !name.contains('.') => name,
        None => return None,
    };
    (!name.is_empty() && !name.contains('.')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id of «name» in a fixture: so that no control token stands in the source whole
    /// (§5.3), fixtures name them.
    fn token(name: &str) -> TokenId {
        if name == "endoftext" {
            return 199_999;
        }
        let text = format!("<|{name}|>");
        Special::ALL
            .into_iter()
            .find(|s| s.text() == text)
            .map(Special::id)
            .unwrap_or_else(|| panic!("no token {name}"))
    }

    /// A generation from a fixture: «name» is that control token, and the text between is
    /// cut into tokens of `size` bytes, so token edges fall anywhere, inside characters too.
    fn generation(fixture: &str, size: usize) -> Vec<(TokenId, Vec<u8>)> {
        let mut out = Vec::new();
        for (i, part) in fixture.split(['«', '»']).enumerate() {
            if i % 2 == 1 {
                out.push((token(part), Vec::new()));
                continue;
            }
            for chunk in part.as_bytes().chunks(size) {
                out.push((1000 + out.len() as TokenId, chunk.to_vec()));
            }
        }
        out
    }

    fn read_tokens(tokens: &[(TokenId, Vec<u8>)]) -> (Reader, Vec<Event>) {
        let mut r = Reader::new();
        let mut events = Vec::new();
        for (id, piece) in tokens {
            events.extend(r.push(*id, piece));
        }
        events.extend(r.finish());
        (r, events)
    }

    /// The messages in short: kind, recipient, content type (`!` when constrained), end and
    /// content.
    fn summary(r: &Reader) -> Vec<String> {
        r.messages()
            .iter()
            .map(|m| {
                let mut s = format!("{:?}", m.kind).to_lowercase();
                if let Some(to) = &m.header.recipient {
                    s += &format!(" {to}");
                }
                if let Some(t) = &m.header.content_type {
                    s += &format!(" {}{t}", if m.header.constrained { "!" } else { "" });
                }
                format!("{s} {:?} | {}", m.end, m.text())
            })
            .collect()
    }

    /// Reads `fixture` cut every way and checks that the readings agree and that the
    /// events tell the same messages; the messages in short.
    fn check(fixture: &str) -> Vec<String> {
        let want = summary(&read_tokens(&generation(fixture, 1)).0);
        for size in [1, 2, 3, 5, 64] {
            let (r, events) = read_tokens(&generation(fixture, size));
            assert_eq!(summary(&r), want, "cut every {size} bytes");
            let mut texts = vec![String::new(); r.messages().len()];
            let mut open = None;
            for e in &events {
                match e {
                    Event::Open { index, kind, .. } => {
                        assert_eq!(open, None);
                        assert_eq!(*kind, r.messages()[*index].kind);
                        open = Some(*index);
                    }
                    Event::Text { index, text } => {
                        assert_eq!(open, Some(*index));
                        texts[*index] += text;
                    }
                    Event::Close { index, end } => {
                        assert_eq!(open, Some(*index));
                        assert_eq!(*end, r.messages()[*index].end);
                        open = None;
                    }
                }
            }
            assert_eq!(open, None);
            let mut last = 0;
            for (m, t) in r.messages().iter().zip(&texts) {
                assert_eq!(*t, m.text());
                assert!(last <= m.tokens.start && m.tokens.end <= r.ids().len());
                last = m.tokens.end;
            }
        }
        want
    }

    #[test]
    fn reads_a_call_in_either_header_order() {
        let calls = [
            // as gpt-oss writes it: the recipient after the channel
            "«channel»analysis«message»Need the file.«end»«start»assistant«channel»commentary to=functions.read_file «constrain»json«message»{\"path\":\"src/lib.rs\"}«call»",
            // as the reference renders it: after the role
            "«channel»analysis«message»Need the file.«end»«start»assistant to=functions.read_file«channel»commentary «constrain»json«message»{\"path\":\"src/lib.rs\"}«call»",
        ];
        for fixture in calls {
            assert_eq!(
                check(fixture),
                [
                    "analysis End | Need the file.",
                    "call functions.read_file !json Call | {\"path\":\"src/lib.rs\"}"
                ]
            );
            let (r, _) = read_tokens(&generation(fixture, 3));
            assert!(r.stopped());
            assert_eq!(r.messages()[1].header.role.as_deref(), Some("assistant"));
            assert!(r.messages().iter().all(|m| m.header.junk.is_empty()));
            // the messages cover the generation, header to end token
            assert_eq!(r.messages()[0].tokens.start, 0);
            assert_eq!(r.messages()[0].tokens.end, r.messages()[1].tokens.start);
            assert_eq!(r.messages()[1].tokens.end, r.ids().len());
        }
    }

    #[test]
    fn takes_the_content_type_in_every_form() {
        // plain, glued to the recipient (llama.cpp #27720), none on the analysis channel with
        // the recipient after the role (llama.cpp's own tests)
        assert_eq!(
            check("«channel»commentary to=functions.read_file json«message»{}«call»"),
            ["call functions.read_file json Call | {}"]
        );
        assert_eq!(
            check(
                "«channel»commentary to=functions.turn_on«constrain»json«message»{\"name\":\"Light\"}«call»"
            ),
            ["call functions.turn_on !json Call | {\"name\":\"Light\"}"]
        );
        assert_eq!(
            check(" to=functions.special_function«channel»analysis«message»{\"arg1\": 1}«call»"),
            ["call functions.special_function Call | {\"arg1\": 1}"]
        );
    }

    #[test]
    fn stops_after_the_answer_or_the_call() {
        assert_eq!(
            check(
                "«channel»analysis«message»Done.«end»«start»assistant«channel»final«message»All tests pass.«return»"
            ),
            ["analysis End | Done.", "final Return | All tests pass."]
        );
        assert_eq!(
            check(
                "«channel»commentary«message»Reading it first.«end»«start»assistant«channel»commentary to=functions.read_file «constrain»json«message»{}«call»"
            ),
            [
                "preamble End | Reading it first.",
                "call functions.read_file !json Call | {}"
            ]
        );
        // an answer or a call that ends with the end token ends the generation all the same, and
        // what comes after it is not read
        for (fixture, read) in [
            ("«channel»final«message»Done.«end»", "final End | Done."),
            (
                "«channel»commentary to=functions.read_file json«message»{}«end»",
                "call functions.read_file json End | {}",
            ),
        ] {
            let mut tokens = generation(fixture, 2);
            let n = tokens.len();
            tokens.extend(generation(
                "«start»assistant«channel»analysis«message»More.",
                2,
            ));
            let (r, events) = read_tokens(&tokens);
            assert!(r.stopped());
            assert_eq!(summary(&r), [read]);
            assert_eq!(r.ids().len(), n);
            assert!(matches!(events.last(), Some(Event::Close { .. })));
        }
    }

    #[test]
    fn junk_headers_read_as_reasoning() {
        // llama.cpp #27720: the nine headers gpt-oss-20b wrote at temperature 1, the text
        // around them made neutral, then the stray header llama.cpp tolerates itself (#21286)
        let cases: [(&str, &[&str]); 10] = [
            (
                "«channel»commentary to=assistant«channel»commentary?commentary=short?comment?…???«end»«start»assistant«channel»analysis«message»We need to answer the question.«end»«start»assistant«channel»final«message»It runs at night.«return»",
                &[
                    "junk End | commentary to=assistant commentary?commentary=short?comment?…???",
                    "analysis End | We need to answer the question.",
                    "final Return | It runs at night.",
                ],
            ),
            (
                "«channel»We need to answer whether the build is broken. We must check the log.«end»«start»assistant«channel»final«message»It is not.«return»",
                &[
                    "junk End | We need to answer whether the build is broken. We must check the log.",
                    "final Return | It is not.",
                ],
            ),
            (
                "«channel»commentary to=assistant ...commentary?«end»«start»assistant«channel»analysis«message»The user asks when to update.«end»«start»assistant«channel»final«message»In spring.«return»",
                &[
                    "junk End | commentary to=assistant ...commentary?",
                    "analysis End | The user asks when to update.",
                    "final Return | In spring.",
                ],
            ),
            (
                "«channel»commentary... The user asks whether the tests passed. The log says they did. No tool needed.«end»«start»assistant«channel»final«message»Yes, they passed.«return»",
                &[
                    "junk End | commentary... The user asks whether the tests passed. The log says they did. No tool needed.",
                    "final Return | Yes, they passed.",
                ],
            ),
            (
                "«channel»commentary 2? The request is ambiguous. Ask one short question.«end»«start»assistant«channel»commentary to=functions.turn_on«constrain»json«message»{\"name\":\"Light\",\"area\":\"Hall\"}«call»",
                &[
                    "junk End | commentary 2? The request is ambiguous. Ask one short question.",
                    "call functions.turn_on !json Call | {\"name\":\"Light\",\"area\":\"Hall\"}",
                ],
            ),
            (
                "«channel»commentary?commentary?We need the outdoor temperature too.«end»«start»assistant«channel»analysis«message»Need the outdoor sensor.«end»«start»assistant«channel»commentary to=functions.get_state «constrain»json«message»{\"name\":\"outdoor\"}«call»",
                &[
                    "junk End | commentary?commentary?We need the outdoor temperature too.",
                    "analysis End | Need the outdoor sensor.",
                    "call functions.get_state !json Call | {\"name\":\"outdoor\"}",
                ],
            ),
            (
                "«channel»commentary says tool returned, so now respond.«end»«start»assistant«channel»analysis«message»The user asked about the forecast.«end»«start»assistant«channel»final«message»No storms tonight.«return»",
                &[
                    "junk End | commentary says tool returned, so now respond.",
                    "analysis End | The user asked about the forecast.",
                    "final Return | No storms tonight.",
                ],
            ),
            (
                "«channel»...?We need answer. The user asks about yesterday.«end»«start»assistant«channel»final«message»They lost.«return»",
                &[
                    "junk End | ...?We need answer. The user asks about yesterday.",
                    "final Return | They lost.",
                ],
            ),
            (
                "«channel»??«end»«start»assistant«channel»analysis«message»We must follow the rules.«end»«start»assistant«channel»final«message»Not this weekend.«return»",
                &[
                    "junk End | ??",
                    "analysis End | We must follow the rules.",
                    "final Return | Not this weekend.",
                ],
            ),
            (
                "«channel»commentary to=assistant«channel»analysis«message»Thinking.«end»«start»assistant«channel»final«message»Hi.«return»",
                &["analysis End | Thinking.", "final Return | Hi."],
            ),
        ];
        for (fixture, want) in cases {
            assert_eq!(check(fixture), want, "{fixture}");
            let (r, _) = read_tokens(&generation(fixture, 4));
            // the junk is told, and what follows it is clean
            assert!(!r.messages()[0].header.junk.is_empty(), "{fixture}");
            assert!(r.messages()[1..].iter().all(|m| m.header.junk.is_empty()));
            assert!(r.stopped());
        }
        let (r, _) = read_tokens(&generation(cases[0].0, 4));
        assert_eq!(
            r.messages()[0].header.junk,
            [
                "2 channel tokens",
                "to=assistant",
                "commentary",
                "channel commentary?commentary=short?comment?…???",
                "the header has no message"
            ]
        );
        // an unknown channel that reaches its message is junk too, unless it has a recipient
        assert_eq!(
            check(
                "«channel»thinking«message»Hmm.«end»«start»assistant«channel»final«message»Ok.«return»"
            ),
            ["junk End | Hmm.", "final Return | Ok."]
        );
        assert_eq!(
            check("«channel»commentary?? to=functions.read_file«message»{}«call»"),
            ["call functions.read_file Call | {}"]
        );
    }

    #[test]
    fn a_message_that_runs_into_the_next_header_ends_there() {
        // M1 stamp A: the content of a write went on with the model's next headers. The call
        // ends where the next header begins, and that is where reading stops.
        let head = "«channel»analysis«message»Now write the test.«end»«start»assistant«channel»commentary to=functions.write «constrain»json«message»{\"path\":\"tests/range.rs\",\"content\":\"#[test]\\nfn parses() {}\\n\\\"} to=assistant";
        let tail = "«channel»commentary«message»The file is written.«end»«start»assistant«channel»commentary to=functions.read«message»{\"«call»";
        let fixture = format!("{head}{tail}");
        assert_eq!(
            check(&fixture),
            [
                "analysis End | Now write the test.",
                "call functions.write !json RanOn | {\"path\":\"tests/range.rs\",\"content\":\"#[test]\\nfn parses() {}\\n\\\"} to=assistant",
            ]
        );
        let (r, _) = read_tokens(&generation(&fixture, 3));
        assert!(r.stopped());
        assert_eq!(r.ids().len(), generation(head, 3).len());
        // a thought that runs on, and headers without their start token
        for fixture in [
            "«channel»analysis«message»Plan.«start»assistant«channel»final«message»Done.«return»",
            "«channel»analysis«message»Plan.«channel»final«message»Done.«return»",
        ] {
            assert_eq!(
                check(fixture),
                ["analysis RanOn | Plan.", "final Return | Done."]
            );
        }
        for fixture in [
            "«channel»analysis«message»Plan.«end»«channel»final«message»Done.«return»",
            "«channel»analysis«message»Plan.«end»assistant«channel»final«message»Done.«return»",
        ] {
            assert_eq!(
                check(fixture),
                ["analysis End | Plan.", "final Return | Done."]
            );
        }
    }

    #[test]
    fn a_cut_generation_leaves_its_message_cut() {
        assert_eq!(
            check("«channel»analysis«message»Let me think about"),
            ["analysis Cut | Let me think about"]
        );
        assert_eq!(
            check(
                "«channel»analysis«message»Plan.«end»«start»assistant«channel»commentary to=functions.write «constrain»json«message»{\"path\":\"a.rs\",\"content\":\"fn main() {"
            ),
            [
                "analysis End | Plan.",
                "call functions.write !json Cut | {\"path\":\"a.rs\",\"content\":\"fn main() {"
            ]
        );
        assert_eq!(
            check("«channel»analysis«message»Plan.«end»«start»assistant«channel»comm"),
            ["analysis End | Plan.", "junk Cut | assistant comm"]
        );
        let (r, _) = read_tokens(&generation("«channel»analysis«message»Let me", 2));
        assert!(!r.stopped());
    }

    #[test]
    fn control_tokens_have_no_place_in_content() {
        let (r, _) = read_tokens(&generation(
            "«channel»commentary to=functions.write «constrain»json«message»{\"content\":\"a«endoftext»b«message»c\"}«call»",
            3,
        ));
        assert_eq!(
            summary(&r),
            ["call functions.write !json Call | {\"content\":\"abc\"}"]
        );
        assert_eq!(r.messages()[0].foreign, [199_999, Special::Message.id()]);
    }

    #[test]
    fn text_that_spells_a_control_token_stays_text() {
        let mut tokens = generation("«channel»final«message»Stop at ", 64);
        tokens.push((1000, Special::End.text().into_bytes()));
        tokens.push((Special::Return.id(), Vec::new()));
        let (r, _) = read_tokens(&tokens);
        assert_eq!(
            summary(&r),
            [format!("final Return | Stop at {}", Special::End.text())]
        );
        assert!(r.messages()[0].foreign.is_empty());
    }

    #[test]
    fn characters_come_whole_however_tokens_cut_them() {
        // check() reads it cut every 1, 2, 3 and 5 bytes, and every event's text must add up
        assert_eq!(
            check("«channel»final«message»Привет, мир — ✓ 🎉«return»"),
            ["final Return | Привет, мир — ✓ 🎉"]
        );
        // a byte that is no UTF-8, and a character the generation never finished
        let mut tokens = generation("«channel»final«message»a", 64);
        tokens.push((1001, vec![0xFF]));
        tokens.push((1002, "б".as_bytes()[..1].to_vec()));
        let (r, events) = read_tokens(&tokens);
        assert!(!r.messages()[0].valid_utf8());
        let shown: String = events
            .iter()
            .filter_map(|e| match e {
                Event::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(shown, "a\u{FFFD}\u{FFFD}");
        assert_eq!(r.messages()[0].end, End::Cut);
    }

    #[test]
    fn arguments_are_read_as_gpt_oss_writes_them() {
        let read = |s: &str| arguments(s).map(|(m, r)| (Value::Object(m).to_string(), r));
        let path = "{\"path\":\"a\"}".to_owned();
        assert_eq!(read("{\"path\": \"a\"}"), Ok((path.clone(), vec![])));
        assert_eq!(
            read("```json\n{\"path\": \"a\"}\n```"),
            Ok((path.clone(), vec![Repair::Fence]))
        );
        assert_eq!(
            read("[{\"path\": \"a\"}]"),
            Ok((path.clone(), vec![Repair::Array]))
        );
        assert_eq!(
            read("\"{\\\"path\\\": \\\"a\\\"}\""),
            Ok((path.clone(), vec![Repair::Quoted]))
        );
        assert_eq!(
            read("{\"path\": \"a\"} to=assistant"),
            Ok((path, vec![Repair::Trailing]))
        );
        assert_eq!(read("  "), Ok(("{}".into(), vec![Repair::Empty])));
        for bad in ["{\"path\":", "[1, 2]", "42", "\"just text\"", "[[{}]]"] {
            assert!(read(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn functions_are_found_by_their_names() {
        assert_eq!(function("functions.read_file"), Some("read_file"));
        assert_eq!(function("read_file"), Some("read_file"));
        assert_eq!(function("mcp__git__status"), Some("mcp__git__status"));
        assert_eq!(
            function(&format!(
                "functions.turn_on{}json",
                Special::Constrain.text()
            )),
            Some("turn_on")
        );
        assert_eq!(function("functions.read_file?"), Some("read_file"));
        assert_eq!(function("container.exec"), None);
        assert_eq!(function("repo_browser.open_file"), None);
        assert_eq!(function("functions."), None);
    }
}
