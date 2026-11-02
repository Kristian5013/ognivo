//! The file tools: read, write and edit (docs/m0/baseline.md, requirements 9 and 11).
//!
//! Paths arrive already resolved ([`resolve`]); whether a path may be touched at all is
//! the approval policy's decision, made before any of these run.

use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use crate::diff::{self, Hunk};
use crate::edit::{self, EditError, EditOutcome};

/// Lines and bytes of file content one read returns at most.
pub const READ_MAX_LINES: usize = 2000;
pub const READ_MAX_BYTES: usize = 48 * 1024;
/// Longer lines are cut: minified files would otherwise blow the budget in one line.
const READ_MAX_LINE_CHARS: usize = 2000;
/// Lines of context shown around a successful edit.
const EDIT_CONTEXT: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{0}: no such file")]
    NotFound(PathBuf),
    #[error("{0} is a directory; use glob to list it")]
    IsDirectory(PathBuf),
    #[error("{0} looks binary (it contains NUL bytes); not shown")]
    Binary(PathBuf),
    #[error("{0} is not UTF-8 text; edit refuses to rewrite it")]
    NotUtf8(PathBuf),
    #[error("offset {offset} is past the end of {path} ({lines} lines)")]
    OffsetPastEnd {
        path: PathBuf,
        offset: usize,
        lines: usize,
    },
    #[error("{0} exists and was not read in this session; read it before overwriting it")]
    UnreadOverwrite(PathBuf),
    #[error("the content is apply_patch text, not file content; use edit for changes")]
    PatchText,
    #[error("{path}: {source}")]
    Edit {
        path: PathBuf,
        #[source]
        source: EditError,
        /// The fragment of the file most similar to the anchor, numbered.
        nearest: Option<String>,
    },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io(shown: &Path) -> impl FnOnce(std::io::Error) -> FileError + '_ {
    move |source| FileError::Io {
        path: shown.to_owned(),
        source,
    }
}

/// A file as the tools see it: where it is, and how results name it. Inside the project
/// the name is relative to the root, which keeps results short and is the form the model
/// writes paths in; outside it is absolute. Only names change, never file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub shown: PathBuf,
}

impl Target {
    pub fn new(path: PathBuf, root: &Path) -> Self {
        let shown = match path.strip_prefix(root) {
            Ok(rel) if rel.as_os_str().is_empty() => PathBuf::from("."),
            Ok(rel) => rel.to_owned(),
            Err(_) => path.clone(),
        };
        Self { path, shown }
    }
}

/// How line numbers are printed: `N│` for gpt-oss and Gemma, the right-aligned
/// `N<TAB>` of `cat -n` for Qwen, which knows that form from Claude Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStyle {
    Bar,
    Tab,
}

impl LineStyle {
    fn number(self, n: usize, width: usize) -> String {
        match self {
            LineStyle::Bar => format!("{n:>width$}│"),
            LineStyle::Tab => format!("{n:>6}\t"),
        }
    }
}

