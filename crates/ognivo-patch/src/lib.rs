//! The apply_patch format (V4A) of OpenAI's models: a patch read into its hunks, and the
//! text a hunk makes of a file (docs/research/2026-10-29/gpt-oss.md §I).
//!
//! The parser and the search for a hunk's lines in a file are OpenAI Codex's
//! (`codex-rs/apply-patch` at b127a3643, 2025-09-03, as it was when Codex served gpt-oss;
//! Apache-2.0, see LICENSE and NOTICE): the search tries exact lines, then without trailing
//! white space, then trimmed, then with typographic punctuation made ASCII. Reading and
//! writing files is the caller's, which keeps an edit journal; [`apply_chunks`] works on
//! text.
//!
//! One rule is ognivo's own: lines a hunk replaces without an `@@` line before them must
//! be there once in the rest of the file. Codex takes the first place they match, and with
//! too little context that is the wrong one: gpt-oss changed the first of two equal lines
//! (T2, edit-python-indent), where ognivo's edit asks for more context.

mod parser;
mod seek_sequence;

pub use parser::{Hunk, ParseError, UpdateFileChunk, parse_patch};

/// A patch read: its hunks, and its lines between and with the markers.
#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub hunks: Vec<Hunk>,
    pub patch: String,
}

/// Why a hunk does not apply to a file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("the context line {context:?} is not in the file")]
    Context { context: String },
    #[error("these lines are not in the file:\n{}", .lines.join("\n"))]
    Lines {
        lines: Vec<String>,
        /// The 0-based line the search began at.
        from: usize,
    },
    #[error(
        "these lines are in the file {} times (lines {}); add the lines around them, or an @@ line with the function they are in:\n{}",
        .at.len(),
        .at.iter().map(|i| (i + 1).to_string()).collect::<Vec<_>>().join(", "),
        .lines.join("\n")
    )]
    Ambiguous {
        lines: Vec<String>,
        /// The 0-based lines where they start.
        at: Vec<usize>,
    },
}

