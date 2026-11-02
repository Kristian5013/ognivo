//! `ognivo run` end to end against a fake llama-server that answers with recorded-shape
//! SSE streams: read, then edit, then the final answer. No GPU, no network.

mod common;

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use common::{answer_stream, call_stream, fake_server};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn reads_edits_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("AGENTS.md"), "Keep the greeting short.\n").unwrap();
    let (url, bodies) = fake_server(vec![
        call_stream("r1", "read", r#"{"path":"main.rs"}"#),
        call_stream(
            "e1",
            "edit",
            r#"{"path":"main.rs","old_string":"\"hi\"","new_string":"\"hello\""}"#,
        ),
        answer_stream("Changed the greeting."),
    ])
    .await;

    let bin = env!("CARGO_BIN_EXE_ognivo");
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(bin)
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "gpt-oss-20b",
                "--seed",
                "5",
                "--data-dir",
            ])
            .arg(dir.path().join("data"))
            .arg("--cwd")
            .arg(&root)
            .env("HOME", dir.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"Say hello instead of hi.")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (out, root, dir)
    })
    .await
    .unwrap();
    let (out, root, _dir) = out;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
    assert_eq!(
        std::fs::read_to_string(root.join("main.rs")).unwrap(),
        "fn main() {\n    println!(\"hello\");\n}\n"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Changed the greeting."), "{stdout}");
    // nothing was built or tested after the edit, and ognivo says so under the answer
    assert!(stdout.contains("[ognivo] NOT verified: main.rs changed after the last passing build or test; no build or test ran after it"), "{stdout}");
    assert!(
        stderr.contains("→ read") && stderr.contains("← [edit: applied exact]"),
        "{stderr}"
    );
    assert!(
        stderr.starts_with("· instructions: AGENTS.md\n"),
        "{stderr}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    // the project's instructions follow the fixed ones; the prompt from stdin is the user
    // message; the second request carries the first step's call, its verbatim reasoning
    // and the read result
    let system = bodies[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.ends_with(
            "\n\n<instructions file=\"AGENTS.md\">\nKeep the greeting short.\n</instructions>"
        ),
        "{system}"
    );
    assert_eq!(
        bodies[0]["messages"][1],
        json!({"role": "user", "content": "Say hello instead of hi."})
    );
    let m = &bodies[1]["messages"];
    assert_eq!(m[2]["reasoning_content"], json!("next step"));
    assert_eq!(
        m[2]["tool_calls"][0]["function"]["arguments"],
        json!(r#"{"path":"main.rs"}"#)
    );
    assert_eq!(m[3]["role"], json!("tool"));
    assert!(m[3]["content"].as_str().unwrap().starts_with("[read: "));
    assert_eq!(
        (bodies[0]["seed"].clone(), bodies[2]["seed"].clone()),
        (json!(6), json!(8))
    );
}

#[test]
fn auto_approval_refuses_to_run_on_the_host() {
    if std::path::Path::new("/run/.containerenv").exists()
        || std::path::Path::new("/.dockerenv").exists()
    {
        return; // inside a container this mode is allowed
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["run", "--url", "http://127.0.0.1:1", "--approve", "auto"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("only inside a container"));
}

/// `timeout` stops the agent with SIGTERM: the end event is still written, exit 143.
#[tokio::test]
async fn sigterm_ends_the_run_with_an_end_event() {
    // a server that accepts the request and never answers
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    });
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let bin = env!("CARGO_BIN_EXE_ognivo");
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(bin)
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "qwen3.8-27b",
                "--jsonl",
                "--seed",
                "1",
                "--data-dir",
            ])
            .arg(&data)
            .arg("--cwd")
            .arg(dir.path())
            .env("HOME", dir.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"hello").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(800));
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        out.status.code(),
        Some(143),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let last = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    let end: Value = serde_json::from_str(&last).unwrap();
    assert_eq!(
        (
            end["type"].as_str(),
            end["stopped"].as_str(),
            end["exit"].as_i64()
        ),
        (Some("end"), Some("SIGTERM"), Some(143))
    );
}

/// At the time limit the run stops itself: exit 124 as with `timeout`, the end event says
/// why and what was verified, and the model had been told the limit.
#[tokio::test]
async fn the_time_limit_ends_the_run_with_exit_124() {
    // a server that reads the request and never answers
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let request = Arc::new(Mutex::new(String::new()));
    let seen = request.clone();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut part = [0u8; 65536];
        loop {
            let n = sock.read(&mut part).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            seen.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&part[..n]));
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let bin = env!("CARGO_BIN_EXE_ognivo");
    let started = std::time::Instant::now();
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(bin)
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "gemma-4-26b-a4b",
                "--jsonl",
            ])
            .args(["--seed", "1", "--time-limit", "2", "--data-dir"])
            .arg(&data)
            .arg("--cwd")
            .arg(dir.path())
            .env("HOME", dir.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"hello").unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        out.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let last = String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned();
    let end: Value = serde_json::from_str(&last).unwrap();
    assert_eq!(
        (end["stopped"].as_str(), end["exit"].as_i64()),
        (Some("time_limit"), Some(124))
    );
    assert_eq!(end["verification"], json!({"status": "no_changes"}));
    assert!(
        request.lock().unwrap().contains(
            "hello\\n\\n[ognivo] Time limit: 2 s. ognivo stops the turn when it runs out."
        ),
        "the model was not told the limit"
    );
}