/// Resolves a path the model gave against `cwd`: absolute, `.` and `..` removed, symlinks
/// resolved for the longest existing prefix, so the policy sees where a write would land.
pub fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let joined = cwd.join(raw);
    let mut lexical = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut existing = lexical.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = fs::canonicalize(existing) {
            let mut out = real;
            out.extend(rest.iter().rev());
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

/// Reads one page of a text file with line numbers. The header comes first, so the model
/// learns the file's size before its content (M0: one 40K-token fixture doubled a context).
pub fn read(
    t: &Target,
    offset: Option<usize>,
    limit: Option<usize>,
    style: LineStyle,
) -> Result<String, FileError> {
    let bytes = read_bytes(t)?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Err(FileError::Binary(t.shown.clone()));
    }
    let utf8 = std::str::from_utf8(&bytes).is_ok();
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let first = offset.unwrap_or(1).max(1);
    if total > 0 && first > total {
        return Err(FileError::OffsetPastEnd {
            path: t.shown.clone(),
            offset: first,
            lines: total,
        });
    }
    let want = limit.unwrap_or(READ_MAX_LINES).clamp(1, READ_MAX_LINES);
    let width = (first + want).min(total.max(1)).to_string().len();
    let mut body = String::new();
    let mut last = first.saturating_sub(1);
    for (i, line) in lines.iter().enumerate().skip(first - 1).take(want) {
        let mut shown: String = line.chars().take(READ_MAX_LINE_CHARS).collect();
        if shown.len() < line.len() {
            shown.push_str(" … [line cut]");
        }
        let row = format!("{}{shown}\n", style.number(i + 1, width));
        if body.len() + row.len() > READ_MAX_BYTES && last >= first {
            break;
        }
        body.push_str(&row);
        last = i + 1;
    }
    let mut header = format!(
        "[read: {} — {} of {total} lines, {}, ~{} tokens]",
        t.shown.display(),
        if total == 0 {
            "0".to_owned()
        } else {
            format!("lines {first}-{last}")
        },
        human_bytes(bytes.len()),
        estimate_tokens(bytes.len()),
    );
    if last < total {
        header.push_str(&format!("\n[more: continue with offset={}]", last + 1));
    }
    if !utf8 {
        header.push_str("\n[not UTF-8: invalid bytes are shown as U+FFFD]");
    }
    Ok(format!("{header}\n{body}"))
}

/// A file change computed but not yet written. The approval policy looks at `path`,
/// `before` and `after` first; [`Change::commit`] then writes it atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// The current content; `None` when the file is created.
    pub before: Option<String>,
    pub after: String,
    /// The name results use ([`Target::shown`]).
    pub shown: PathBuf,
    /// What the model reads once the change is written.
    report: String,
}

impl Change {
    /// What the change does to the file, line by line.
    pub fn hunks(&self) -> Vec<Hunk> {
        diff::hunks(self.before.as_deref().unwrap_or_default(), &self.after)
    }

    pub fn commit(self) -> Result<String, FileError> {
        atomic_write(&self.path, &self.shown, self.after.as_bytes())?;
        Ok(self.report)
    }
}

/// The result of planning an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPlan {
    Change(Change),
    /// Nothing to write: an earlier edit already made this change.
    AlreadyApplied(String),
}

/// Plans writing a whole file (a temporary file in the same directory, then rename,
/// keeping the permissions of the file it replaces). `was_read` says whether this session
/// read the existing file: overwriting a file the model has not seen is refused.
pub fn plan_write(t: &Target, content: &str, was_read: bool) -> Result<Change, FileError> {
    if content.trim_start().starts_with("*** Begin Patch") {
        return Err(FileError::PatchText);
    }
    let before = match fs::metadata(&t.path) {
        Ok(m) if m.is_dir() => return Err(FileError::IsDirectory(t.shown.clone())),
        Ok(_) => Some(String::from_utf8_lossy(&read_bytes(t)?).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io(&t.shown)(e)),
    };
    if before.is_some() && !was_read {
        return Err(FileError::UnreadOverwrite(t.shown.clone()));
    }
    let report = format!(
        "[write: {} {} — {} lines, {}]",
        if before.is_some() {
            "overwrote"
        } else {
            "created"
        },
        t.shown.display(),
        content.lines().count(),
        human_bytes(content.len()),
    );
    Ok(Change {
        path: t.path.clone(),
        before,
        after: content.to_owned(),
        shown: t.shown.clone(),
        report,
    })
}

