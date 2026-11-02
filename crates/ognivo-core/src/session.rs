//! A session kept in the store (docs/architecture.md §11). The agent saves its conversation,
//! events, edit journal and counters as it goes, so `chat --resume` picks a session up
//! where it ended, after a crash too:
//! - the messages after every call result and every note: a saved message changes only
//!   while it is the last one (a note is added to it), so a save rewrites that one and
//!   adds the new ones;
//! - the events as they happen, a run of streamed text or reasoning as one event, and the
//!   user's message, every answer to a question and the end of each turn with them;
//! - the edit journal after every call that changed it, file contents as blobs.
//!
//! When the store fails, the session goes on in memory, and [`Session::failure`] says so
//! once.

use std::collections::HashSet;
use std::path::PathBuf;

use ognivo_models::ChatMessage;
use ognivo_store::{JournalChanges, NewSession, SavedJournal, SessionId, Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{AgentEvent, ApprovalRequest, Stats};
use crate::instructions::{Given, Skipped};

/// The kinds of paths a conversation keeps: files read or written, and directories whose
/// instruction files it got.
const SEEN: &str = "seen";
const LOOKED: &str = "looked";
/// Characters of a session's title.
const TITLE_CHARS: usize = 80;

#[derive(Debug)]
pub struct Session {
    store: Store,
    id: SessionId,
    /// The conversation being saved; `/clear` starts the next.
    conversation: u32,
    turns: u32,
    /// Messages of the conversation saved so far, and the last of them as saved.
    saved: usize,
    last: Option<String>,
    seen: HashSet<PathBuf>,
    looked: HashSet<PathBuf>,
    /// The conversation's notes as saved.
    notes: String,
    /// Streamed text or reasoning not in the log yet.
    pending: Option<(&'static str, String)>,
    failed: bool,
    /// The store's first failure, until it is reported.
    failure: Option<String>,
}

/// What the agent keeps about a conversation besides its messages.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notes {
    /// The instruction files of its system prompt, and those it could not take.
    pub instructions: Vec<Given>,
    pub skipped: Vec<Skipped>,
    /// Files the user reverted since the model's last turn.
    pub reverted: Vec<String>,
    /// The user's requests, oldest first: a compaction quotes them.
    pub requests: Vec<String>,
    /// A turn began and did not end: ognivo stopped in it.
    pub in_turn: bool,
}

/// What a stored session left for the agent to go on with.
#[derive(Debug, Clone)]
pub struct Resumed {
    /// The last conversation; empty if none was saved.
    pub messages: Vec<ChatMessage>,
    pub notes: Notes,
    pub seen: Vec<PathBuf>,
    pub looked: Vec<PathBuf>,
    pub journal: SavedJournal,
    pub stats: Stats,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("there is no session {0}")]
    NotFound(SessionId),
    #[error("session {id} cannot be read back: {why}")]
    Damaged { id: SessionId, why: String },
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl Session {
    /// A new session in `store`.
    pub fn create(mut store: Store, new: &NewSession) -> Result<Self, StoreError> {
        let id = store.create_session(new)?;
        Ok(Self::at(store, id, 0, 0))
    }

    /// A stored session, ready to go on, and what it left.
    pub fn open(store: Store, id: SessionId) -> Result<(Self, Resumed), SessionError> {
        let row = store.session(id)?.ok_or(SessionError::NotFound(id))?;
        let damaged = |why: String| SessionError::Damaged { id, why };
        let stats = serde_json::from_str(&row.stats).map_err(|e| damaged(e.to_string()))?;
        let journal = store.journal(id)?;
        let conversation = store.last_conversation(id)?;
        let n = conversation.as_ref().map_or(0, |c| c.n);
        let mut s = Self::at(store, id, n, row.turns);
        let mut resumed = Resumed {
            messages: Vec::new(),
            notes: Notes::default(),
            seen: Vec::new(),
            looked: Vec::new(),
            journal,
            stats,
        };
        if let Some(c) = conversation {
            resumed.messages = c
                .messages
                .iter()
                .map(|m| serde_json::from_str(m))
                .collect::<Result<_, _>>()
                .map_err(|e| damaged(e.to_string()))?;
            resumed.notes = serde_json::from_str(&c.state).map_err(|e| damaged(e.to_string()))?;
            for (kind, path) in c.paths {
                match kind.as_str() {
                    SEEN => resumed.seen.push(path),
                    LOOKED => resumed.looked.push(path),
                    _ => {}
                }
            }
            s.saved = c.messages.len();
            s.last = c.messages.last().cloned();
            s.seen = resumed.seen.iter().cloned().collect();
            s.looked = resumed.looked.iter().cloned().collect();
            s.notes = c.state;
        }
        Ok((s, resumed))
    }

    fn at(store: Store, id: SessionId, conversation: u32, turns: u32) -> Self {
        Self {
            store,
            id,
            conversation,
            turns,
            saved: 0,
            last: None,
            seen: HashSet::new(),
            looked: HashSet::new(),
            notes: String::new(),
            pending: None,
            failed: false,
            failure: None,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn turns(&self) -> u32 {
        self.turns
    }

    /// Why the store stopped saving the session, once.
    pub fn failure(&mut self) -> Option<String> {
        self.failure.take()
    }

    /// Ends the keeping of the session: one without a turn leaves nothing behind.
    pub fn close(mut self) {
        self.flush();
        if self.turns == 0 && !self.failed {
            let r = self.store.delete_session(self.id);
            self.ok(r);
        }
    }

    /// Starts the next conversation with its first messages.
    pub(crate) fn start_conversation(&mut self, messages: &[ChatMessage], notes: &Notes) {
        self.flush();
        self.conversation += 1;
        self.saved = 0;
        self.last = None;
        self.seen.clear();
        self.looked.clear();
        if self.failed {
            return;
        }
        let state = to_json(notes);
        let r = self
            .store
            .start_conversation(self.id, self.conversation, &state);
        if self.ok(r).is_some() {
            self.notes = state;
            self.save_messages(messages);
        }
    }

    /// Saves the messages that are new or changed since the last save.
    pub(crate) fn save_messages(&mut self, messages: &[ChatMessage]) {
        if self.failed {
            return;
        }
        let keeps_last = self.saved > 0
            && messages
                .get(self.saved - 1)
                .is_some_and(|m| self.last.as_deref() == Some(to_json(m).as_str()));
        let from = if keeps_last {
            self.saved
        } else {
            self.saved.saturating_sub(1)
        }
        .min(messages.len());
        if from == messages.len() {
            return;
        }
        let mut bodies: Vec<String> = messages[from..].iter().map(to_json).collect();
        let r = self
            .store
            .put_messages(self.id, self.conversation, from, &bodies);
        if self.ok(r).is_some() {
            self.saved = messages.len();
            self.last = bodies.pop();
        }
    }

    /// Saves the files read or written and the directories looked into, as they are now.
    pub(crate) fn save_paths(&mut self, seen: &HashSet<PathBuf>, looked: &HashSet<PathBuf>) {
        self.save_set(SEEN, seen);
        self.save_set(LOOKED, looked);
    }

    fn save_set(&mut self, kind: &'static str, now: &HashSet<PathBuf>) {
        if self.failed {
            return;
        }
        let saved = if kind == SEEN {
            &self.seen
        } else {
            &self.looked
        };
        let added: Vec<PathBuf> = now.difference(saved).cloned().collect();
        let removed: Vec<PathBuf> = saved.difference(now).cloned().collect();
        if added.is_empty() && removed.is_empty() {
            return;
        }
        let r = self
            .store
            .update_paths(self.id, self.conversation, kind, &added, &removed);
        if self.ok(r).is_some() {
            *(if kind == SEEN {
                &mut self.seen
            } else {
                &mut self.looked
            }) = now.clone();
        }
    }

    pub(crate) fn save_notes(&mut self, notes: &Notes) {
        if self.failed {
            return;
        }
        let state = to_json(notes);
        if state == self.notes {
            return;
        }
        let r = self
            .store
            .set_conversation_state(self.id, self.conversation, &state);
        if self.ok(r).is_some() {
            self.notes = state;
        }
    }

    pub(crate) fn save_journal(&mut self, changes: &JournalChanges) {
        if self.failed || changes.is_empty() {
            return;
        }
        let r = self.store.save_journal(self.id, changes);
        self.ok(r);
    }

    /// A turn begins with the user's message; the first one names the session.
    pub(crate) fn user(&mut self, text: &str) {
        self.flush();
        self.turns += 1;
        if self.failed {
            return;
        }
        let title: String = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .chars()
            .take(TITLE_CHARS)
            .collect();
        let r = self.store.begin_turn(self.id, self.turns, &title);
        self.ok(r);
        self.append(json!({"type": "user", "text": text}));
        let r = self.store.index(self.id, self.conversation, "user", text);
        self.ok(r);
    }

    pub(crate) fn event(&mut self, e: &AgentEvent) {
        match e {
            AgentEvent::Reasoning { text } => self.pend("reasoning", text),
            AgentEvent::Text { text } => self.pend("text", text),
            _ => {
                self.flush();
                if let Ok(v) = serde_json::to_value(e) {
                    self.append(v);
                }
            }
        }
    }

    /// The user's answer to a question.
    pub(crate) fn approval(&mut self, req: &ApprovalRequest, allowed: bool) {
        self.flush();
        self.append(json!({
            "type": "approval",
            "tool": req.tool,
            "action": req.action,
            "reason": req.reason,
            "allowed": allowed,
        }));
    }

    /// A turn ended (`end` says how), with the counters after it.
    pub(crate) fn end_turn(&mut self, mut end: Value, answer: Option<&str>, stats: &Stats) {
        self.flush();
        if self.failed {
            return;
        }
        end["type"] = json!("end");
        self.append(end);
        if let Some(text) = answer {
            let r = self.store.index(self.id, self.conversation, "answer", text);
            self.ok(r);
        }
        let r = self.store.end_turn(self.id, &to_json(stats));
        self.ok(r);
    }

    fn pend(&mut self, kind: &'static str, text: &str) {
        match &mut self.pending {
            Some((k, run)) if *k == kind => run.push_str(text),
            _ => {
                self.flush();
                self.pending = Some((kind, text.to_owned()));
            }
        }
    }

    fn flush(&mut self) {
        if let Some((kind, text)) = self.pending.take() {
            self.append(json!({"type": kind, "text": text}));
        }
    }

    fn append(&mut self, data: Value) {
        if self.failed {
            return;
        }
        let kind = data["type"].as_str().unwrap_or("event").to_owned();
        let r = self
            .store
            .append_event(self.id, self.turns, &kind, &data.to_string());
        self.ok(r);
    }

    /// The value of a store call; its failure stops all saving and waits for the user.
    fn ok<T>(&mut self, r: Result<T, StoreError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                if !self.failed {
                    self.failed = true;
                    self.failure = Some(e.to_string());
                }
                None
            }
        }
    }
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_default()
}
