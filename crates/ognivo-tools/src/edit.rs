//! Find-and-replace edits with a tolerance cascade (docs/m0/baseline.md, requirement 11).
//!
//! M0 misses were mostly anchors the model recalled almost right: copied line-number
//! prefixes from `read` output, trailing whitespace, a different indentation. Each pass
//! below forgives one of those, and every pass applies only a unique match; nothing is
//! fuzzy in the content itself. Passes, in order:
//! 1. exact substring;
//! 2. exact after stripping the line-number prefixes copied into `old`, and the same
//!    prefixes in `new`;
//! 3. line by line, ignoring trailing whitespace;
//! 4. line by line, ignoring indentation; the replacement is re-indented by the same
//!    shift.

use std::fmt;

/// Which pass found the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    LineNumbers,
    TrailingWhitespace,
    Indentation,
}

impl fmt::Display for MatchKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MatchKind::Exact => "exact",
            MatchKind::LineNumbers => "line numbers stripped",
            MatchKind::TrailingWhitespace => "trailing whitespace ignored",
            MatchKind::Indentation => "indentation ignored",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Applied {
        text: String,
        kind: MatchKind,
        replacements: usize,
        /// 0-based line where the first replacement starts in the new text.
        first_line: usize,
        /// Lines the first replacement occupies in the new text.
        new_lines: usize,
    },
    /// `old` is not in the file but `new` is: an earlier edit already did this.
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("old_string is empty; use write to create a file")]
    EmptyOld,
    #[error("old_string and new_string are identical")]
    NoChange,
    #[error(
        "old_string occurs {count} times (lines {}); add surrounding lines to make it unique or set replace_all",
        line_list(lines)
    )]
    Ambiguous { count: usize, lines: Vec<usize> },
    #[error(
        "old_string matches {count} places when {kind} (lines {}); add surrounding lines to make it unique",
        line_list(lines)
    )]
    AmbiguousTolerant {
        kind: MatchKind,
        count: usize,
        lines: Vec<usize>,
    },
    #[error("old_string not found")]
    NotFound {
        /// 0-based start and the lines of the most similar fragment, if any line matched.
        nearest: Option<(usize, Vec<String>)>,
    },
}

/// 1-based line numbers for a message, at most ten of them (M1 live run: an anchor of `}`
/// listed 131 lines).
fn line_list(lines: &[usize]) -> String {
    const SHOWN: usize = 10;
    let mut s: Vec<String> = lines
        .iter()
        .take(SHOWN)
        .map(|l| (l + 1).to_string())
        .collect();
    if lines.len() > SHOWN {
        s.push(format!("and {} more", lines.len() - SHOWN));
    }
    s.join(", ")
}