/// Plans one find-and-replace edit; once committed, the report shows the changed lines
/// with fresh numbers.
pub fn plan_edit(
    t: &Target,
    old: &str,
    new: &str,
    replace_all: bool,
    style: LineStyle,
) -> Result<EditPlan, FileError> {
    let bytes = read_bytes(t)?;
    let text = String::from_utf8(bytes).map_err(|_| FileError::NotUtf8(t.shown.clone()))?;
    match edit::apply(&text, old, new, replace_all) {
        Ok(EditOutcome::AlreadyApplied) => Ok(EditPlan::AlreadyApplied(format!(
            "[edit: already applied] {}: old_string is absent and new_string is present; nothing changed",
            t.shown.display()
        ))),
        Ok(EditOutcome::Applied {
            text: changed,
            kind,
            replacements,
            first_line,
            new_lines,
        }) => {
            let lines: Vec<&str> = changed.lines().collect();
            let from = first_line.saturating_sub(EDIT_CONTEXT);
            let to = (first_line + new_lines.max(1) + EDIT_CONTEXT).min(lines.len());
            let width = to.to_string().len();
            let snippet: String = (from..to)
                .map(|i| format!("{}{}\n", style.number(i + 1, width), lines[i]))
                .collect();
            let count = if replacements > 1 {
                format!(", {replacements} replacements")
            } else {
                String::new()
            };
            let report = format!(
                "[edit: applied {kind}{count}] {} — lines {}-{} now:\n{snippet}",
                t.shown.display(),
                from + 1,
                to
            );
            Ok(EditPlan::Change(Change {
                path: t.path.clone(),
                before: Some(text),
                after: changed,
                shown: t.shown.clone(),
                report,
            }))
        }
        Err(source) => {
            let nearest = match &source {
                EditError::NotFound {
                    nearest: Some((at, lines)),
                } => {
                    let width = (at + lines.len()).to_string().len();
                    Some(
                        lines
                            .iter()
                            .enumerate()
                            .map(|(i, l)| format!("{}{l}\n", style.number(at + i + 1, width)))
                            .collect(),
                    )
                }
                _ => None,
            };
            Err(FileError::Edit {
                path: t.shown.clone(),
                source,
                nearest,
            })
        }
    }
}

/// Writes back content a file had, the way changes are written: atomically, keeping the
/// permissions of the file it replaces. For the edit journal's revert.
pub fn restore(path: &Path, shown: &Path, bytes: &[u8]) -> Result<(), FileError> {
    atomic_write(path, shown, bytes)
}

fn read_bytes(t: &Target) -> Result<Vec<u8>, FileError> {
    match fs::metadata(&t.path) {
        Ok(m) if m.is_dir() => Err(FileError::IsDirectory(t.shown.clone())),
        Ok(_) => fs::read(&t.path).map_err(io(&t.shown)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(FileError::NotFound(t.shown.clone()))
        }
        Err(e) => Err(io(&t.shown)(e)),
    }
}

fn atomic_write(path: &Path, shown: &Path, bytes: &[u8]) -> Result<(), FileError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(io(shown))?;
    let perms = fs::metadata(path).ok().map(|m| m.permissions());
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io(shown))?;
    tmp.write_all(bytes).map_err(io(shown))?;
    tmp.as_file().sync_all().map_err(io(shown))?;
    if let Some(p) = perms {
        fs::set_permissions(tmp.path(), p).map_err(io(shown))?;
    }
    tmp.persist(path).map_err(|e| io(shown)(e.error))?;
    Ok(())
}

fn human_bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0))
    }
}

