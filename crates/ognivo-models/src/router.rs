//! The router (docs/architecture.md §4): the servers ognivo works with, the model each
//! runs, and whose turn it is at their slots.
//!
//! A server generates for as many requests at once as it has slots. Each generation step
//! of an agent leases one of them, and when all are taken, the next one let go goes by
//! priority (the main agent, then interactive requests, subagents, service tasks) and,
//! within one priority, to whoever asked first. The main agent never waits: without a free
//! slot its step goes all the same, and the server queues it.
//!
//! Which slot a request runs in, and which KV cache it takes up again, stays the server's.
//! llama-server keeps each conversation's prompt state in host memory and puts it back
//! into whatever slot the conversation comes to, unless the request names a slot with
//! `id_slot`: with `--kv-unified` an idle slot is cleared when another one starts, and a
//! request pinned to it starts from nothing (2026-11-03, b11924: a conversation resumed
//! after another one ran took 0 of 486 prompt tokens from the cache pinned, 559 of 564
//! unpinned). So the lease is a turn, not a place: requests are not pinned to it.
//!
//! A lease holds against other processes too: it locks a file of its server and slot, so
//! ognivo sessions on one server share its slots, and a process that ends lets its leases
//! go.

use std::cmp::Reverse;
use std::fs::{File, TryLockError};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::stream::{self, BoxStream, StreamExt as _};
use tokio::sync::Notify;

use crate::driver::{DriverError, DriverEvent, ModelDriver, StepRequest};
use crate::llama::LlamaServer;
use crate::template::sha256_hex;

/// How often a waiter looks again for a slot another process may have let go.
#[cfg(not(test))]
const RECHECK: Duration = Duration::from_secs(2);
#[cfg(test)]
const RECHECK: Duration = Duration::from_millis(20);

/// Who gets a freed slot first: the greatest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Work nobody waits on, such as a commit message.
    Service,
    Subagent,
    /// A one-off request the user waits for.
    Interactive,
    /// The agent the user talks to.
    Main,
}

/// A model at a server: what the router leases slots of.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Names its model for llama-server in router mode ([`LlamaServer::for_model`]).
    pub server: LlamaServer,
    /// The profile of the model.
    pub profile: String,
    /// Its slots, `/props` `total_slots`; 0 when unknown, and the server places every
    /// request.
    pub slots: u32,
}

/// What the router last saw of a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Not asked yet.
    Unknown,
    Up,
    /// The server, or the router's model, is loading.
    Loading,
    /// The router's model is not running; a request starts it if the router loads on
    /// demand.
    Unloaded,
    /// No answer, or an error: why.
    Down(String),
}

