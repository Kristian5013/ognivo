//! `ognivo core` end to end over its socket: a client starts a thread and runs a turn,
//! answers the question it is asked, reviews and reverts what changed; a second client
//! reads what it missed from the store; a turn is stopped and the model hears of it; and
//! the core quits once nobody is there. No GPU, no network.

mod common;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{
    HANG, answer_stream, call_stream, fake_keyed_server, fake_router, fake_seeing_server,
    fake_server, fake_switched_server, no_projector, progress_call_stream,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines, ReadHalf, WriteHalf};

/// The core's socket for a test's directory: a Unix socket in it, or on Windows a pipe
/// named after it.
fn socket(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let name = dir.file_name().unwrap().to_string_lossy();
        PathBuf::from(format!(r"\\.\pipe\ognivo-test-{name}"))
    } else {
        dir.join("run/core.sock")
    }
}

fn core(dir: &Path, idle: &str) -> Child {
    core_with(dir, idle, &[])
}

/// The same with more of the environment.
fn core_with(dir: &Path, idle: &str, env: &[(&str, &Path)]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["core", "--idle", idle, "--socket"])
        .arg(socket(dir))
        .arg("--data-dir")
        .arg(dir.join("data"))
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        .envs(env.iter().copied())
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// A client of the core, as the GUI is one.
struct Client {
    read: Lines<BufReader<ReadHalf<ognivo_os::ipc::Client>>>,
    write: WriteHalf<ognivo_os::ipc::Client>,
    next: u64,
    /// Messages not taken yet.
    pending: VecDeque<Value>,
    /// The events it got, in order.
    events: Vec<Value>,
}

impl Client {
    async fn connect(socket: &Path) -> Self {
        let began = Instant::now();
        let stream = loop {
            match ognivo_os::ipc::connect(socket).await {
                Ok(s) => break s,
                Err(_) if began.elapsed() < Duration::from_secs(20) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("{}: {e}", socket.display()),
            }
        };
        let (read, write) = tokio::io::split(stream);
        Self {
            read: BufReader::new(read).lines(),
            write,
            next: 0,
            pending: VecDeque::new(),
            events: Vec::new(),
        }
    }

    async fn send(&mut self, message: Value) {
        let line = format!("{message}\n");
        self.write.write_all(line.as_bytes()).await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(30), self.read.next_line())
            .await
            .expect("nothing from the core in 30 s")
            .unwrap()
            .expect("the core hung up");
        let message: Value = serde_json::from_str(&line).unwrap();
        if message["method"] == "event" {
            self.events.push(message["params"].clone());
        }
        message
    }

    /// The next message that is not an answer to a call.
    async fn next(&mut self) -> Value {
        match self.pending.pop_front() {
            Some(m) => m,
            None => self.read().await,
        }
    }

    /// Messages up to the first that `last` holds of, that one included.
    async fn until(&mut self, last: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            let m = self.next().await;
            let done = last(&m);
            got.push(m);
            if done {
                return got;
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            let m = self.read().await;
            if m.get("method").is_none() && m["id"] == json!(id) {
                return match m.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(m["result"].clone()),
                };
            }
            self.pending.push_back(m);
        }
    }

    async fn ok(&mut self, method: &str, params: Value) -> Value {
        match self.call(method, params).await {
            Ok(v) => v,
            Err(e) => panic!("{method}: {e}"),
        }
    }
}

fn event(m: &Value, name: &str) -> bool {
    m["method"] == "event" && m["params"]["event"] == name
}

/// The events the store does not keep, as `EventKind::kept` says: what streams, and what
/// only tells the clients there how things stand.
const NOT_KEPT: [&str; 4] = ["item_delta", "call_draft", "progress", "changes_updated"];

fn is_kept(e: &Value) -> bool {
    !NOT_KEPT.iter().any(|k| e["event"] == *k)
}

fn server(url: &str) -> Value {
    json!({"url": url, "api_key": null, "model": null})
}

fn start(project: &Path, url: &str) -> Value {
    json!({
        "project": project, "server": server(url), "profile": "gpt-oss-20b", "level": null,
        "native": false, "priors": false, "subagents": false
    })
}

