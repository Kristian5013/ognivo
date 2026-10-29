//! Event model shared by the ognivo core and every client (Electron GUI, VS Code
//! extension, CLI).
//!
//! A thread holds turns, a turn holds items, and an item goes `started` → deltas →
//! `completed` (docs/architecture.md §3). Events are append-only and numbered by `seq`,
//! so a client reconnects by replaying from the last `seq` it saw. The model freezes at
//! the end of M2; until then it is expected to change.

use serde::{Deserialize, Serialize};

/// One conversation, bound to one model for its whole life.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ThreadId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub u64);

/// Position in the append-only event log. Global and dense across all threads: a client
/// replays everything after the last seq it saw and filters by thread.
pub type Seq = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: Seq,
    pub thread: ThreadId,
    pub turn: TurnId,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    TurnStarted,
    ItemStarted { item: ItemId, kind: ItemKind },
    ItemDelta { item: ItemId, delta: Delta },
    ItemCompleted { item: ItemId },
    TurnCompleted { stop: StopReason },
}

/// What an item is; its payload arrives through deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    UserMessage,
    /// Text the model shows the user: a Harmony preamble or the final answer.
    AgentMessage,
    /// Chain of thought: Harmony analysis, the Gemma thought channel, Qwen `<think>`.
    Reasoning,
    ToolCall,
    CommandExecution,
    FileChange,
    Plan,
    ApprovalRequest,
    Compaction,
    Usage,
    Notice,
}

/// Raw token ids are not part of the client protocol: the store keeps them, sealed, next
/// to the event (ognivo-store `Record`), and the Inspector fetches them on demand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    Text { text: String },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Completed,
    Interrupted,
    WaitingForApproval,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_wire_shape() {
        let ev = Event {
            seq: 7,
            thread: ThreadId("t1".into()),
            turn: TurnId(2),
            kind: EventKind::ItemDelta {
                item: ItemId(3),
                delta: Delta::Text { text: "hi".into() },
            },
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "seq": 7, "thread": "t1", "turn": 2,
                "event": "item_delta", "item": 3,
                "delta": {"type": "text", "text": "hi"}
            })
        );
        assert_eq!(serde_json::from_value::<Event>(json).unwrap(), ev);
    }
}