impl Health {
    /// Which servers a lease tries first.
    fn rank(&self) -> u8 {
        match self {
            Health::Up | Health::Unknown | Health::Unloaded => 0,
            Health::Loading => 1,
            Health::Down(_) => 2,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("no server runs {0}")]
    NoEndpoint(String),
}

/// The endpoints and their slots; clones share them.
#[derive(Debug, Clone)]
pub struct Router {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    endpoints: Vec<Endpoint>,
    /// Per endpoint, the name of its lock files.
    keys: Vec<String>,
    /// What [`Router::check`] last saw, per endpoint.
    health: Mutex<Vec<Health>>,
    state: Mutex<State>,
    /// A slot was let go, or a waiter left.
    changed: Notify,
    /// Where leases lock their files; `None`: leases hold in this process only.
    locks: Option<PathBuf>,
}

#[derive(Debug)]
struct State {
    /// Per endpoint, per slot: leased in this process.
    taken: Vec<Vec<bool>>,
    waiting: Vec<Waiter>,
    tickets: u64,
}

#[derive(Debug)]
struct Waiter {
    ticket: u64,
    priority: Priority,
    profile: String,
}

/// One of a server's slots for a generation step, until the lease is dropped.
#[derive(Debug)]
pub struct Lease {
    inner: Arc<Inner>,
    endpoint: usize,
    /// `None`: no slot was free, or the server's slots are unknown, and the server places
    /// each request.
    slot: Option<u32>,
    /// Holds the slot against other processes.
    lock: Option<File>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Router {
    /// A router of `endpoints`, whose leases lock files in `locks` when it is given.
    pub fn new(endpoints: Vec<Endpoint>, locks: Option<PathBuf>) -> Self {
        let keys = endpoints
            .iter()
            .map(|e| {
                let id = format!(
                    "{}\n{}",
                    e.server.base_url(),
                    e.server.model().unwrap_or("")
                );
                sha256_hex(&id)[..16].to_owned()
            })
            .collect();
        let state = State {
            taken: endpoints
                .iter()
                .map(|e| vec![false; e.slots as usize])
                .collect(),
            waiting: Vec::new(),
            tickets: 0,
        };
        Self {
            inner: Arc::new(Inner {
                health: Mutex::new(vec![Health::Unknown; endpoints.len()]),
                endpoints,
                keys,
                state: Mutex::new(state),
                changed: Notify::new(),
                locks,
            }),
        }
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.inner.endpoints
    }

    /// A free slot of a server that runs `profile`, if one is free now and nobody waiting
    /// for one comes before the asker.
    pub fn try_lease(
        &self,
        profile: &str,
        priority: Priority,
    ) -> Result<Option<Lease>, RouterError> {
        let order = self.inner.order(profile);
        if order.is_empty() {
            return Err(RouterError::NoEndpoint(profile.to_owned()));
        }
        if self.inner.unknown_slots(&order) {
            return Ok(Some(self.unpinned(order[0])));
        }
        Ok(self
            .inner
            .take(profile, priority, None)
            .map(|(endpoint, slot, lock)| self.leased(endpoint, slot, lock)))
    }

    /// A slot of a server that runs `profile`: a free one now, or the next one let go that
    /// no one before it in the queue takes. The main agent does not wait: without a free
    /// slot its lease has none, and the server queues its step.
    pub async fn lease(&self, profile: &str, priority: Priority) -> Result<Lease, RouterError> {
        if let Some(lease) = self.try_lease(profile, priority)? {
            return Ok(lease);
        }
        if priority == Priority::Main {
            return Ok(self.unpinned(self.inner.order(profile)[0]));
        }
        let ticket = Ticket::new(&self.inner, profile, priority);
        loop {
            let changed = self.inner.changed.notified();
            tokio::pin!(changed);
            // registered before the look, so a slot let go after it wakes this waiter
            changed.as_mut().enable();
            if let Some((endpoint, slot, lock)) = self.inner.take(profile, priority, Some(ticket.0))
            {
                drop(ticket);
                return Ok(self.leased(endpoint, slot, lock));
            }
            // another process lets its slots go without a word: look again now and then
            let _ = tokio::time::timeout(RECHECK, changed).await;
        }
    }

    fn leased(&self, endpoint: usize, slot: u32, lock: Option<File>) -> Lease {
        Lease {
            inner: self.inner.clone(),
            endpoint,
            slot: Some(slot),
            lock,
        }
    }

    fn unpinned(&self, endpoint: usize) -> Lease {
        Lease {
            inner: self.inner.clone(),
            endpoint,
            slot: None,
            lock: None,
        }
    }

    /// Asks every server how it is.
    pub async fn check(&self) {
        let seen = futures::future::join_all(self.inner.endpoints.iter().map(health_of)).await;
        *lock(&self.inner.health) = seen;
    }

    /// What the last [`check`](Self::check) saw, in the order of [`endpoints`](Self::endpoints).
    pub fn health(&self) -> Vec<Health> {
        lock(&self.inner.health).clone()
    }

    /// Checks the servers every `every`, until the router and its leases are gone.
    pub fn watch(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            while let Some(inner) = inner.upgrade() {
                Router { inner }.check().await;
                tokio::time::sleep(every).await;
            }
        })
    }

    #[cfg(test)]
    fn waiting(&self) -> usize {
        lock(&self.inner.state).waiting.len()
    }
}

impl Inner {
    /// The endpoints that run `profile`, the healthy first.
    fn order(&self, profile: &str) -> Vec<usize> {
        let health = lock(&self.health);
        let mut order: Vec<usize> = (0..self.endpoints.len())
            .filter(|&e| self.endpoints[e].profile == profile)
            .collect();
        order.sort_by_key(|&e| health[e].rank());
        order
    }

    fn unknown_slots(&self, order: &[usize]) -> bool {
        order.iter().all(|&e| self.endpoints[e].slots == 0)
    }

