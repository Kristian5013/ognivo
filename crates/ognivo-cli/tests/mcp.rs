//! MCP servers in a session, end to end: `ognivo run` starts the user's server (a script of
//! ognivo-mcp's tests), the model sees its tools after ognivo's and calls one, a rule lets it
//! run and its answer goes back; without the rule nobody can say yes; the project's servers
//! wait for the user's trust. Python 3 and a fake llama-server, no GPU, no network.

mod common;

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::{answer_stream, call_stream, fake_server};
use serde_json::{Value, json};

fn echo_server() -> Value {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ognivo-mcp/tests/fixtures/echo.py");
    json!({"command": "python3", "args": ["-I", script]})
}

/// `ognivo run` in `dir/proj` with `dir` as the home, the task on stdin.
async fn run(dir: &Path, url: &str) -> Output {
    let (dir, url) = (dir.to_owned(), url.to_owned());
    tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ognivo"))
            .args([
                "run",
                "--url",
                &url,
                "--profile",
                "gpt-oss-20b",
                "--seed",
                "1",
                "--data-dir",
            ])
            .arg(dir.join("data"))
            .arg("--cwd")
            .arg(dir.join("proj"))
            .env("HOME", &dir)
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
            .write_all(b"Echo hi there.")
            .unwrap();
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap()
}

fn home(dir: &Path, allow: bool) {
    std::fs::create_dir_all(dir.join(".ognivo")).unwrap();
    std::fs::create_dir_all(dir.join("proj")).unwrap();
    let config = json!({"mcpServers": {"echo": echo_server()}});
    std::fs::write(dir.join(".ognivo/mcp.json"), config.to_string()).unwrap();
    if allow {
        std::fs::write(
            dir.join(".ognivo/config.toml"),
            "[permissions]\nallow = [\"mcp__echo__echo\"]\n",
        )
        .unwrap();
    }
}

#[tokio::test]
async fn a_tool_of_the_users_server_runs_when_a_rule_allows_it() {
    let (url, bodies) = fake_server(vec![
        call_stream("m1", "mcp__echo__echo", r#"{"text":"hi there"}"#),
        answer_stream("It said hi there."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    home(dir.path(), true);
    let out = run(dir.path(), &url).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("· mcp echo: 3 tools"), "{stderr}");
    let bodies = bodies.lock().unwrap();
    // the server's tools come after ognivo's, by name
    let names: Vec<&str> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        &names[names.len() - 3..],
        ["mcp__echo__add_up", "mcp__echo__echo", "mcp__echo__fail"]
    );
    let echo = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "mcp__echo__echo")
        .unwrap();
    assert_eq!(
        echo["function"]["description"],
        "Says the text back. (MCP server echo)"
    );
    assert_eq!(echo["function"]["parameters"]["required"], json!(["text"]));
    let result = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(result["role"], "tool");
    assert_eq!(result["content"], "[mcp: echo · echo]\nhi there");
}

#[tokio::test]
async fn without_a_rule_nobody_can_say_yes_and_the_projects_servers_wait() {
    let (url, bodies) = fake_server(vec![
        call_stream("m1", "mcp__echo__echo", r#"{"text":"hi there"}"#),
        answer_stream("It was refused."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    home(dir.path(), false);
    // the project's own server waits for the user's trust, given in chat
    std::fs::write(
        dir.path().join("proj/.mcp.json"),
        json!({"mcpServers": {"repo": echo_server()}}).to_string(),
    )
    .unwrap();
    let out = run(dir.path(), &url).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("· the project's MCP servers do not start: trust them once in ognivo chat"),
        "{stderr}"
    );
    assert!(!stderr.contains("mcp repo"), "{stderr}");
    let bodies = bodies.lock().unwrap();
    let result = bodies[1]["messages"].as_array().unwrap().last().unwrap()["content"].clone();
    let text = result.as_str().unwrap();
    assert!(
        text.starts_with("[refused] echo · echo needs the user's approval"),
        "{text}"
    );
}