/// A rough size in model tokens (about 3.5 bytes per token for code): for orientation
/// only, never for budgets, which come from the server's own counts.
fn estimate_tokens(bytes: usize) -> usize {
    bytes * 2 / 7
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target named relative to its directory, as inside a project root.
    fn t(p: &Path) -> Target {
        Target::new(p.to_owned(), p.parent().unwrap())
    }

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn read_pages_with_a_header_first() {
        let d = tempfile::tempdir().unwrap();
        let text: String = (1..=5).map(|i| format!("line {i}\n")).collect();
        let p = file(d.path(), "a.txt", &text);
        let out = read(&t(&p), Some(2), Some(2), LineStyle::Bar).unwrap();
        let mut lines = out.lines();
        assert!(
            lines.next().unwrap().contains("lines 2-3 of 5 lines"),
            "{out}"
        );
        assert_eq!(lines.next(), Some("[more: continue with offset=4]"));
        assert_eq!(lines.next(), Some("2│line 2"));
        assert_eq!(lines.next(), Some("3│line 3"));
        let tab = read(&t(&p), None, Some(1), LineStyle::Tab).unwrap();
        assert!(tab.lines().nth(2) == Some("     1\tline 1"), "{tab}");
        assert!(matches!(
            read(&t(&p), Some(9), None, LineStyle::Bar),
            Err(FileError::OffsetPastEnd { .. })
        ));
        let bin = d.path().join("b.bin");
        fs::write(&bin, b"\x7fELF\0\0").unwrap();
        assert!(matches!(
            read(&t(&bin), None, None, LineStyle::Bar),
            Err(FileError::Binary(_))
        ));
        assert!(matches!(
            read(
                &Target::new(d.path().to_owned(), d.path()),
                None,
                None,
                LineStyle::Bar
            ),
            Err(FileError::IsDirectory(_))
        ));
    }

    #[test]
    fn read_stops_at_the_byte_cap() {
        let d = tempfile::tempdir().unwrap();
        let text = "x".repeat(1000) + "\n";
        let p = file(d.path(), "big.txt", &text.repeat(200));
        let out = read(&t(&p), None, None, LineStyle::Bar).unwrap();
        assert!(out.len() < READ_MAX_BYTES + 512, "{}", out.len());
        assert!(out.contains("[more: continue with offset="));
    }

    #[test]
    fn write_is_atomic_and_guarded() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/new.rs");
        let created = plan_write(&t(&p), "fn a() {}\n", false).unwrap();
        assert_eq!(created.before, None);
        assert!(!p.exists(), "planning writes nothing");
        assert!(created.commit().unwrap().contains("created"));
        assert!(matches!(
            plan_write(&t(&p), "x", false),
            Err(FileError::UnreadOverwrite(_))
        ));
        let over = plan_write(&t(&p), "x\n", true).unwrap();
        assert_eq!(over.before.as_deref(), Some("fn a() {}\n"));
        assert!(over.commit().unwrap().contains("overwrote"));
        assert_eq!(fs::read_to_string(&p).unwrap(), "x\n");
        assert!(matches!(
            plan_write(&t(&p), "*** Begin Patch\n", true),
            Err(FileError::PatchText)
        ));
    }

    #[test]
    fn edit_shows_the_changed_lines_or_the_nearest_fragment() {
        let d = tempfile::tempdir().unwrap();
        let p = file(d.path(), "m.rs", "a\nb\nc\nd\ne\nf\ng\n");
        let EditPlan::Change(c) = plan_edit(&t(&p), "d", "D", false, LineStyle::Bar).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "a\nb\nc\nd\ne\nf\ng\n",
            "planning writes nothing"
        );
        let out = c.commit().unwrap();
        assert!(out.starts_with("[edit: applied exact]"), "{out}");
        assert!(
            out.contains("4│D") && out.contains("1│a") && out.contains("7│g"),
            "{out}"
        );
        assert_eq!(fs::read_to_string(&p).unwrap(), "a\nb\nc\nD\ne\nf\ng\n");
        assert!(matches!(
            plan_edit(&t(&p), "x", "D", false, LineStyle::Bar),
            Ok(EditPlan::AlreadyApplied(_))
        ));
        match plan_edit(&t(&p), "c\nx", "y", false, LineStyle::Bar) {
            Err(FileError::Edit {
                nearest: Some(n), ..
            }) => assert!(n.starts_with("3│c"), "{n}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resolve_handles_dots_and_missing_tails() {
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        assert_eq!(
            resolve(&root, "src/../src/new/x.rs"),
            root.join("src/new/x.rs")
        );
        assert_eq!(
            resolve(&root.join("src"), "/etc/../tmp"),
            PathBuf::from("/tmp")
        );
        std::os::unix::fs::symlink("/usr", root.join("link")).unwrap();
        assert_eq!(
            resolve(&root, "link/nothing-here"),
            PathBuf::from("/usr/nothing-here")
        );
    }

    #[test]
    fn targets_are_named_from_the_root() {
        let root = Path::new("/home/u/proj");
        assert_eq!(
            Target::new(root.join("src/a.rs"), root).shown,
            PathBuf::from("src/a.rs")
        );
        assert_eq!(Target::new(root.to_owned(), root).shown, PathBuf::from("."));
        assert_eq!(
            Target::new("/etc/hosts".into(), root).shown,
            PathBuf::from("/etc/hosts")
        );
    }
}
