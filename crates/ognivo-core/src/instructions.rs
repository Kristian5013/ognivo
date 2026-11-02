//! Instructions from the user and the project: AGENTS.md and CLAUDE.md files, read the way
//! Codex and Claude Code read them, so a project set up for either works as it is
//! (docs/architecture.md §9, decision 18).
//!
//! - A conversation starts with the user's own files ([`UserInstructions`]), then those of
//!   every directory from the top of the filesystem down to the project root: AGENTS.md,
//!   CLAUDE.md, .claude/CLAUDE.md and CLAUDE.local.md, in that order. They go to the system
//!   prompt general first, and the model is told that the later file wins.
//! - The files of a directory below the root come with the result of the first call that
//!   reads or changes a file under it, and apply to that subtree: the nearest wins.
//! - `@path` imports a file, as in Claude Code: relative to the file that names it, `~/` or
//!   absolute; not inside code or HTML comments; at most five levels deep. An import comes
//!   right after the file that names it.
//! - A file of the project may import or link only to files of the project (its git work
//!   tree, or the root outside git): a cloned repository could otherwise put `~/.ssh` into
//!   the prompt. Reads outside the project ask for the same reason (§8). The user's files,
//!   and those in the directories above the project, may point anywhere, so they count
//!   only when the user or root owns them and the link to them: anyone may write to
//!   `/tmp`.
//! - A conversation gets each file once. A link, an import of a file it already has, a copy
//!   of one whose scope covers it, or a file of nothing but imports adds nothing.
//! - One load takes at most 32 KiB, Codex's default: the files nearest the work keep their
//!   room, and a file cut short says so.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs;
use std::io::{ErrorKind, Read as _};
use std::ops::Range;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The files a directory may hold, in the order they are given.
const NAMES: [&str; 4] = [
    "AGENTS.md",
    "CLAUDE.md",
    ".claude/CLAUDE.md",
    "CLAUDE.local.md",
];
/// Bytes of instructions in one load.
pub const BUDGET: usize = 32 * 1024;
/// Imports within imports, as in Claude Code.
const MAX_DEPTH: usize = 5;
/// Bytes read of one file; more never fits the budget anyway.
const MAX_READ: u64 = 1 << 20;

/// The user's own instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserInstructions {
    /// For `~/` in imports and in the names shown.
    pub home: Option<PathBuf>,
    /// General first. The user wrote them, so their imports may point anywhere.
    pub files: Vec<PathBuf>,
}

impl UserInstructions {
    /// Claude Code's `~/.claude/CLAUDE.md` (in `claude_dir`, Claude Code's
    /// `CLAUDE_CONFIG_DIR`, when that is set), then ognivo's own `~/.ognivo/AGENTS.md`.
    pub fn at(home: PathBuf, claude_dir: Option<PathBuf>) -> Self {
        let claude = claude_dir.unwrap_or_else(|| home.join(".claude"));
        Self {
            files: vec![claude.join("CLAUDE.md"), home.join(".ognivo/AGENTS.md")],
            home: Some(home),
        }
    }
}

/// A file the model got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Given {
    /// Relative to the project root inside it, `~/…` under the home directory, absolute
    /// elsewhere.
    pub path: String,
    /// Bytes of it the model got.
    pub bytes: usize,
    /// It did not fit whole.
    pub cut: bool,
}

/// A file the model did not get, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skipped {
    pub path: String,
    pub why: String,
}

/// What one load gives the model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Load {
    /// The text for the model; empty when no file had any.
    pub text: String,
    pub files: Vec<Given>,
    pub skipped: Vec<Skipped>,
}

/// Finds the instruction files and remembers what the conversation has of them.
#[derive(Debug)]
pub struct Instructions {
    root: PathBuf,
    user: UserInstructions,
    /// The files of the project, and their imports, must stay inside this: the git work
    /// tree of the root, or the root itself.
    project: PathBuf,
    given: Vec<Seen>,
    /// Directories below the root already looked into.
    looked: HashSet<PathBuf>,
}

