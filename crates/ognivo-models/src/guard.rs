//! Special-token guard of the Generic path (docs/architecture.md §5.3).
//!
//! llama-server renders the chat template and tokenizes the whole prompt with
//! `parse_special=true`, so the text of a control token anywhere in a message becomes that
//! token: a file can forge a message boundary, and in a task about tokenizers the model sees
//! a real BOS where the file has its text (M0, misc-ai-chat-multiturn). The guard breaks
//! every guarded string of the profile ([`TokenTable::guarded`]) with U+2060 WORD JOINER
//! after its first character, in everything the driver sends, and doubles the joiners that
//! were there already, so [`unescape`] is its exact inverse; the driver runs the inverse on
//! everything the model sends back. The model reads the broken string as text and may copy
//! it with or without the joiner: the tools get the original either way.
//!
//! The model does not always copy the joiner itself: in M1 T2-mini gpt-oss wrote U+200D
//! ZERO WIDTH JOINER in its place, and Qwen, asked for U+2060, wrote U+2063 INVISIBLE
//! SEPARATOR. A guarded string broken by a run of such invisible format characters
//! ([`LOOKALIKES`], the joiner among them) after its first character comes back whole too
//! ([`Guard::restore`]). Elsewhere they stay, so emoji sequences keep their joiners.
//!
//! A string that frames the model's own messages and calls ([`TokenTable::framing`]) and
//! comes back unbroken inside tool arguments is not such a copy: the model wrote its own
//! format there and ran past the end of the call. In M1 stamp A gpt-oss wrote its next
//! message headers into a test file, and Gemma the end of its thought channel and a new
//! call header into two files. [`Guard::framing_in_json`] finds them, and such calls do not
//! run.

use std::borrow::Cow;

use serde_json::Value;

use crate::profile::TokenTable;

/// U+2060 WORD JOINER: invisible, and outside every control token of the three profiles.
pub const JOINER: char = '\u{2060}';

/// Invisible format characters a model may write where it saw the joiner: ZERO WIDTH SPACE,
/// NON-JOINER and JOINER, the byte order mark, and the invisible operators next to the
/// joiner (U+2061 to U+2064).
pub const LOOKALIKES: [char; 8] = [
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}',
];

#[derive(Debug, Clone, Default)]
pub struct Guard {
    /// Broken on the way in.
    guarded: Vec<String>,
    /// The model's own message and call format: unbroken inside tool arguments, a leak.
    framing: Vec<String>,
}

impl Guard {
    pub fn new(tokens: &TokenTable) -> Self {
        Self {
            guarded: tokens.guarded().map(str::to_owned).collect(),
            framing: tokens.framing.clone(),
        }
    }

    /// The guard with one more guarded string: llama-server's media marker, in a prompt
    /// that goes as text with images (§5.8).
    pub fn with(&self, extra: &str) -> Guard {
        let mut g = self.clone();
        if !extra.is_empty() && !g.guarded.iter().any(|s| s == extra) {
            g.guarded.push(extra.to_owned());
        }
        g
    }

