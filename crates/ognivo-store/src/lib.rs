//! The SQLite store of ognivo sessions (docs/architecture.md §11), `~/.ognivo/data/ognivo.db`:
//! - `sessions`: one row per `chat` or `run`, bound to one project, profile and reasoning
//!   level;
//! - `conversations` and `messages`: the history the model gets, as it was sent; `/clear`
//!   starts the next conversation of the same session;
//! - `paths`: the files a conversation read or wrote and the directories whose instruction
//!   files it got;
//! - `events`: the append-only log of what happened, numbered by a global `seq`, with a
//!   column for sealed token ids (the Native path), empty on the Generic path;
//! - `journal_files`, `journal_dirs`, `journal_commands`: the edit journal; the contents of
//!   files live in `blobs`, keyed by their blake3 hash and compressed with zstd;
//! - `transcript`: FTS5 over the user's messages and the answers;
//! - `trusted_rules`: the user's answer about a project's allow rules, for exactly those.
//!
//! The store keeps what the core gives it: messages, events, counters and conversation
//! notes are JSON the core writes and reads. Paths are kept as the OS spells them, so a
//! revert after a restart writes exactly the file that was read.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, TransactionBehavior, params};

/// The schema steps, oldest first: a store at version `n` has run the first `n`; the
/// version this build writes is their count (`PRAGMA user_version`).
const MIGRATIONS: &[&str] = &[SCHEMA_1, SCHEMA_2];
const VERSION: i64 = MIGRATIONS.len() as i64;
const ZSTD_LEVEL: i32 = 3;

const SCHEMA_2: &str = "
CREATE TABLE trusted_rules (
    root BLOB NOT NULL,
    rules TEXT NOT NULL,
    trusted INTEGER NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (root, rules)
);
";

const SCHEMA_1: &str = "
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    root BLOB NOT NULL,
    profile TEXT NOT NULL,
    level TEXT,
    server TEXT NOT NULL,
    seed INTEGER NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    turns INTEGER NOT NULL DEFAULT 0,
    stats TEXT NOT NULL DEFAULT '{}',
    inherited INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX sessions_by_root ON sessions (root, updated);

CREATE TABLE conversations (
    session INTEGER NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    n INTEGER NOT NULL,
    started INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (session, n)
);

CREATE TABLE messages (
    session INTEGER NOT NULL,
    conversation INTEGER NOT NULL,
    idx INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (session, conversation, idx),
    FOREIGN KEY (session, conversation) REFERENCES conversations (session, n) ON DELETE CASCADE
);

CREATE TABLE paths (
    session INTEGER NOT NULL,
    conversation INTEGER NOT NULL,
    kind TEXT NOT NULL,
    path BLOB NOT NULL,
    PRIMARY KEY (session, conversation, kind, path),
    FOREIGN KEY (session, conversation) REFERENCES conversations (session, n) ON DELETE CASCADE
);

CREATE TABLE events (
    seq INTEGER PRIMARY KEY,
    session INTEGER NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    turn INTEGER NOT NULL,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL,
    sealed_ids BLOB,
    codec TEXT
);
CREATE INDEX events_by_session ON events (session, seq);

CREATE TABLE blobs (
    hash BLOB PRIMARY KEY,
    size INTEGER NOT NULL,
    data BLOB NOT NULL
) WITHOUT ROWID;

CREATE TABLE journal_files (
    session INTEGER NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    path BLOB NOT NULL,
    base BLOB,
    current BLOB NOT NULL,
    drifted INTEGER NOT NULL,
    PRIMARY KEY (session, path)
);
CREATE INDEX journal_by_base ON journal_files (base);
CREATE INDEX journal_by_current ON journal_files (current);

CREATE TABLE journal_dirs (
    session INTEGER NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    path BLOB NOT NULL,
    PRIMARY KEY (session, path)
);

CREATE TABLE journal_commands (
    session INTEGER NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    command TEXT NOT NULL,
    PRIMARY KEY (session, idx)
);

CREATE VIRTUAL TABLE transcript USING fts5 (
    text,
    session UNINDEXED,
    conversation UNINDEXED,
    role UNINDEXED
);
";

