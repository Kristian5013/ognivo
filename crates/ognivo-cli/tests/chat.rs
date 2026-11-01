//! `ognivo chat` end to end against the fake llama-server. Off a terminal the line editor
//! reads plain lines, so stdin carries both the messages and the answers to ognivo's
//! questions.

mod common;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{HANG, answer_stream, call_stream, fake_server};
use serde_json::json;

fn spawn(url: &str, root: &Path, data: &Path, stdin: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args([
            "chat",
            "--url",
            url,
            "--profile",
            "gpt-oss-20b",
            "--seed",
            "3",
            "--data-dir",
        ])
        .arg(data)
        .arg("--cwd")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    (dir, root)
}

#[tokio::test]
async fn asks_runs_and_remembers_always_for_the_session() {
    // each turn: the call, an answer sent back once to verify the side effect, the answer
    let turn = |id: &str, text: &str| {
        [
            call_stream(id, "shell", r#"{"command":"touch made.txt"}"#),
            answer_stream(text),
            answer_stream(text),
        ]
    };
    let mut responses = Vec::new();
    for (id, text) in [("c1", "Touched."), ("c2", "Again."), ("c3", "Third.")] {
        responses.extend(turn(id, text));
    }
    let (url, bodies) = fake_server(responses).await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    let stdin = "make a file\ny\n/clear\nonce more\na\nand again\n/exit\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(root.join("made.txt").exists());
    assert!(
        stdout.starts_with("ognivo chat · gpt-oss-20b · http://127.0.0.1:"),
        "{stdout}"
    );
    assert!(stdout.contains("n_ctx 4096"), "{stdout}");
    // asked twice, the second time answered "a"; the third run went without a question
    assert_eq!(
        stdout.matches("? shell touch made.txt").count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .matches("· allowed for this session: touch made.txt")
            .count(),
        1,
        "{stdout}"
    );
    for text in [
        "→ shell",
        "Touched.",
        "· new conversation",
        "Again.",
        "Third.",
    ] {
        assert!(stdout.contains(text), "{text}: {stdout}");
    }
    assert!(
        stdout.contains("[ognivo] NOT verified: `touch made.txt`"),
        "{stdout}"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 9);
    // after /clear the conversation starts over: system and the new message
    assert_eq!(
        bodies[3]["messages"][1],
        json!({"role": "user", "content": "once more"})
    );
    assert_eq!(bodies[3]["messages"].as_array().unwrap().len(), 2);
    let history = data.join("chat_history");
    let text = std::fs::read_to_string(&history).unwrap();
    assert!(
        text.contains("make a file") && text.contains("and again"),
        "{text}"
    );
    assert!(!text.contains("/exit"), "{text}");
    let mode = std::fs::metadata(&history).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn a_declined_command_tells_the_model_why() {
    let (url, bodies) = fake_server(vec![
        call_stream("c1", "shell", r#"{"command":"touch nope.txt"}"#),
        answer_stream("OK, not touching it."),
    ])
    .await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    // a block message, then the answer to the question; stdin ends after it
    let stdin = "\"\"\"\nfirst line\nsecond line\n\"\"\"\nn use echo instead\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert!(!root.join("nope.txt").exists());
    assert!(
        stdout.contains("✗ shell: The user declined `touch nope.txt` and says: use echo instead"),
        "{stdout}"
    );
    assert!(stdout.contains("OK, not touching it."), "{stdout}");
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies[0]["messages"][1],
        json!({"role": "user", "content": "first line\nsecond line"})
    );
    let last = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["role"], json!("tool"));
    assert_eq!(
        last["content"],
        json!("[refused] The user declined `touch nope.txt` and says: use echo instead")
    );
}

#[tokio::test]
async fn ctrl_c_stops_the_turn_and_the_chat_goes_on() {
    let (url, bodies) = fake_server(vec![HANG.to_owned(), answer_stream("Going on.")]).await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    let watched = bodies.clone();
    let out = tokio::task::spawn_blocking(move || {
        let child = spawn(&url, &root, &data, "hello\ngo on\n/exit\n");
        // the first request is at the server: the turn runs
        for _ in 0..200 {
            if !watched.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
        child.wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert!(stdout.contains("· stopped"), "{stdout}");
    assert!(stdout.contains("Going on."), "{stdout}");
    // the model learns that the first turn was stopped
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let m = &bodies[1]["messages"];
    assert_eq!(
        m[1],
        json!({"role": "user", "content": "hello\n\n[ognivo] The user stopped this turn."})
    );
    assert_eq!(m[2], json!({"role": "user", "content": "go on"}));
}