    /// `text` with a joiner after the first character of every guarded string and every
    /// joiner already in it doubled.
    pub fn escape<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !text.contains(JOINER) && !self.guarded.iter().any(|g| text.contains(g.as_str())) {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len() + 8);
        for (i, c) in text.char_indices() {
            out.push(c);
            if c == JOINER
                || self
                    .guarded
                    .iter()
                    .any(|g| text[i..].starts_with(g.as_str()))
            {
                out.push(JOINER);
            }
        }
        Cow::Owned(out)
    }

    /// [`Guard::escape`] on every string of a JSON value, keys included; whether anything
    /// changed.
    pub fn escape_json(&self, v: &mut Value) -> bool {
        map_strings(v, &|s| match self.escape(s) {
            Cow::Owned(s) => Some(s),
            Cow::Borrowed(_) => None,
        })
    }

    /// What the model sent, as the tools and the user must get it: every guarded string it
    /// wrote with [`LOOKALIKES`] in place of the joiner whole, then [`unescape`].
    pub fn restore<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match self.rejoin(text) {
            Cow::Borrowed(t) => unescape(t),
            Cow::Owned(t) => Cow::Owned(unescape(&t).into_owned()),
        }
    }

    /// [`Guard::restore`] on every string of a JSON value, keys included; whether anything
    /// changed.
    pub fn restore_json(&self, v: &mut Value) -> bool {
        map_strings(v, &|s| match self.restore(s) {
            Cow::Owned(s) => Some(s),
            Cow::Borrowed(_) => None,
        })
    }

    /// Guarded strings broken by a run of invisible format characters with at least one of
    /// [`LOOKALIKES`] in it, whole; runs of the joiner alone are left to [`unescape`].
    fn rejoin<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !text.contains(&LOOKALIKES[..]) {
            return Cow::Borrowed(text);
        }
        let invisible = |c: char| c == JOINER || LOOKALIKES.contains(&c);
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        'scan: while let Some(c) = rest.chars().next() {
            let after = &rest[c.len_utf8()..];
            let tail = after.trim_start_matches(invisible);
            let run = &after[..after.len() - tail.len()];
            if run.contains(&LOOKALIKES[..]) {
                for g in &self.guarded {
                    let mut chars = g.chars();
                    if chars.next() == Some(c) && tail.starts_with(chars.as_str()) {
                        out.push_str(g);
                        rest = &tail[chars.as_str().len()..];
                        continue 'scan;
                    }
                }
            }
            out.push(c);
            rest = after;
        }
        Cow::Owned(out)
    }

    /// The first framing string, unbroken, in any string of a JSON value.
    pub fn framing_in_json(&self, v: &Value) -> Option<&str> {
        match v {
            Value::String(s) => self.framing_in(s),
            Value::Array(a) => a.iter().find_map(|x| self.framing_in_json(x)),
            Value::Object(m) => m
                .iter()
                .find_map(|(k, x)| self.framing_in(k).or_else(|| self.framing_in_json(x))),
            _ => None,
        }
    }

    /// The framing string that occurs first in `text`.
    fn framing_in(&self, text: &str) -> Option<&str> {
        self.framing
            .iter()
            .filter_map(|s| text.find(s.as_str()).map(|at| (at, s.as_str())))
            .min_by_key(|&(at, _)| at)
            .map(|(_, s)| s)
    }
}

/// The exact inverse of [`Guard::escape`]: a run of 2k joiners becomes k, and a lone joiner
/// (one the guard put in, or one the model wrote on its own) is dropped.
pub fn unescape(text: &str) -> Cow<'_, str> {
    if !text.contains(JOINER) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut run = 0;
    for c in text.chars() {
        if c == JOINER {
            run += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(JOINER, run / 2));
        run = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(JOINER, run / 2));
    Cow::Owned(out)
}

/// [`unescape`] on every string of a JSON value, keys included; whether anything changed.
pub fn unescape_json(v: &mut Value) -> bool {
    map_strings(v, &|s| match unescape(s) {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    })
}