/// Applies one edit to `text`. `new` uses `\n` line ends; they follow the file's own
/// line ends in the result.
pub fn apply(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<EditOutcome, EditError> {
    if old.is_empty() {
        return Err(EditError::EmptyOld);
    }
    if old == new {
        return Err(EditError::NoChange);
    }
    let crlf = text.contains("\r\n");
    let fit = |s: &str| {
        if crlf {
            s.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            s.to_owned()
        }
    };
    let (old_f, new_f) = (fit(old), fit(new));

    // 1. exact
    if let Some(done) = exact(text, &old_f, &new_f, replace_all, MatchKind::Exact)? {
        return Ok(done);
    }
    // 2. line-number prefixes copied from read output
    if let Some(numbering) = Numbering::of(old) {
        let (stripped_old, stripped_new) = (numbering.strip(old), numbering.strip(new));
        if stripped_old != stripped_new
            && let Some(done) = exact(
                text,
                &fit(&stripped_old),
                &fit(&stripped_new),
                replace_all,
                MatchKind::LineNumbers,
            )?
        {
            return Ok(done);
        }
    }
    // 3-4. line-based passes on the file's lines
    let lines: Vec<&str> = text.split('\n').collect();
    let old_lines: Vec<&str> = old.trim_end_matches('\n').split('\n').collect();
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.trim_end_matches('\n').split('\n').collect()
    };
    for kind in [MatchKind::TrailingWhitespace, MatchKind::Indentation] {
        let norm = |s: &str| -> String {
            let s = s.trim_end_matches('\r');
            match kind {
                MatchKind::Indentation => s.trim().to_owned(),
                _ => s.trim_end().to_owned(),
            }
        };
        let want: Vec<String> = old_lines.iter().map(|l| norm(l)).collect();
        if want.iter().all(String::is_empty) {
            continue;
        }
        let hits: Vec<usize> = (0..lines.len().saturating_sub(want.len() - 1))
            .filter(|&i| (0..want.len()).all(|j| norm(lines[i + j]) == want[j]))
            .collect();
        match hits.as_slice() {
            [] => continue,
            [at] => {
                let replacement = if kind == MatchKind::Indentation {
                    reindent(&old_lines, &lines[*at..*at + want.len()], &new_lines)
                } else {
                    new_lines.iter().map(|l| (*l).to_owned()).collect()
                };
                let eol = if crlf { "\r" } else { "" };
                let mut out: Vec<String> = lines[..*at].iter().map(|l| (*l).to_owned()).collect();
                out.extend(replacement.iter().map(|l| format!("{l}{eol}")));
                let tail = &lines[*at + want.len()..];
                out.extend(tail.iter().map(|l| (*l).to_owned()));
                let mut text = out.join("\n");
                // a match that ended the file without a newline keeps that shape
                if crlf && tail.is_empty() {
                    text = text.trim_end_matches('\r').to_owned();
                }
                return Ok(EditOutcome::Applied {
                    text,
                    kind,
                    replacements: 1,
                    first_line: *at,
                    new_lines: replacement.len(),
                });
            }
            many => {
                return Err(EditError::AmbiguousTolerant {
                    kind,
                    count: many.len(),
                    lines: many.to_vec(),
                });
            }
        }
    }
    if !new.trim().is_empty() && text.contains(&new_f) {
        return Ok(EditOutcome::AlreadyApplied);
    }
    Err(EditError::NotFound {
        nearest: nearest(&lines, &old_lines),
    })
}

fn exact(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
    kind: MatchKind,
) -> Result<Option<EditOutcome>, EditError> {
    let starts: Vec<usize> = text.match_indices(old).map(|(i, _)| i).collect();
    let line_of = |byte: usize| text[..byte].matches('\n').count();
    match starts.len() {
        0 => Ok(None),
        1 => {
            let at = starts[0];
            let out = format!("{}{new}{}", &text[..at], &text[at + old.len()..]);
            Ok(Some(EditOutcome::Applied {
                text: out,
                kind,
                replacements: 1,
                first_line: line_of(at),
                new_lines: new.matches('\n').count() + 1,
            }))
        }
        n if replace_all => Ok(Some(EditOutcome::Applied {
            text: text.replace(old, new),
            kind,
            replacements: n,
            first_line: line_of(starts[0]),
            new_lines: new.matches('\n').count() + 1,
        })),
        n => Err(EditError::Ambiguous {
            count: n,
            lines: starts.into_iter().map(line_of).collect(),
        }),
    }
}

/// Line-number prefixes copied from line-numbered output (`read`, `cat -n`, `grep -n`):
/// `N│`, `N: `, `N→` or `N<TAB>`, one separator, the numbers left-aligned or right-aligned
/// to one column. Models copy them into some lines and not others: a copy that starts
/// after the first number (M1 stamp A', gpt-oss), lines they add to `new` without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Numbering {
    sep: &'static str,
    /// No number is padded.
    left: bool,
    /// Where every separator starts, when they all start in one column.
    column: Option<usize>,
}

impl Numbering {
    /// The numbering of the lines of `old` that carry a prefix; `None` when no line does or
    /// the prefixes disagree.
    fn of(old: &str) -> Option<Self> {
        let mut prefixes = old.split('\n').filter_map(prefix).map(|(p, _)| p);
        let first = prefixes.next()?;
        let mut n = Numbering {
            sep: first.sep,
            left: first.pad == 0,
            column: Some(first.column),
        };
        for p in prefixes {
            if p.sep != n.sep {
                return None;
            }
            n.left &= p.pad == 0;
            if n.column != Some(p.column) {
                n.column = None;
            }
        }
        (n.left || n.column.is_some()).then_some(n)
    }

