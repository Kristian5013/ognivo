//! The arguments of a call as the model writes them, read as far as they go: each
//! argument's value as it grows, for display while the call streams, never to run it (the
//! call runs from the arguments whole, once the generation ended).
//!
//! Two forms: a JSON object, as the Generic path's server and Harmony write arguments, and
//! the parameters of Qwen's XML calls (`<parameter=NAME>` … `</parameter>`). Anything else
//! is shown as it comes.

/// More of one argument's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// The argument by the model's name for it; empty for text of no argument.
    pub name: String,
    pub text: String,
}

/// One call's arguments, read as they come.
#[derive(Debug, Default)]
pub struct Arguments {
    form: Form,
}

#[derive(Debug, Default)]
enum Form {
    /// Nothing but whitespace yet.
    #[default]
    Unknown,
    Json(Json),
    Xml(Xml),
    Raw,
}

impl Arguments {
    /// More of the arguments: the values they add to, in order.
    pub fn push(&mut self, text: &str) -> Vec<Piece> {
        let mut out = Out::default();
        let mut rest = text;
        if let Form::Unknown = self.form {
            let Some(first) = rest.chars().find(|c| !c.is_whitespace()) else {
                return Vec::new();
            };
            self.form = match first {
                '{' => Form::Json(Json::default()),
                '<' => Form::Xml(Xml::default()),
                _ => Form::Raw,
            };
            rest = rest.trim_start();
        }
        match &mut self.form {
            Form::Unknown => {}
            Form::Json(j) => rest.chars().for_each(|c| j.push(c, &mut out)),
            Form::Xml(x) => x.push(rest, &mut out),
            Form::Raw => out.add("", rest),
        }
        out.0
    }
}

/// Pieces, the text of one argument in a row joined.
#[derive(Debug, Default)]
struct Out(Vec<Piece>);

impl Out {
    fn add(&mut self, name: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.name == name => last.text.push_str(text),
            _ => self.0.push(Piece {
                name: name.to_owned(),
                text: text.to_owned(),
            }),
        }
    }

    fn char(&mut self, name: &str, c: char) {
        self.add(name, c.encode_utf8(&mut [0; 4]));
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before the object's `{`.
    #[default]
    Start,
    /// A key, or the object's end.
    Key,
    InKey,
    Colon,
    Value,
    /// A string value, decoded as it comes.
    InString,
    /// Any other value (a number, a literal, an array, an object), shown as written.
    InRaw,
    /// A `,` or the object's end.
    Next,
    Done,
}

#[derive(Debug, Default)]
struct Json {
    state: State,
    key: String,
    /// After a backslash: the escape so far, without it.
    escape: Option<String>,
    /// A high surrogate whose low half may come next.
    high: Option<u16>,
    /// In a raw value: how deep in brackets, inside a string, after a backslash there.
    depth: u32,
    in_string: bool,
    escaped: bool,
}

impl Json {
    fn push(&mut self, c: char, out: &mut Out) {
        match self.state {
            State::Start => {
                if c == '{' {
                    self.state = State::Key;
                }
            }
            State::Key => match c {
                '"' => {
                    self.key.clear();
                    self.state = State::InKey;
                }
                '}' => self.state = State::Done,
                _ => {}
            },
            State::InKey => match (self.escaped, c) {
                (true, _) => {
                    self.key.push(c);
                    self.escaped = false;
                }
                (false, '\\') => self.escaped = true,
                (false, '"') => self.state = State::Colon,
                (false, _) => self.key.push(c),
            },
            State::Colon => {
                if c == ':' {
                    self.state = State::Value;
                }
            }
            State::Value => match c {
                _ if c.is_whitespace() => {}
                '"' => self.state = State::InString,
                _ => {
                    self.state = State::InRaw;
                    self.depth = 0;
                    self.in_string = false;
                    self.escaped = false;
                    self.raw(c, out);
                }
            },
            State::InString => self.string(c, out),
            State::InRaw => self.raw(c, out),
            State::Next => match c {
                ',' => self.state = State::Key,
                '}' => self.state = State::Done,
                _ => {}
            },
            State::Done => {}
        }
    }

