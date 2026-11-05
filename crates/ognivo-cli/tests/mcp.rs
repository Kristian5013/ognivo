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
    run_with(dir, url, &[]).await
}

/// The same with more flags.
async fn run_with(dir: &Path, url: &str, flags: &[&str]) -> Output {
    let (dir, url) = (dir.to_owned(), url.to_owned());
    let flags: Vec<String> = flags.iter().map(|f| (*f).to_owned()).collect();
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
            .args(&flags)
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

#[tokio::test]
async fn with_search_the_model_finds_a_tool_and_calls_it_through_mcp_call() {
    let (url, bodies) = fake_server(vec![
        call_stream("m1", "tool_search", r#"{"query":"say text back"}"#),
        call_stream(
            "m2",
            "mcp_call",
            r#"{"tool":"mcp__echo__echo","arguments":{"text":"hi there"}}"#,
        ),
        answer_stream("It said hi there."),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    home(dir.path(), true);
    let out = run_with(dir.path(), &url, &["--mcp-tools", "search"]).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("mcp: 3 tools, which the model finds with tool_search"),
        "{stderr}"
    );
    let bodies = bodies.lock().unwrap();
    let names = |b: &Value| -> Vec<String> {
        b["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
            .collect()
    };
    // the same tools in every request: tool_search and mcp_call stand for the server's
    assert_eq!(names(&bodies[0]), names(&bodies[2]));
    assert_eq!(
        &names(&bodies[0])[names(&bodies[0]).len() - 2..],
        ["tool_search", "mcp_call"]
    );
    // its description has what each tool does, and how to call one on this path
    let search = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "tool_search")
        .unwrap()["function"]["description"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        search.contains("then call one with mcp_call.")
            && search.ends_with(
                "The tools:\n- mcp__echo__add_up: Adds numbers up.\n- mcp__echo__echo: Says the text back.\n- mcp__echo__fail: Always fails."
            ),
        "{search}"
    );
    let last = |b: &Value| b["messages"].as_array().unwrap().last().unwrap()["content"].clone();
    let found = last(&bodies[1]);
    let found = found.as_str().unwrap();
    assert!(
        found.starts_with("[tool_search: 1 tool for \"say text back\"]"),
        "{found}"
    );
    assert!(found.contains("type mcp__echo__echo = (_: {"), "{found}");
    // the rule on the tool holds through mcp_call
    assert_eq!(last(&bodies[2]), "[mcp: echo · echo]\nhi there");
}

/// A browser for the tests: it opens the page it is given and, the user saying yes, follows
/// the redirect back to ognivo.
const FOLLOW: &str = r#"import sys
import urllib.error
import urllib.request


class Stay(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


try:
    urllib.request.build_opener(Stay).open(sys.argv[1])
    sys.exit("no redirect")
except urllib.error.HTTPError as e:
    back = e.headers["Location"]
urllib.request.urlopen(back).read()
"#;

#[test]
fn a_remote_server_is_signed_in_to_from_the_command_line_and_out_again() {
    use std::io::BufRead as _;
    use std::os::unix::fs::PermissionsExt as _;
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ognivo-mcp/tests/fixtures/oauth_server.py");
    let mut server = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    std::io::BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", port.trim());
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ognivo")).unwrap();
    std::fs::create_dir_all(dir.path().join("proj")).unwrap();
    let config = json!({"mcpServers": {"docs": {"type": "http", "url": url}}});
    std::fs::write(dir.path().join(".ognivo/mcp.json"), config.to_string()).unwrap();
    let follow = dir.path().join("follow.py");
    std::fs::write(&follow, FOLLOW).unwrap();
    let browser = dir.path().join("browser");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\nexec python3 -I '{}' \"$1\"\n", follow.display()),
    )
    .unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mcp = |args: &[&str]| -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_ognivo"))
            .arg("mcp")
            .args(args)
            .arg("--cwd")
            .arg(dir.path().join("proj"))
            .arg("--data-dir")
            .arg(dir.path().join("data"))
            .env("HOME", dir.path())
            .env("BROWSER", &browser)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    };
    let (ok, out) = mcp(&[]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("✗ docs") && out.contains("it wants you to sign in: ognivo mcp login docs"),
        "{out}"
    );
    let (ok, out) = mcp(&["login", "docs"]);
    assert!(ok, "{out}");
    assert!(out.contains("Sign in to docs in your browser:"), "{out}");
    assert!(out.contains("✓ signed in to docs: 1 tool"), "{out}");
    let (ok, out) = mcp(&[]);
    assert!(ok, "{out}");
    assert!(
        out.contains("✓ docs (~/.ognivo/mcp.json, signed in)") && out.contains("mcp__docs__whoami"),
        "{out}"
    );
    let (ok, out) = mcp(&["logout", "docs"]);
    assert!(ok && out.contains("signed out of docs"), "{out}");
    let (_, out) = mcp(&["logout", "nope"]);
    assert!(out.contains("there is no MCP server nope"), "{out}");
    server.kill().unwrap();
    server.wait().unwrap();
}