/// Removes the contents no edit journal refers to any more.
const DROP_UNUSED_BLOBS: &str = "
DELETE FROM blobs WHERE NOT EXISTS (SELECT 1 FROM journal_files WHERE base = blobs.hash)
AND NOT EXISTS (SELECT 1 FROM journal_files WHERE current = blobs.hash)";

const SELECT_SESSION: &str = "
SELECT s.id, s.kind, s.root, s.profile, s.level, s.server, s.seed, s.title, s.created,
       s.updated, strftime('%Y-%m-%d %H:%M', s.updated / 1000, 'unixepoch', 'localtime'),
       s.turns, s.stats,
       (SELECT count(*) FROM journal_files j WHERE j.session = s.id)
FROM sessions s";

pub type SessionId = i64;
/// Position in the event log: global and dense across sessions.
pub type Seq = i64;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the store has schema {0}; this ognivo knows up to {VERSION}")]
    Newer(i64),
    #[error("blob {0} is missing or damaged")]
    Blob(String),
}

/// What a new session is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// `chat` or `run`.
    pub kind: String,
    /// Canonical.
    pub root: PathBuf,
    pub profile: String,
    pub level: Option<String>,
    pub server: String,
    pub seed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: SessionId,
    pub kind: String,
    pub root: PathBuf,
    pub profile: String,
    pub level: Option<String>,
    pub server: String,
    pub seed: u32,
    /// The first line of the first message.
    pub title: String,
    /// Unix milliseconds.
    pub created: i64,
    pub updated: i64,
    /// `updated` as `2026-11-02 14:03`, local time.
    pub updated_local: String,
    pub turns: u32,
    /// The core's counters, as JSON.
    pub stats: String,
    /// Files with unreviewed changes in its edit journal.
    pub unreviewed_files: u32,
}

/// The last conversation of a session, to pick it up again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub n: u32,
    /// One JSON text per message, in order.
    pub messages: Vec<String>,
    /// The core's notes about it, as JSON.
    pub state: String,
    pub paths: Vec<(String, PathBuf)>,
}

/// One file of an edit journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalFile {
    pub path: PathBuf,
    /// `None`: the agent created the file.
    pub base: Option<Vec<u8>>,
    pub current: Vec<u8>,
    pub drifted: bool,
}

/// The rest of an edit journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalRest {
    pub created_dirs: Vec<PathBuf>,
    pub side_effects: Vec<String>,
    pub inherited: bool,
}

/// What changed in an edit journal since it was last saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalChanges {
    /// Files whose entry is new or changed.
    pub put: Vec<JournalFile>,
    /// Files whose entry is gone.
    pub removed: Vec<PathBuf>,
    pub rest: Option<JournalRest>,
}

