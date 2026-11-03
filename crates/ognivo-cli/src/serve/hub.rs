//! What the core's connections and threads share: the clients, the stream of events with
//! its numbering and its log in the store, the questions waiting for an answer, and the
//! open threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use ognivo_core::protocol::Out;
use ognivo_protocol::{self as p, rpc};
use ognivo_store::Store;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::thread::ThreadHandle;
use crate::session::{self, Failure};

/// How long the shutdown waits for a thread to close.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
pub struct Hub {
    pub data_dir: PathBuf,
    /// For what the clients read: threads, their events, their trajectories.
    pub reader: Mutex<Store>,
    // the locks are taken in this order: log, clients, asks
    log: Mutex<Log>,
    clients: Mutex<HashMap<u64, Client>>,
    asks: Mutex<Asks>,
    pub threads: Mutex<HashMap<i64, ThreadHandle>>,
    /// Clients connected, and turns running.
    activity: watch::Sender<(usize, usize)>,
    next_client: AtomicU64,
}

#[derive(Debug)]
struct Log {
    store: Store,
    /// The last event's place in the stream.
    seq: u64,
    /// The store failed once: said once.
    failed: bool,
}

#[derive(Debug)]
struct Client {
    out: mpsc::UnboundedSender<String>,
    /// It called `initialize`: it gets the stream and the questions.
    ready: bool,
}

#[derive(Debug, Default)]
struct Asks {
    next: u64,
    waiting: HashMap<u64, Waiting>,
}

#[derive(Debug)]
struct Waiting {
    /// The request as the clients get it.
    message: String,
    answer: oneshot::Sender<Option<Value>>,
}

impl Hub {
    pub fn new(data_dir: PathBuf) -> Result<Self, Failure> {
        let store = session::open_store(&data_dir)?;
        let reader = session::open_store(&data_dir)?;
        let seq = store
            .last_wire()
            .map_err(|e| Failure::usage(format!("the session store: {e}")))?;
        Ok(Self {
            data_dir,
            reader: Mutex::new(reader),
            log: Mutex::new(Log {
                store,
                seq: seq.max(0) as u64,
                failed: false,
            }),
            clients: Mutex::new(HashMap::new()),
            asks: Mutex::new(Asks::default()),
            threads: Mutex::new(HashMap::new()),
            activity: watch::Sender::new((0, 0)),
            next_client: AtomicU64::new(1),
        })
    }

    pub fn activity(&self) -> watch::Receiver<(usize, usize)> {
        self.activity.subscribe()
    }

    /// A turn began (`1`) or ended (`-1`).
    pub fn running(&self, by: isize) {
        self.activity
            .send_modify(|(_, turns)| *turns = turns.saturating_add_signed(by));
    }

    pub fn connect(&self, out: mpsc::UnboundedSender<String>) -> u64 {
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        lock(&self.clients).insert(id, Client { out, ready: false });
        self.activity.send_modify(|(clients, _)| *clients += 1);
        id
    }

    pub fn disconnect(&self, client: u64) {
        lock(&self.clients).remove(&client);
        self.activity
            .send_modify(|(clients, _)| *clients = clients.saturating_sub(1));
    }

    /// The client gets the stream from the next event on, and the questions that wait;
    /// returns the last event's place.
    pub fn ready(&self, client: u64) -> u64 {
        let log = lock(&self.log);
        let mut clients = lock(&self.clients);
        let Some(c) = clients.get_mut(&client) else {
            return log.seq;
        };
        c.ready = true;
        for w in lock(&self.asks).waiting.values() {
            let _ = c.out.send(w.message.clone());
        }
        log.seq
    }

    /// A message to one client.
    pub fn send(&self, client: u64, line: String) {
        if let Some(c) = lock(&self.clients).get(&client) {
            let _ = c.out.send(line);
        }
    }

    /// Numbers `out`, keeps it in the store but the deltas, and sends it to the clients.
    /// `root` is the user's thread whose events these are, its subagents' included.
    pub fn emit(&self, root: p::ThreadId, out: Out) {
        if out.is_empty() {
            return;
        }
        let mut log = lock(&self.log);
        let clients = lock(&self.clients);
        for (thread, kind) in out {
            log.seq += 1;
            let keep = !matches!(kind, p::EventKind::ItemDelta { .. });
            let event = p::Event {
                seq: log.seq,
                thread,
                kind,
            };
            let Ok(data) = serde_json::to_string(&event) else {
                continue;
            };
            if keep {
                let seq = log.seq as i64;
                if let Err(e) = log.store.append_wire(seq, thread.0, root.0, &data)
                    && !std::mem::replace(&mut log.failed, true)
                {
                    eprintln!(
                        "ognivo core: the session store failed: {e}; events are no longer kept"
                    );
                }
            }
            let line = format!(
                r#"{{"jsonrpc":"2.0","method":"{}","params":{data}}}"#,
                rpc::EVENT
            );
            for c in clients.values().filter(|c| c.ready) {
                let _ = c.out.send(line.clone());
            }
        }
    }

    /// Asks the clients what only the user can answer: those there now and those that come
    /// while it waits. The first answer counts; `None` when none can come. A question
    /// dropped before its answer is withdrawn.
    pub async fn ask(&self, method: &str, params: Value) -> Option<Value> {
        let (answer, answered) = oneshot::channel();
        let id = {
            let clients = lock(&self.clients);
            let mut asks = lock(&self.asks);
            asks.next += 1;
            let id = asks.next;
            let message =
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
            for c in clients.values().filter(|c| c.ready) {
                let _ = c.out.send(message.clone());
            }
            asks.waiting.insert(id, Waiting { message, answer });
            id
        };
        let _withdraw = Withdraw { hub: self, id };
        answered.await.ok().flatten()
    }

    /// A client's answer to question `id`; `None` for an error, which is no answer.
    pub fn answer(&self, id: u64, result: Option<Value>) {
        let Some(result) = result else { return };
        let waiting = lock(&self.asks).waiting.remove(&id);
        if let Some(w) = waiting {
            let _ = w.answer.send(Some(result));
            // the others' question is answered
            self.cancel(id);
        }
    }

    fn cancel(&self, id: u64) {
        let line = json!({"jsonrpc": "2.0", "method": rpc::CANCEL, "params": rpc::Cancel { id }})
            .to_string();
        for c in lock(&self.clients).values().filter(|c| c.ready) {
            let _ = c.out.send(line.clone());
        }
    }

    /// Closes the open threads, each as `/exit` closes a chat: a running turn stops.
    pub async fn shutdown(&self) {
        let handles: Vec<ThreadHandle> = lock(&self.threads).drain().map(|(_, h)| h).collect();
        for h in handles {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, h.shut()).await;
        }
    }
}

/// Withdraws a question when its asker drops it unanswered.
struct Withdraw<'a> {
    hub: &'a Hub,
    id: u64,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        let waiting = lock(&self.hub.asks).waiting.remove(&self.id);
        if waiting.is_some() {
            self.hub.cancel(self.id);
        }
    }
}
