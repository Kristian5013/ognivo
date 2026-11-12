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
}
