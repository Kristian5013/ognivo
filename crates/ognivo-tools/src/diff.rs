//! Line diffs of the files the agent writes, so the user sees what changed
//! (docs/architecture.md §10).

use std::time::Duration;

use serde::Serialize;
use similar::{ChangeTag, DiffTag, TextDiff};

/// Past this the diff gets coarser rather than slower: it only shows a change.
const TIMEOUT: Duration = Duration::from_millis(200);

/// One changed stretch of a file: `removed` lines from line `old_start` (1-based) of the
/// old text gave way to `added` lines from line `new_start` of the new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hunk {
    pub old_start: usize,
    pub new_start: usize,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

/// The changed stretches from `before` to `after`, without context lines; lines come
/// without their line ends.
pub fn hunks(before: &str, after: &str) -> Vec<Hunk> {
    let diff = TextDiff::configure()
        .timeout(TIMEOUT)
        .diff_lines(before, after);
    diff.grouped_ops(0)
        .iter()
        .filter_map(|group| {
            let first = group.iter().find(|op| op.tag() != DiffTag::Equal)?;
            let mut hunk = Hunk {
                old_start: first.old_range().start + 1,
                new_start: first.new_range().start + 1,
                removed: Vec::new(),
                added: Vec::new(),
            };
            for op in group {
                for change in diff.iter_changes(op) {
                    let line = change.value();
                    let line = line.strip_suffix('\n').unwrap_or(line);
                    let line = line.strip_suffix('\r').unwrap_or(line).to_owned();
                    match change.tag() {
                        ChangeTag::Delete => hunk.removed.push(line),
                        ChangeTag::Insert => hunk.added.push(line),
                        ChangeTag::Equal => {}
                    }
                }
            }
            Some(hunk)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(old_start: usize, new_start: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            old_start,
            new_start,
            removed: removed.iter().map(|s| (*s).to_owned()).collect(),
            added: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn changed_stretches_carry_their_own_line_numbers() {
        let before = "a\nb\nc\nd\ne\nf\n";
        let after = "a\nB\nc\nd\ne\ne2\nf\n";
        assert_eq!(
            hunks(before, after),
            [hunk(2, 2, &["b"], &["B"]), hunk(6, 6, &[], &["e2"])]
        );
        // a removed stretch shifts the numbers of the new text after it
        assert_eq!(
            hunks("a\nb\nc\nd\n", "a\nd\nD\n"),
            [hunk(2, 2, &["b", "c"], &[]), hunk(5, 3, &[], &["D"])]
        );
    }

    #[test]
    fn a_new_file_is_one_stretch_of_added_lines() {
        assert_eq!(hunks("", "x\ny"), [hunk(1, 1, &[], &["x", "y"])]);
        assert!(hunks("same\n", "same\n").is_empty());
        // line ends are not part of the lines, CRLF included
        assert_eq!(
            hunks("a\r\nb\r\n", "a\r\nc\r\n"),
            [hunk(2, 2, &["b"], &["c"])]
        );
    }
}