    /// A character of a string value: decoded, with its escapes.
    fn string(&mut self, c: char, out: &mut Out) {
        let key = self.key.clone();
        if let Some(mut escape) = self.escape.take() {
            escape.push(c);
            if escape.starts_with('u') && escape.len() < 5 {
                self.escape = Some(escape);
                return;
            }
            self.unescape(&escape, &key, out);
            return;
        }
        if let Some(high) = self.high.take() {
            // a high surrogate without its low half
            if c == '\\' {
                self.high = Some(high);
                self.escape = Some(String::new());
                return;
            }
            out.char(&key, char::REPLACEMENT_CHARACTER);
        }
        match c {
            '\\' => self.escape = Some(String::new()),
            '"' => self.state = State::Next,
            _ => out.char(&key, c),
        }
    }

    fn unescape(&mut self, escape: &str, key: &str, out: &mut Out) {
        let high = self.high.take();
        let simple = match escape {
            "\"" => Some('"'),
            "\\" => Some('\\'),
            "/" => Some('/'),
            "b" => Some('\u{8}'),
            "f" => Some('\u{c}'),
            "n" => Some('\n'),
            "r" => Some('\r'),
            "t" => Some('\t'),
            _ => None,
        };
        if let Some(c) = simple {
            if high.is_some() {
                out.char(key, char::REPLACEMENT_CHARACTER);
            }
            out.char(key, c);
            return;
        }
        let unit = escape
            .strip_prefix('u')
            .and_then(|hex| u16::from_str_radix(hex, 16).ok());
        let Some(unit) = unit else {
            out.char(key, char::REPLACEMENT_CHARACTER);
            return;
        };
        match (high, unit) {
            (Some(h), 0xDC00..=0xDFFF) => {
                let code = 0x10000 + ((u32::from(h) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                out.char(
                    key,
                    char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER),
                );
            }
            (h, 0xD800..=0xDBFF) => {
                if h.is_some() {
                    out.char(key, char::REPLACEMENT_CHARACTER);
                }
                self.high = Some(unit);
            }
            (h, _) => {
                if h.is_some() {
                    out.char(key, char::REPLACEMENT_CHARACTER);
                }
                out.char(
                    key,
                    char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
                );
            }
        }
    }

    /// A character of another value, shown as written up to the `,` or `}` that ends it.
    fn raw(&mut self, c: char, out: &mut Out) {
        let key = self.key.clone();
        if self.in_string {
            match (self.escaped, c) {
                (true, _) => self.escaped = false,
                (false, '\\') => self.escaped = true,
                (false, '"') => self.in_string = false,
                _ => {}
            }
            out.char(&key, c);
            return;
        }
        match c {
            '"' => self.in_string = true,
            '[' | '{' => self.depth += 1,
            ']' | '}' if self.depth > 0 => self.depth -= 1,
            '}' => {
                self.state = State::Done;
                return;
            }
            ',' if self.depth == 0 => {
                self.state = State::Key;
                return;
            }
            _ if c.is_whitespace() && self.depth == 0 => return,
            _ => {}
        }
        out.char(&key, c);
    }
}

const OPEN: &str = "<parameter=";
const CLOSE: &str = "</parameter>";

/// Qwen's parameters: `<parameter=NAME>`, a newline, the value, a newline, `</parameter>`.
#[derive(Debug, Default)]
struct Xml {
    /// What came that is not yet shown: text between parameters, a name, or the end of a
    /// value that may be the start of its closing tag.
    held: String,
    /// The parameter whose value comes, and whether its first character came.
    value: Option<(String, bool)>,
    /// Inside `<parameter=`, before its `>`.
    naming: bool,
}

impl Xml {
    fn push(&mut self, text: &str, out: &mut Out) {
        self.held.push_str(text);
        loop {
            if self.naming {
                let Some(end) = self.held.find('>') else {
                    return;
                };
                let name = self.held[..end].trim().to_owned();
                self.held.drain(..=end);
                self.naming = false;
                self.value = Some((name, false));
                continue;
            }
            if let Some((name, started)) = &mut self.value {
                if !*started {
                    if self.held.is_empty() {
                        return;
                    }
                    if self.held.starts_with('\n') {
                        self.held.remove(0);
                    }
                    *started = true;
                }
                let name = name.clone();
                if let Some(end) = self.held.find(CLOSE) {
                    let value = self.held[..end]
                        .strip_suffix('\n')
                        .unwrap_or(&self.held[..end]);
                    out.add(&name, value);
                    self.held.drain(..end + CLOSE.len());
                    self.value = None;
                    continue;
                }
                // the newline and the closing tag may be on their way
                let keep = floor(&self.held, self.held.len().saturating_sub(CLOSE.len() + 1));
                out.add(&name, &self.held[..keep]);
                self.held.drain(..keep);
                return;
            }
            if let Some(start) = self.held.find(OPEN) {
                self.held.drain(..start + OPEN.len());
                self.naming = true;
                continue;
            }
            let keep = floor(&self.held, self.held.len().saturating_sub(OPEN.len()));
            self.held.drain(..keep);
            return;
        }
    }
}

/// The largest char boundary of `s` at or below `at`.
fn floor(s: &str, mut at: usize) -> usize {
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values of `whole` read in pieces of `size` bytes (cut at char boundaries).
    fn read(whole: &str, size: usize) -> Vec<(String, String)> {
        let mut args = Arguments::default();
        let mut values: Vec<(String, String)> = Vec::new();
        let mut at = 0;
        while at < whole.len() {
            let mut end = (at + size).min(whole.len());
            while !whole.is_char_boundary(end) {
                end += 1;
            }
            for p in args.push(&whole[at..end]) {
                match values.last_mut() {
                    Some((name, text)) if *name == p.name => text.push_str(&p.text),
                    _ => values.push((p.name, p.text)),
                }
            }
            at = end;
        }
        values
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn json_values_come_decoded_whatever_the_cut() {
        let content = "fn main() {\n\tprintln!(\"héllo 🔥 \\\\ /\");\n}\n";
        let whole = format!(
            r#"{{"path": "src/main.rs", "content": {}, "n": 3}}"#,
            serde_json::to_string(content).unwrap()
        );
        let want = pairs(&[("path", "src/main.rs"), ("content", content), ("n", "3")]);
        for size in [1, 2, 3, 5, 7, 64, 4096] {
            assert_eq!(read(&whole, size), want, "pieces of {size}");
        }
        // what serde_json escapes is not all a model may: \u escapes, a surrogate pair
        let escaped = r#"{"text": "aé🔥\/\b\"x\"", "list": ["a", "b,}"], "o": {"k": [1, 2]}}"#;
        let want = pairs(&[
            ("text", "aé🔥/\u{8}\"x\""),
            ("list", "[\"a\", \"b,}\"]"),
            ("o", "{\"k\": [1, 2]}"),
        ]);
        for size in [1, 2, 3, 9, 100] {
            assert_eq!(read(escaped, size), want, "pieces of {size}");
        }
    }

    #[test]
    fn a_json_value_shows_as_far_as_it_came() {
        let mut args = Arguments::default();
        assert!(args.push("  ").is_empty());
        let first = args.push("{\"path\": \"a.r");
        assert_eq!(
            first,
            vec![Piece {
                name: "path".into(),
                text: "a.r".into()
            }]
        );
        let next = args.push("s\", \"content\": \"line 1\\");
        assert_eq!(
            next,
            vec![
                Piece {
                    name: "path".into(),
                    text: "s".into()
                },
                Piece {
                    name: "content".into(),
                    text: "line 1".into()
                }
            ]
        );
        // the escape completes with the next piece
        assert_eq!(
            args.push("nline 2"),
            vec![Piece {
                name: "content".into(),
                text: "\nline 2".into()
            }]
        );
    }

    #[test]
    fn qwen_parameters_come_without_their_newlines() {
        let whole = "\n<parameter=file_path>\nsrc/lib.rs\n</parameter>\n<parameter=content>\npub fn f() -> u8 {\n    1 // </param\n}\n\n</parameter>\n</function>\n";
        let want = pairs(&[
            ("file_path", "src/lib.rs"),
            ("content", "pub fn f() -> u8 {\n    1 // </param\n}\n"),
        ]);
        for size in [1, 2, 3, 5, 11, 13, 4096] {
            assert_eq!(read(whole, size), want, "pieces of {size}");
        }
    }

    #[test]
    fn other_text_comes_as_it_is() {
        assert_eq!(
            read("*** Begin Patch\n*** Add File: a\n+x\n", 4),
            pairs(&[("", "*** Begin Patch\n*** Add File: a\n+x\n")])
        );
    }
}
