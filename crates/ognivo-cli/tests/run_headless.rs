//! `ognivo run` end to end against a fake llama-server that answers with recorded-shape
//! SSE streams: read, then edit, then the final answer. No GPU, no network.

mod common;

use std::io::Write as _;
use std::process::{Command, Stdio};

use common::{HANG, answer_stream, call_stream, fake_seeing_server, fake_server};
use serde_json::{Value, json};

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
            .arg("--atif")
            .arg(dir.path().join("trajectory.json"))
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
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
    // the project and its instructions follow the fixed system prompt; the prompt from
    // stdin is the user message; the second request carries the first step's call, its
    // verbatim reasoning and the read result
    let project = bodies[0]["messages"][1]["content"].as_str().unwrap();
    assert!(
        project.starts_with("[ognivo] The project is at ")
            && project.ends_with(
                "\n\n<instructions file=\"AGENTS.md\">\nKeep the greeting short.\n</instructions>"
            ),
        "{project}"
    );
    assert_eq!(
        bodies[0]["messages"][2],
        json!({"role": "user", "content": "Say hello instead of hi."})
    );
    let m = &bodies[1]["messages"];
    assert_eq!(m[3]["reasoning_content"], json!("next step"));
    assert_eq!(
        m[3]["tool_calls"][0]["function"]["arguments"],
        json!(r#"{"path":"main.rs"}"#)
    );
    assert_eq!(m[4]["role"], json!("tool"));
    assert!(m[4]["content"].as_str().unwrap().starts_with("[read: "));
    assert_eq!(
        (bodies[0]["seed"].clone(), bodies[2]["seed"].clone()),
        (json!(6), json!(8))
    );
    drop(bodies);

    // the trajectory in ATIF, the same as the store gives it later
    let text = std::fs::read_to_string(_dir.path().join("trajectory.json")).unwrap();
    let t: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(t["schema_version"], json!("ATIF-v1.7"));
    let sources: Vec<&str> = t["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    // the system prompt, then the project's context, an ognivo note to the model
    assert_eq!(
        sources,
        ["system", "system", "user", "agent", "agent", "agent"]
    );
    assert!(
        t["steps"][1]["message"]
            .as_str()
            .unwrap()
            .starts_with("[ognivo] The project is at ")
    );
    assert_eq!(t["steps"][5]["message"], json!("Changed the greeting."));
    let later = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["sessions", "--atif", "1", "--data-dir"])
        .arg(_dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        later.status.success(),
        "{}",
        String::from_utf8_lossy(&later.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&later.stdout), text);
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
// Windows has no SIGTERM to send
#[cfg(unix)]
#[tokio::test]
async fn sigterm_ends_the_run_with_an_end_event() {
    // a server that takes the request and never answers it
    let (url, _) = fake_server(vec![HANG.to_owned()]).await;
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
            .env("USERPROFILE", dir.path())
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
            end["exit"].as_i64(),
            end["session"].as_i64()
        ),
        (Some("end"), Some("SIGTERM"), Some(143), Some(1))
    );
}

/// At the time limit the run stops itself: exit 124 as with `timeout`, the end event says
/// why and what was verified, and the model had been told the limit.
#[tokio::test]
async fn the_time_limit_ends_the_run_with_exit_124() {
    // a server that takes the request and never answers it
    let (url, bodies) = fake_server(vec![HANG.to_owned()]).await;
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
            .env("USERPROFILE", dir.path())
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
    assert_eq!(
        bodies.lock().unwrap()[0]["messages"][2]["content"],
        json!("hello\n\n[ognivo] Time limit: 2 s. ognivo stops the turn when it runs out."),
        "the model was not told the limit"
    );
}

/// `--events`: the run in the clients' event model, as the GUI gets a session.
#[tokio::test]
async fn events_are_the_run_in_the_clients_model() {
    let (url, _) = fake_server(vec![
        call_stream("r1", "read", r#"{"path":"a.txt"}"#),
        answer_stream("It says one."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let data = dir.path().join("data");
    let bin = env!("CARGO_BIN_EXE_ognivo");
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(bin)
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "gpt-oss-20b",
                "--events",
                "--seed",
                "1",
                "--data-dir",
            ])
            .arg(&data)
            .arg("--cwd")
            .arg(dir.path())
            .env("HOME", dir.path())
            .env("USERPROFILE", dir.path())
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
            .write_all(b"What does a.txt say?")
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert_eq!(seqs, (1..=events.len() as i64).collect::<Vec<_>>());
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[..2], ["thread_started", "turn_started"]);
    assert_eq!(kinds.last(), Some(&"turn_completed"));
    assert_eq!(events[0]["info"]["profile"], json!("gpt-oss-20b"));
    assert_eq!(events[1]["user"], json!("What does a.txt say?"));
    // the call completes with its result, and the answer is the turn's
    let read = events
        .iter()
        .find(|e| e["event"] == "item_completed" && e["item"]["kind"] == "tool_call")
        .unwrap();
    assert_eq!(read["item"]["call"]["name"], json!("Read"));
    assert_eq!(read["item"]["result"]["outcome"], json!("ok"));
    let end = events.last().unwrap();
    assert_eq!(
        (end["end"].as_str(), end["answer"].as_str()),
        (Some("answer"), Some("It says one."))
    );
    assert_eq!(kinds.iter().filter(|k| **k == "generation").count(), 2);
}

/// `ognivo run --image` in `dir/proj` against `url`: its exit, stdout and stderr.
async fn run_with_image(url: String, dir: &std::path::Path) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_ognivo");
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(bin)
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "gemma-4-26b-a4b",
                "--data-dir",
            ])
            .arg(dir.join("data"))
            .arg("--cwd")
            .arg(dir.join("proj"))
            .args(["--image", "shot.png"])
            .current_dir(dir.join("proj"))
            .env("HOME", &dir)
            .env("USERPROFILE", &dir)
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
            .write_all(b"What is on it?")
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn an_image_goes_with_the_prompt_to_a_model_that_sees() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("proj")).unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    png.extend(3u32.to_be_bytes());
    png.extend(2u32.to_be_bytes());
    std::fs::write(dir.path().join("proj/shot.png"), &png).unwrap();

    let (url, bodies) = fake_seeing_server(vec![answer_stream("A dot.")]).await;
    let out = run_with_image(url, dir.path()).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
    let bodies = bodies.lock().unwrap().clone();
    let user = bodies[0]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(user["content"][0]["type"], "image_url");
    assert_eq!(
        user["content"][1],
        json!({"type": "text", "text": "What is on it?"})
    );

    // a model whose server runs no projector gets no image, and the run says why
    let (blind, bodies) = fake_server(Vec::new()).await;
    let out = run_with_image(blind, dir.path()).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--image: the model reads no images: its llama-server runs without a projector (--mmproj)"), "{stderr}");
    assert!(bodies.lock().unwrap().is_empty());
}