    /// A free slot for `profile`, if nobody waiting for one comes before the asker (`me`,
    /// when the asker waits too): its endpoint, number and lock.
    fn take(
        &self,
        profile: &str,
        priority: Priority,
        me: Option<u64>,
    ) -> Option<(usize, u32, Option<File>)> {
        let mut state = lock(&self.state);
        let before = |w: &Waiter| {
            w.profile == profile
                && match me {
                    Some(me) => (w.priority, Reverse(w.ticket)) > (priority, Reverse(me)),
                    None => w.priority >= priority,
                }
        };
        if state.waiting.iter().any(before) {
            return None;
        }
        for e in self.order(profile) {
            for slot in 0..self.endpoints[e].slots {
                if state.taken[e][slot as usize] {
                    continue;
                }
                if let Some(lock) = self.claim(e, slot) {
                    state.taken[e][slot as usize] = true;
                    return Some((e, slot, lock));
                }
            }
        }
        None
    }

    /// The slot's lock file locked, `Some(None)` when there is no file to lock, `None`
    /// when another process holds it.
    fn claim(&self, endpoint: usize, slot: u32) -> Option<Option<File>> {
        let Some(dir) = &self.locks else {
            return Some(None);
        };
        let path = dir.join(format!("{}-{slot}.lock", self.keys[endpoint]));
        let opened = {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .and_then(|()| File::options().create(true).append(true).open(&path))
        };
        match opened.map(|f| (f.try_lock(), f)) {
            Ok((Ok(()), file)) => Some(Some(file)),
            Ok((Err(TryLockError::WouldBlock), _)) => None,
            // where no file can be locked, the lease holds in this process only
            Ok((Err(TryLockError::Error(_)), _)) | Err(_) => Some(None),
        }
    }
}

/// A place in the queue for a slot, left when it is dropped: by the lease it became, or by
/// a waiter that gave up.
struct Ticket<'r>(u64, &'r Inner);

impl<'r> Ticket<'r> {
    fn new(inner: &'r Inner, profile: &str, priority: Priority) -> Self {
        let mut state = lock(&inner.state);
        state.tickets += 1;
        let ticket = state.tickets;
        state.waiting.push(Waiter {
            ticket,
            priority,
            profile: profile.to_owned(),
        });
        Self(ticket, inner)
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        lock(&self.1.state).waiting.retain(|w| w.ticket != self.0);
        // whoever waited behind it may come first now
        self.1.changed.notify_waiters();
    }
}

impl Lease {
    pub fn server(&self) -> &LlamaServer {
        &self.inner.endpoints[self.endpoint].server
    }

    /// Which of the server's slots it counts as; `None`: none was free, or the server's
    /// slots are unknown. Requests are not pinned to it.
    pub fn slot(&self) -> Option<u32> {
        self.slot
    }

    /// The slots of its server; 0 when unknown.
    pub fn slots(&self) -> u32 {
        self.inner.endpoints[self.endpoint].slots
    }

    pub fn profile(&self) -> &str {
        &self.inner.endpoints[self.endpoint].profile
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let Some(slot) = self.slot else {
            return;
        };
        // the file first, so a waiter woken next finds it free
        drop(self.lock.take());
        lock(&self.inner.state).taken[self.endpoint][slot as usize] = false;
        self.inner.changed.notify_waiters();
    }
}

async fn health_of(e: &Endpoint) -> Health {
    match e.server.get("/health").await {
        Ok((200, _)) => {}
        Ok((503, _)) => return Health::Loading,
        Ok((status, _)) => return Health::Down(format!("/health returned {status}")),
        Err(err) => return Health::Down(err.to_string()),
    }
    // a router answers for itself: how its model is, its list says
    let Some(model) = e.server.model() else {
        return Health::Up;
    };
    match e.server.models().await {
        Ok(models) => match models
            .iter()
            .find(|m| m.id == model || m.aliases.iter().any(|a| a == model))
        {
            Some(m) => match m.status.as_str() {
                "loaded" => Health::Up,
                "loading" => Health::Loading,
                "unloaded" | "sleeping" => Health::Unloaded,
                other => Health::Down(format!("{model} is {other}")),
            },
            None => Health::Down(format!("the router has no model {model}")),
        },
        Err(err) => Health::Down(err.to_string()),
    }
}

/// A driver whose every step waits for its turn at a slot of the router's servers and
/// holds it until the step's stream ends or is dropped.
#[derive(Debug)]
pub struct Scheduled {
    driver: Box<dyn ModelDriver>,
    router: Router,
    profile: String,
    priority: Priority,
}

impl Scheduled {
    pub fn new(
        driver: Box<dyn ModelDriver>,
        router: Router,
        profile: &str,
        priority: Priority,
    ) -> Self {
        Self {
            driver,
            router,
            profile: profile.to_owned(),
            priority,
        }
    }
}

