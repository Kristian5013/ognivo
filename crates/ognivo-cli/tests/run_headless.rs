//! `ognivo run` end to end against a fake llama-server that answers with recorded-shape
//! SSE streams: read, then edit, then the final answer. No GPU, no network.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn call_stream(id: &str, name: &str, args: &str) -> String {
    let events = [
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(json!({"reasoning_content": "next step"}), None),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": "{"}}]}),
            None,
        ),
        chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": &args[1..]}}]}),
            None,
        ),
        chunk(json!({}), Some("tool_calls")),
        json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 20}, "timings": {"cache_n": 90, "prompt_n": 10, "predicted_n": 20}}),
    ];
    sse(&events)
}

fn answer_stream(text: &str) -> String {
    sse(&[
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(json!({"content": text}), None),
        chunk(json!({}), Some("stop")),
    ])
}

fn sse(events: &[Value]) -> String {
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}data: [DONE]\n\n"
    )
}

async fn fake_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    tokio::spawn(async move {
        for resp in responses {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let body = loop {
                let mut part = [0u8; 8192];
                let n = sock.read(&mut part).await.unwrap();
                buf.extend_from_slice(&part[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len: usize = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break buf[end + 4..end + 4 + len].to_vec();
                    }
                }
            };
            seen.lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap());
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        }
    });
    (url, bodies)
}

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
    assert!(String::from_utf8_lossy(&out.stdout).contains("Changed the greeting."));
    assert!(
        stderr.contains("→ read") && stderr.contains("← [edit: applied exact]"),
        "{stderr}"
    );

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    // the prompt from stdin is the user message; the second request carries the first
    // step's call, its verbatim reasoning and the read result
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
