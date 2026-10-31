//! Find-and-replace edits with a tolerance cascade (docs/m0/baseline.md, requirement 11).
//!
//! M0 misses were mostly anchors the model recalled almost right: copied line-number
//! prefixes from `read` output, trailing whitespace, a different indentation. Each pass
//! below forgives one of those, and every pass applies only a unique match; nothing is
//! fuzzy in the content itself. Passes, in order:
//! 1. exact substring;
//! 2. exact after stripping a line-number prefix that every line of `old` carries;
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
        lines.iter().map(|l| (l + 1).to_string()).collect::<Vec<_>>().join(", ")
    )]
    Ambiguous { count: usize, lines: Vec<usize> },
    #[error("old_string matches {count} places when {kind} (lines {}); add surrounding lines to make it unique", lines.iter().map(|l| (l + 1).to_string()).collect::<Vec<_>>().join(", "))]
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
    if let Some(stripped_old) = strip_line_numbers(old) {
        let stripped_new = strip_line_numbers(new).unwrap_or_else(|| new.to_owned());
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

/// Strips `N│`, `N: `, `N→` or a right-aligned `N<TAB>` prefix when every non-empty line
/// carries one, as copied from line-numbered output. `None` when they do not all carry it.
fn strip_line_numbers(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut any = false;
    for line in s.split('\n') {
        if line.trim().is_empty() {
            out.push(line.to_owned());
            continue;
        }
        let digits_start = line.len() - line.trim_start().len();
        let rest = &line[digits_start..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let after = &rest[digits..];
        let body = ["│", ": ", "→", "\t"]
            .iter()
            .find_map(|sep| after.strip_prefix(sep))?;
        out.push(body.to_owned());
        any = true;
    }
    any.then(|| out.join("\n"))
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
        assert!(strip_line_numbers("let a = 1;\n2 + 3").is_none());
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
}
