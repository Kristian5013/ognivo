//! grep and glob over the project, embedded (the machine has no system ripgrep). Both walk
//! with the `ignore` crate, so `.gitignore`, `.ignore` and hidden files are treated the
//! way ripgrep treats them, and both cap their output (docs/m0/baseline.md, req. 9).

use std::path::Path;
use std::time::SystemTime;

use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;

use crate::files::Target;

pub const GREP_MAX_MATCHES: usize = 200;
pub const GLOB_MAX_FILES: usize = 200;
/// Matched lines are cut to this many characters.
const GREP_MAX_LINE_CHARS: usize = 300;
/// Files above this size are skipped by grep (generated or data files).
const GREP_MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("searching / is not allowed; give a directory inside the project")]
    FilesystemRoot,
    #[error("{0}: no such file or directory")]
    NotFound(String),
    #[error("bad glob {pattern:?}: {message}")]
    BadGlob { pattern: String, message: String },
}

/// Searches `base` (a directory or one file) for `pattern`, a Rust regex, one line at a
/// time. An invalid regex is searched as literal text and the result says so. `glob`
/// filters the files of a directory by their path relative to it. Paths in the result are
/// named relative to `root` (see [`Target`]).
pub fn grep(
    base: &Path,
    root: &Path,
    pattern: &str,
    glob: Option<&str>,
) -> Result<String, SearchError> {
    check_base(base)?;
    let (re, note) = match Regex::new(pattern) {
        Ok(re) => (re, None),
        Err(e) => {
            let first = e
                .to_string()
                .lines()
                .last()
                .unwrap_or_default()
                .trim()
                .to_owned();
            let literal = Regex::new(&regex::escape(pattern)).expect("escaped regex compiles");
            (
                literal,
                Some(format!(
                    "[invalid regex ({first}); searched as literal text]"
                )),
            )
        }
    };
    // a single file is searched whatever the glob says: the model named it
    let filter = if base.is_file() {
        None
    } else {
        glob.map(matcher).transpose()?
    };
    let mut rows = Vec::new();
    let mut files = 0usize;
    let mut capped = false;
    'walk: for path in walk(base) {
        let rel = path.strip_prefix(base).unwrap_or(&path);
        if let Some(f) = &filter
            && !f.is_match(rel)
        {
            continue;
        }
        let Ok(meta) = path.metadata() else { continue };
        if meta.len() > GREP_MAX_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut hit = false;
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                if rows.len() == GREP_MAX_MATCHES {
                    capped = true;
                    break 'walk;
                }
                let mut shown: String = line.chars().take(GREP_MAX_LINE_CHARS).collect();
                if shown.len() < line.len() {
                    shown.push_str(" …");
                }
                rows.push(format!("{}:{}:{shown}", named(&path, root), i + 1));
                hit = true;
            }
        }
        files += usize::from(hit);
    }
    let mut out = format!(
        "[grep: {} matches in {files} files for /{pattern}/ under {}{}]",
        rows.len(),
        named(base, root),
        glob.map(|g| format!(", files {g}")).unwrap_or_default()
    );
    if pattern.contains('\n') || pattern.contains("\\n") {
        out.push_str(
            "\n[grep matches one line at a time; a pattern with a line break never matches]",
        );
    }
    if capped {
        out.push_str(&format!(
            "\n[first {GREP_MAX_MATCHES} matches shown; narrow the pattern, path or glob]"
        ));
    }
    if let Some(n) = note {
        out.push('\n');
        out.push_str(&n);
    }
    for r in rows {
        out.push('\n');
        out.push_str(&r);
    }
    Ok(out)
}