/// A file the conversation has.
#[derive(Debug)]
struct Seen {
    real: PathBuf,
    text: String,
    /// Where it applies.
    scope: PathBuf,
}

/// Where a file's instructions apply, and what the file may point to.
#[derive(Debug, Clone)]
struct Place {
    scope: PathBuf,
    /// Higher is nearer the work: it keeps its room first.
    rank: usize,
    /// For a file of the project: the directory it and its imports must stay in.
    bound: Option<PathBuf>,
}

#[derive(Debug)]
struct Candidate {
    shown: String,
    text: String,
    /// The file's size on disk.
    size: u64,
    scope: PathBuf,
    rank: usize,
}

#[derive(Debug, Default)]
struct Found {
    files: Vec<Candidate>,
    skipped: Vec<Skipped>,
}

/// An `@path` in a text.
#[derive(Debug, PartialEq, Eq)]
struct Mention {
    at: Range<usize>,
    path: String,
}

impl Instructions {
    /// `root` must be canonical.
    pub fn new(root: PathBuf, user: UserInstructions) -> Self {
        let project = root
            .ancestors()
            .find(|d| d.join(".git").exists())
            .unwrap_or(&root)
            .to_path_buf();
        Self {
            root,
            user,
            project,
            given: Vec::new(),
            looked: HashSet::new(),
        }
    }

    /// The files of a new conversation: the user's, then those from the top of the
    /// filesystem down to the root. What earlier conversations had is forgotten.
    pub fn start(&mut self) -> Load {
        self.given.clear();
        self.looked.clear();
        let mut found = Found::default();
        let user = Place {
            scope: self.root.clone(),
            rank: 0,
            bound: None,
        };
        for file in self.user.files.clone() {
            self.file(&file, None, &user, 0, &mut found);
        }
        let dirs: Vec<PathBuf> = self
            .root
            .ancestors()
            .filter(|d| d.parent().is_some())
            .map(Path::to_path_buf)
            .collect();
        for (rank, dir) in dirs.into_iter().rev().enumerate() {
            let place = Place {
                scope: self.root.clone(),
                rank: rank + 1,
                bound: dir.starts_with(&self.project).then(|| self.project.clone()),
            };
            self.directory(&dir, &place, &mut found);
        }
        found.load(&self.root, false)
    }

    /// The files of the directories between the root and `path` (resolved the way the
    /// tool opens it) that the conversation has not looked into yet.
    pub fn near(&mut self, path: &Path) -> Load {
        let Some(dir) = path.parent().filter(|d| d.starts_with(&self.root)) else {
            return Load::default();
        };
        let dirs: Vec<PathBuf> = dir
            .ancestors()
            .take_while(|d| *d != self.root.as_path())
            .map(Path::to_path_buf)
            .collect();
        let mut found = Found::default();
        for (rank, dir) in dirs.into_iter().rev().enumerate() {
            if !dir.is_dir() || !self.looked.insert(dir.clone()) {
                continue;
            }
            let place = Place {
                scope: dir.clone(),
                rank,
                bound: Some(self.project.clone()),
            };
            self.directory(&dir, &place, &mut found);
        }
        found.load(&self.root, true)
    }

    fn directory(&mut self, dir: &Path, place: &Place, found: &mut Found) {
        for name in NAMES {
            // ~/.claude is Claude Code's own directory: its CLAUDE.md is a user file
            if name == ".claude/CLAUDE.md" && self.user.home.as_deref() == Some(dir) {
                continue;
            }
            self.file(&dir.join(name), None, place, 0, found);
        }
    }