#[tokio::test]
async fn a_client_runs_a_thread_through_the_core_and_another_reads_it() {
    let (url, bodies) = fake_server(vec![
        call_stream("c1", "write", r#"{"path":"new.txt","content":"fresh\n"}"#),
        call_stream("c2", "shell", r#"{"command":"touch made.txt"}"#),
        answer_stream("Done."),
        answer_stream("Done."),
        // the second thread: a turn that is stopped, then one that answers
        HANG.to_owned(),
        answer_stream("Going on."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    // a project with a build: the unverified answer is sent back once to run it
    std::fs::write(root.join("Makefile"), "all:\n").unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let socket = socket(dir.path());
    let mut child = core(dir.path(), "1");
    let mut a = Client::connect(&socket).await;

    let init = a
        .ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    assert_eq!(init["protocol"], 1);
    assert_eq!(init["seq"], 0);
    // a second core for the same data does not start
    let second = core(dir.path(), "1").wait_with_output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("another core runs on"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    // the model of a server that does not answer: why, and no profile
    let none = a
        .call(
            "server/detect",
            json!({"url": "http://127.0.0.1:9", "api_key": null, "model": null}),
        )
        .await
        .unwrap_err();
    assert_eq!(none["code"], -32003, "{none}");

    // behind a router, given its OpenAI base URL: its models with the profiles their names
    // pick, and the one asked for, known by its name while it is not loaded
    let models = json!([
        {"id": "gemma-4-26b-a4b-q4", "aliases": [], "status": {"value": "unloaded", "args": []}},
        {"id": "gemma-4-26b-a4b-q8", "aliases": [], "status": {"value": "unloaded", "args": []}},
        {"id": "embeddings", "aliases": [], "status": {"value": "unloaded", "args": []}}
    ]);
    let (routed, posted, gets) = fake_router(models, vec![]).await;
    let asked = a
        .ok(
            "server/detect",
            json!({"url": format!("{routed}/v1"), "api_key": null, "model": "gemma-4-26b-a4b-q8"}),
        )
        .await;
    assert_eq!(asked["model"], "gemma-4-26b-a4b-q8", "{asked}");
    assert_eq!(asked["profile"], "gemma-4-26b-a4b", "{asked}");
    assert_eq!(asked["window"], Value::Null, "{asked}");
    assert_eq!(
        asked["models"],
        json!([
            {"id": "gemma-4-26b-a4b-q4", "status": "unloaded", "profile": "gemma-4-26b-a4b"},
            {"id": "gemma-4-26b-a4b-q8", "status": "unloaded", "profile": "gemma-4-26b-a4b"},
            {"id": "embeddings", "status": "unloaded", "profile": null}
        ])
    );
    // none loaded and none asked for: the user chooses
    let open = a
        .ok(
            "server/detect",
            json!({"url": routed, "api_key": null, "model": "a model of another server"}),
        )
        .await;
    assert_eq!(
        [&open["model"], &open["profile"]],
        [&Value::Null, &Value::Null]
    );
    // and detecting has the router start nothing
    assert!(posted.lock().unwrap().is_empty());
    assert!(
        gets.lock().unwrap().iter().all(|g| !g.contains("model=")),
        "{:?}",
        gets.lock().unwrap()
    );

    let opened = a.ok("thread/start", start(&root, &url)).await;
    let thread = opened["thread"].clone();
    assert_eq!(opened["mode"], "edit_automatically");
    assert_eq!(opened["info"]["kind"], "chat");
    assert_eq!(opened["info"]["project"], json!(root));
    assert!(
        opened["notes"].as_array().unwrap().contains(&json!(
            "not a clean git tree: builds and tests ask until you accept the changes"
        )),
        "{opened}"
    );

    assert_eq!(
        a.ok(
            "turn/start",
            json!({"thread": thread, "text": "make a file"})
        )
        .await,
        json!({})
    );
    // the command asks, and the question names its item in the stream
    let got = a.until(|m| m["method"] == "approval/ask").await;
    let ask = got.last().unwrap().clone();
    let item = got
        .iter()
        .rev()
        .find(|m| event(m, "item_started") && m["params"]["item"]["kind"] == "approval")
        .unwrap()["params"]["item"]["id"]
        .clone();
    assert_eq!(ask["params"]["thread"], thread);
    assert_eq!(
        ask["params"]["item"],
        json!({"thread": thread, "item": item})
    );
    assert_eq!(ask["params"]["tool"], "shell");
    assert_eq!(ask["params"]["action"], "touch made.txt");
    assert_eq!(ask["params"]["agent"], Value::Null);
    a.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"decision": "allow", "instead": null}}))
        .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    // the question is withdrawn from the clients once it is answered
    assert!(
        got.iter()
            .any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == ask["id"]),
        "{got:?}"
    );
    let end = &got.last().unwrap()["params"];
    assert_eq!(end["end"], "answer");
    assert_eq!(end["answer"], "Done.");
    assert!(root.join("made.txt").exists() && root.join("new.txt").exists());
    let kinds: Vec<(String, String)> = a
        .events
        .iter()
        .filter(|e| is_kept(e) && e["event"] != "generation")
        .map(|e| {
            let kind = e["item"]["kind"].as_str().unwrap_or_default();
            (e["event"].as_str().unwrap().to_owned(), kind.to_owned())
        })
        .collect();
    let want = [
        ("thread_started", ""),
        ("mode_changed", ""),
        ("turn_started", ""),
        ("item_started", "reasoning"),
        ("item_completed", "reasoning"),
        ("item_started", "tool_call"),
        ("item_completed", "tool_call"),
        ("item_started", "reasoning"),
        ("item_completed", "reasoning"),
        ("item_started", "tool_call"),
        ("item_started", "approval"),
        ("item_completed", "approval"),
        ("item_completed", "tool_call"),
    ];
    let have: Vec<(&str, &str)> = kinds
        .iter()
        .map(|(e, k)| (e.as_str(), k.as_str()))
        .collect();
    assert_eq!(&have[..want.len()], &want, "{have:?}");
    // each call showed as the model wrote it: its tool, then its arguments as they came,
    // by ognivo's names
    let drafts: Vec<&Value> = a
        .events
        .iter()
        .filter(|e| e["event"] == "call_draft")
        .map(|e| &e["draft"])
        .collect();
    let written = |generation: u32, field: &str| -> String {
        drafts
            .iter()
            .filter(|d| d["generation"] == generation && d["index"] == 0 && d["field"] == field)
            .map(|d| d["text"].as_str().unwrap())
            .collect()
    };
    assert_eq!(drafts[0]["tool"], "Write", "{drafts:?}");
    assert_eq!(drafts[0]["field"], Value::Null);
    assert_eq!(written(1, "path"), "new.txt");
    assert_eq!(written(1, "content"), "fresh\n");
    assert_eq!(written(2, "command"), "touch made.txt");
    let shell = a
        .events
        .iter()
        .find(|e| e["event"] == "item_completed" && e["item"]["call"]["name"] == "Bash")
        .unwrap();
    assert_eq!(shell["item"]["call"]["command"], "touch made.txt");
    assert_eq!(shell["item"]["result"]["outcome"], "ok");
    assert_eq!(shell["item"]["result"]["exit_code"], 0);
    let write = a
        .events
        .iter()
        .find(|e| e["event"] == "item_completed" && e["item"]["call"]["name"] == "Write")
        .unwrap();
    assert_eq!(write["item"]["changes"][0]["path"], "new.txt");

    // what waits for review, and its review
    let changes = a.ok("changes/list", json!({"thread": thread})).await;
    assert_eq!(
        changes["files"],
        json!([{"path": "new.txt", "created": true, "deleted": false, "drifted": false,
                "hunks": [{"old_start": 1, "new_start": 1, "removed": [], "added": ["fresh"]}]}])
    );
    assert_eq!(changes["commands"], json!(["touch made.txt"]));
    assert_eq!(changes["inherited"], true);
    let reverted = a
        .ok(
            "changes/revert",
            json!({"thread": thread, "path": "new.txt", "force": false}),
        )
        .await;
    assert_eq!(reverted, json!({"removed": true}));
    assert!(!root.join("new.txt").exists());
    let error = a
        .call(
            "changes/revert",
            json!({"thread": thread, "path": "new.txt", "force": false}),
        )
        .await
        .unwrap_err();
    assert_eq!(error["code"], -32004);
    let accepted = a
        .ok("changes/accept", json!({"thread": thread, "path": null}))
        .await;
    assert_eq!(
        accepted,
        json!({"accepted": ["1 command with side effects", "what the project had before this session"], "unreviewed": false})
    );

    // each review was told to the clients there, and is not kept
    let updates = a
        .events
        .iter()
        .filter(|e| e["event"] == "changes_updated")
        .count();
    assert_eq!(updates, 2, "a revert and an accept, not the refused revert");

    // another client reads what it missed: every event that is kept, as they were sent
    let mut b = Client::connect(&socket).await;
    let init = b
        .ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let last = a.events.last().unwrap()["seq"].clone();
    assert_eq!(init["seq"], last);
    let read = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    let kept: Vec<&Value> = a.events.iter().filter(|e| is_kept(e)).collect();
    let read: Vec<&Value> = read["events"].as_array().unwrap().iter().collect();
    assert_eq!(read, kept);
    let after = b
        .ok(
            "thread/read",
            json!({"thread": thread, "after": kept[3]["seq"]}),
        )
        .await;
    assert_eq!(after["events"][0], *kept[4]);
    let list = b
        .ok("thread/list", json!({"project": root, "limit": null}))
        .await;
    let t = &list["threads"][0];
    assert_eq!(
        (
            &t["thread"],
            &t["title"],
            &t["turns"],
            &t["open"],
            &t["running"]
        ),
        (
            &thread,
            &json!("make a file"),
            &json!(1),
            &json!(true),
            &json!(false)
        )
    );
    // its mode switched: every client hears it, and the list says it
    assert_eq!(
        b.ok("thread/mode", json!({"thread": thread, "mode": "plan"}))
            .await,
        json!({})
    );
    let got = a.until(|m| event(m, "mode_changed")).await;
    assert_eq!(got.last().unwrap()["params"]["mode"], "plan");
    let list = b
        .ok("thread/list", json!({"project": root, "limit": null}))
        .await;
    assert_eq!(list["threads"][0]["mode"], "plan");
    let closed = b
        .call("thread/mode", json!({"thread": 999_999, "mode": "auto"}))
        .await
        .unwrap_err();
    assert_eq!(closed["code"], -32001);
    let trajectory = b.ok("thread/trajectory", json!({"thread": thread})).await;
    assert_eq!(
        trajectory["trajectory"]["session_id"],
        json!(thread.to_string())
    );

    // a turn stopped while it waits for the server; the next one says so to the model
    let opened = a.ok("thread/start", start(&root, &url)).await;
    let second = opened["thread"].clone();
    assert_ne!(second, thread);
    a.ok("turn/start", json!({"thread": second, "text": "wait"}))
        .await;
    a.until(|m| event(m, "turn_started") && m["params"]["thread"] == second)
        .await;
    // stopped once its request is at the server, which holds it
    let began = Instant::now();
    while bodies.lock().unwrap().len() < 5 {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let busy = a
        .call("turn/start", json!({"thread": second, "text": "more"}))
        .await
        .unwrap_err();
    assert_eq!(busy["code"], -32002);
    a.ok("turn/interrupt", json!({"thread": second})).await;
    let got = a
        .until(|m| event(m, "turn_completed") && m["params"]["thread"] == second)
        .await;
    assert_eq!(got.last().unwrap()["params"]["end"], "interrupted");
    a.ok("turn/start", json!({"thread": second, "text": "go on"}))
        .await;
    let got = a
        .until(|m| event(m, "turn_completed") && m["params"]["thread"] == second)
        .await;
    assert_eq!(got.last().unwrap()["params"]["answer"], "Going on.");
    let sent = bodies.lock().unwrap().last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .clone();
    assert!(
        sent.as_str()
            .unwrap()
            .ends_with("[ognivo] The user stopped the last turn."),
        "{sent}"
    );
    // b got it all live
    let got = b
        .until(|m| event(m, "turn_completed") && m["params"]["answer"] == "Going on.")
        .await;
    assert!(
        got.iter()
            .any(|m| event(m, "thread_started") && m["params"]["thread"] == second)
    );

    for t in [&thread, &second] {
        a.ok("thread/close", json!({"thread": t})).await;
    }
    let list = a
        .ok("thread/list", json!({"project": root, "limit": null}))
        .await;
    assert!(
        list["threads"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["open"] == false),
        "{list}"
    );
    let closed = a
        .call("turn/start", json!({"thread": thread, "text": "more"}))
        .await
        .unwrap_err();
    assert_eq!(closed["code"], -32001);

    // nobody there: the core quits, and its socket goes with it
    drop((a, b));
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the core did not quit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "{status:?}");
    assert!(!socket.exists());
}

/// The changes of a thread as `changes/list` gives them: the file's path and its hunks.
fn hunks_of(changes: &Value) -> Vec<(String, Value)> {
    changes["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap().to_owned(), f["hunks"].clone()))
        .collect()
}

#[tokio::test]
async fn hunks_are_kept_and_undone_and_a_running_turn_shows_its_changes() {
    let edit = |id: &str, old: &str, new: &str| {
        let args = json!({"path": "notes.txt", "old_string": old, "new_string": new});
        call_stream(id, "edit", &args.to_string())
    };
    let (url, bodies) = fake_server(vec![
        edit("e1", "one", "ONE"),
        edit("e2", "five", "FIVE"),
        answer_stream("Edited."),
        // the second turn edits, then waits for the server until it is stopped
        edit("e3", "two", "TWO"),
        HANG.to_owned(),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let notes = root.join("notes.txt");
    std::fs::write(&notes, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let socket = socket(dir.path());
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    // a second client, which hears of what the first does
    let mut b = Client::connect(&socket).await;
    b.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    a.ok("turn/start", json!({"thread": thread, "text": "shout"}))
        .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    assert_eq!(got.last().unwrap()["params"]["answer"], "Edited.");
    let first = json!({"old_start": 1, "new_start": 1, "removed": ["one"], "added": ["ONE"]});
    let last = json!({"old_start": 5, "new_start": 5, "removed": ["five"], "added": ["FIVE"]});
    let listed = a.ok("changes/list", json!({"thread": thread})).await;
    assert_eq!(
        hunks_of(&listed),
        [("notes.txt".to_owned(), json!([first, last]))]
    );
    let updates = |c: &Client| {
        c.events
            .iter()
            .filter(|e| e["event"] == "changes_updated")
            .count()
    };

    // kept: the file stays, the answer is the changes after it, and every client hears
    let hunk = |hunk: &Value| json!({"thread": thread, "path": "notes.txt", "hunk": hunk});
    let after = a.ok("changes/accept_hunk", hunk(&first)).await;
    assert_eq!(hunks_of(&after), [("notes.txt".to_owned(), json!([last]))]);
    assert_eq!(after["inherited"], listed["inherited"]);
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "ONE\ntwo\nthree\nfour\nFIVE\n"
    );
    assert_eq!(updates(&a), 1);
    let heard = b.until(|m| event(m, "changes_updated")).await;
    assert_eq!(heard.last().unwrap()["params"]["thread"], thread);
    // the same hunk again: the change has it no more, and nothing changed
    let stale = a
        .call("changes/accept_hunk", hunk(&first))
        .await
        .unwrap_err();
    assert_eq!(stale["code"], -32007, "{stale}");
    let none = a
        .call(
            "changes/revert_hunk",
            json!({"thread": thread, "path": "other.txt", "hunk": first}),
        )
        .await
        .unwrap_err();
    assert_eq!(none["code"], -32004, "{none}");
    assert_eq!(updates(&a), 1, "a refused one changes nothing");

    // undone: the file loses it, and nothing of the change is left
    let after = a.ok("changes/revert_hunk", hunk(&last)).await;
    assert_eq!(after["files"], json!([]));
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "ONE\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(updates(&a), 2);
    b.until(|m| event(m, "changes_updated")).await;

    // during a turn, the changes can be looked at but not changed
    a.ok("turn/start", json!({"thread": thread, "text": "and two"}))
        .await;
    let began = Instant::now();
    while bodies.lock().unwrap().len() < 5 {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let two = json!({"old_start": 2, "new_start": 2, "removed": ["two"], "added": ["TWO"]});
    let live = a.ok("changes/list", json!({"thread": thread})).await;
    assert_eq!(hunks_of(&live), [("notes.txt".to_owned(), json!([two]))]);
    let base = a
        .ok(
            "changes/base",
            json!({"thread": thread, "path": "notes.txt"}),
        )
        .await;
    assert_eq!(base["text"], "ONE\ntwo\nthree\nfour\nfive\n");
    for (method, params) in [
        ("changes/accept_hunk", hunk(&two)),
        ("changes/revert_hunk", hunk(&two)),
        ("changes/accept", json!({"thread": thread, "path": null})),
        (
            "changes/revert",
            json!({"thread": thread, "path": "notes.txt", "force": false}),
        ),
    ] {
        let busy = a.call(method, params).await.unwrap_err();
        assert_eq!(busy["code"], -32002, "{method}: {busy}");
    }
    assert_eq!(
        std::fs::read_to_string(&notes).unwrap(),
        "ONE\nTWO\nthree\nfour\nfive\n"
    );
    // the turn's first request told the model what was undone
    let sent = bodies.lock().unwrap()[3]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .clone();
    assert_eq!(
        sent,
        "and two\n\n[ognivo] Since your last turn the user undid part of your changes to notes.txt. Read them again before you edit them."
    );
    a.ok("turn/interrupt", json!({"thread": thread})).await;
    a.until(|m| event(m, "turn_completed")).await;
    // between turns, from the agent's own journal: the same
    let now = a.ok("changes/list", json!({"thread": thread})).await;
    assert_eq!(now, live);
    // what only told the clients there is not kept
    let read = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert!(
        read["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["event"] != "changes_updated"),
        "{read}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Each generation's progress in the stream as each of its phases began: the phase, how far
/// the prompt was read, the tokens so far.
fn phases_of(events: &[Value], generation: u32) -> Vec<(String, Value, Value)> {
    let mut began: Vec<(String, Value, Value)> = Vec::new();
    for p in events
        .iter()
        .filter(|e| e["event"] == "progress" && e["progress"]["generation"] == generation)
        .map(|e| &e["progress"])
    {
        let phase = p["phase"].as_str().unwrap().to_owned();
        if began.last().is_none_or(|(last, ..)| *last != phase) {
            began.push((phase, p["prompt_done"].clone(), p["generated"].clone()));
        }
    }
    began
}

/// What the user sees while a turn works, and what they write to it meanwhile: how far each
/// generation got, live and never kept; what they write during a step reaches the model
/// with the next one, as an item of its own; what a stopped turn never gave the model comes
/// back with its end; and with no turn running nothing is taken.
#[tokio::test]
async fn a_turn_shows_how_far_it_got_and_takes_what_the_user_writes_meanwhile() {
    let (url, bodies) = fake_server(vec![
        progress_call_stream("c1", "shell", r#"{"command":"touch made.txt"}"#),
        answer_stream("Done."),
        // the second turn waits for the server until it is stopped
        HANG.to_owned(),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    let steer = |text: &str| json!({"thread": thread, "text": text});

    // no turn runs: the client starts one with the text instead; an empty text is none;
    // a thread that is not open has no turn
    let idle = a
        .call("turn/steer", steer("Then list it."))
        .await
        .unwrap_err();
    assert_eq!(idle["code"], -32008, "{idle}");
    let empty = a.call("turn/steer", steer(" \n")).await.unwrap_err();
    assert_eq!(empty["code"], -32602, "{empty}");
    let closed = a
        .call("turn/steer", json!({"thread": 999_999, "text": "x"}))
        .await
        .unwrap_err();
    assert_eq!(closed["code"], -32001, "{closed}");

    // written while the command waits for its answer: the model gets it with its next step
    a.ok(
        "turn/start",
        json!({"thread": thread, "text": "make a file"}),
    )
    .await;
    let ask = a
        .until(|m| m["method"] == "approval/ask")
        .await
        .pop()
        .unwrap();
    assert_eq!(a.ok("turn/steer", steer("Then list it.")).await, json!({}));
    a.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"decision": "allow", "instead": null}}))
        .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    let end = &got.last().unwrap()["params"];
    assert_eq!(end["answer"], "Done.");
    assert_eq!(
        end.get("undelivered"),
        None,
        "all of it was delivered: {end}"
    );
    let sent = bodies.lock().unwrap()[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(sent["role"], "tool", "{sent}");
    assert!(
        sent["content"]
            .as_str()
            .unwrap()
            .ends_with("\n\n[ognivo] The user wrote while you worked: «Then list it.»"),
        "{sent}"
    );
    // an item of no generation, after the call and before the next step's answer
    let started: Vec<(&str, String)> = a
        .events
        .iter()
        .filter(|e| e["event"] == "item_started")
        .map(|e| {
            let item = &e["item"];
            (
                item["kind"].as_str().unwrap(),
                item["generation"].to_string(),
            )
        })
        .collect();
    assert_eq!(
        started,
        [
            ("reasoning", "1".to_owned()),
            ("tool_call", "1".to_owned()),
            ("approval", "null".to_owned()),
            ("steer", "null".to_owned()),
            ("agent_message", "2".to_owned()),
        ]
    );
    let item = a
        .events
        .iter()
        .find(|e| e["event"] == "item_completed" && e["item"]["kind"] == "steer")
        .unwrap();
    assert_eq!(item["item"]["text"], "Then list it.");

    // how far each generation got, live, in the server's numbers where it gives them
    let null = Value::Null;
    let phase =
        |p: &str, done: &Value, generated: u32| (p.to_owned(), done.clone(), json!(generated));
    assert_eq!(
        phases_of(&a.events, 1),
        [
            phase("waiting", &null, 0),
            phase("prompt", &json!(4000), 0),
            phase("reasoning", &json!(12400), 1),
            phase("writing", &json!(12400), 2),
        ]
    );
    let prompt = a
        .events
        .iter()
        .find(|e| e["event"] == "progress" && e["progress"]["phase"] == "prompt")
        .unwrap();
    assert_eq!(
        (
            &prompt["thread"],
            &prompt["progress"]["prompt_total"],
            &prompt["progress"]["prompt_cached"]
        ),
        (&thread, &json!(12400), &json!(4000))
    );
    // a server that does not count: the fragments streamed
    assert_eq!(
        phases_of(&a.events, 2),
        [phase("waiting", &null, 0), phase("writing", &null, 1)]
    );
    assert_eq!(bodies.lock().unwrap()[0]["return_progress"], json!(true));
    // the store keeps the steer's item, not the progress
    let read = a
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    let kept: Vec<&Value> = a.events.iter().filter(|e| is_kept(e)).collect();
    let read: Vec<&Value> = read["events"].as_array().unwrap().iter().collect();
    assert_eq!(read, kept);
    assert!(read.iter().all(|e| e["event"] != "progress"));
    assert!(read.iter().any(|e| e["item"]["kind"] == "steer"));

    // written while the next turn waits for the server, which never answers: stopped, the
    // turn gives it back, for the next one
    a.ok("turn/start", json!({"thread": thread, "text": "and more"}))
        .await;
    let began = Instant::now();
    while bodies.lock().unwrap().len() < 3 {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a.ok("turn/steer", steer("And the README.")).await;
    a.ok("turn/interrupt", json!({"thread": thread})).await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    let end = got.last().unwrap()["params"].clone();
    assert_eq!(
        (&end["end"], &end["undelivered"]),
        (&json!("interrupted"), &json!(["And the README."]))
    );
    let read = a
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert_eq!(read["events"].as_array().unwrap().last(), Some(&end));
    let idle = a.call("turn/steer", steer("late")).await.unwrap_err();
    assert_eq!(idle["code"], -32008, "{idle}");
    child.kill().unwrap();
    child.wait().unwrap();
}

// it stops the core with SIGTERM, which Windows has not
#[cfg(unix)]
#[tokio::test]
async fn the_projects_allow_rules_count_once_a_client_trusts_them() {
    let (url, _) = fake_server(vec![
        call_stream("c1", "shell", r#"{"command":"touch made.txt"}"#),
        answer_stream("Touched."),
        answer_stream("Touched."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir_all(root.join(".claude")).unwrap();
    std::fs::write(
        root.join(".claude/settings.json"),
        r#"{"permissions": {"allow": ["Bash(touch:*)"]}}"#,
    )
    .unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    // the start waits for the answer, so it goes on another task
    a.send(json!({"jsonrpc": "2.0", "id": 100, "method": "thread/start", "params": start(&root, &url)}))
        .await;
    let ask = a
        .until(|m| m["method"] == "rules/trust")
        .await
        .pop()
        .unwrap();
    assert_eq!(
        ask["params"],
        json!({"project": root, "rules": [{"text": "Bash(touch:*)", "source": ".claude/settings.json", "any": false}]})
    );
    a.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"trust": true}}))
        .await;
    let opened = loop {
        let m = a.read().await;
        if m["id"] == 100 {
            break m["result"].clone();
        }
    };
    assert!(
        opened["notes"]
            .as_array()
            .unwrap()
            .contains(&json!("rules .claude/settings.json (1)")),
        "{opened}"
    );
    let thread = opened["thread"].clone();
    a.ok(
        "turn/start",
        json!({"thread": thread, "text": "make a file"}),
    )
    .await;
    // trusted: the command runs without a question
    let got = a.until(|m| event(m, "turn_completed")).await;
    assert!(got.iter().all(|m| m["method"] != "approval/ask"), "{got:?}");
    assert!(root.join("made.txt").exists());
    // SIGTERM closes the open thread, which is kept, and the socket goes
    let pid = rustix::process::Pid::from_child(&child);
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let status = tokio::task::spawn_blocking(move || child.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success(), "{status:?}");
    assert!(!dir.path().join("run/core.sock").exists());
    let mut core = core(dir.path(), "30");
    let mut b = Client::connect(&socket(dir.path())).await;
    let list = b
        .ok("thread/list", json!({"project": root, "limit": null}))
        .await;
    assert_eq!(list["threads"][0]["thread"], thread);
    assert_eq!(list["threads"][0]["turns"], 1);
    core.kill().unwrap();
    core.wait().unwrap();
}

#[tokio::test]
async fn a_turn_is_written_anew_and_a_thread_is_deleted() {
    let (url, bodies) = fake_server(vec![
        answer_stream("First."),
        answer_stream("Second."),
        answer_stream("Better."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    for text in ["first", "second"] {
        a.ok("turn/start", json!({"thread": thread, "text": text}))
            .await;
        a.until(|m| event(m, "turn_completed")).await;
    }

    // the second message, written anew: the conversation goes back to before it
    a.ok(
        "turn/edit",
        json!({"thread": thread, "turn": 2, "text": "second, better"}),
    )
    .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    let names: Vec<&str> = got
        .iter()
        .filter(|m| m["method"] == "event")
        .map(|m| m["params"]["event"].as_str().unwrap())
        .take(2)
        .collect();
    assert_eq!(names, ["thread_rewound", "turn_started"]);
    let rewound = got.iter().find(|m| event(m, "thread_rewound")).unwrap();
    assert_eq!(rewound["params"]["turn"], 2);
    let started = got.iter().find(|m| event(m, "turn_started")).unwrap();
    assert_eq!(
        (&started["params"]["turn"], &started["params"]["user"]),
        (&json!(3), &json!("second, better"))
    );
    assert_eq!(got.last().unwrap()["params"]["answer"], "Better.");
    let sent = bodies.lock().unwrap().last().unwrap()["messages"].clone();
    let users: Vec<&str> = sent
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(users.len(), 3, "{users:?}");
    assert_eq!(users[1], "first");
    assert!(
        users[2].starts_with(
            "second, better\n\n[ognivo] The user took the conversation back to before this message"
        ),
        "{users:?}"
    );
    // what is not in the conversation cannot be written anew
    for turn in [0, 2, 9] {
        let e = a
            .call(
                "turn/edit",
                json!({"thread": thread, "turn": turn, "text": "x"}),
            )
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32006, "turn {turn}: {e}");
    }
    // the store keeps the rewind as it went
    let read = a
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert!(
        read["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "thread_rewound")
    );

    // listed by another spelling of the project's path, as VS Code names `C:\` as `c:\`
    let spelled = root.join("sub").join("..");
    std::fs::create_dir(root.join("sub")).unwrap();
    let list = a
        .ok("thread/list", json!({"project": spelled, "limit": null}))
        .await;
    assert_eq!(list["threads"][0]["thread"], thread, "{list}");

    // deleted, it is gone from the list and the store, and closed
    a.ok("thread/delete", json!({"thread": thread})).await;
    let list = a
        .ok("thread/list", json!({"project": root, "limit": null}))
        .await;
    assert_eq!(list["threads"], json!([]));
    let read = a
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert_eq!(read["events"], json!([]));
    let e = a
        .call("turn/start", json!({"thread": thread, "text": "more"}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], -32001);
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A store that fails is told to the clients once, as ognivo's notice in the running turn,
/// and the thread goes on in memory. Here another program took the thread's session out of
/// the store, so every save of it breaks a foreign key.
#[tokio::test]
async fn a_store_that_fails_is_told_to_the_clients_in_the_turn() {
    let (url, _) = fake_server(vec![answer_stream("Done."), answer_stream("Still done.")]).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    ognivo_store::Store::open(&dir.path().join("data/ognivo.db"))
        .unwrap()
        .delete_session(thread.as_i64().unwrap())
        .unwrap();
    let told = |events: &[Value]| -> Vec<String> {
        events
            .iter()
            .filter(|e| e["event"] == "item_completed" && e["item"]["kind"] == "notice")
            .filter_map(|e| e["item"]["text"].as_str())
            .filter(|t| t.starts_with("[ognivo] The session store failed: "))
            .map(str::to_owned)
            .collect()
    };
    let params = |got: Vec<Value>| -> Vec<Value> {
        got.into_iter()
            .filter(|m| m["method"] == "event")
            .map(|m| m["params"].clone())
            .collect()
    };
    a.ok("turn/start", json!({"thread": thread, "text": "fix it"}))
        .await;
    let got = params(a.until(|m| event(m, "turn_completed")).await);
    assert_eq!(
        told(&got),
        [
            "[ognivo] The session store failed: FOREIGN KEY constraint failed. The session goes on in memory and is no longer saved."
        ]
    );
    assert_eq!(got.last().unwrap()["answer"], "Done.");
    // once: the next turn has nothing more to tell
    a.ok("turn/start", json!({"thread": thread, "text": "and now?"}))
        .await;
    let got = params(a.until(|m| event(m, "turn_completed")).await);
    assert!(told(&got).is_empty(), "{got:?}");
    assert_eq!(got.last().unwrap()["answer"], "Still done.");
    // the core's log of the stream keeps it, for a client that comes later
    let read = a
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert_eq!(told(read["events"].as_array().unwrap()).len(), 1);
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A store that fails while the server holds the turn's request is first found out by the
/// saves of the stop: the stopped turn tells it before its end, as a running turn would.
#[tokio::test]
async fn a_store_that_fails_as_a_turn_is_stopped_is_told_in_that_turn() {
    let (url, bodies) = fake_server(vec![HANG.to_owned()]).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    a.ok("turn/start", json!({"thread": thread, "text": "wait"}))
        .await;
    let began = Instant::now();
    while bodies.lock().unwrap().is_empty() {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // nothing is saved while the server holds the request: the stop is the first to find
    // the session gone
    ognivo_store::Store::open(&dir.path().join("data/ognivo.db"))
        .unwrap()
        .delete_session(thread.as_i64().unwrap())
        .unwrap();
    a.ok("turn/interrupt", json!({"thread": thread})).await;
    let got: Vec<Value> = a
        .until(|m| event(m, "turn_completed"))
        .await
        .into_iter()
        .filter(|m| m["method"] == "event")
        .map(|m| m["params"].clone())
        .collect();
    let end = got.last().unwrap();
    assert_eq!(end["end"], "interrupted", "{end}");
    let told: Vec<&Value> = got
        .iter()
        .filter(|e| e["event"] == "item_completed" && e["item"]["kind"] == "notice")
        .filter(|e| {
            e["item"]["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("[ognivo] The session store failed: "))
        })
        .collect();
    assert_eq!(told.len(), 1, "{got:?}");
    assert_eq!(
        told[0]["item"]["text"],
        "[ognivo] The session store failed: FOREIGN KEY constraint failed. The session goes on in memory and is no longer saved."
    );
    // an item of the stopped turn
    assert_eq!(told[0]["item"]["turn"], end["turn"]);
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn a_thread_plans_in_plan_mode_and_carries_the_plan_out_once_switched() {
    let write = r#"{"path":"plan.txt","content":"one\n"}"#;
    let (url, _) = fake_server(vec![
        call_stream("w1", "write", write),
        answer_stream("The plan: plan.txt."),
        call_stream("w2", "write", write),
        answer_stream("Done."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let mut params = start(&root, &url);
    params["mode"] = json!("plan");
    let opened = a.ok("thread/start", params).await;
    assert_eq!(opened["mode"], "plan");
    let thread = opened["thread"].clone();

    // the write is refused without a question, and the plan comes
    a.ok(
        "turn/start",
        json!({"thread": thread, "text": "plan a file"}),
    )
    .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    assert!(got.iter().all(|m| m["method"] != "approval/ask"), "{got:?}");
    let refused = got
        .iter()
        .find(|m| event(m, "item_completed") && m["params"]["item"]["call"]["name"] == "Write")
        .unwrap();
    assert_eq!(refused["params"]["item"]["result"]["outcome"], "refused");
    assert_eq!(
        got.last().unwrap()["params"]["answer"],
        "The plan: plan.txt."
    );
    assert!(!root.join("plan.txt").exists());

    // switched, the next turn changes the file
    a.ok(
        "thread/mode",
        json!({"thread": thread, "mode": "edit_automatically"}),
    )
    .await;
    a.ok(
        "turn/start",
        json!({"thread": thread, "text": "Carry out the plan."}),
    )
    .await;
    a.until(|m| event(m, "turn_completed")).await;
    assert_eq!(
        std::fs::read_to_string(root.join("plan.txt")).unwrap(),
        "one\n"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A PNG of `width`×`height`, in base64: its header, which is all ognivo reads of it.
fn png(width: u32, height: u32) -> (Vec<u8>, String) {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend(width.to_be_bytes());
    b.extend(height.to_be_bytes());
    b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&b);
    (b, data)
}

#[tokio::test]
async fn images_reach_a_model_that_sees_and_a_thread_whose_model_does_not_refuses_them() {
    let (url, bodies) = fake_seeing_server(vec![
        call_stream("r1", "read", r#"{"file":"shot.png"}"#),
        answer_stream("A cat, twice."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let (shot, _) = png(8, 6);
    std::fs::write(root.join("shot.png"), &shot).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let mut params = start(&root, &url);
    params["profile"] = json!("gemma-4-26b-a4b");
    let opened = a.ok("thread/start", params).await;
    assert_eq!(opened["vision"], true);
    let thread = opened["thread"].clone();

    // what is not an image, or too many, is refused before a turn starts
    let e = a
        .call(
            "turn/start",
            json!({"thread": thread, "text": "see", "images": [{"data": "not base64!"}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (e["code"].as_i64(), e["message"].as_str()),
        (Some(-32602), Some("image 1: not base64"))
    );
    let (_, data) = png(4, 2);
    let many: Vec<Value> = (0..9).map(|_| json!({"data": data})).collect();
    let e = a
        .call(
            "turn/start",
            json!({"thread": thread, "text": "see", "images": many}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], -32602, "{e}");

    // the user's image goes with the message, and the image file the call reads with its
    // result
    let url_of = format!("data:image/png;base64,{data}");
    a.ok(
        "turn/start",
        json!({"thread": thread, "text": "What is it?", "images": [{"data": url_of}]}),
    )
    .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    let started = got.iter().find(|m| event(m, "turn_started")).unwrap();
    assert_eq!(
        started["params"]["images"],
        json!([{"mime": "image/png", "data": data, "width": 4, "height": 2}])
    );
    let read = got
        .iter()
        .find(|m| event(m, "item_completed") && m["params"]["item"]["call"]["name"] == "Read")
        .unwrap_or_else(|| panic!("{got:#?}"));
    let result = &read["params"]["item"]["result"];
    assert_eq!(result["images"][0]["width"], 8);
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .ends_with("PNG, 8×6, 33 bytes; it follows]"),
        "{result}"
    );
    let bodies = bodies.lock().unwrap().clone();
    let user = bodies[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|m| m["role"] == "user")
        .unwrap()
        .clone();
    assert_eq!(
        user["content"],
        json!([
            {"type": "image_url", "image_url": {"url": url_of}},
            {"type": "text", "text": "What is it?"},
        ])
    );
    let tool = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(tool["role"], "tool");
    assert_eq!(
        tool["content"][1]["image_url"]["url"],
        format!("data:image/png;base64,{}", png(8, 6).1)
    );

    // a thread whose server runs no projector takes none
    let (blind, _) = fake_server(Vec::new()).await;
    let mut params = start(&root, &blind);
    params["profile"] = json!("gemma-4-26b-a4b");
    let opened = a.ok("thread/start", params).await;
    assert_eq!(opened["vision"], false);
    let e = a
        .call(
            "turn/start",
            json!({"thread": opened["thread"], "text": "see", "images": [{"data": data}]}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], -32005, "{e}");
    assert!(e["message"].as_str().unwrap().contains("--mmproj"), "{e}");
    let listed = a
        .ok("thread/list", json!({"project": null, "limit": 10}))
        .await;
    let seeing: Vec<&Value> = listed["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["vision"])
        .collect();
    assert_eq!(seeing, [&json!(false), &json!(true)]);
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Whether `thread/list` says the model of `thread` takes images.
async fn listed_vision(a: &mut Client, thread: &Value) -> Value {
    let all = a
        .ok("thread/list", json!({"project": null, "limit": 10}))
        .await;
    let listed = all["threads"].as_array().unwrap();
    let mine = listed.iter().find(|t| &t["thread"] == thread).unwrap();
    mine["vision"].clone()
}

/// How a turn of `thread` with `text` ends.
async fn turn_end(a: &mut Client, thread: &Value, text: &str) -> Value {
    a.ok("turn/start", json!({"thread": thread, "text": text}))
        .await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    got.last().unwrap()["params"]["end"].clone()
}

/// A server started with `--api-key` (2026-11-20, the owner: «нужно добавить поддержку API
/// ключей для llama server»): a client without its key, or with another, is told so, apart
/// from a server that does not answer, by `server/detect` and by a thread that would start
/// there, its profile named or not; with the key the thread runs. The store keeps no key,
/// so a thread resumed without it is told the same, and with it goes on.
#[tokio::test]
async fn a_server_with_an_api_key_serves_the_clients_that_bring_it() {
    const UNAUTHORIZED: i64 = -32009;
    let (url, bodies) = fake_keyed_server(
        "sekret",
        vec![answer_stream("Hi."), answer_stream("Again.")],
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let keyed = |key: Option<&str>| json!({"url": url, "api_key": key, "model": null});
    let started = |key: Option<&str>, profile: Value| {
        let mut params = start(&root, &url);
        params["server"] = keyed(key);
        params["profile"] = profile;
        params
    };
    for key in [None, Some("another")] {
        let refused = a.call("server/detect", keyed(key)).await.unwrap_err();
        assert_eq!(refused["code"], UNAUTHORIZED, "{refused}");
        assert_eq!(
            refused["message"],
            format!(
                "{url} wants an API key, or does not take the one given (HTTP 401: Invalid API Key)"
            )
        );
        for profile in [Value::Null, json!("gpt-oss-20b")] {
            let refused = a
                .call("thread/start", started(key, profile))
                .await
                .unwrap_err();
            assert_eq!(refused["code"], UNAUTHORIZED, "{refused}");
        }
    }
    assert!(bodies.lock().unwrap().is_empty());

    let detected = a.ok("server/detect", keyed(Some("sekret"))).await;
    assert_eq!(detected["profile"], "gpt-oss-20b", "{detected}");
    let opened = a
        .ok("thread/start", started(Some("sekret"), Value::Null))
        .await;
    let thread = opened["thread"].clone();
    assert_eq!(opened["info"]["server"], url.as_str(), "{opened}");
    assert_eq!(turn_end(&mut a, &thread, "Hi").await, "answer");
    assert_eq!(bodies.lock().unwrap().len(), 1);

    a.ok("thread/close", json!({"thread": thread})).await;
    let resume = |server: Value| {
        json!({"thread": thread, "server": server, "native": false, "priors": false,
            "subagents": false, "context": null, "mode": null})
    };
    let refused = a
        .call("thread/resume", resume(Value::Null))
        .await
        .unwrap_err();
    assert_eq!(refused["code"], UNAUTHORIZED, "{refused}");
    a.ok("thread/resume", resume(keyed(Some("sekret")))).await;
    assert_eq!(turn_end(&mut a, &thread, "Again").await, "answer");
    assert_eq!(bodies.lock().unwrap().len(), 2);
    // nothing of the key is kept: not in the store, nor in its log of writes
    for entry in std::fs::read_dir(dir.path().join("data")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(6).any(|w| w == b"sekret"),
                "{}",
                path.display()
            );
        }
    }
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A thread opened while its llama-server still loaded the model, or ran without its
/// projector, takes images once the server runs one: `turn/start` asks the server again
/// before it refuses them, and `thread/vision` whenever a client asks, during a turn too.
/// What the server says counts either way; a server that cannot say leaves it as it was
/// (2026-11-16: the composer refused images of a server started with --mmproj).
#[tokio::test]
async fn a_thread_takes_images_once_its_server_runs_a_projector_and_asks_again_on_thread_vision() {
    let (url, bodies, projector) = fake_switched_server(
        vec![answer_stream("A cat."), HANG.to_owned()],
        // the server still loads its model
        None,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let mut params = start(&root, &url);
    params["profile"] = json!("gemma-4-26b-a4b");
    let opened = a.ok("thread/start", params).await;
    assert_eq!(opened["vision"], false, "{opened}");
    let thread = opened["thread"].clone();
    let switch = |sees: Option<bool>| *projector.lock().unwrap() = sees;
    let vision = json!({"thread": thread});

    // the server runs its projector now: the image goes, without a client asking first
    switch(Some(true));
    let (_, data) = png(4, 2);
    let url_of = format!("data:image/png;base64,{data}");
    let with_image = json!({"thread": thread, "text": "What is it?", "images": [{"data": url_of}]});
    a.ok("turn/start", with_image.clone()).await;
    let got = a.until(|m| event(m, "turn_completed")).await;
    let ended = got.last().unwrap();
    assert_eq!(ended["params"]["end"], "answer", "{got:#?}");
    let user = bodies.lock().unwrap()[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|m| m["role"] == "user")
        .unwrap()
        .clone();
    assert_eq!(
        user["content"],
        json!([
            {"type": "image_url", "image_url": {"url": url_of}},
            {"type": "text", "text": "What is it?"},
        ])
    );
    assert_eq!(listed_vision(&mut a, &thread).await, true);

    // started again without it: asked, the thread says so, and refuses images
    switch(Some(false));
    assert_eq!(
        a.ok("thread/vision", vision.clone()).await,
        json!({"vision": false})
    );
    assert_eq!(listed_vision(&mut a, &thread).await, false);
    let e = a.call("turn/start", with_image.clone()).await.unwrap_err();
    assert_eq!(e["code"], -32005, "{e}");
    assert!(e["message"].as_str().unwrap().contains("--mmproj"), "{e}");
    assert_eq!(bodies.lock().unwrap().len(), 1);

    // with it again, asked while a turn runs: beside the thread's task
    a.ok("turn/start", json!({"thread": thread, "text": "wait"}))
        .await;
    let began = Instant::now();
    while bodies.lock().unwrap().len() < 2 {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    switch(Some(true));
    assert_eq!(
        a.ok("thread/vision", vision.clone()).await,
        json!({"vision": true})
    );
    // a server that cannot say (it loads its model again) leaves the flag as it was
    switch(None);
    assert_eq!(
        a.ok("thread/vision", vision.clone()).await,
        json!({"vision": true})
    );
    a.ok("turn/interrupt", vision.clone()).await;
    a.until(|m| event(m, "turn_completed")).await;
    assert_eq!(listed_vision(&mut a, &thread).await, true);

    // a thread the core does not have open
    a.ok("thread/close", vision.clone()).await;
    let e = a.call("thread/vision", vision).await.unwrap_err();
    assert_eq!(e["code"], -32001, "{e}");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Each turn sees as the server says: a thread opened while its llama-server loaded the
/// model shows the model the image file a Read gives once the server runs its projector,
/// with no client asking; a server started again without it refuses the conversation's
/// image once, and the thread, asking it after that error, gives the model what the image
/// was from the next turn on instead of failing every turn.
#[tokio::test]
async fn each_turn_sees_as_the_server_says_and_one_that_lost_its_projector_fails_only_once() {
    let (url, bodies, projector) = fake_switched_server(
        vec![
            call_stream("r1", "read", r#"{"file":"shot.png"}"#),
            answer_stream("A cat."),
            no_projector(),
            answer_stream("Going on."),
        ],
        // the server still loads its model
        None,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let (shot, data) = png(8, 6);
    std::fs::write(root.join("shot.png"), &shot).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let mut params = start(&root, &url);
    params["profile"] = json!("gemma-4-26b-a4b");
    let opened = a.ok("thread/start", params).await;
    assert_eq!(opened["vision"], false, "{opened}");
    let thread = opened["thread"].clone();
    let switch = |sees: Option<bool>| *projector.lock().unwrap() = sees;

    // with its projector now, the Read's image goes after its result
    switch(Some(true));
    assert_eq!(
        turn_end(&mut a, &thread, "What does shot.png show?").await,
        "answer"
    );
    let tool = bodies.lock().unwrap()[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(tool["role"], "tool");
    assert_eq!(
        tool["content"][1]["image_url"]["url"],
        format!("data:image/png;base64,{data}")
    );
    assert_eq!(listed_vision(&mut a, &thread).await, true);

    // started again without it: the image the conversation holds is refused
    switch(Some(false));
    assert_eq!(turn_end(&mut a, &thread, "And now?").await, "error");
    assert!(bodies.lock().unwrap()[2].to_string().contains("image_url"));
    // and the next turn has what it was instead
    assert_eq!(turn_end(&mut a, &thread, "Go on.").await, "answer");
    let last = bodies.lock().unwrap()[3].to_string();
    assert!(!last.contains("image_url"), "{last}");
    assert!(
        last.contains("Not shown, as this model reads no images"),
        "{last}"
    );
    assert_eq!(listed_vision(&mut a, &thread).await, false);
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Behind llama-server's router, a thread asks its model's server: `/props` with `?model=`.
#[tokio::test]
async fn behind_a_router_thread_vision_asks_the_server_of_the_threads_model() {
    let models = json!([
        {"id": "gpt-oss-20b-F16", "aliases": [], "status": {"value": "loaded", "args": []}}
    ]);
    let (url, _, gets) = fake_router(models, vec![]).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let opened = a.ok("thread/start", start(&root, &url)).await;
    let before = gets.lock().unwrap().len();
    let vision = a
        .ok("thread/vision", json!({"thread": opened["thread"]}))
        .await;
    // its /props do not say: the flag stays
    assert_eq!(vision, json!({"vision": false}));
    let gets = gets.lock().unwrap();
    assert!(
        gets[before..]
            .iter()
            .any(|g| g.starts_with("GET /props?model=gpt-oss-20b-F16 ")),
        "{gets:?}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

// the browser and the OAuth server are Python under /bin/sh
#[cfg(unix)]
#[tokio::test]
async fn a_client_is_asked_to_sign_in_and_the_thread_starts_signed_in() {
    let (mut mcp, mcp_url) = common::oauth_server();
    let (url, _) = fake_server(Vec::new()).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ognivo")).unwrap();
    let config = json!({"mcpServers": {"docs": {"type": "http", "url": mcp_url}}});
    std::fs::write(dir.path().join(".ognivo/mcp.json"), config.to_string()).unwrap();
    let browser = common::browser(dir.path());
    let root = dir.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let mut child = core_with(dir.path(), "30", &[("BROWSER", &browser)]);
    let mut a = Client::connect(&socket(dir.path())).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    a.send(json!({"jsonrpc": "2.0", "id": 100, "method": "thread/start", "params": start(&root, &url)}))
        .await;
    let ask = a
        .until(|m| m["method"] == "mcp/sign_in")
        .await
        .pop()
        .unwrap();
    assert_eq!(
        ask["params"],
        json!({"project": root, "server": {"name": "docs", "command": mcp_url}})
    );
    a.send(json!({"jsonrpc": "2.0", "id": ask["id"], "result": {"sign_in": true}}))
        .await;
    let opened = loop {
        let m = a.read().await;
        if m["id"] == 100 {
            break m["result"].clone();
        }
    };
    let notes = opened["notes"].as_array().unwrap();
    assert!(notes.contains(&json!("mcp docs: 1 tool")), "{opened}");
    assert!(
        !notes
            .iter()
            .any(|n| n.as_str().is_some_and(|n| n.contains("sign in"))),
        "{opened}"
    );
    assert!(dir.path().join(".ognivo/mcp-auth/docs.json").exists());

    // the panel: the thread's servers, the config files, a sign-out and a sign-in again
    let servers = a
        .ok("mcp/servers", json!({"thread": opened["thread"]}))
        .await;
    let docs = &servers["servers"][0];
    assert_eq!(
        (
            &docs["name"],
            &docs["state"],
            &docs["remote"],
            &docs["signed_in"]
        ),
        (
            &json!("docs"),
            &json!("running"),
            &json!(true),
            &json!(true)
        ),
        "{servers}"
    );
    assert_eq!(docs["tools"][0]["name"], "mcp__docs__whoami", "{servers}");
    let project = json!({"project": root});
    let files = a.ok("mcp/config", project.clone()).await;
    assert_eq!(files["files"][0]["scope"], "user", "{files}");
    assert_eq!(files["files"][0]["path"], "~/.ognivo/mcp.json", "{files}");
    assert_eq!(files["files"][0]["servers"][0]["name"], "docs", "{files}");
    assert_eq!(files["files"][1]["scope"], "project", "{files}");
    let echo = json!({"command": common::PYTHON, "args": ["-I", "echo.py"]});
    let set =
        |value: Value| json!({"project": root, "scope": "project", "name": "echo", "value": value});
    a.ok("mcp/config/set", set(echo.clone())).await;
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(written["mcpServers"]["echo"], echo);
    let refused = a
        .call("mcp/config/set", set(json!({"args": ["no command"]})))
        .await
        .unwrap_err();
    assert_eq!(refused["code"], -32602, "{refused}");
    a.ok("mcp/config/set", set(Value::Null)).await;
    assert_eq!(
        a.ok("mcp/config", project.clone()).await["files"][1]["servers"],
        json!([])
    );
    let docs = json!({"project": root, "server": "docs"});
    a.ok("mcp/logout", docs.clone()).await;
    assert!(!dir.path().join(".ognivo/mcp-auth/docs.json").exists());
    a.ok("mcp/login", docs.clone()).await;
    assert!(dir.path().join(".ognivo/mcp-auth/docs.json").exists());
    child.kill().unwrap();
    child.wait().unwrap();
    mcp.kill().unwrap();
    mcp.wait().unwrap();
}

/// A client that would start another build of the core finds out which program runs
/// (`exe`), and asks it to quit: not while a turn runs, at once when none does, however
/// long its idle time.
#[tokio::test]
async fn the_core_names_its_program_and_quits_when_asked_while_no_turn_runs() {
    let (url, bodies) = fake_server(vec![HANG.to_owned()]).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let socket = socket(dir.path());
    let mut child = core(dir.path(), "600");
    let mut a = Client::connect(&socket).await;
    let init = a
        .ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let exe = init["exe"].as_str().unwrap().to_owned();
    assert!(Path::new(&exe).is_file(), "{exe}");

    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    a.ok("turn/start", json!({"thread": thread, "text": "wait"}))
        .await;
    let began = Instant::now();
    while bodies.lock().unwrap().is_empty() {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // a turn runs: the core stays
    let busy = a.call("core/quit", json!({})).await.unwrap_err();
    assert_eq!(busy["code"], -32002, "{busy}");
    assert!(child.try_wait().unwrap().is_none());

    a.ok("turn/interrupt", json!({"thread": thread})).await;
    a.until(|m| event(m, "turn_completed")).await;
    a.ok("core/quit", json!({})).await;
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the core did not quit"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "{status:?}");
}

/// A core that went away mid-turn (killed here, as a crash does, or a machine that goes
/// down) never wrote its turn's end. The next core ends that turn in the kept stream before
/// a client reads it, and once: no client shows it running with a stop that reaches no
/// core. The thread goes on from there, and its model hears why the turn ended.
#[tokio::test]
async fn a_turn_a_core_left_running_ends_before_the_next_core_shows_it() {
    let (url, bodies) = fake_server(vec![HANG.to_owned(), answer_stream("Going on.")]).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    let root = ognivo_os::canonicalize(&root).unwrap();
    let socket = socket(dir.path());
    let mut child = core(dir.path(), "60");
    let mut a = Client::connect(&socket).await;
    a.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let thread = a.ok("thread/start", start(&root, &url)).await["thread"].clone();
    a.ok("turn/start", json!({"thread": thread, "text": "wait"}))
        .await;
    a.until(|m| event(m, "turn_started")).await;
    // the core goes while the server holds its request
    let began = Instant::now();
    while bodies.lock().unwrap().is_empty() {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the request did not come"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    drop(a);

    let mut child = core(dir.path(), "60");
    let mut b = Client::connect(&socket).await;
    b.ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let listed = b
        .ok("thread/list", json!({"project": null, "limit": null}))
        .await;
    assert_eq!(listed["threads"][0]["running"], false, "{listed}");
    let ends = |read: &Value| -> Vec<Value> {
        read["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "turn_completed")
            .cloned()
            .collect()
    };
    let read = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    let last = read["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["event"], "turn_completed", "{read}");
    assert_eq!(last["turn"], 1);
    assert_eq!(last["end"], "interrupted");
    assert_eq!(last["why"], "ognivo stopped before the turn ended");
    // a client there as it ended got it live
    let live = b.until(|m| event(m, "turn_completed")).await;
    assert_eq!(live.last().unwrap()["params"]["seq"], last["seq"]);
    // read again: ended once
    let again = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    assert_eq!(ends(&again).len(), 1, "{again}");

    // the thread goes on, and its model hears why the last turn ended
    b.ok(
        "thread/resume",
        json!({
            "thread": thread, "server": null, "native": false, "priors": false,
            "subagents": false, "context": null, "mode": null
        }),
    )
    .await;
    b.ok("turn/start", json!({"thread": thread, "text": "go on"}))
        .await;
    let got = b
        .until(|m| event(m, "turn_completed") && m["params"]["turn"] == 2)
        .await;
    assert_eq!(got.last().unwrap()["params"]["answer"], "Going on.");
    let read = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    let ends = ends(&read);
    assert_eq!(
        ends.iter()
            .map(|e| (e["turn"].clone(), e["end"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!(1), json!("interrupted")),
            (json!(2), json!("answer"))
        ],
        "{read}"
    );
    let sent = bodies.lock().unwrap().last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"]
        .clone();
    assert!(
        sent.as_str()
            .unwrap()
            .contains("[ognivo] ognivo stopped before the last turn ended"),
        "{sent}"
    );
    drop(b);
    child.kill().unwrap();
    child.wait().unwrap();
}