/// Replaces each string `f` maps to `Some`; whether any was replaced.
fn map_strings(v: &mut Value, f: &impl Fn(&str) -> Option<String>) -> bool {
    match v {
        Value::String(s) => match f(s) {
            Some(new) => {
                *s = new;
                true
            }
            None => false,
        },
        Value::Array(a) => a.iter_mut().fold(false, |acc, x| map_strings(x, f) | acc),
        Value::Object(m) => {
            let mut changed = false;
            for (k, mut x) in std::mem::take(m) {
                changed |= map_strings(&mut x, f);
                let k = match f(&k) {
                    Some(new) => {
                        changed = true;
                        new
                    }
                    None => k,
                };
                m.insert(k, x);
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile;
    use serde_json::json;

    // The repository rule of §5.3: no control-token literal is written in one piece here.
    fn tok(parts: &[&str]) -> String {
        parts.concat()
    }

    fn guard(id: &str) -> Guard {
        let p = profile::builtin().unwrap();
        Guard::new(&p.iter().find(|p| p.id == id).unwrap().tokens)
    }

    #[test]
    fn escape_breaks_every_guarded_string_and_unescape_inverts_it() {
        for p in profile::builtin().unwrap() {
            let g = Guard::new(&p.tokens);
            for t in p.tokens.guarded() {
                let text = format!("a{t}b{t}{t}\n");
                let e = g.escape(&text);
                for u in p.tokens.guarded() {
                    assert!(!e.contains(u), "{}: {u:?} survives in {e:?}", p.id);
                }
                assert_eq!(unescape(&e), text, "{}", p.id);
            }
        }
    }

    #[test]
    fn joiners_already_in_the_text_survive_the_round_trip() {
        let g = guard("gemma-4-26b-a4b");
        let bos = tok(&["<", "bos>"]);
        let j = JOINER;
        for text in [
            format!("{j}"),
            format!("{j}{j}{j}"),
            format!("<{j}bos>"),
            format!("{j}{bos}{j}"),
            format!("x{j}{j}{bos}"),
            "no specials, no joiners".to_owned(),
        ] {
            let e = g.escape(&text);
            assert!(!e.contains(bos.as_str()), "{e:?}");
            assert_eq!(unescape(&e), text);
        }
        assert!(matches!(g.escape("plain <b>"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_copy_without_the_joiner_restores_the_same_text() {
        let g = guard("gemma-4-26b-a4b");
        let bos = tok(&["<", "bos>"]);
        let shown = g.escape(&format!("SPECIALS = [\"{bos}\"]")).into_owned();
        assert_eq!(shown, format!("SPECIALS = [\"<{JOINER}bos>\"]"));
        // the model copies what it saw, or writes the plain text: the tools get the file text
        assert_eq!(unescape(&shown), format!("SPECIALS = [\"{bos}\"]"));
        assert_eq!(unescape(&format!("[\"{bos}\"]")), format!("[\"{bos}\"]"));
    }

    #[test]
    fn a_copy_with_another_invisible_joiner_restores_the_same_text() {
        let g = guard("gpt-oss-20b");
        let start = tok(&["<|", "start|>"]);
        let rest = &start[1..];
        // M1 T2-mini: gpt-oss saw `<` U+2060 `|start|>` and wrote `<` U+200D `|start|>`;
        // Qwen, writing U+2060 on purpose, wrote U+2063
        for c in LOOKALIKES {
            let copy = format!("ends with <{c}{rest} here");
            assert_eq!(g.restore(&copy), format!("ends with {start} here"), "{c:?}");
        }
        let mixed = format!("<{JOINER}\u{200D}{JOINER}{rest}");
        assert_eq!(g.restore(&mixed), start);
        // the joiner alone keeps the exact inverse, and a guarded string's text stays
        assert_eq!(g.restore(&g.escape(&start)), start);
        assert_eq!(
            g.restore(&format!("<{JOINER}{JOINER}{rest}")),
            format!("<{JOINER}{rest}")
        );
        // outside guarded strings the characters are text: emoji sequences keep them
        for text in [
            "👨\u{200D}👩\u{200D}👧",
            "a\u{200B}b",
            "a\u{2063}b",
            "<\u{200D}|nothing|>",
            "\u{FEFF}x",
        ] {
            assert_eq!(g.restore(text), text);
        }
        let mut args = json!({"content": format!("<\u{200B}{rest}"), "k": ["x"]});
        assert!(g.restore_json(&mut args));
        assert_eq!(args["content"], start);
    }

    #[test]
    fn json_strings_and_keys_are_mapped() {
        let g = guard("qwen3.8-27b");
        let think = tok(&["<", "think>"]);
        let mut v = json!({"file_path": "a.py", "content": format!("x = '{think}'"), think.clone(): [think.clone(), 1]});
        assert!(g.escape_json(&mut v));
        assert!(!v.to_string().contains(think.as_str()));
        assert!(unescape_json(&mut v));
        assert_eq!(
            v,
            json!({"file_path": "a.py", "content": format!("x = '{think}'"), think.clone(): [think, 1]})
        );
        let mut plain = json!({"path": "a", "n": 3});
        assert!(!g.escape_json(&mut plain) && !unescape_json(&mut plain));
    }

    #[test]
    fn framing_strings_are_found_in_arguments_and_others_are_not() {
        let g = guard("gpt-oss-20b");
        // M1 stamp A, cforge-cpuset-tests: a write whose content ran into the next message
        let leaked = json!({"path": "tests/serde.rs", "content": tok(&[
            "}\n\"} to=assistant<|", "channel|>commentary<|", "message|>The file is written.<|", "end|>",
        ])});
        assert_eq!(
            g.framing_in_json(&leaked),
            Some(tok(&["<|", "channel|>"]).as_str())
        );
        let quoted = json!({"content": tok(&["print('<|", "endoftext|>')"])});
        assert_eq!(g.framing_in_json(&quoted), None);
        let broken = json!({"content": g.escape(&tok(&["<|", "end|>"])).into_owned()});
        assert_eq!(g.framing_in_json(&broken), None);
        // Qwen's reasoning tags are the text of a task, not a leak
        let tags = json!({"content": tok(&["SPECIALS = ['<", "think>', '</", "think>']"])});
        assert_eq!(guard("qwen3.8-27b").framing_in_json(&tags), None);
    }
}