    /// Takes in one file and what it imports; `by` names the file that imports it.
    fn file(
        &mut self,
        path: &Path,
        by: Option<&str>,
        place: &Place,
        depth: usize,
        found: &mut Found,
    ) {
        let meta = match fs::metadata(path) {
            Ok(m) if m.is_file() => m,
            Ok(_) => return,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                return;
            }
            Err(e) => return found.skip(self.shown(path), &e.to_string()),
        };
        let Ok(real) = fs::canonicalize(path) else {
            return;
        };
        let shown = self.shown(if by.is_some() { &real } else { path });
        if place.bound.is_none() && !owned(path, &meta) {
            return found.skip(shown, "owned by another user");
        }
        if place.bound.as_ref().is_some_and(|b| !real.starts_with(b)) {
            let why = match by {
                Some(by) => format!("imported by {by} from outside the project"),
                None => "a link to outside the project".to_owned(),
            };
            return found.skip(shown, &why);
        }
        if self
            .given
            .iter()
            .any(|g| g.real == real && place.scope.starts_with(&g.scope))
        {
            return;
        }
        if depth > MAX_DEPTH {
            return found.skip(
                shown,
                &format!("imported more than {MAX_DEPTH} levels deep"),
            );
        }
        let mut bytes = Vec::new();
        if let Err(e) = fs::File::open(path).and_then(|f| f.take(MAX_READ).read_to_end(&mut bytes))
        {
            return found.skip(shown, &e.to_string());
        }
        if bytes.contains(&0) {
            return found.skip(shown, "not a text file");
        }
        let text =
            String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes[..]))
                .trim_end()
                .to_owned();
        let dir = path.parent().unwrap_or(Path::new("/"));
        let imports: Vec<(Range<usize>, PathBuf)> = mentions(&text)
            .into_iter()
            .filter_map(|m| Some((m.at, self.target(&m.path, dir)?)))
            .collect();
        let copy = self
            .given
            .iter()
            .any(|g| g.text == text && place.scope.starts_with(&g.scope));
        let own = says_more(&text, &imports);
        self.given.push(Seen {
            real,
            text: text.clone(),
            scope: place.scope.clone(),
        });
        if own && !copy {
            found.files.push(Candidate {
                shown: shown.clone(),
                text,
                size: meta.len(),
                scope: place.scope.clone(),
                rank: place.rank,
            });
        }
        for (_, target) in imports {
            self.file(&target, Some(&shown), place, depth + 1, found);
        }
    }

    /// The file an `@` names, if there is one: as written, or without the punctuation of
    /// the sentence around it.
    fn target(&self, raw: &str, dir: &Path) -> Option<PathBuf> {
        let bare = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
        [raw, bare]
            .into_iter()
            .filter(|p| !p.is_empty())
            .find_map(|p| {
                let path = match p.strip_prefix("~/") {
                    Some(rest) => self.user.home.as_ref()?.join(rest),
                    None => dir.join(p),
                };
                path.is_file().then_some(path)
            })
    }

    fn shown(&self, path: &Path) -> String {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return rel.display().to_string();
        }
        match self.user.home.as_deref().map(|h| path.strip_prefix(h)) {
            Some(Ok(rel)) => format!("~/{}", rel.display()),
            _ => path.display().to_string(),
        }
    }
}

impl Found {
    fn skip(&mut self, path: String, why: &str) {
        let s = Skipped {
            path,
            why: why.to_owned(),
        };
        if !self.skipped.contains(&s) {
            self.skipped.push(s);
        }
    }

