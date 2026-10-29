//! Append-only event log (docs/architecture.md §11).
//!
//! The SQLite implementation arrives in M1; [`MemoryLog`] serves tests and replay until
//! then. Each stored [`Record`] keeps the client-facing event plus what only the core
//! needs for verbatim replay: the item's token ids after `Codec::seal`, and the codec
//! version that produced them (nullable from day one, so the Native path needs no
//! migration).

use ognivo_protocol::{Event, Seq, ThreadId};

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub event: Event,
    /// Sealed token ids of a completed item (Native path); `None` on the Generic path.
    pub sealed_ids: Option<Vec<u32>>,
    /// Codec name and semver that rendered or sealed the item, e.g. `harmony@0.1.0`.
    pub codec: Option<String>,
}

impl From<Event> for Record {
    fn from(event: Event) -> Self {
        Record {
            event,
            sealed_ids: None,
            codec: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("event seq {got} breaks the log order (expected {expected})")]
    OutOfOrder { expected: Seq, got: Seq },
}

pub trait EventLog {
    /// Appends a record. Seqs are global, dense and start at 1, so a gap means a lost event.
    fn append(&mut self, record: Record) -> Result<(), StoreError>;

    /// Records of `thread` with `seq > after`, in order: what a reconnecting client replays.
    fn read_after(&self, thread: &ThreadId, after: Seq) -> Vec<Record>;

    fn last_seq(&self) -> Seq;
}

#[derive(Debug, Default)]
pub struct MemoryLog {
    records: Vec<Record>,
}

impl EventLog for MemoryLog {
    fn append(&mut self, record: Record) -> Result<(), StoreError> {
        let expected = self.last_seq() + 1;
        if record.event.seq != expected {
            return Err(StoreError::OutOfOrder {
                expected,
                got: record.event.seq,
            });
        }
        self.records.push(record);
        Ok(())
    }

    fn read_after(&self, thread: &ThreadId, after: Seq) -> Vec<Record> {
        self.records
            .iter()
            .filter(|r| r.event.seq > after && &r.event.thread == thread)
            .cloned()
            .collect()
    }

    fn last_seq(&self) -> Seq {
        self.records.last().map_or(0, |r| r.event.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ognivo_protocol::{EventKind, ItemId, TurnId};

    fn ev(seq: Seq, thread: &str) -> Event {
        Event {
            seq,
            thread: ThreadId(thread.into()),
            turn: TurnId(1),
            kind: EventKind::TurnStarted,
        }
    }

    #[test]
    fn replay_after_seq() {
        let mut log = MemoryLog::default();
        for (seq, thread) in [(1, "a"), (2, "b"), (3, "a")] {
            log.append(ev(seq, thread).into()).unwrap();
        }
        let replay = log.read_after(&ThreadId("a".into()), 1);
        assert_eq!(
            replay.iter().map(|r| r.event.seq).collect::<Vec<_>>(),
            vec![3]
        );
    }

    #[test]
    fn keeps_sealed_ids_next_to_the_event() {
        let mut log = MemoryLog::default();
        let done = Event {
            seq: 1,
            thread: ThreadId("a".into()),
            turn: TurnId(1),
            kind: EventKind::ItemCompleted { item: ItemId(1) },
        };
        let record = Record {
            event: done,
            sealed_ids: Some(vec![200005, 17]),
            codec: Some("harmony@0.1.0".into()),
        };
        log.append(record.clone()).unwrap();
        assert_eq!(log.read_after(&ThreadId("a".into()), 0), vec![record]);
    }

    #[test]
    fn rejects_gaps() {
        let mut log = MemoryLog::default();
        log.append(ev(1, "a").into()).unwrap();
        assert!(matches!(
            log.append(ev(3, "a").into()),
            Err(StoreError::OutOfOrder {
                expected: 2,
                got: 3
            })
        ));
    }
}