/// `original` with `chunks` applied, ending in a newline. Chunks apply in order, each
/// after the one before; a chunk without lines to replace appends at the end.
pub fn apply_chunks(original: &str, chunks: &[UpdateFileChunk]) -> Result<String, ApplyError> {
    let mut lines: Vec<String> = original.split('\n').map(str::to_owned).collect();
    // the empty piece after the last newline: line counts as `diff` has them
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let replacements = replacements(&lines, chunks)?;
    let mut lines = replace(lines, &replacements);
    if !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// Where each chunk goes: `(start, old_len, new_lines)`, from Codex's
/// `compute_replacements`.
fn replacements(
    lines: &[String],
    chunks: &[UpdateFileChunk],
) -> Result<Vec<(usize, usize, Vec<String>)>, ApplyError> {
    let mut out = Vec::new();
    let mut at = 0;
    for chunk in chunks {
        // a context line moves the search past it
        if let Some(context) = &chunk.change_context {
            match seek_sequence::seek_sequence(lines, std::slice::from_ref(context), at, false) {
                Some(i) => at = i + 1,
                None => {
                    return Err(ApplyError::Context {
                        context: context.clone(),
                    });
                }
            }
        }
        if chunk.old_lines.is_empty() {
            // a pure addition goes at the end, before a final empty line if there is one
            let end = if lines.last().is_some_and(String::is_empty) {
                lines.len() - 1
            } else {
                lines.len()
            };
            out.push((end, 0, chunk.new_lines.clone()));
            continue;
        }
        let mut old: &[String] = &chunk.old_lines;
        let mut new: &[String] = &chunk.new_lines;
        let mut found = seek_sequence::seek_sequence(lines, old, at, chunk.is_end_of_file);
        // a last empty line stands for the file's final newline, which `lines` lacks
        if found.is_none() && old.last().is_some_and(String::is_empty) {
            old = &old[..old.len() - 1];
            if new.last().is_some_and(String::is_empty) {
                new = &new[..new.len() - 1];
            }
            found = seek_sequence::seek_sequence(lines, old, at, chunk.is_end_of_file);
        }
        match found {
            Some(start) => {
                // without an @@ line, the place must be the only one
                if chunk.change_context.is_none() {
                    let mut places = vec![start];
                    let mut next = start + 1;
                    while let Some(i) =
                        seek_sequence::seek_sequence(lines, old, next, chunk.is_end_of_file)
                    {
                        if i < next || places.len() > 8 {
                            break;
                        }
                        places.push(i);
                        next = i + 1;
                    }
                    if places.len() > 1 {
                        return Err(ApplyError::Ambiguous {
                            lines: chunk.old_lines.clone(),
                            at: places,
                        });
                    }
                }
                out.push((start, old.len(), new.to_vec()));
                at = start + old.len();
            }
            None => {
                return Err(ApplyError::Lines {
                    lines: chunk.old_lines.clone(),
                    from: at,
                });
            }
        }
    }
    Ok(out)
}

/// The replacements applied from the last, so none moves another; Codex's
/// `apply_replacements`.
fn replace(mut lines: Vec<String>, replacements: &[(usize, usize, Vec<String>)]) -> Vec<String> {
    for (start, old_len, new) in replacements.iter().rev() {
        let end = (*start + *old_len).min(lines.len());
        lines.splice(*start..end, new.iter().cloned());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(patch: &str) -> Vec<UpdateFileChunk> {
        let text = format!("*** Begin Patch\n*** Update File: f.txt\n{patch}\n*** End Patch");
        match parse_patch(&text).unwrap().hunks.remove(0) {
            Hunk::UpdateFile { chunks, .. } => chunks,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_line_is_replaced() {
        let chunks = update("@@\n foo\n-bar\n+baz");
        assert_eq!(apply_chunks("foo\nbar\n", &chunks).unwrap(), "foo\nbaz\n");
    }

    #[test]
    fn chunks_apply_one_after_another_and_at_the_end() {
        // Codex's test_update_file_hunk_interleaved_changes
        let chunks = update("@@\n a\n-b\n+B\n@@\n c\n d\n-e\n+E\n@@\n f\n+g\n*** End of File");
        assert_eq!(
            apply_chunks("a\nb\nc\nd\ne\nf\n", &chunks).unwrap(),
            "a\nB\nc\nd\nE\nf\ng\n"
        );
    }

    #[test]
    fn a_context_line_finds_the_place() {
        let original = "fn one() {\n    let x = 1;\n}\nfn two() {\n    let x = 1;\n}\n";
        let chunks = update("@@ fn two() {\n-    let x = 1;\n+    let x = 2;");
        assert_eq!(
            apply_chunks(original, &chunks).unwrap(),
            "fn one() {\n    let x = 1;\n}\nfn two() {\n    let x = 2;\n}\n"
        );
    }

    #[test]
    fn lines_match_without_trailing_space_and_with_typographic_dashes() {
        // Codex's test_update_line_with_unicode_dash, and white space at a line's end
        let chunks = update(
            "@@\n-import asyncio  # local import - avoids top-level dep\n+import asyncio  # HELLO",
        );
        assert_eq!(
            apply_chunks(
                "import asyncio  # local import \u{2013} avoids top-level dep\n",
                &chunks
            )
            .unwrap(),
            "import asyncio  # HELLO\n"
        );
        let chunks = update("@@\n-let a = 1;\n+let a = 2;");
        assert_eq!(
            apply_chunks("let a = 1;   \n", &chunks).unwrap(),
            "let a = 2;\n"
        );
    }

    #[test]
    fn a_pure_addition_goes_at_the_end() {
        let chunks = update("@@\n+last");
        assert_eq!(apply_chunks("first\n", &chunks).unwrap(), "first\nlast\n");
        // a file without a final newline gets one
        let chunks = update("@@\n-only\n+changed");
        assert_eq!(apply_chunks("only", &chunks).unwrap(), "changed\n");
    }

    #[test]
    fn lines_that_are_there_twice_need_context() {
        let original = "fn one() {\n    let n = 1;\n}\nfn two() {\n    let n = 1;\n}\n";
        let chunks = update("@@\n-    let n = 1;\n+    let n = 2;");
        assert_eq!(
            apply_chunks(original, &chunks),
            Err(ApplyError::Ambiguous {
                lines: vec!["    let n = 1;".into()],
                at: vec![1, 4]
            })
        );
        // the lines around it, or the function it is in, tell them apart
        let chunks = update("@@\n fn two() {\n-    let n = 1;\n+    let n = 2;");
        assert!(
            apply_chunks(original, &chunks)
                .unwrap()
                .ends_with("    let n = 2;\n}\n")
        );
        let chunks = update("@@ fn two() {\n-    let n = 1;\n+    let n = 2;");
        assert!(
            apply_chunks(original, &chunks)
                .unwrap()
                .ends_with("    let n = 2;\n}\n")
        );
    }

    #[test]
    fn what_is_not_there_says_what_was_looked_for() {
        let chunks = update("@@\n-missing\n+x");
        assert_eq!(
            apply_chunks("a\nb\n", &chunks),
            Err(ApplyError::Lines {
                lines: vec!["missing".into()],
                from: 0
            })
        );
        let chunks = update("@@ fn absent()\n-a\n+b");
        assert_eq!(
            apply_chunks("a\n", &chunks),
            Err(ApplyError::Context {
                context: "fn absent()".into()
            })
        );
    }
}