    /// The files that fit, the nearest first, written the most general first.
    fn load(self, root: &Path, near: bool) -> Load {
        let mut order: Vec<usize> = (0..self.files.len()).collect();
        order.sort_by_key(|&i| (Reverse(self.files[i].rank), i));
        let mut room = vec![0; self.files.len()];
        let mut left = BUDGET;
        for i in order {
            room[i] = self.files[i].text.len().min(left);
            left -= room[i];
        }
        let mut load = Load {
            skipped: self.skipped,
            ..Load::default()
        };
        let mut blocks = Vec::new();
        let mut scopes: Vec<&Path> = Vec::new();
        for (c, room) in self.files.iter().zip(room) {
            let text = fit(&c.text, room);
            if text.is_empty() {
                load.skipped.push(Skipped {
                    path: c.shown.clone(),
                    why: format!("no room: instructions take at most {} KiB", BUDGET / 1024),
                });
                continue;
            }
            let cut = text.len() < c.text.len();
            let mut block = format!("<instructions file=\"{}\">\n{text}\n", c.shown);
            if cut {
                let more = c.size.saturating_sub(text.len() as u64);
                block.push_str(&format!(
                    "[ognivo] Cut here: {} more did not fit. Read the file for the rest.\n",
                    size(more)
                ));
            }
            block.push_str("</instructions>");
            blocks.push(block);
            if !scopes.contains(&c.scope.as_path()) {
                scopes.push(&c.scope);
            }
            load.files.push(Given {
                path: c.shown.clone(),
                bytes: text.len(),
                cut,
            });
        }
        if blocks.is_empty() {
            return load;
        }
        let intro = if near {
            let dirs: Vec<String> = scopes
                .iter()
                .map(|&s| format!("{}/", s.strip_prefix(root).unwrap_or(s).display()))
                .collect();
            let mut intro = format!(
                "[ognivo] Instructions for the files under {} follow; where they disagree with the earlier ones, they win",
                and_list(&dirs)
            );
            if blocks.len() > 1 {
                intro.push_str(", and the later file wins over the earlier one");
            }
            intro + "."
        } else {
            "Instructions from the user and the project are in the files below; follow them. Where two disagree, the later file wins: it is closer to the work. The user's own messages win over all of them.".to_owned()
        };
        load.text = format!("{intro}\n\n{}", blocks.join("\n\n"));
        load
    }
}

/// Whether the user or root owns a file, and the link to it if it is one.
fn owned(path: &Path, meta: &fs::Metadata) -> bool {
    let me = rustix::process::getuid().as_raw();
    let ok = |uid: u32| uid == me || uid == 0;
    ok(meta.uid()) && fs::symlink_metadata(path).is_ok_and(|link| ok(link.uid()))
}

/// The `@path` mentions of a text that may be imports, found the way Claude Code finds
/// them: an `@` at the start of a line or after whitespace, up to the next whitespace
/// (`\ ` is a space), outside fenced code, code spans and HTML comments. Indented code
/// blocks are not told apart from text.
fn mentions(text: &str) -> Vec<Mention> {
    let mut out = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut comment = false;
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let at = start;
        start += line.len();
        let line = line.trim_end_matches(['\n', '\r']);
        let body = line.trim_start_matches(' ');
        let indent = line.len() - body.len();
        if let Some((mark, len)) = fence {
            let run = run_of(body.as_bytes(), mark);
            if indent < 4 && run >= len && body[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if !comment && indent < 4 {
            fence = b"`~".iter().find_map(|&mark| {
                let run = run_of(body.as_bytes(), mark);
                // the info string of a backtick fence has no backtick
                (run >= 3 && !(mark == b'`' && body[run..].contains('`'))).then_some((mark, run))
            });
            if fence.is_some() {
                continue;
            }
        }
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if comment {
                match line[i..].find("-->") {
                    Some(end) => {
                        comment = false;
                        i += end + 3;
                    }
                    None => i = b.len(),
                }
                continue;
            }
            match b[i] {
                b'<' if line[i..].starts_with("<!--") => {
                    comment = true;
                    i += 4;
                }
                b'`' => {
                    let run = run_of(&b[i..], b'`');
                    // a code span ends at the next run of as many backticks
                    i += run + closing(&b[i + run..], run).unwrap_or(0);
                }
                b'@' if line[..i]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace) =>
                {
                    let (path, len) = path_at(&line[i + 1..]);
                    if !path.is_empty() {
                        out.push(Mention {
                            at: at + i..at + i + 1 + len,
                            path,
                        });
                    }
                    i += 1 + len;
                }
                // the bytes of a multibyte character are never one of the above
                _ => i += 1,
            }
        }
    }
    out
}

