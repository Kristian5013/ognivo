//! The edit journal (docs/architecture.md §8). The agent's own edits and writes apply at
//! once; for every file they changed since the user last reviewed it, the journal keeps the
//! bytes it had before and the bytes of the agent's last write, so the user can see the
//! change, accept it or revert it. Commands with side effects are listed, never undone.
//! The session store keeps the journal ([`Journal::unsaved`], [`Journal::restore`]), so a
//! resumed session can still revert what an earlier run of ognivo changed.

use std::collections::btree_map::Entry as Slot;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::ops::Range;
use std::path::{Path, PathBuf};

use ognivo_store::{JournalChanges, JournalFile, JournalRest, SavedJournal};
use ognivo_tools::diff::{self, Hunk};
use ognivo_tools::files::{self, FileError, Target};

#[derive(Debug, Default)]
pub struct Journal {
    files: BTreeMap<PathBuf, Entry>,
    /// Directories the agent's writes created; a revert removes those it leaves empty.
    created_dirs: BTreeSet<PathBuf>,
    /// Commands with side effects since the last accept.
    side_effects: Vec<String>,
    /// The project may have had changes before the session, which the journal cannot show.
    inherited: bool,
    /// Files whose entry changed since the last [`Journal::unsaved`].
    unsaved_files: BTreeSet<PathBuf>,
    /// The directories, the commands or `inherited` changed since then.
    unsaved_rest: bool,
}

#[derive(Debug)]
struct Entry {
    /// Relative to the project root inside it, absolute outside.
    shown: PathBuf,
    /// The bytes before the first unreviewed change; `None` for a file the agent created.
    base: Option<Vec<u8>>,
    /// The bytes of the agent's last write.
    current: Vec<u8>,
    /// Something else changed the file between two of the agent's writes.
    drifted: bool,
}

/// What a write is about to replace, read just before it.
#[derive(Debug)]
pub struct Before {
    bytes: Option<Vec<u8>>,
    missing_dirs: Vec<PathBuf>,
}

