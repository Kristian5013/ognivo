//! Repeated calls (docs/architecture.md §6, «Петли и застой»): a call that gives the same
//! result as an identical call before it, while nothing has changed in between, tells the
//! model nothing new.
//!
//! - The second time its result is marked unchanged; the third time it does not run and
//!   the model gets alternatives; a fourth time with nothing new since the third ends the
//!   turn, and the user decides. With something new in between, a fourth time is skipped
//!   like the third: in M1 stamp A' gpt-oss solved a task while coming back to one grep
//!   four times between new searches.
//! - Identical means the same tool, arguments and directory, without an edit's new text:
//!   an edit that does not apply fails the same way whatever it would write (stamp A',
//!   Gemma sent one wrong old_string twice with two replacements). Results compare as the
//!   model reads them, the run time in a command's status line included.
//! - A change is an applied edit or write, or a command that is not read-only. A call that
//!   changes something is never a repeat, and no call before a change is repeated after
//!   it: in M0 OpenCode counted `cargo test` runs between edits as a loop. Nothing else
//!   changes the project between calls: whatever a command leaves running dies with it.
//! - It relies on every earlier result still being in the history: whatever rewrites the
//!   history (compaction) has to start a new [`Repeats`].

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use ognivo_tools::projection::Call;

/// What a call's result depends on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fingerprint {
    Call {
        cwd: PathBuf,
        call: Call,
    },
    /// Arguments that did not parse, as they were sent.
    Unparsed {
        tool: String,
        arguments: String,
    },
}

impl Fingerprint {
    /// `call` run in `cwd`, without an edit's new text.
    pub fn of(cwd: &Path, call: &Call) -> Self {
        let mut call = call.clone();
        if let Call::Edit { new, .. } = &mut call {
            new.clear();
        }
        Fingerprint::Call {
            cwd: cwd.to_owned(),
            call,
        }
    }

    fn is_edit(&self) -> bool {
        matches!(
            self,
            Fingerprint::Call {
                call: Call::Edit { .. },
                ..
            }
        )
    }

    /// How a note names the call.
    fn what(&self) -> &'static str {
        if self.is_edit() {
            "an edit with this old_string"
        } else {
            "this same call"
        }
    }
}

/// What to do with a call before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Run,
    /// Not run; the text is its result.
    Skip(String),
    /// Not run, and the turn ends; the text is its result.
    Stop(String),
}

#[derive(Debug, Clone, Copy)]
struct Seen {
    /// [`Repeats::changes`] when it last ran.
    changes: u64,
    /// Hash of its result.
    result: u64,
    /// Times in a row it ran or was skipped with that result.
    times: u32,
    /// [`Repeats::news`] at its last time.
    news: u64,
}

/// The repeat detector of one turn.
#[derive(Debug, Default)]
pub struct Repeats {
    /// Changes of the project so far.
    changes: u64,
    /// Calls so far that brought something new: a change, or a result not seen since the
    /// last change.
    news: u64,
    /// `changes` when the running call started.
    started: u64,
    seen: HashMap<Fingerprint, Seen>,
}

impl Repeats {
    /// Before a call runs.
    pub fn check(&mut self, key: &Fingerprint) -> Check {
        self.started = self.changes;
        let Some(s) = self.seen.get_mut(key) else {
            return Check::Run;
        };
        if s.changes != self.changes || s.times < 2 {
            return Check::Run;
        }
        if s.times >= 3 && s.news == self.news {
            return Check::Stop(
                "[ognivo] Not run: this call kept giving the same result with nothing new in between, so ognivo ended the turn here for the user to decide.".to_owned(),
            );
        }
        s.times += 1;
        s.news = self.news;
        let advice = match key {
            Fingerprint::Unparsed { .. } => "Fix the arguments as its error above says.",
            _ if key.is_edit() => {
                "Read the lines you want to change again and copy them exactly, with enough of them to be unique, or rewrite the file with write."
            }
            _ => "Its result is above: use it, or do something else.",
        };
        Check::Skip(format!(
            "[ognivo] Not run: {} gave the same result twice, and nothing has changed since, so it would only give it again. {advice}",
            key.what()
        ))
    }

    /// The project may have changed.
    pub fn changed(&mut self) {
        self.changes += 1;
    }