fn run_of(b: &[u8], mark: u8) -> usize {
    b.iter().take_while(|&&c| c == mark).count()
}

/// Where a code span opened by `run` backticks ends: just after the next run of exactly
/// as many.
fn closing(b: &[u8], run: usize) -> Option<usize> {
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'`' {
            let n = run_of(&b[i..], b'`');
            if n == run {
                return Some(i + n);
            }
            i += n;
        } else {
            i += 1;
        }
    }
    None
}

/// The path after an `@` and its length in the text.
fn path_at(s: &str) -> (String, usize) {
    let mut path = String::new();
    let mut chars = s.char_indices();
    let mut end = 0;
    while let Some((i, c)) = chars.next() {
        if c == '\\' && s[i + 1..].starts_with(' ') {
            path.push(' ');
            chars.next();
            end = i + 2;
            continue;
        }
        if c.is_whitespace() {
            break;
        }
        path.push(c);
        end = i + c.len_utf8();
    }
    (path, end)
}

/// Whether a text says anything besides its imports.
fn says_more(text: &str, imports: &[(Range<usize>, PathBuf)]) -> bool {
    let mut at = 0;
    for (r, _) in imports {
        if !text[at..r.start].trim().is_empty() {
            return true;
        }
        at = r.end;
    }
    !text[at..].trim().is_empty()
}

