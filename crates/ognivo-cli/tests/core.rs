//! `ognivo core` end to end over its socket: a client starts a thread and runs a turn,
//! answers the question it is asked, reviews and reverts what changed; a second client
//! reads what it missed from the store; a turn is stopped and the model hears of it; and
//! the core quits once nobody is there. No GPU, no network.

mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{HANG, answer_stream, call_stream, fake_server};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

fn core(dir: &Path, idle: &str) -> Child {
    Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["core", "--idle", idle, "--socket"])
        .arg(dir.join("run/core.sock"))
        .arg("--data-dir")
        .arg(dir.join("data"))
        .env("HOME", dir)
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// A client of the core, as the GUI is one.
struct Client {
    read: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
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
            match UnixStream::connect(socket).await {
                Ok(s) => break s,
                Err(_) if began.elapsed() < Duration::from_secs(20) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(e) => panic!("{}: {e}", socket.display()),
            }
        };
        let (read, write) = stream.into_split();
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
    let root = root.canonicalize().unwrap();
    let socket = dir.path().join("run/core.sock");
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

    let opened = a.ok("thread/start", start(&root, &url)).await;
    let thread = opened["thread"].clone();
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
        .map(|e| {
            let kind = e["item"]["kind"].as_str().unwrap_or_default();
            (e["event"].as_str().unwrap().to_owned(), kind.to_owned())
        })
        .filter(|(e, _)| e != "item_delta" && e != "generation")
        .collect();
    let want = [
        ("thread_started", ""),
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

    // another client reads what it missed: every event but the deltas, as they were sent
    let mut b = Client::connect(&socket).await;
    let init = b
        .ok("initialize", json!({"client": "test", "protocol": 1}))
        .await;
    let last = a.events.last().unwrap()["seq"].clone();
    assert_eq!(init["seq"], last);
    let read = b
        .ok("thread/read", json!({"thread": thread, "after": null}))
        .await;
    let kept: Vec<&Value> = a
        .events
        .iter()
        .filter(|e| e["event"] != "item_delta")
        .collect();
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
    let root = root.canonicalize().unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&dir.path().join("run/core.sock")).await;
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
    let mut b = Client::connect(&dir.path().join("run/core.sock")).await;
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
    let root = root.canonicalize().unwrap();
    let mut child = core(dir.path(), "30");
    let mut a = Client::connect(&dir.path().join("run/core.sock")).await;
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