/// One file's unreviewed change, as the file is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: PathBuf,
    /// Relative to the project root inside it, absolute outside.
    pub shown: String,
    /// The agent created the file.
    pub created: bool,
    /// The file is gone.
    pub deleted: bool,
    pub hunks: Vec<Hunk>,
    /// Something other than the agent's edits changed it too: a revert would lose that.
    pub drifted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reverted {
    /// Back to the content it had.
    Restored,
    /// The agent had created it, so it is gone.
    Removed,
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("{0}: no unreviewed change by ognivo")]
    NotChanged(String),
    #[error(
        "{0} also changed after ognivo's edits, by a command or outside ognivo; a revert would lose that"
    )]
    Drifted(String),
    /// The file's change has no hunk equal to the one given: the file changed since.
    #[error("{0} changed since its changes were listed: list them again")]
    Stale(String),
    /// Only the whole change of the file can be kept or undone, for the reason given.
    #[error("{0}: {1}; keep or undo the whole file")]
    WholeFile(String, &'static str),
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    File(#[from] FileError),
}

impl Journal {
    /// `inherited`: the project may have had unreviewed changes before the session.
    pub fn new(inherited: bool) -> Self {
        Self {
            inherited,
            unsaved_rest: true,
            ..Self::default()
        }
    }

    /// The journal a stored session left; `root` is the project's.
    pub fn restore(saved: SavedJournal, root: &Path) -> Self {
        let files = saved
            .files
            .into_iter()
            .map(|f| {
                let entry = Entry {
                    shown: Target::new(f.path.clone(), root).shown,
                    base: f.base,
                    current: f.current,
                    drifted: f.drifted,
                };
                (f.path, entry)
            })
            .collect();
        Self {
            files,
            created_dirs: saved.rest.created_dirs.into_iter().collect(),
            side_effects: saved.rest.side_effects,
            inherited: saved.rest.inherited,
            unsaved_files: BTreeSet::new(),
            unsaved_rest: false,
        }
    }

    /// Notes that the project may hold changes from outside the journal, such as those a
    /// resumed session finds in the git tree.
    pub fn inherit(&mut self) {
        if !self.inherited {
            self.inherited = true;
            self.unsaved_rest = true;
        }
    }

    /// What changed since the last call, for the session store.
    pub fn unsaved(&mut self) -> JournalChanges {
        let mut changes = JournalChanges::default();
        for path in std::mem::take(&mut self.unsaved_files) {
            match self.files.get(&path) {
                Some(e) => changes.put.push(JournalFile {
                    path,
                    base: e.base.clone(),
                    current: e.current.clone(),
                    drifted: e.drifted,
                }),
                None => changes.removed.push(path),
            }
        }
        if std::mem::take(&mut self.unsaved_rest) {
            changes.rest = Some(JournalRest {
                created_dirs: self.created_dirs.iter().cloned().collect(),
                side_effects: self.side_effects.clone(),
                inherited: self.inherited,
            });
        }
        changes
    }

    /// Reads what a write to `path` is about to replace. An error means the write must not
    /// go ahead: without the old content no revert is possible.
    pub fn before_write(path: &Path) -> Result<Before, JournalError> {
        let bytes = match fs::read(path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(source) => {
                return Err(JournalError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let missing_dirs = path
            .ancestors()
            .skip(1)
            .take_while(|d| !d.as_os_str().is_empty() && !d.exists())
            .map(Path::to_path_buf)
            .collect();
        Ok(Before {
            bytes,
            missing_dirs,
        })
    }

    /// Notes a write that happened; `after` is what it wrote.
    pub fn record(&mut self, path: PathBuf, shown: PathBuf, before: Before, after: Vec<u8>) {
        self.unsaved_rest |= !before.missing_dirs.is_empty();
        self.created_dirs.extend(before.missing_dirs);
        self.unsaved_files.insert(path.clone());
        match self.files.entry(path) {
            Slot::Occupied(mut slot) => {
                let e = slot.get_mut();
                e.drifted |= before.bytes.as_deref() != Some(e.current.as_slice());
                e.current = after;
                if e.base.as_deref() == Some(e.current.as_slice()) {
                    slot.remove();
                }
            }
            Slot::Vacant(slot) => {
                if before.bytes.as_deref() != Some(after.as_slice()) {
                    slot.insert(Entry {
                        shown,
                        base: before.bytes,
                        current: after,
                        drifted: false,
                    });
                }
            }
        }
    }

    pub fn side_effect(&mut self, command: &str) {
        self.side_effects.push(command.to_owned());
        self.unsaved_rest = true;
    }

    /// Takes in what another journal of the same project holds, a subagent's: its changes
    /// count as this one's, from before the first of them on both sides.
    pub fn absorb(&mut self, other: Journal) {
        for (path, theirs) in other.files {
            self.unsaved_files.insert(path.clone());
            match self.files.entry(path) {
                Slot::Occupied(mut slot) => {
                    let e = slot.get_mut();
                    e.drifted |=
                        theirs.drifted || theirs.base.as_deref() != Some(e.current.as_slice());
                    e.current = theirs.current;
                    if e.base.as_deref() == Some(e.current.as_slice()) {
                        slot.remove();
                    }
                }
                Slot::Vacant(slot) => {
                    slot.insert(theirs);
                }
            }
        }
        self.unsaved_rest |= !other.created_dirs.is_empty() || !other.side_effects.is_empty();
        self.created_dirs.extend(other.created_dirs);
        self.side_effects.extend(other.side_effects);
    }

    /// Nothing is unreviewed: every file the agent changed is as it was, no command with
    /// side effects ran, and nothing came from before the session.
    pub fn is_clean(&self) -> bool {
        !self.inherited
            && self.side_effects.is_empty()
            && self.files.iter().all(|(path, e)| settled(path, e))
    }

    /// The unreviewed changes of the files, as they are on disk now.
    pub fn changes(&mut self) -> Vec<FileChange> {
        self.prune();
        self.files
            .iter()
            .map(|(path, e)| {
                let now = fs::read(path).ok();
                let text =
                    |b: Option<&[u8]>| String::from_utf8_lossy(b.unwrap_or_default()).into_owned();
                FileChange {
                    path: path.clone(),
                    shown: e.shown.display().to_string(),
                    created: e.base.is_none(),
                    deleted: !path.exists(),
                    hunks: diff::hunks(&text(e.base.as_deref()), &text(now.as_deref())),
                    drifted: e.drifted || now.as_deref() != Some(e.current.as_slice()),
                }
            })
            .collect()
    }

    /// What `path` held before its first unreviewed change: `Some(None)` for a file the
    /// agent created, `None` for a file without an unreviewed change.
    pub fn base(&self, path: &Path) -> Option<Option<&[u8]>> {
        self.files.get(path).map(|e| e.base.as_deref())
    }

    /// Commands with side effects since the last accept.
    pub fn side_effects(&self) -> &[String] {
        &self.side_effects
    }

    /// Whether changes from before the session still wait for an accept.
    pub fn inherited(&self) -> bool {
        self.inherited
    }

    /// What keeps the project unreviewed, one item each, for a question about running
    /// project code.
    pub fn summary(&mut self) -> Vec<String> {
        self.prune();
        let mut items: Vec<String> = self
            .files
            .values()
            .map(|e| e.shown.display().to_string())
            .collect();
        items.extend(self.side_effects.iter().map(|c| {
            let line = c.lines().next().unwrap_or_default();
            format!("`{line}`")
        }));
        if self.inherited {
            items.push("changes from before this session".to_owned());
        }
        items
    }

    /// Marks one file's change as reviewed, or everything with `None`: the files, the
    /// commands and what came from before the session. Returns the files it covered.
    pub fn accept(&mut self, path: Option<&Path>) -> Result<Vec<String>, JournalError> {
        self.prune();
        let Some(path) = path else {
            let files = std::mem::take(&mut self.files);
            self.side_effects.clear();
            self.inherited = false;
            self.created_dirs.clear();
            self.unsaved_files.extend(files.keys().cloned());
            self.unsaved_rest = true;
            return Ok(files
                .into_values()
                .map(|e| e.shown.display().to_string())
                .collect());
        };
        let e = self
            .files
            .remove(path)
            .ok_or_else(|| JournalError::NotChanged(path.display().to_string()))?;
        self.unsaved_files.insert(path.to_owned());
        Ok(vec![e.shown.display().to_string()])
    }

    /// Puts a file back as it was before the agent's first unreviewed change. A file that
    /// something else changed too is reverted only with `force`.
    pub fn revert(&mut self, path: &Path, force: bool) -> Result<Reverted, JournalError> {
        self.prune();
        let e = self
            .files
            .get(path)
            .ok_or_else(|| JournalError::NotChanged(path.display().to_string()))?;
        let now = fs::read(path).ok();
        if !force && (e.drifted || now.as_deref() != Some(e.current.as_slice())) {
            return Err(JournalError::Drifted(e.shown.display().to_string()));
        }
        let how = match &e.base {
            Some(bytes) => {
                files::restore(path, &e.shown, bytes)?;
                Reverted::Restored
            }
            None => {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(JournalError::Io {
                            path: e.shown.display().to_string(),
                            source,
                        });
                    }
                }
                Reverted::Removed
            }
        };
        self.files.remove(path);
        self.unsaved_files.insert(path.to_owned());
        // deepest first; a directory with anything in it stays
        let dirs: Vec<PathBuf> = self.created_dirs.iter().rev().cloned().collect();
        for dir in dirs {
            if fs::remove_dir(&dir).is_ok() || !dir.exists() {
                self.created_dirs.remove(&dir);
                self.unsaved_rest = true;
            }
        }
        Ok(how)
    }

    /// Keeps one hunk of a file's unreviewed change, as [`Journal::changes`] gives it: what
    /// the file had before takes the hunk's lines, so its change no longer has them. The
    /// file stays as it is.
    pub fn accept_hunk(&mut self, path: &Path, hunk: &Hunk) -> Result<(), JournalError> {
        let found = self.find(path, hunk)?;
        let base = found.kept();
        self.unsaved_files.insert(path.to_owned());
        if base == found.now.as_bytes() {
            self.files.remove(path);
        } else if let Some(e) = self.files.get_mut(path) {
            e.base = Some(base);
        }
        Ok(())
    }

    /// Undoes one hunk of a file's unreviewed change, as [`Journal::changes`] gives it: the
    /// file gets back the lines it had there before the agent's edits and keeps the rest as
    /// it is now.
    pub fn revert_hunk(&mut self, path: &Path, hunk: &Hunk) -> Result<(), JournalError> {
        let found = self.find(path, hunk)?;
        let now = found.undone();
        let Some(e) = self.files.get_mut(path) else {
            return Err(JournalError::NotChanged(path.display().to_string()));
        };
        files::restore(path, &e.shown, &now)?;
        // the agent's next write starts from this one, so it is no drift; a drift before
        // it stays known
        e.drifted |= found.now.as_bytes() != e.current.as_slice();
        e.current = now;
        if e.base.as_deref() == Some(e.current.as_slice()) {
            self.files.remove(path);
        }
        self.unsaved_files.insert(path.to_owned());
        Ok(())
    }

    /// The hunk of `path`'s change as it is now that is equal to `hunk`, in all its numbers
    /// and lines: anything that moved or changed it since the client listed it makes it
    /// stale. A file the agent created, a file that is gone and one that is not UTF-8 text
    /// have no hunks to keep or undo apart.
    fn find(&mut self, path: &Path, hunk: &Hunk) -> Result<Found, JournalError> {
        self.prune();
        let e = self
            .files
            .get(path)
            .ok_or_else(|| JournalError::NotChanged(path.display().to_string()))?;
        let shown = e.shown.display().to_string();
        let whole = |why| JournalError::WholeFile(shown.clone(), why);
        let Some(base) = &e.base else {
            return Err(whole("ognivo created it"));
        };
        let now = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Err(whole("it is gone")),
            Err(source) => {
                return Err(JournalError::Io {
                    path: shown.clone(),
                    source,
                });
            }
        };
        let (Ok(base), Ok(now)) = (String::from_utf8(base.clone()), String::from_utf8(now)) else {
            return Err(whole("it is not UTF-8 text"));
        };
        let stale = || JournalError::Stale(shown.clone());
        if !diff::hunks(&base, &now).contains(hunk) {
            return Err(stale());
        }
        // the diff's numbers are from 1; its lines are those of `diff::lines`
        let old = hunk.old_start - 1..hunk.old_start - 1 + hunk.removed.len();
        let new = hunk.new_start - 1..hunk.new_start - 1 + hunk.added.len();
        if old.end > diff::lines(&base).len() || new.end > diff::lines(&now).len() {
            return Err(stale());
        }
        Ok(Found {
            base,
            now,
            old,
            new,
        })
    }

    /// Forgets the files that are back to what they were, by a `git checkout` or the
    /// agent's own undo.
    fn prune(&mut self) {
        let unsaved = &mut self.unsaved_files;
        self.files.retain(|path, e| {
            let keep = !settled(path, e);
            if !keep {
                unsaved.insert(path.clone());
            }
            keep
        });
    }
}

/// One hunk of a file's change, as the change is now ([`Journal::find`]): the file's text
/// before the change and now, and the hunk's lines in each.
struct Found {
    base: String,
    now: String,
    old: Range<usize>,
    new: Range<usize>,
}

impl Found {
    /// What the file had before, with the hunk's lines as they are now.
    fn kept(&self) -> Vec<u8> {
        splice(&self.base, self.old.clone(), &self.now, self.new.clone())
    }

    /// The file now, with the hunk's lines as they were before.
    fn undone(&self) -> Vec<u8> {
        splice(&self.now, self.new.clone(), &self.base, self.old.clone())
    }
}

/// `into` with `from`'s lines `take` in place of its own lines `at`: lines with their ends,
/// as the diff counts them, so every line end and a last line without one stay as they are.
fn splice(into: &str, at: Range<usize>, from: &str, take: Range<usize>) -> Vec<u8> {
    let (into, from) = (diff::lines(into), diff::lines(from));
    into[..at.start]
        .iter()
        .chain(&from[take])
        .chain(&into[at.end..])
        .flat_map(|line| line.bytes())
        .collect()
}

/// The file is as it was before the agent's changes.
fn settled(path: &Path, e: &Entry) -> bool {
    match fs::read(path) {
        Ok(bytes) => e.base.as_deref() == Some(bytes.as_slice()),
        Err(err) if err.kind() == ErrorKind::NotFound => e.base.is_none(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::slice;

    use super::*;

    /// A project with `a.txt`; `write` changes a file the way the agent's tools do.
    struct Project {
        _dir: tempfile::TempDir,
        root: PathBuf,
        journal: Journal,
    }

    impl Project {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = ognivo_os::canonicalize(dir.path()).unwrap();
            fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
            Self {
                _dir: dir,
                root,
                journal: Journal::new(false),
            }
        }

        fn write(&mut self, name: &str, text: &str) {
            let path = self.root.join(name);
            let before = Journal::before_write(&path).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            self.journal
                .record(path, PathBuf::from(name), before, text.into());
        }

        fn read(&self, name: &str) -> Option<String> {
            fs::read_to_string(self.root.join(name)).ok()
        }

        /// The file as it is before the agent's first edit of it, or any change from
        /// outside the agent: a command's, the user's.
        fn put(&self, name: &str, bytes: impl AsRef<[u8]>) {
            fs::write(self.root.join(name), bytes).unwrap();
        }

        /// The file's unreviewed hunks as `changes` gives them, none when it has no change.
        fn hunks(&mut self, name: &str) -> Vec<Hunk> {
            self.change(name).map(|c| c.hunks).unwrap_or_default()
        }

        fn change(&mut self, name: &str) -> Option<FileChange> {
            self.journal.changes().into_iter().find(|c| c.shown == name)
        }

        fn keep(&mut self, name: &str, hunk: &Hunk) -> Result<(), JournalError> {
            let path = self.root.join(name);
            self.journal.accept_hunk(&path, hunk)
        }

        fn undo(&mut self, name: &str, hunk: &Hunk) -> Result<(), JournalError> {
            let path = self.root.join(name);
            self.journal.revert_hunk(&path, hunk)
        }
    }

    fn hunk(old_start: usize, new_start: usize, removed: &[&str], added: &[&str]) -> Hunk {
        Hunk {
            old_start,
            new_start,
            removed: removed.iter().map(|s| (*s).to_owned()).collect(),
            added: added.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn the_base_is_what_a_file_had_before_its_first_unreviewed_change() {
        let mut p = Project::new();
        let (a, b) = (p.root.join("a.txt"), p.root.join("b.txt"));
        assert_eq!(p.journal.base(&a), None);
        p.write("a.txt", "one\nTWO\n");
        p.write("a.txt", "one\nTWO\nthree\n");
        p.write("b.txt", "new\n");
        // before the first of both writes, not between them
        assert_eq!(p.journal.base(&a), Some(Some(&b"one\ntwo\n"[..])));
        // the agent created it
        assert_eq!(p.journal.base(&b), Some(None));
        p.journal.accept(Some(&a)).unwrap();
        assert_eq!(p.journal.base(&a), None);
    }

    #[test]
    fn a_change_is_shown_against_what_the_file_had_and_reverted_to_it() {
        let mut p = Project::new();
        assert!(p.journal.is_clean());
        p.write("a.txt", "one\nTWO\n");
        p.write("a.txt", "one\nTWO\nthree\n");
        assert!(!p.journal.is_clean());
        let changes = p.journal.changes();
        assert_eq!(changes.len(), 1);
        let c = &changes[0];
        assert_eq!(
            (c.shown.as_str(), c.created, c.drifted),
            ("a.txt", false, false)
        );
        // both writes against the file before the first
        assert_eq!(
            c.hunks,
            [Hunk {
                old_start: 2,
                new_start: 2,
                removed: vec!["two".into()],
                added: vec!["TWO".into(), "three".into()],
            }]
        );
        let a = p.root.join("a.txt");
        assert_eq!(p.journal.revert(&a, false).unwrap(), Reverted::Restored);
        assert_eq!(p.read("a.txt").as_deref(), Some("one\ntwo\n"));
        assert!(p.journal.is_clean());
        assert!(matches!(
            p.journal.revert(&a, false),
            Err(JournalError::NotChanged(_))
        ));
    }

    #[test]
    fn a_subagent_s_changes_become_the_session_s_from_before_either_touched_them() {
        let mut p = Project::new();
        p.write("a.txt", "one\nTWO\n");
        // a subagent's own journal, with a change to the same file and a new one
        let main = std::mem::replace(&mut p.journal, Journal::new(false));
        p.write("a.txt", "one\nTWO\nthree\n");
        p.write("b.txt", "b\n");
        p.journal.side_effect("touch c");
        let sub = std::mem::replace(&mut p.journal, main);
        p.journal.absorb(sub);
        let changes = p.journal.changes();
        let shown: Vec<&str> = changes.iter().map(|c| c.shown.as_str()).collect();
        assert_eq!(shown, ["a.txt", "b.txt"]);
        // a.txt reverts to what it had before the session's first edit, not the subagent's
        assert!(!changes[0].drifted);
        p.journal.revert(&p.root.join("a.txt"), false).unwrap();
        assert_eq!(p.read("a.txt").as_deref(), Some("one\ntwo\n"));
        assert_eq!(p.journal.side_effects(), ["touch c"]);
    }

    #[test]
    fn a_created_file_is_removed_with_the_directories_made_for_it() {
        let mut p = Project::new();
        p.write("new/deep/b.txt", "b\n");
        assert!(p.journal.changes()[0].created);
        let b = p.root.join("new/deep/b.txt");
        assert_eq!(p.journal.revert(&b, false).unwrap(), Reverted::Removed);
        assert!(!p.root.join("new").exists());
        assert!(p.journal.is_clean());
    }

    #[test]
    fn a_file_changed_otherwise_too_is_reverted_only_when_forced() {
        let mut p = Project::new();
        p.write("a.txt", "one\nTWO\n");
        // a command reformats it after the agent's edit
        fs::write(p.root.join("a.txt"), "one\nTWO\n\n").unwrap();
        let a = p.root.join("a.txt");
        assert!(p.journal.changes()[0].drifted);
        assert!(matches!(
            p.journal.revert(&a, false),
            Err(JournalError::Drifted(_))
        ));
        assert_eq!(p.read("a.txt").as_deref(), Some("one\nTWO\n\n"));
        p.journal.revert(&a, true).unwrap();
        assert_eq!(p.read("a.txt").as_deref(), Some("one\ntwo\n"));
        // changed between two of the agent's writes: the last write is not the whole story
        p.write("a.txt", "one\n2\n");
        fs::write(p.root.join("a.txt"), "zero\none\n2\n").unwrap();
        p.write("a.txt", "zero\none\n3\n");
        assert!(matches!(
            p.journal.revert(&a, false),
            Err(JournalError::Drifted(_))
        ));
    }

    #[test]
    fn a_file_back_as_it_was_needs_no_review() {
        let mut p = Project::new();
        p.write("a.txt", "x\n");
        // a `git checkout a.txt`
        fs::write(p.root.join("a.txt"), "one\ntwo\n").unwrap();
        assert!(p.journal.is_clean());
        assert!(p.journal.changes().is_empty());
        // the agent's own undo
        p.write("a.txt", "x\n");
        p.write("a.txt", "one\ntwo\n");
        assert!(p.journal.is_clean());
    }

    #[test]
    fn accepting_clears_files_commands_and_what_came_before() {
        let mut p = Project::new();
        p.journal = Journal::new(true);
        p.write("a.txt", "x\n");
        p.write("b.txt", "y\n");
        p.journal.side_effect("touch c\nmore");
        assert_eq!(
            p.journal.summary(),
            [
                "a.txt",
                "b.txt",
                "`touch c`",
                "changes from before this session"
            ]
        );
        let a = p.root.join("a.txt");
        assert_eq!(p.journal.accept(Some(&a)).unwrap(), ["a.txt"]);
        assert_eq!(p.journal.accept(None).unwrap(), ["b.txt"]);
        assert!(p.journal.is_clean());
        // an accepted change stays as it is
        assert_eq!(p.read("a.txt").as_deref(), Some("x\n"));
        assert!(matches!(
            p.journal.accept(Some(&a)),
            Err(JournalError::NotChanged(_))
        ));
    }

    #[test]
    fn what_changed_is_saved_once_and_a_restored_journal_reverts_it() {
        let mut p = Project::new();
        p.write("a.txt", "one\nTWO\n");
        p.write("d/new.txt", "made\n");
        let first = p.journal.unsaved();
        assert_eq!(first.put.len(), 2);
        assert!(first.removed.is_empty());
        let rest = first.rest.clone().unwrap();
        assert_eq!(rest.created_dirs, [p.root.join("d")]);
        assert!(p.journal.unsaved().is_empty());
        // a later run of ognivo gets it back from the store
        let saved = SavedJournal {
            files: first.put,
            rest,
        };
        let mut back = Journal::restore(saved, &p.root);
        assert!(back.unsaved().is_empty());
        assert_eq!(back.changes()[1].shown, "d/new.txt");
        assert_eq!(
            back.revert(&p.root.join("d/new.txt"), false).unwrap(),
            Reverted::Removed
        );
        assert!(!p.root.join("d").exists());
        assert_eq!(
            back.revert(&p.root.join("a.txt"), false).unwrap(),
            Reverted::Restored
        );
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo\n");
        let after = back.unsaved();
        assert_eq!(after.removed.len(), 2);
        assert!(after.rest.unwrap().created_dirs.is_empty());
        back.inherit();
        assert!(back.unsaved().rest.unwrap().inherited);
    }

    #[test]
    fn a_kept_hunk_leaves_the_file_and_the_other_hunks_as_they_are() {
        let mut p = Project::new();
        p.put("a.txt", "one\ntwo\nthree\nfour\nfive\n");
        p.write("a.txt", "ONE\ntwo\nthree\nfour\nFIVE\n");
        let first = hunk(1, 1, &["one"], &["ONE"]);
        let last = hunk(5, 5, &["five"], &["FIVE"]);
        assert_eq!(p.hunks("a.txt"), [first.clone(), last.clone()]);
        p.journal.unsaved();
        p.keep("a.txt", &first).unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "ONE\ntwo\nthree\nfour\nFIVE\n");
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&last));
        let a = p.root.join("a.txt");
        assert_eq!(
            p.journal.base(&a),
            Some(Some(&b"ONE\ntwo\nthree\nfour\nfive\n"[..]))
        );
        assert!(!p.change("a.txt").unwrap().drifted);
        // the store gets the base that took the hunk
        let saved = p.journal.unsaved();
        assert_eq!(
            saved.put[0].base.as_deref(),
            Some(&b"ONE\ntwo\nthree\nfour\nfive\n"[..])
        );
        // keeping the last hunk too leaves nothing to review, and the file as it is
        p.keep("a.txt", &last).unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "ONE\ntwo\nthree\nfour\nFIVE\n");
        assert!(p.journal.is_clean());
        assert_eq!(p.journal.unsaved().removed, [a]);
    }

    #[test]
    fn an_undone_hunk_leaves_the_other_hunks_and_the_agent_goes_on_from_it() {
        let mut p = Project::new();
        p.put("a.txt", "one\ntwo\nthree\nfour\nfive\n");
        p.write("a.txt", "ONE\ntwo\nthree\nfour\nFIVE\n");
        let first = hunk(1, 1, &["one"], &["ONE"]);
        let last = hunk(5, 5, &["five"], &["FIVE"]);
        p.undo("a.txt", &first).unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo\nthree\nfour\nFIVE\n");
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&last));
        // ognivo's own write: neither it nor the agent's next edit is a drift
        assert!(!p.change("a.txt").unwrap().drifted);
        p.write("a.txt", "one\ntwo\nthree\nfour\nFIVE\nsix\n");
        assert!(!p.change("a.txt").unwrap().drifted);
        assert_eq!(p.hunks("a.txt"), [hunk(5, 5, &["five"], &["FIVE", "six"])]);
        // undoing the last of it leaves the file as it was
        p.undo("a.txt", &hunk(5, 5, &["five"], &["FIVE", "six"]))
            .unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo\nthree\nfour\nfive\n");
        assert!(p.journal.is_clean());
    }

    #[test]
    fn hunks_keep_their_line_ends_byte_for_byte() {
        let mut p = Project::new();
        p.put("a.txt", "one\r\ntwo\r\nthree\r\n");
        p.write("a.txt", "one\r\nTWO\r\nthree\r\nfour\r\n");
        let two = hunk(2, 2, &["two"], &["TWO"]);
        let four = hunk(4, 4, &[], &["four"]);
        assert_eq!(p.hunks("a.txt"), [two.clone(), four.clone()]);
        p.undo("a.txt", &two).unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "one\r\ntwo\r\nthree\r\nfour\r\n");
        p.keep("a.txt", &four).unwrap();
        assert!(p.journal.is_clean());
        // a line whose end alone changed: its text is the same on both sides
        p.put("b.txt", "a\r\nb\r\nc\rd\n");
        p.write("b.txt", "a\r\nb\nc\rd\n");
        let end = hunk(2, 2, &["b"], &["b"]);
        assert_eq!(p.hunks("b.txt"), slice::from_ref(&end));
        p.undo("b.txt", &end).unwrap();
        assert!(p.change("b.txt").is_none());
        assert_eq!(fs::read(p.root.join("b.txt")).unwrap(), b"a\r\nb\r\nc\rd\n");
    }

    #[test]
    fn a_last_line_without_its_end_stays_without_it() {
        let mut p = Project::new();
        p.put("a.txt", "one\ntwo");
        p.write("a.txt", "one\ntwo\nthree");
        let grown = hunk(2, 2, &["two"], &["two", "three"]);
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&grown));
        p.undo("a.txt", &grown).unwrap();
        assert!(p.change("a.txt").is_none());
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo");
        // the agent took the last line's end away: kept, it stays away
        p.put("b.txt", "one\ntwo\n");
        p.write("b.txt", "one\ntwo");
        let cut = hunk(2, 2, &["two"], &["two"]);
        assert_eq!(p.hunks("b.txt"), slice::from_ref(&cut));
        p.keep("b.txt", &cut).unwrap();
        assert!(p.journal.is_clean());
        assert_eq!(p.read("b.txt").unwrap(), "one\ntwo");
    }

    #[test]
    fn a_line_added_at_the_end_and_a_line_taken_out_are_hunks_of_their_own() {
        let mut p = Project::new();
        for name in ["a.txt", "b.txt"] {
            p.put(name, "a\nb\nc\nd\n");
            p.write(name, "a\nc\nd\ne\n");
        }
        // a pure deletion, and an insertion past the last line
        let out = hunk(2, 2, &["b"], &[]);
        let end = hunk(5, 4, &[], &["e"]);
        assert_eq!(p.hunks("a.txt"), [out.clone(), end.clone()]);
        // kept at the end, then the deletion undone
        p.keep("a.txt", &end).unwrap();
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&out));
        p.undo("a.txt", &out).unwrap();
        assert!(p.change("a.txt").is_none());
        assert_eq!(p.read("a.txt").unwrap(), "a\nb\nc\nd\ne\n");
        // undone at the end, then the deletion kept
        p.undo("b.txt", &end).unwrap();
        assert_eq!(p.read("b.txt").unwrap(), "a\nc\nd\n");
        assert_eq!(p.hunks("b.txt"), slice::from_ref(&out));
        p.keep("b.txt", &out).unwrap();
        assert!(p.journal.is_clean());
        assert_eq!(p.read("b.txt").unwrap(), "a\nc\nd\n");
    }

    #[test]
    fn a_hunk_the_file_no_longer_has_is_stale() {
        let mut p = Project::new();
        p.put("a.txt", "one\ntwo\nthree\n");
        p.write("a.txt", "ONE\ntwo\nthree\n");
        let listed = hunk(1, 1, &["one"], &["ONE"]);
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&listed));
        // the user changed the hunk's line since
        p.put("a.txt", "ONE!\ntwo\nthree\n");
        for result in [p.keep("a.txt", &listed), p.undo("a.txt", &listed)] {
            assert!(matches!(result, Err(JournalError::Stale(_))), "{result:?}");
        }
        assert_eq!(p.read("a.txt").unwrap(), "ONE!\ntwo\nthree\n");
        assert_eq!(p.hunks("a.txt"), [hunk(1, 1, &["one"], &["ONE!"])]);
        // a change elsewhere that does not move the hunk leaves it as it is
        p.put("a.txt", "ONE!\ntwo\nTHREE\n");
        p.keep("a.txt", &hunk(1, 1, &["one"], &["ONE!"])).unwrap();
        assert_eq!(p.hunks("a.txt"), [hunk(3, 3, &["three"], &["THREE"])]);
        // made up, or of a file without a change
        let wrong = hunk(2, 2, &["three"], &["THREE"]);
        assert!(matches!(
            p.keep("a.txt", &wrong),
            Err(JournalError::Stale(_))
        ));
        assert!(matches!(
            p.keep("c.txt", &wrong),
            Err(JournalError::NotChanged(_))
        ));
    }

    #[test]
    fn a_drifted_file_keeps_and_undoes_hunks_and_stays_drifted() {
        let mut p = Project::new();
        p.put("a.txt", "one\ntwo\nthree\nfour\n");
        p.write("a.txt", "ONE\ntwo\nthree\nfour\n");
        // a formatter changes another line after the agent's edit
        p.put("a.txt", "ONE\ntwo\nthree\nFOUR\n");
        let c = p.change("a.txt").unwrap();
        assert!(c.drifted);
        let ours = hunk(1, 1, &["one"], &["ONE"]);
        let theirs = hunk(4, 4, &["four"], &["FOUR"]);
        assert_eq!(c.hunks, [ours.clone(), theirs.clone()]);
        // undone, the agent's hunk goes and the other change stays
        p.undo("a.txt", &ours).unwrap();
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo\nthree\nFOUR\n");
        assert_eq!(p.hunks("a.txt"), slice::from_ref(&theirs));
        // a whole revert would still lose the other change
        assert!(p.change("a.txt").unwrap().drifted);
        let a = p.root.join("a.txt");
        assert!(matches!(
            p.journal.revert(&a, false),
            Err(JournalError::Drifted(_))
        ));
        // kept, the other change is reviewed too
        p.keep("a.txt", &theirs).unwrap();
        assert!(p.journal.is_clean());
        assert_eq!(p.read("a.txt").unwrap(), "one\ntwo\nthree\nFOUR\n");
    }

    #[test]
    fn a_created_deleted_or_binary_file_is_kept_or_undone_whole() {
        let mut p = Project::new();
        p.write("b.txt", "b\n");
        let created = hunk(1, 1, &[], &["b"]);
        assert_eq!(p.hunks("b.txt"), slice::from_ref(&created));
        let refused = p.keep("b.txt", &created).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "b.txt: ognivo created it; keep or undo the whole file"
        );
        assert!(matches!(
            p.undo("b.txt", &created),
            Err(JournalError::WholeFile(..))
        ));
        assert_eq!(p.read("b.txt").as_deref(), Some("b\n"));
        // deleted after the agent's edit, by a command
        p.write("a.txt", "one\nTWO\n");
        fs::remove_file(p.root.join("a.txt")).unwrap();
        let c = p.change("a.txt").unwrap();
        assert!(c.deleted);
        let gone = c.hunks[0].clone();
        for result in [p.keep("a.txt", &gone), p.undo("a.txt", &gone)] {
            assert!(
                matches!(result, Err(JournalError::WholeFile(_, "it is gone"))),
                "{result:?}"
            );
        }
        // not UTF-8: its lines are not the hunks' lines
        p.put("c.txt", b"caf\xe9\n");
        p.write("c.txt", "cafe\n");
        let latin = p.hunks("c.txt")[0].clone();
        assert!(matches!(
            p.keep("c.txt", &latin),
            Err(JournalError::WholeFile(_, "it is not UTF-8 text"))
        ));
        assert_eq!(p.journal.changes().len(), 3);
    }
}