/// The start of `text` that fits in `room` bytes, ending at a line end when one is near.
fn fit(text: &str, room: usize) -> &str {
    if text.len() <= room {
        return text;
    }
    let mut end = room;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match text[..end].rfind('\n') {
        Some(nl) if nl >= end / 2 => text[..nl].trim_end(),
        _ => &text[..end],
    }
}

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{} KiB", bytes.div_ceil(1024))
    }
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// A home directory with a project in it, `home/proj`, a git work tree.
    fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let home = fs::canonicalize(dir.path()).unwrap();
        let root = home.join("proj");
        fs::create_dir_all(root.join(".git")).unwrap();
        (dir, home, root)
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn start(home: &Path, root: &Path) -> Load {
        Instructions::new(root.to_owned(), UserInstructions::at(home.to_owned(), None)).start()
    }

    /// The names given, without files from above the temporary directory.
    fn names(load: &Load) -> Vec<&str> {
        load.files
            .iter()
            .map(|f| f.path.as_str())
            .filter(|p| !p.starts_with('/'))
            .collect()
    }

    fn skipped(load: &Load) -> Vec<(&str, &str)> {
        load.skipped
            .iter()
            .map(|s| (s.path.as_str(), s.why.as_str()))
            .filter(|(p, _)| !p.starts_with('/'))
            .collect()
    }

    #[test]
    fn a_conversation_starts_with_the_user_files_then_the_directories_down_to_the_root() {
        let (_dir, home, root) = tree();
        write(&root.join("CLAUDE.local.md"), "mine only");
        write(&root.join("AGENTS.md"), "project rules\n\n");
        write(&root.join(".claude/CLAUDE.md"), "from .claude");
        write(&root.join("CLAUDE.md"), "\u{feff}claude rules");
        write(&home.join("AGENTS.md"), "above the project");
        write(&home.join(".claude/CLAUDE.md"), "user rules");
        write(&home.join(".ognivo/AGENTS.md"), "user rules for ognivo");
        let load = start(&home, &root);
        assert_eq!(
            names(&load),
            [
                "~/.claude/CLAUDE.md",
                "~/.ognivo/AGENTS.md",
                "~/AGENTS.md",
                "AGENTS.md",
                "CLAUDE.md",
                ".claude/CLAUDE.md",
                "CLAUDE.local.md"
            ]
        );
        assert!(load.skipped.is_empty(), "{:?}", load.skipped);
        assert!(
            load.text
                .starts_with("Instructions from the user and the project")
        );
        assert!(
            load.text
                .contains("\n\n<instructions file=\"AGENTS.md\">\nproject rules\n</instructions>\n\n<instructions file=\"CLAUDE.md\">\nclaude rules\n</instructions>"),
            "{}",
            load.text
        );
        let at = |s: &str| load.text.find(s).unwrap();
        assert!(at("user rules") < at("above the project"));
        assert!(at("above the project") < at("project rules"));
        assert!(at("from .claude") < at("mine only"));
    }

    #[test]
    fn links_imports_and_copies_of_a_file_add_nothing() {
        let (_dir, home, root) = tree();
        write(&root.join("AGENTS.md"), "the rules");
        write(&root.join("CLAUDE.md"), "@AGENTS.md\n");
        write(&root.join("CLAUDE.local.md"), "the rules\n");
        fs::create_dir(root.join(".claude")).unwrap();
        symlink("../AGENTS.md", root.join(".claude/CLAUDE.md")).unwrap();
        let load = start(&home, &root);
        assert_eq!(names(&load), ["AGENTS.md"]);
        assert_eq!(load.text.matches("the rules").count(), 1);
    }

    #[test]
    fn imports_follow_the_rules_of_claude_code() {
        let (_dir, home, root) = tree();
        write(
            &root.join("CLAUDE.md"),
            "Style: @docs/style.md, and @docs/my\\ notes.md.\n\n```\n@docs/code.md\n```\nNot `@docs/span.md` nor <!-- @docs/comment.md -->, nor me@docs/mail.md, nor @nobody.\n",
        );
        write(&root.join("docs/style.md"), "tabs; more in @deeper.md");
        write(&root.join("docs/deeper.md"), "deeper");
        write(&root.join("docs/my notes.md"), "notes");
        for f in ["code", "span", "comment", "mail"] {
            write(&root.join(format!("docs/{f}.md")), f);
        }
        let load = start(&home, &root);
        assert_eq!(
            names(&load),
            [
                "CLAUDE.md",
                "docs/style.md",
                "docs/deeper.md",
                "docs/my notes.md"
            ]
        );
        assert!(load.skipped.is_empty(), "{:?}", load.skipped);
    }

    #[test]
    fn imports_stop_five_levels_deep_and_at_a_cycle() {
        let (_dir, home, root) = tree();
        write(&root.join("CLAUDE.md"), "top @1.md");
        for n in 1..=6 {
            write(
                &root.join(format!("{n}.md")),
                &format!("level {n} @{}.md", n + 1),
            );
        }
        write(&root.join("5.md"), "level 5 @6.md @CLAUDE.md");
        let load = start(&home, &root);
        assert_eq!(
            names(&load),
            ["CLAUDE.md", "1.md", "2.md", "3.md", "4.md", "5.md"]
        );
        assert_eq!(
            skipped(&load),
            [("6.md", "imported more than 5 levels deep")]
        );
    }

    #[test]
    fn a_project_file_reaches_nothing_outside_the_project() {
        let (_dir, home, root) = tree();
        write(&home.join("secret.md"), "the secret");
        write(&home.join("mine.md"), "the user's notes");
        write(&root.join("CLAUDE.md"), "@~/secret.md @../secret.md");
        symlink(home.join("secret.md"), root.join("AGENTS.md")).unwrap();
        write(
            &root.join("CLAUDE.local.md"),
            &format!("@{}", home.join("binary").display()),
        );
        write(&home.join("binary"), "\0");
        // the user's own file may import anything
        write(&home.join(".claude/CLAUDE.md"), "@~/mine.md @~/binary");
        let load = start(&home, &root);
        assert_eq!(names(&load), ["~/mine.md"]);
        assert!(!load.text.contains("the secret"), "{}", load.text);
        assert_eq!(
            skipped(&load),
            [
                ("~/binary", "not a text file"),
                ("AGENTS.md", "a link to outside the project"),
                (
                    "~/secret.md",
                    "imported by CLAUDE.md from outside the project"
                ),
                (
                    "~/binary",
                    "imported by CLAUDE.local.md from outside the project"
                ),
            ]
        );
    }

    #[test]
    fn a_subdirectory_gives_its_files_once_with_the_first_call_under_it() {
        let (_dir, home, root) = tree();
        write(&root.join("AGENTS.md"), "general");
        write(&root.join("crates/AGENTS.md"), "for crates");
        write(
            &root.join("crates/foo/CLAUDE.md"),
            "for foo\n@../../AGENTS.md",
        );
        write(&root.join("crates/foo/src/lib.rs"), "");
        let mut i = Instructions::new(root.clone(), UserInstructions::at(home, None));
        assert_eq!(names(&i.start()), ["AGENTS.md"]);
        assert_eq!(i.near(&root.join("lib.rs")), Load::default());
        let near = i.near(&root.join("crates/foo/src/lib.rs"));
        assert_eq!(names(&near), ["crates/AGENTS.md", "crates/foo/CLAUDE.md"]);
        assert!(
            near.text.starts_with("[ognivo] Instructions for the files under crates/ and crates/foo/ follow; where they disagree with the earlier ones, they win, and the later file wins over the earlier one.\n\n<instructions file=\"crates/AGENTS.md\">\nfor crates\n</instructions>"),
            "{}",
            near.text
        );
        assert_eq!(
            i.near(&root.join("crates/foo/src/main.rs")),
            Load::default()
        );
        assert_eq!(
            i.near(&root.join("crates/foo/new/dir/x.rs")),
            Load::default()
        );
        // a new conversation gets them again
        assert_eq!(names(&i.start()), ["AGENTS.md"]);
        let near = i.near(&root.join("crates/foo/x.rs"));
        assert_eq!(names(&near), ["crates/AGENTS.md", "crates/foo/CLAUDE.md"]);
    }

    #[test]
    fn the_files_nearest_the_work_keep_their_room() {
        let (_dir, home, root) = tree();
        let line = "x".repeat(99) + "\n";
        write(&home.join(".claude/CLAUDE.md"), &line.repeat(300));
        write(&home.join(".ognivo/AGENTS.md"), "no room for this");
        write(&root.join("AGENTS.md"), &line.repeat(50));
        let load = start(&home, &root);
        let files: Vec<&Given> = load
            .files
            .iter()
            .filter(|f| !f.path.starts_with('/'))
            .collect();
        assert_eq!(
            files,
            [
                &Given {
                    path: "~/.claude/CLAUDE.md".into(),
                    bytes: 27_699,
                    cut: true
                },
                &Given {
                    path: "AGENTS.md".into(),
                    bytes: 4_999,
                    cut: false
                }
            ]
        );
        assert!(load.text.contains(
            "x\n[ognivo] Cut here: 3 KiB more did not fit. Read the file for the rest.\n</instructions>"
        ));
        assert_eq!(
            skipped(&load),
            [(
                "~/.ognivo/AGENTS.md",
                "no room: instructions take at most 32 KiB"
            )]
        );
    }

    #[test]
    fn mentions_skip_code_comments_and_mail() {
        let text = "@a.md first, then @b\\ c.md.\n```md\n@code.md\n```\n  ~~~~\n@tilde.md\n~~~\n~~~~\n`@span.md` and ``@two `x` .md`` but @d.md\n<!-- @comment.md\n@still.md --> @e.md\nme@mail.md (@paren.md) é @f.md\n";
        let got: Vec<String> = mentions(text).into_iter().map(|m| m.path).collect();
        assert_eq!(got, ["a.md", "b c.md.", "d.md", "e.md", "f.md"]);
        let m = &mentions("see @x.md")[0];
        assert_eq!(&"see @x.md"[m.at.clone()], "@x.md");
    }
}