    /// `s` without the prefixes of this numbering; other lines stay as they are.
    fn strip(self, s: &str) -> String {
        s.split('\n')
            .map(|line| match prefix(line) {
                Some((p, body)) if self.fits(p) => body,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn fits(self, p: Prefix) -> bool {
        p.sep == self.sep && ((self.left && p.pad == 0) || self.column == Some(p.column))
    }
}

/// A number at the start of a line and the separator after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Prefix {
    /// Spaces before the number: right-aligned numbers have them.
    pad: usize,
    /// Where the separator starts.
    column: usize,
    sep: &'static str,
}

/// The prefix of `line` and the text after it.
fn prefix(line: &str) -> Option<(Prefix, &str)> {
    let rest = line.trim_start_matches(' ');
    let pad = line.len() - rest.len();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    ["│", ": ", "→", "\t"].into_iter().find_map(|sep| {
        rest[digits..].strip_prefix(sep).map(|body| {
            let column = pad + digits;
            (Prefix { pad, column, sep }, body)
        })
    })
}

/// Shifts `new` by the indentation difference between `old`'s first non-blank line and
/// the matched file line.
fn reindent(old: &[&str], found: &[&str], new: &[&str]) -> Vec<String> {
    let indent = |s: &str| s.len() - s.trim_start().len();
    let Some(k) = old.iter().position(|l| !l.trim().is_empty()) else {
        return new.iter().map(|l| (*l).to_owned()).collect();
    };
    let have = &found[k][..indent(found[k])];
    let had = indent(old[k]);
    new.iter()
        .map(|l| {
            if l.trim().is_empty() {
                return String::new();
            }
            let own = indent(l);
            let keep = &l[own.min(had)..];
            if own >= had {
                format!("{have}{keep}")
            } else {
                // a line less indented than the anchor keeps its own relative position
                format!(
                    "{}{}",
                    &have[..have.len().saturating_sub(had - own)],
                    l.trim_start()
                )
            }
        })
        .collect()
}

/// The window of the file most similar to `old`, by count of equal trimmed lines.
fn nearest(lines: &[&str], old: &[&str]) -> Option<(usize, Vec<String>)> {
    let want: Vec<&str> = old.iter().map(|l| l.trim()).collect();
    let n = want.len();
    let mut best = (0usize, 0usize);
    for i in 0..lines.len() {
        let score = (0..n)
            .filter(|&j| i + j < lines.len() && !want[j].is_empty())
            .filter(|&j| lines[i + j].trim() == want[j])
            .count();
        if score > best.1 {
            best = (i, score);
        }
    }
    (best.1 > 0).then(|| {
        let end = (best.0 + n).min(lines.len());
        (
            best.0,
            lines[best.0..end]
                .iter()
                .map(|l| l.trim_end_matches('\r').to_owned())
                .collect(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied(r: Result<EditOutcome, EditError>) -> (String, MatchKind) {
        match r {
            Ok(EditOutcome::Applied { text, kind, .. }) => (text, kind),
            other => panic!("{other:?}"),
        }
    }

    const SRC: &str = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";

    #[test]
    fn exact_and_its_errors() {
        let (t, k) = applied(apply(SRC, "let x = 1;", "let x = 2;", false));
        assert_eq!(
            (t.as_str(), k),
            (
                "fn main() {\n    let x = 2;\n    println!(\"{x}\");\n}\n",
                MatchKind::Exact
            )
        );
        assert_eq!(apply(SRC, "", "a", false), Err(EditError::EmptyOld));
        assert_eq!(apply(SRC, "x", "x", false), Err(EditError::NoChange));
        assert!(
            matches!(apply("a\na\n", "a", "b", false), Err(EditError::Ambiguous { count: 2, lines }) if lines == [0, 1])
        );
        assert_eq!(applied(apply("a\na\n", "a", "b", true)).0, "b\nb\n");
    }

    #[test]
    fn copied_line_numbers_are_stripped() {
        for prefix in ["2│", "     2\t", "2: ", "2→"] {
            let old = format!("{prefix}    let x = 1;");
            let (t, k) = applied(apply(SRC, &old, "    let x = 3;", false));
            assert!(t.contains("    let x = 3;\n"), "{prefix}: {t}");
            assert_eq!(k, MatchKind::LineNumbers);
        }
        // a code line that merely starts with a number is not a prefix
        assert!(Numbering::of("let a = 1;\n2 + 3").is_none());
        // nor are numbers with two separators, or neither left-aligned nor in one column
        assert!(Numbering::of("1│a\n2: b").is_none());
        assert!(Numbering::of(" 9│a\n100│b").is_none());
    }

    #[test]
    fn numbers_copied_into_some_lines_are_stripped_there() {
        // M1 stamp A', gpt-oss, flowcoin-ct-memeq-multibit: the copy starts after the
        // first number, the next one is wrong, and the added line has none
        let src =
            "{\n    {.name = \"a\", .run = test_a},\n    {.name = \"b\", .run = test_b},\n}\n";
        let old = "    {.name = \"a\", .run = test_a},\n260│    {.name = \"b\", .run = test_b},\n";
        let new = "    {.name = \"a\", .run = test_a},\n    {.name = \"c\", .run = test_c},\n260│    {.name = \"b\", .run = test_b},\n";
        let (t, k) = applied(apply(src, old, new, false));
        assert_eq!(k, MatchKind::LineNumbers);
        assert_eq!(
            t,
            "{\n    {.name = \"a\", .run = test_a},\n    {.name = \"c\", .run = test_c},\n    {.name = \"b\", .run = test_b},\n}\n"
        );
        // right-aligned numbers; text in new that only looks numbered stays
        let src = "codes = {\n    200: \"OK\",\n}\n";
        let old = " 8│codes = {\n 9│    200: \"OK\",\n10│}\n";
        let new = " 8│codes = {\n 9│    200: \"OK\",\n    404: \"Not Found\",\n10│}\n";
        let (t, k) = applied(apply(src, old, new, false));
        assert_eq!(k, MatchKind::LineNumbers);
        assert_eq!(
            t,
            "codes = {\n    200: \"OK\",\n    404: \"Not Found\",\n}\n"
        );
    }

    #[test]
    fn trailing_whitespace_and_indentation() {
        let src = "a {\n    b();   \n    c();\n}\n";
        let (t, k) = applied(apply(src, "    b();\n    c();", "    d();", false));
        assert_eq!(
            (t.as_str(), k),
            ("a {\n    d();\n}\n", MatchKind::TrailingWhitespace)
        );
        // anchor recalled with 2 spaces instead of 4: the replacement moves by +2
        let (t, k) = applied(apply(
            SRC,
            "  let x = 1;\n  println!(\"{x}\");",
            "  let y = 1;\n  if y > 0 {\n    dbg!(y);\n  }",
            false,
        ));
        assert_eq!(k, MatchKind::Indentation);
        assert_eq!(
            t,
            "fn main() {\n    let y = 1;\n    if y > 0 {\n      dbg!(y);\n    }\n}\n"
        );
    }

    #[test]
    fn tolerant_passes_need_a_unique_match() {
        let src = "  x();\n    x();\n";
        assert!(matches!(
            apply(src, "x();\n", "y();", false),
            Err(EditError::Ambiguous { .. })
        ));
        assert!(matches!(
            apply(src, " x(); ", "y();", false),
            Err(EditError::AmbiguousTolerant {
                kind: MatchKind::Indentation,
                count: 2,
                ..
            })
        ));
    }

    #[test]
    fn already_applied_and_nearest_fragment() {
        assert_eq!(
            apply(SRC, "let x = 9;", "let x = 1;", false),
            Ok(EditOutcome::AlreadyApplied)
        );
        match apply(SRC, "    let x = 1;\n    print!(\"{x}\");", "z", false) {
            Err(EditError::NotFound {
                nearest: Some((at, lines)),
            }) => {
                assert_eq!(at, 1);
                assert_eq!(lines[0], "    let x = 1;");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn crlf_files_keep_their_line_ends() {
        let src = "a\r\nb\r\nc\r\n";
        assert_eq!(
            applied(apply(src, "b", "x\ny", false)).0,
            "a\r\nx\r\ny\r\nc\r\n"
        );
        assert_eq!(applied(apply(src, "b  \nc", "z", false)).0, "a\r\nz\r\n");
    }

    #[test]
    fn long_ambiguity_lists_are_cut() {
        let src = "}\n".repeat(131);
        let msg = apply(&src, "}", "x", false).unwrap_err().to_string();
        assert!(
            msg.starts_with(
                "old_string occurs 131 times (lines 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, and 121 more)"
            ),
            "{msg}"
        );
    }
}