impl JournalChanges {
    pub fn is_empty(&self) -> bool {
        self.put.is_empty() && self.removed.is_empty() && self.rest.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedJournal {
    pub files: Vec<JournalFile>,
    pub rest: JournalRest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: Seq,
    pub turn: u32,
    /// Unix milliseconds.
    pub at: i64,
    pub kind: String,
    /// JSON.
    pub data: String,
}

/// A row of `journal_files`, with the hashes of the contents.
#[derive(Debug)]
struct HashedFile {
    path: Vec<u8>,
    base: Option<Vec<u8>>,
    current: Vec<u8>,
    drifted: bool,
}

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the store at `path`, creating the file (mode 0600: it holds code and
    /// conversations) and its schema when they do not exist.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let io = |p: &Path| {
            let path = p.display().to_string();
            move |source| StoreError::Io { path, source }
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io(dir))?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(path)(e)),
        }
        Self::init(Connection::open(path)?)
    }

    /// Another connection to this store's file, for a session kept beside this one's;
    /// `None` for a store in memory.
    pub fn reopen(&self) -> Result<Option<Self>, StoreError> {
        match self.conn.path() {
            Some(path) if !path.is_empty() => Self::open(Path::new(path)).map(Some),
            _ => Ok(None),
        }
    }

    /// A store that lives in memory, for tests.
    pub fn memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, StoreError> {
        // another ognivo may be writing: wait for it rather than fail
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > VERSION {
            return Err(StoreError::Newer(version));
        }
        for step in &MIGRATIONS[version.max(0) as usize..] {
            tx.execute_batch(step)?;
        }
        tx.pragma_update(None, "user_version", VERSION)?;
        tx.commit()?;
        Ok(Self { conn })
    }

    pub fn create_session(&mut self, s: &NewSession) -> Result<SessionId, StoreError> {
        self.conn.execute(
            "INSERT INTO sessions (kind, root, profile, level, server, seed, created, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                s.kind,
                bytes(&s.root),
                s.profile,
                s.level,
                s.server,
                s.seed,
                now()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn session(&self, id: SessionId) -> Result<Option<SessionRow>, StoreError> {
        let sql = format!("{SELECT_SESSION} WHERE s.id = ?1");
        Ok(self.conn.query_row(&sql, [id], session_row).optional()?)
    }

    /// The sessions of a project, or all with `None`; the most recent first.
    pub fn sessions(&self, root: Option<&Path>, limit: u32) -> Result<Vec<SessionRow>, StoreError> {
        let rows = match root {
            Some(root) => {
                let sql = format!(
                    "{SELECT_SESSION} WHERE s.root = ?1 ORDER BY s.updated DESC, s.id DESC LIMIT ?2"
                );
                let mut st = self.conn.prepare(&sql)?;
                st.query_map(params![bytes(root), limit], session_row)?
                    .collect::<Result<_, _>>()?
            }
            None => {
                let sql = format!("{SELECT_SESSION} ORDER BY s.updated DESC, s.id DESC LIMIT ?1");
                let mut st = self.conn.prepare(&sql)?;
                st.query_map([limit], session_row)?
                    .collect::<Result<_, _>>()?
            }
        };
        Ok(rows)
    }

    /// The most recent session of `kind` in a project.
    pub fn latest(&self, root: &Path, kind: &str) -> Result<Option<SessionRow>, StoreError> {
        let sql = format!(
            "{SELECT_SESSION} WHERE s.root = ?1 AND s.kind = ?2 ORDER BY s.updated DESC, s.id DESC LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(&sql, params![bytes(root), kind], session_row)
            .optional()?)
    }

    /// Sessions whose messages or answers hold every word of `query`, the best match
    /// first, each with a snippet around the words.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<(SessionId, String)>, StoreError> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.conn.prepare(
            "SELECT session, snippet(transcript, 0, '[', ']', '…', 12) FROM transcript
             WHERE transcript MATCH ?1 ORDER BY rank",
        )?;
        let mut hits: Vec<(SessionId, String)> = Vec::new();
        let mut rows = st.query([words.join(" ")])?;
        while let Some(row) = rows.next()? {
            let session: SessionId = row.get(0)?;
            if !hits.iter().any(|(s, _)| *s == session) {
                hits.push((session, row.get(1)?));
                if hits.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(hits)
    }

    /// The user's answer about exactly these allow rules of a project, if they gave one.
    pub fn trusted(&self, root: &Path, rules: &str) -> Result<Option<bool>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT trusted FROM trusted_rules WHERE root = ?1 AND rules = ?2",
                params![bytes(root), rules],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_trusted(
        &mut self,
        root: &Path,
        rules: &str,
        trusted: bool,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO trusted_rules (root, rules, trusted, at) VALUES (?1, ?2, ?3, ?4)",
            params![bytes(root), rules, trusted, now()],
        )?;
        Ok(())
    }

    /// Removes a session and everything kept for it.
    pub fn delete_session(&mut self, id: SessionId) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM transcript WHERE session = ?1", [id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        tx.execute(DROP_UNUSED_BLOBS, [])?;
        tx.commit()?;
        Ok(())
    }

    /// Notes that turn number `turns` began; the first one's message names the session.
    pub fn begin_turn(&mut self, id: SessionId, turns: u32, title: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET turns = ?2, updated = ?3,
             title = CASE WHEN title = '' THEN ?4 ELSE title END WHERE id = ?1",
            params![id, turns, now(), title],
        )?;
        Ok(())
    }

    /// Notes the end of a turn, with the counters after it.
    pub fn end_turn(&mut self, id: SessionId, stats: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET stats = ?2, updated = ?3 WHERE id = ?1",
            params![id, stats, now()],
        )?;
        Ok(())
    }

    pub fn start_conversation(
        &mut self,
        id: SessionId,
        n: u32,
        state: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO conversations (session, n, started, state) VALUES (?1, ?2, ?3, ?4)",
            params![id, n, now(), state],
        )?;
        self.touch(id)
    }

    pub fn set_conversation_state(
        &mut self,
        id: SessionId,
        n: u32,
        state: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE conversations SET state = ?3 WHERE session = ?1 AND n = ?2",
            params![id, n, state],
        )?;
        Ok(())
    }

    /// Writes the messages of a conversation from index `from` on, over what is stored
    /// there: a conversation only grows, but its last message may gain a note.
    pub fn put_messages(
        &mut self,
        id: SessionId,
        n: u32,
        from: usize,
        bodies: &[String],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for (i, body) in bodies.iter().enumerate() {
            tx.execute(
                "INSERT OR REPLACE INTO messages (session, conversation, idx, body)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, n, (from + i) as i64, body],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET updated = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Makes a user's message or an answer findable by [`Store::search`].
    pub fn index(
        &mut self,
        id: SessionId,
        n: u32,
        role: &str,
        text: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO transcript (text, session, conversation, role) VALUES (?1, ?2, ?3, ?4)",
            params![text, id, n, role],
        )?;
        Ok(())
    }

    /// Adds paths of one kind to a conversation and removes others.
    pub fn update_paths(
        &mut self,
        id: SessionId,
        n: u32,
        kind: &str,
        added: &[PathBuf],
        removed: &[PathBuf],
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for p in added {
            tx.execute(
                "INSERT OR IGNORE INTO paths (session, conversation, kind, path) VALUES (?1, ?2, ?3, ?4)",
                params![id, n, kind, bytes(p)],
            )?;
        }
        for p in removed {
            tx.execute(
                "DELETE FROM paths WHERE session = ?1 AND conversation = ?2 AND kind = ?3 AND path = ?4",
                params![id, n, kind, bytes(p)],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The session's last conversation, as stored.
    pub fn last_conversation(&self, id: SessionId) -> Result<Option<Conversation>, StoreError> {
        let Some((n, state)) = self
            .conn
            .query_row(
                "SELECT n, state FROM conversations WHERE session = ?1 ORDER BY n DESC LIMIT 1",
                [id],
                |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut st = self.conn.prepare(
            "SELECT body FROM messages WHERE session = ?1 AND conversation = ?2 ORDER BY idx",
        )?;
        let messages = st
            .query_map(params![id, n], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let mut st = self.conn.prepare(
            "SELECT kind, path FROM paths WHERE session = ?1 AND conversation = ?2 ORDER BY kind, path",
        )?;
        let paths = st
            .query_map(params![id, n], |r| {
                Ok((r.get::<_, String>(0)?, path_of(r.get(1)?)))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Some(Conversation {
            n,
            messages,
            state,
            paths,
        }))
    }

    pub fn append_event(
        &mut self,
        id: SessionId,
        turn: u32,
        kind: &str,
        data: &str,
    ) -> Result<Seq, StoreError> {
        self.conn.execute(
            "INSERT INTO events (session, turn, at, kind, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, turn, now(), kind, data],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The events of a session, in order.
    pub fn events(&self, id: SessionId) -> Result<Vec<EventRow>, StoreError> {
        let mut st = self.conn.prepare(
            "SELECT seq, turn, at, kind, data FROM events WHERE session = ?1 ORDER BY seq",
        )?;
        let rows = st
            .query_map([id], |r| {
                Ok(EventRow {
                    seq: r.get(0)?,
                    turn: r.get(1)?,
                    at: r.get(2)?,
                    kind: r.get(3)?,
                    data: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Saves what changed in a session's edit journal, all of it or nothing, and drops the
    /// contents no journal refers to any more.
    pub fn save_journal(
        &mut self,
        id: SessionId,
        changes: &JournalChanges,
    ) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for f in &changes.put {
            let base = f.base.as_deref().map(|b| put_blob(&tx, b)).transpose()?;
            let current = put_blob(&tx, &f.current)?;
            tx.execute(
                "INSERT OR REPLACE INTO journal_files (session, path, base, current, drifted)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, bytes(&f.path), base, current, f.drifted],
            )?;
        }
        for p in &changes.removed {
            tx.execute(
                "DELETE FROM journal_files WHERE session = ?1 AND path = ?2",
                params![id, bytes(p)],
            )?;
        }
        if let Some(rest) = &changes.rest {
            tx.execute("DELETE FROM journal_dirs WHERE session = ?1", [id])?;
            for d in &rest.created_dirs {
                tx.execute(
                    "INSERT INTO journal_dirs (session, path) VALUES (?1, ?2)",
                    params![id, bytes(d)],
                )?;
            }
            tx.execute("DELETE FROM journal_commands WHERE session = ?1", [id])?;
            for (i, c) in rest.side_effects.iter().enumerate() {
                tx.execute(
                    "INSERT INTO journal_commands (session, idx, command) VALUES (?1, ?2, ?3)",
                    params![id, i as i64, c],
                )?;
            }
            tx.execute(
                "UPDATE sessions SET inherited = ?2 WHERE id = ?1",
                params![id, rest.inherited],
            )?;
        }
        tx.execute(DROP_UNUSED_BLOBS, [])?;
        tx.commit()?;
        Ok(())
    }

    /// A session's edit journal, contents included.
    pub fn journal(&self, id: SessionId) -> Result<SavedJournal, StoreError> {
        let mut st = self.conn.prepare(
            "SELECT path, base, current, drifted FROM journal_files WHERE session = ?1 ORDER BY path",
        )?;
        // the paths and the hashes of their contents first; the contents after
        let rows: Vec<HashedFile> = st
            .query_map([id], |r| {
                Ok(HashedFile {
                    path: r.get(0)?,
                    base: r.get(1)?,
                    current: r.get(2)?,
                    drifted: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut files = Vec::with_capacity(rows.len());
        for f in rows {
            files.push(JournalFile {
                path: path_of(f.path),
                base: f.base.map(|h| self.blob(&h)).transpose()?,
                current: self.blob(&f.current)?,
                drifted: f.drifted,
            });
        }
        let mut st = self
            .conn
            .prepare("SELECT path FROM journal_dirs WHERE session = ?1 ORDER BY path")?;
        let created_dirs = st
            .query_map([id], |r| Ok(path_of(r.get(0)?)))?
            .collect::<Result<_, _>>()?;
        let mut st = self
            .conn
            .prepare("SELECT command FROM journal_commands WHERE session = ?1 ORDER BY idx")?;
        let side_effects = st
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let inherited = self
            .conn
            .query_row("SELECT inherited FROM sessions WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(false);
        Ok(SavedJournal {
            files,
            rest: JournalRest {
                created_dirs,
                side_effects,
                inherited,
            },
        })
    }

    /// The bytes stored under `hash`, checked against it.
    fn blob(&self, hash: &[u8]) -> Result<Vec<u8>, StoreError> {
        let damaged = || StoreError::Blob(hex(hash));
        let data: Vec<u8> = self
            .conn
            .query_row("SELECT data FROM blobs WHERE hash = ?1", [hash], |r| {
                r.get(0)
            })
            .optional()?
            .ok_or_else(damaged)?;
        let bytes = zstd::decode_all(data.as_slice()).map_err(|_| damaged())?;
        if blake3::hash(&bytes).as_bytes().as_slice() != hash {
            return Err(damaged());
        }
        Ok(bytes)
    }

    fn touch(&mut self, id: SessionId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET updated = ?2 WHERE id = ?1",
            params![id, now()],
        )?;
        Ok(())
    }
}

/// Stores `bytes` once, compressed, and returns their hash.
fn put_blob(tx: &Transaction<'_>, bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
    let hash = blake3::hash(bytes).as_bytes().to_vec();
    let known: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM blobs WHERE hash = ?1)",
        [&hash],
        |r| r.get(0),
    )?;
    if !known {
        let data = zstd::encode_all(bytes, ZSTD_LEVEL).map_err(|e| StoreError::Io {
            path: "zstd".into(),
            source: e,
        })?;
        tx.execute(
            "INSERT INTO blobs (hash, size, data) VALUES (?1, ?2, ?3)",
            params![hash, bytes.len() as i64, data],
        )?;
    }
    Ok(hash)
}

fn session_row(r: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        root: path_of(r.get(2)?),
        profile: r.get(3)?,
        level: r.get(4)?,
        server: r.get(5)?,
        seed: r.get(6)?,
        title: r.get(7)?,
        created: r.get(8)?,
        updated: r.get(9)?,
        updated_local: r.get(10)?,
        turns: r.get(11)?,
        stats: r.get(12)?,
        unreviewed_files: r.get(13)?,
    })
}

fn bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_bytes()
}

fn path_of(b: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(b))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Unix milliseconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn new(root: &str, kind: &str) -> NewSession {
        NewSession {
            kind: kind.into(),
            root: PathBuf::from(root),
            profile: "gpt-oss-20b".into(),
            level: Some("high".into()),
            server: "http://127.0.0.1:8090".into(),
            seed: 7,
        }
    }

    #[test]
    fn a_new_store_is_private_and_opens_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/ognivo.db");
        let mut store = Store::open(&path).unwrap();
        let id = store.create_session(&new("/p", "chat")).unwrap();
        drop(store);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.session(id).unwrap().unwrap().profile, "gpt-oss-20b");
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ognivo.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", VERSION + 1)
            .unwrap();
        assert!(matches!(Store::open(&path), Err(StoreError::Newer(v)) if v == VERSION + 1));
    }

    #[test]
    fn an_older_store_is_brought_up_to_date_and_keeps_its_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ognivo.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO sessions (kind, root, profile, server, seed, created, updated)
                 VALUES ('chat', x'2f70', 'gpt-oss-20b', 'u', 1, 0, 0)",
                [],
            )
            .unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.session(1).unwrap().unwrap().root, Path::new("/p"));
        assert_eq!(store.trusted(Path::new("/p"), "r").unwrap(), None);
        store.set_trusted(Path::new("/p"), "r", true).unwrap();
        assert_eq!(store.trusted(Path::new("/p"), "r").unwrap(), Some(true));
        // another set of rules is another question
        assert_eq!(store.trusted(Path::new("/p"), "r2").unwrap(), None);
        store.set_trusted(Path::new("/p"), "r", false).unwrap();
        assert_eq!(store.trusted(Path::new("/p"), "r").unwrap(), Some(false));
    }

    #[test]
    fn sessions_are_listed_by_project_newest_first() {
        let mut store = Store::memory().unwrap();
        let a = store.create_session(&new("/p", "chat")).unwrap();
        let b = store.create_session(&new("/q", "chat")).unwrap();
        let c = store.create_session(&new("/p", "run")).unwrap();
        store.begin_turn(a, 1, "fix the bug").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        store.begin_turn(a, 2, "not this one").unwrap();
        store.end_turn(a, r#"{"steps":3}"#).unwrap();
        let ids = |rows: Vec<SessionRow>| rows.iter().map(|r| r.id).collect::<Vec<_>>();
        // `a` was updated last; ties go to the newer session
        assert_eq!(
            ids(store.sessions(Some(Path::new("/p")), 10).unwrap()),
            [a, c]
        );
        assert_eq!(ids(store.sessions(None, 2).unwrap()), [a, c]);
        assert_eq!(
            store.latest(Path::new("/p"), "chat").unwrap().unwrap().id,
            a
        );
        assert_eq!(store.latest(Path::new("/q"), "run").unwrap(), None);
        let row = store.session(a).unwrap().unwrap();
        assert_eq!(
            (row.title.as_str(), row.turns, row.stats.as_str()),
            ("fix the bug", 2, r#"{"steps":3}"#)
        );
        assert_eq!(row.updated_local.len(), "2026-11-02 14:03".len());
        assert_eq!(store.session(b).unwrap().unwrap().root, Path::new("/q"));
    }

    #[test]
    fn a_conversation_keeps_its_messages_its_paths_and_its_state() {
        let mut store = Store::memory().unwrap();
        let id = store.create_session(&new("/p", "chat")).unwrap();
        store.start_conversation(id, 1, "{}").unwrap();
        store
            .put_messages(id, 1, 0, &["s".into(), "u".into(), "t".into()])
            .unwrap();
        // the last one gained a note; a new one came
        store
            .put_messages(id, 1, 2, &["t + note".into(), "a".into()])
            .unwrap();
        let odd = PathBuf::from(OsString::from_vec(b"/p/caf\xe9".to_vec()));
        store
            .update_paths(id, 1, "seen", &[odd.clone(), "/p/b".into()], &[])
            .unwrap();
        store
            .update_paths(id, 1, "seen", &[], &["/p/b".into()])
            .unwrap();
        store.set_conversation_state(id, 1, r#"{"x":1}"#).unwrap();
        store.start_conversation(id, 2, "{}").unwrap();
        store.put_messages(id, 2, 0, &["s2".into()]).unwrap();
        let last = store.last_conversation(id).unwrap().unwrap();
        assert_eq!((last.n, last.messages), (2, vec!["s2".to_owned()]));
        // the first is still there, as it was
        let mut conn_rows = store
            .conn
            .prepare(
                "SELECT body FROM messages WHERE session = ?1 AND conversation = 1 ORDER BY idx",
            )
            .unwrap();
        let first: Vec<String> = conn_rows
            .query_map([id], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(first, ["s", "u", "t + note", "a"]);
        drop(conn_rows);
        let state: String = store
            .conn
            .query_row(
                "SELECT state FROM conversations WHERE session = ?1 AND n = 1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(state, r#"{"x":1}"#);
        let paths: Vec<(String, PathBuf)> = {
            let mut st = store
                .conn
                .prepare("SELECT kind, path FROM paths WHERE session = ?1 AND conversation = 1")
                .unwrap();
            st.query_map([id], |r| Ok((r.get(0)?, path_of(r.get(1)?))))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(paths, [("seen".to_owned(), odd)]);
    }

    #[test]
    fn events_are_numbered_across_sessions() {
        let mut store = Store::memory().unwrap();
        let a = store.create_session(&new("/p", "chat")).unwrap();
        let b = store.create_session(&new("/p", "run")).unwrap();
        let s1 = store
            .append_event(a, 1, "user", r#"{"text":"hi"}"#)
            .unwrap();
        let s2 = store.append_event(b, 1, "user", "{}").unwrap();
        let s3 = store
            .append_event(a, 1, "text", r#"{"text":"hello"}"#)
            .unwrap();
        assert_eq!((s1, s2, s3), (1, 2, 3));
        let events = store.events(a).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|e| (e.seq, e.kind.as_str()))
                .collect::<Vec<_>>(),
            [(1, "user"), (3, "text")]
        );
    }

    #[test]
    fn the_journal_keeps_exact_bytes_once_and_drops_what_nothing_needs() {
        let mut store = Store::memory().unwrap();
        let a = store.create_session(&new("/p", "chat")).unwrap();
        let b = store.create_session(&new("/p", "chat")).unwrap();
        let file = |path: &str, base: Option<&[u8]>, current: &[u8]| JournalFile {
            path: path.into(),
            base: base.map(<[u8]>::to_vec),
            current: current.to_vec(),
            drifted: false,
        };
        let latin1 = b"caf\xe9\r\n".as_slice();
        store
            .save_journal(
                a,
                &JournalChanges {
                    put: vec![
                        file("/p/x", Some(latin1), b"new"),
                        file("/p/made", None, b""),
                    ],
                    removed: vec![],
                    rest: Some(JournalRest {
                        created_dirs: vec!["/p/d".into()],
                        side_effects: vec!["touch y".into()],
                        inherited: true,
                    }),
                },
            )
            .unwrap();
        // the same content in another session is stored once
        store
            .save_journal(
                b,
                &JournalChanges {
                    put: vec![file("/p/x", Some(latin1), b"other")],
                    ..JournalChanges::default()
                },
            )
            .unwrap();
        let blobs = |s: &Store| -> i64 {
            s.conn
                .query_row("SELECT count(*) FROM blobs", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(blobs(&store), 4);
        let saved = store.journal(a).unwrap();
        assert_eq!(
            saved.files,
            [
                file("/p/made", None, b""),
                file("/p/x", Some(latin1), b"new")
            ]
        );
        assert_eq!(
            saved.rest,
            JournalRest {
                created_dirs: vec!["/p/d".into()],
                side_effects: vec!["touch y".into()],
                inherited: true,
            }
        );
        store
            .save_journal(
                a,
                &JournalChanges {
                    removed: vec!["/p/x".into(), "/p/made".into()],
                    ..JournalChanges::default()
                },
            )
            .unwrap();
        // `b` still needs the shared base and its own content; "new" and "" are gone
        assert_eq!(blobs(&store), 2);
        assert_eq!(
            store.journal(b).unwrap().files,
            [file("/p/x", Some(latin1), b"other")]
        );
        assert_eq!(store.session(a).unwrap().unwrap().unreviewed_files, 0);
        assert_eq!(store.session(b).unwrap().unwrap().unreviewed_files, 1);
        // a session removed takes its contents with it
        store.delete_session(b).unwrap();
        assert_eq!(blobs(&store), 0);
        assert_eq!(store.session(b).unwrap(), None);
    }

    #[test]
    fn a_damaged_blob_is_an_error_not_wrong_bytes() {
        let mut store = Store::memory().unwrap();
        let a = store.create_session(&new("/p", "chat")).unwrap();
        store
            .save_journal(
                a,
                &JournalChanges {
                    put: vec![JournalFile {
                        path: "/p/x".into(),
                        base: Some(b"old".to_vec()),
                        current: b"new".to_vec(),
                        drifted: false,
                    }],
                    ..JournalChanges::default()
                },
            )
            .unwrap();
        let other = zstd::encode_all(b"evil".as_slice(), ZSTD_LEVEL).unwrap();
        store
            .conn
            .execute(
                "UPDATE blobs SET data = ?1 WHERE size = 3 AND hash = ?2",
                params![other, blake3::hash(b"old").as_bytes().as_slice()],
            )
            .unwrap();
        assert!(matches!(store.journal(a), Err(StoreError::Blob(_))));
    }

    #[test]
    fn search_finds_sessions_by_the_words_of_their_transcript() {
        let mut store = Store::memory().unwrap();
        let a = store.create_session(&new("/p", "chat")).unwrap();
        let b = store.create_session(&new("/p", "chat")).unwrap();
        for id in [a, b] {
            store.start_conversation(id, 1, "{}").unwrap();
        }
        store
            .index(a, 1, "user", "fix the February bug in timefmt")
            .unwrap();
        store
            .index(a, 1, "answer", "Fixed: month <= 2 now, and \"tests\" pass")
            .unwrap();
        store.index(b, 1, "user", "rename the crate").unwrap();
        store.start_conversation(b, 2, "{}").unwrap();
        store.put_messages(b, 2, 0, &["m".into()]).unwrap();
        store.append_event(b, 1, "user", "{}").unwrap();
        store.delete_session(b).unwrap();
        assert!(store.search("rename", 10).unwrap().is_empty());
        assert!(store.events(b).unwrap().is_empty());
        let left: i64 = store
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        let hits = store.search("february TIMEFMT", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, a);
        assert!(hits[0].1.contains("[February]"), "{}", hits[0].1);
        // quotes and operators are words, not syntax
        assert_eq!(store.search("\"tests\" AND", 10).unwrap().len(), 1);
        assert!(store.search("  ", 10).unwrap().is_empty());
    }
}
