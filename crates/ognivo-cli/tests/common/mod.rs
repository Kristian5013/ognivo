//! A fake llama-server for the CLI tests: `/props`, and recorded-shape SSE streams for
//! `/v1/chat/completions` in order; or llama-server's router in front of it. No GPU, no
//! network.

// each test binary uses its own part of it
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Python as the OS names it: Windows has `python`, and a `python3` there may be the
/// Microsoft Store's stand-in.
pub const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

/// A chat answer that never comes: the connection stays open.
pub const HANG: &str = "";

pub fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

pub fn call_stream(id: &str, name: &str, args: &str) -> String {
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

pub fn answer_stream(text: &str) -> String {
    sse(&[
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(json!({"content": text}), None),
        chunk(json!({}), Some("stop")),
    ])
}

pub fn sse(events: &[Value]) -> String {
    let body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}data: [DONE]\n\n"
    )
}

/// Serves `/props` (n_ctx 4096, one slot) whenever asked and `responses` to the chat
/// requests in order; returns its URL and the bodies of the chat requests.
pub async fn fake_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let (url, bodies, _) = serve(None, responses, false).await;
    (url, bodies)
}

/// [`fake_server`] with a projector: its model takes images.
#[allow(dead_code)]
pub async fn fake_seeing_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let (url, bodies, _) = serve(None, responses, true).await;
    (url, bodies)
}

/// llama-server's router with `models` (its `/models` list) in front of a fake server with
/// two slots; returns its URL, the bodies of the chat requests, and the GET request lines.
pub async fn fake_router(
    models: Value,
    responses: Vec<String>,
) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<String>>>) {
    serve(Some(models), responses, false).await
}

fn json_response(v: &Value) -> String {
    let body = v.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn serve(
    router: Option<Value>,
    responses: Vec<String>,
    vision: bool,
) -> (String, Arc<Mutex<Vec<Value>>>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let gets = Arc::new(Mutex::new(Vec::new()));
    let (seen, got) = (bodies.clone(), gets.clone());
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (head, body) = read_request(&mut sock).await;
            // a connection the client dropped before it asked is no request
            if head.is_empty() {
                continue;
            }
            if head.starts_with("GET ") {
                let line = head.lines().next().unwrap_or_default().to_owned();
                got.lock().unwrap().push(line);
            }
            let resp = match &router {
                Some(_) if head.starts_with("GET /props?model=") => json_response(
                    &json!({"default_generation_settings": {"n_ctx": 4096}, "total_slots": 2}),
                ),
                Some(_) if head.starts_with("GET /props") => json_response(&json!({
                    "role": "router",
                    "build_info": "b11924-12cc59ba4",
                    "default_generation_settings": {"n_ctx": 0, "params": {}}
                })),
                Some(models) if head.starts_with("GET /models") => {
                    json_response(&json!({"data": models, "object": "list"}))
                }
                None if head.starts_with("GET /props") => json_response(&json!({
                    "default_generation_settings": {"n_ctx": 4096},
                    "total_slots": 1,
                    "modalities": {"vision": vision},
                })),
                _ => {
                    seen.lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    match responses.next() {
                        Some(r) if r == HANG => {
                            tokio::spawn(async move {
                                let _sock = sock;
                                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            });
                            continue;
                        }
                        Some(r) => r,
                        None => continue,
                    }
                }
            };
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.shutdown().await.unwrap();
        }
    });
    (url, bodies, gets)
}

/// The request line with the headers, and the body.
async fn read_request(sock: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    loop {
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
                return (text[..end].to_owned(), buf[end + 4..end + 4 + len].to_vec());
            }
        }
        if n == 0 {
            return (text, Vec::new());
        }
    }
}

/// A browser for the tests, as `$BROWSER` names one: it opens the page it is given and,
/// the user saying yes, follows the redirect back to ognivo. Written under `dir`.
#[cfg(unix)]
pub fn browser(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
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
    let follow = dir.join("follow.py");
    std::fs::write(&follow, FOLLOW).unwrap();
    let browser = dir.join("browser");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\nexec python3 -I '{}' \"$1\"\n", follow.display()),
    )
    .unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    browser
}

/// ognivo-mcp's MCP server behind OAuth, its own authorization server too, and its URL.
#[cfg(unix)]
pub fn oauth_server() -> (std::process::Child, String) {
    use std::io::BufRead as _;
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../ognivo-mcp/tests/fixtures/oauth_server.py");
    let mut child = std::process::Command::new(PYTHON)
        .arg("-I")
        .arg(&script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    (child, format!("http://127.0.0.1:{}/mcp", port.trim()))
}