/// Lists files under `base` whose path relative to `base` matches `pattern`, newest first,
/// named relative to `root`.
pub fn glob(base: &Path, root: &Path, pattern: &str) -> Result<String, SearchError> {
    check_base(base)?;
    let m = matcher(pattern)?;
    let mut found: Vec<(SystemTime, String)> = walk(base)
        .filter(|p| m.is_match(p.strip_prefix(base).unwrap_or(p)))
        .map(|p| {
            let t = p
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (t, named(&p, root))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = found.len();
    let mut out = format!(
        "[glob: {total} files match {pattern} under {}]",
        named(base, root)
    );
    if total > GLOB_MAX_FILES {
        out.push_str(&format!(
            "\n[newest {GLOB_MAX_FILES} shown; narrow the pattern]"
        ));
    }
    for (_, p) in found.into_iter().take(GLOB_MAX_FILES) {
        out.push('\n');
        out.push_str(&p);
    }
    Ok(out)
}

fn check_base(base: &Path) -> Result<(), SearchError> {
    if base.parent().is_none() {
        return Err(SearchError::FilesystemRoot);
    }
    if !base.exists() {
        return Err(SearchError::NotFound(base.display().to_string()));
    }
    Ok(())
}

fn matcher(pattern: &str) -> Result<GlobMatcher, SearchError> {
    Glob::new(pattern)
        .map(|g| g.compile_matcher())
        .map_err(|e| SearchError::BadGlob {
            pattern: pattern.to_owned(),
            message: e.kind().to_string(),
        })
}

/// Files under `base` in a stable order, as ripgrep would see them.
fn walk(base: &Path) -> impl Iterator<Item = std::path::PathBuf> {
    WalkBuilder::new(base)
        .sort_by_file_name(|a, b| a.cmp(b))
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
}

fn named(path: &Path, root: &Path) -> String {
    Target::new(path.to_owned(), root)
        .shown
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        fs::create_dir_all(r.join("src/deep")).unwrap();
        fs::create_dir_all(r.join("target")).unwrap();
        fs::write(r.join(".gitignore"), "target/\n").unwrap();
        fs::write(r.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        fs::write(r.join("src/deep/b.rs"), "// alpha(x)\n").unwrap();
        fs::write(r.join("target/gen.rs"), "fn alpha() {}\n").unwrap();
        fs::write(r.join("notes.md"), "alpha\n").unwrap();
        d
    }

    #[test]
    fn grep_respects_gitignore_and_globs() {
        let d = tree();
        let out = grep(d.path(), d.path(), "alpha", Some("*.rs")).unwrap();
        assert!(out.starts_with("[grep: 2 matches in 2 files"), "{out}");
        assert!(out.contains("src/a.rs:1:fn alpha() {}"));
        assert!(out.contains("src/deep/b.rs:1:// alpha(x)"));
        assert!(!out.contains("target/"), "ignored dir searched: {out}");
        assert!(!out.contains("notes.md"));
    }

    #[test]
    fn invalid_regex_falls_back_to_literal() {
        let d = tree();
        let out = grep(d.path(), d.path(), "alpha(x", None).unwrap();
        assert!(out.contains("searched as literal text"), "{out}");
        assert!(out.contains("src/deep/b.rs:1:// alpha(x)"), "{out}");
    }

    #[test]
    fn grep_and_glob_refuse_the_filesystem_root() {
        assert!(matches!(
            grep(Path::new("/"), Path::new("/"), "x", None),
            Err(SearchError::FilesystemRoot)
        ));
        assert!(matches!(
            glob(Path::new("/"), Path::new("/"), "*"),
            Err(SearchError::FilesystemRoot)
        ));
    }

    #[test]
    fn glob_lists_matching_files() {
        let d = tree();
        let out = glob(d.path(), d.path(), "**/*.rs").unwrap();
        assert!(out.starts_with("[glob: 2 files match **/*.rs"), "{out}");
        assert!(out.contains("src/a.rs") && out.contains("src/deep/b.rs"));
        assert!(matches!(
            glob(d.path(), d.path(), "a["),
            Err(SearchError::BadGlob { .. })
        ));
    }

    /// M1 live run: a file path plus a glob found nothing; rows were relative to the base.
    #[test]
    fn a_file_base_ignores_the_glob_and_rows_name_paths_from_the_root() {
        let d = tree();
        let file = d.path().join("src/deep/b.rs");
        let out = grep(&file, d.path(), "alpha", Some("**/*.rs")).unwrap();
        assert!(
            out.starts_with("[grep: 1 matches in 1 files for /alpha/ under src/deep/b.rs"),
            "{out}"
        );
        assert!(out.ends_with("\nsrc/deep/b.rs:1:// alpha(x)"), "{out}");
        let sub = grep(&d.path().join("src"), d.path(), "beta", None).unwrap();
        assert!(sub.ends_with("\nsrc/a.rs:2:fn beta() {}"), "{sub}");
        let multi = grep(d.path(), d.path(), "}\n}", None).unwrap();
        assert!(multi.contains("one line at a time"), "{multi}");
    }
}