impl ModelDriver for Scheduled {
    fn step(&self, req: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
        // a driver's step starts nothing until it is polled, so it waits for the lease
        let steps = self.driver.step(req);
        let (router, profile, priority) =
            (self.router.clone(), self.profile.clone(), self.priority);
        stream::once(async move {
            // a model no server of the router runs still goes, where the driver sends it
            let lease = router.lease(&profile, priority).await.ok();
            steps.map(move |event| {
                let _held = &lease;
                event
            })
        })
        .flatten()
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn endpoint(url: &str, slots: u32) -> Endpoint {
        Endpoint {
            server: LlamaServer::new(url, None).unwrap(),
            profile: "p".into(),
            slots,
        }
    }

    fn router(slots: u32, locks: Option<PathBuf>) -> Router {
        Router::new(vec![endpoint("http://127.0.0.1:1", slots)], locks)
    }

    /// Lets the spawned waiters run until `n` of them wait.
    async fn until_waiting(r: &Router, n: usize) {
        for _ in 0..1000 {
            if r.waiting() == n {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("{} waiting, not {n}", r.waiting());
    }

    #[tokio::test]
    async fn a_freed_slot_goes_by_priority_then_to_whoever_asked_first() {
        let r = router(1, None);
        let first = r.lease("p", Priority::Subagent).await.unwrap();
        assert_eq!(first.slot(), Some(0));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let askers = [
            ("service", Priority::Service),
            ("subagent 1", Priority::Subagent),
            ("interactive", Priority::Interactive),
            ("subagent 2", Priority::Subagent),
        ];
        for (i, (name, priority)) in askers.into_iter().enumerate() {
            let (r2, tx) = (r.clone(), tx.clone());
            tokio::spawn(async move {
                let lease = r2.lease("p", priority).await.unwrap();
                tx.send((name, lease)).unwrap();
            });
            until_waiting(&r, i + 1).await;
        }
        drop(first);
        let mut order = Vec::new();
        for _ in 0..4 {
            let (name, lease) = rx.recv().await.unwrap();
            assert_eq!(lease.slot(), Some(0));
            order.push(name);
            drop(lease);
        }
        assert_eq!(
            order,
            ["interactive", "subagent 1", "subagent 2", "service"]
        );
    }

    #[tokio::test]
    async fn the_main_agent_does_not_wait_and_takes_a_slot_before_the_queue() {
        let r = router(1, None);
        let sub = r.lease("p", Priority::Subagent).await.unwrap();
        let main = r.lease("p", Priority::Main).await.unwrap();
        assert_eq!(
            main.slot(),
            None,
            "no slot free: the server places its requests"
        );
        drop(main);
        // a subagent waits; the slot let go is the main agent's if it asks now
        let r2 = r.clone();
        let waiting = tokio::spawn(async move { r2.lease("p", Priority::Subagent).await });
        until_waiting(&r, 1).await;
        drop(sub);
        let main = r.try_lease("p", Priority::Main).unwrap().unwrap();
        assert_eq!(main.slot(), Some(0));
        // the queue keeps its order against anyone below it
        assert!(r.try_lease("p", Priority::Service).unwrap().is_none());
        drop(main);
        assert_eq!(waiting.await.unwrap().unwrap().slot(), Some(0));
    }

    #[tokio::test]
    async fn slots_are_leased_one_each_and_a_waiter_that_gives_up_leaves_the_queue() {
        let r = router(2, None);
        let a = r.lease("p", Priority::Main).await.unwrap();
        let b = r.lease("p", Priority::Subagent).await.unwrap();
        assert_eq!((a.slot(), b.slot()), (Some(0), Some(1)));
        let gave_up = tokio::time::timeout(
            Duration::from_millis(50),
            r.lease("p", Priority::Interactive),
        )
        .await;
        assert!(gave_up.is_err());
        assert_eq!(r.waiting(), 0);
        drop(b);
        assert_eq!(
            r.try_lease("p", Priority::Service).unwrap().unwrap().slot(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn a_slot_another_process_holds_is_passed_over_until_it_is_let_go() {
        let dir = std::env::temp_dir().join(format!("ognivo-router-{}", std::process::id()));
        // two routers with one lock directory stand for two processes on one server
        let (one, two) = (router(2, Some(dir.clone())), router(2, Some(dir.clone())));
        let a = one.lease("p", Priority::Main).await.unwrap();
        let b = two.lease("p", Priority::Main).await.unwrap();
        assert_eq!((a.slot(), b.slot()), (Some(0), Some(1)));
        // all held: the main agent goes unpinned, a subagent waits for the other process
        assert_eq!(two.lease("p", Priority::Main).await.unwrap().slot(), None);
        let two2 = two.clone();
        let waiting = tokio::spawn(async move { two2.lease("p", Priority::Subagent).await });
        until_waiting(&two, 1).await;
        drop(a);
        let c = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(c.slot(), Some(0));
        drop((b, c));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn without_known_slots_or_a_server_for_the_model() {
        let r = router(0, None);
        // the server places every request, and nobody waits
        assert_eq!(r.lease("p", Priority::Subagent).await.unwrap().slot(), None);
        assert!(matches!(
            r.try_lease("other", Priority::Main),
            Err(RouterError::NoEndpoint(_))
        ));
    }

    /// A driver whose steps say "go" at once.
    #[derive(Debug)]
    struct Go;

    impl ModelDriver for Go {
        fn step(&self, _: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
            stream::iter([Ok(DriverEvent::Text("go".into()))]).boxed()
        }
    }

    fn request() -> StepRequest {
        StepRequest {
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: crate::ToolChoice::Auto,
            max_tokens: 1,
            seed: 1,
        }
    }

    #[tokio::test]
    async fn a_step_waits_for_its_turn_and_holds_it_while_it_streams() {
        let r = router(1, None);
        let held = r.lease("p", Priority::Interactive).await.unwrap();
        let sub = Scheduled::new(Box::new(Go), r.clone(), "p", Priority::Subagent);
        let mut step = sub.step(request());
        let waited = tokio::time::timeout(Duration::from_millis(50), step.next()).await;
        assert!(waited.is_err(), "the only slot is taken");
        drop(held);
        assert_eq!(
            step.next().await.unwrap().unwrap(),
            DriverEvent::Text("go".into())
        );
        // the step is over, and its slot is free again
        drop(step);
        assert!(r.try_lease("p", Priority::Service).unwrap().is_some());
        // the main agent's step goes without a free slot
        let _held = r.lease("p", Priority::Interactive).await.unwrap();
        let main = Scheduled::new(Box::new(Go), r.clone(), "p", Priority::Main);
        let first = tokio::time::timeout(Duration::from_secs(1), main.step(request()).next()).await;
        assert!(matches!(first, Ok(Some(Ok(DriverEvent::Text(_))))));
    }

    /// A server that answers `/health` with `health` and `/models` with `models`.
    async fn serve(health: u16, models: serde_json::Value) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let models = models.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap();
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let (status, body) = if head.starts_with("GET /health ") {
                        (health, "{}".to_owned())
                    } else if head.starts_with("GET /models?model=") {
                        (200, models.to_string())
                    } else {
                        (404, "{}".to_owned())
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn health_comes_from_the_server_and_for_a_router_from_its_list() {
        let up = serve(200, json!({})).await;
        let loading = serve(503, json!({})).await;
        let list = json!({"data": [
            {"id": "gpt-oss", "aliases": ["oss"], "status": {"value": "loaded", "args": []}},
            {"id": "qwen", "aliases": [], "status": {"value": "unloaded", "args": []}}
        ]});
        let routed = serve(200, list).await;
        let gone = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let model = |name: &str| Endpoint {
            server: LlamaServer::new(&routed, None).unwrap().for_model(name),
            profile: "p".into(),
            slots: 1,
        };
        let r = Router::new(
            vec![
                endpoint(&up, 1),
                endpoint(&loading, 1),
                endpoint(&gone, 1),
                model("oss"),
                model("qwen"),
                model("gemma"),
            ],
            None,
        );
        assert_eq!(r.health()[0], Health::Unknown);
        r.check().await;
        let h = r.health();
        assert_eq!(h[..2], [Health::Up, Health::Loading]);
        assert!(
            matches!(&h[2], Health::Down(why) if why.contains("refused")),
            "{:?}",
            h[2]
        );
        assert_eq!(h[3..5], [Health::Up, Health::Unloaded]);
        assert_eq!(h[5], Health::Down("the router has no model gemma".into()));
        // a lease goes to a healthy server first
        assert_eq!(r.inner.order("p")[..2], [0, 3]);
    }
}