    /// After a call ran: the note for its result when it repeats an identical call's.
    pub fn record(&mut self, key: Fingerprint, result: &str) -> Option<String> {
        if self.changes != self.started {
            // what changed something is not repeated by its twin
            self.seen.remove(&key);
            self.news += 1;
            return None;
        }
        let result = hash(result);
        let times = match self.seen.get(&key) {
            Some(s) if s.changes == self.changes && s.result == result => s.times + 1,
            _ => {
                self.news += 1;
                1
            }
        };
        let note = (times > 1).then(|| {
            format!(
                "[ognivo] Unchanged: {} gave this same result before, and nothing has changed since.",
                key.what()
            )
        });
        self.seen.insert(
            key,
            Seen {
                changes: self.changes,
                result,
                times,
                news: self.news,
            },
        );
        note
    }
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str) -> Fingerprint {
        read_in("/p", path)
    }

    fn read_in(cwd: &str, path: &str) -> Fingerprint {
        Fingerprint::of(
            Path::new(cwd),
            &Call::Read {
                path: path.into(),
                offset: None,
                limit: None,
            },
        )
    }

    /// One call that changes nothing; what the model reads.
    fn call(r: &mut Repeats, key: &Fingerprint, result: &str) -> String {
        match r.check(key) {
            Check::Run => match r.record(key.clone(), result) {
                Some(note) => format!("{result}\n\n{note}"),
                None => result.to_owned(),
            },
            Check::Skip(text) => format!("skip: {text}"),
            Check::Stop(text) => format!("stop: {text}"),
        }
    }

    #[test]
    fn the_second_time_is_marked_the_third_skipped_the_fourth_stops() {
        let mut r = Repeats::default();
        let a = read("a.rs");
        assert_eq!(call(&mut r, &a, "text"), "text");
        assert_eq!(
            call(&mut r, &a, "text"),
            "text\n\n[ognivo] Unchanged: this same call gave this same result before, and nothing has changed since."
        );
        let skipped = call(&mut r, &a, "text");
        assert!(
            skipped
                .starts_with("skip: [ognivo] Not run: this same call gave the same result twice")
                && skipped.ends_with("Its result is above: use it, or do something else."),
            "{skipped}"
        );
        assert!(call(&mut r, &a, "text").starts_with("stop: "));
    }

    #[test]
    fn something_new_in_between_skips_again_instead_of_stopping() {
        let mut r = Repeats::default();
        let (a, b) = (read("a"), read("b"));
        call(&mut r, &a, "1");
        call(&mut r, &a, "1");
        assert!(call(&mut r, &a, "1").starts_with("skip: "));
        call(&mut r, &b, "2");
        assert!(call(&mut r, &a, "1").starts_with("skip: "));
        // a cycle that brings nothing new stops
        assert!(call(&mut r, &b, "2").contains("Unchanged"));
        assert!(call(&mut r, &a, "1").starts_with("stop: "));
    }

    #[test]
    fn a_change_starts_over_and_is_never_a_repeat_itself() {
        let mut r = Repeats::default();
        let a = read("a");
        let touch = Fingerprint::of(
            Path::new("/p"),
            &Call::Shell {
                command: "touch x".into(),
                timeout: None,
            },
        );
        let change = |r: &mut Repeats| {
            assert_eq!(r.check(&touch), Check::Run);
            r.changed();
            r.record(touch.clone(), "[shell: exit 0, 0.0 s]")
        };
        call(&mut r, &a, "1");
        call(&mut r, &a, "1");
        assert_eq!(change(&mut r), None);
        assert_eq!(call(&mut r, &a, "1"), "1");
        for _ in 0..3 {
            assert_eq!(change(&mut r), None);
        }
    }

    #[test]
    fn an_edit_is_known_by_its_old_string() {
        // M1 stamp A', Gemma: one wrong old_string twice, with two different replacements
        let edit = |new: &str| {
            Fingerprint::of(
                Path::new("/p"),
                &Call::Edit {
                    path: "t.py".into(),
                    old: "x = 1".into(),
                    new: new.into(),
                    replace_all: false,
                },
            )
        };
        let mut r = Repeats::default();
        let error = "[error] t.py: old_string not found";
        call(&mut r, &edit("x = 2"), error);
        assert!(
            call(&mut r, &edit("x = 3"), error)
                .ends_with("an edit with this old_string gave this same result before, and nothing has changed since.")
        );
        assert!(call(&mut r, &edit("x = 4"), error).contains("or rewrite the file with write."));
    }

    #[test]
    fn another_result_or_directory_is_no_repeat() {
        let mut r = Repeats::default();
        let a = read("a");
        call(&mut r, &a, "1");
        assert_eq!(call(&mut r, &a, "2"), "2");
        assert_eq!(call(&mut r, &read_in("/p/sub", "a"), "2"), "2");
        let bad = Fingerprint::Unparsed {
            tool: "read".into(),
            arguments: "{".into(),
        };
        call(
            &mut r,
            &bad,
            "[error] read: arguments are not a JSON object",
        );
        call(
            &mut r,
            &bad,
            "[error] read: arguments are not a JSON object",
        );
        assert!(call(&mut r, &bad, "-").ends_with("Fix the arguments as its error above says."));
    }
}
