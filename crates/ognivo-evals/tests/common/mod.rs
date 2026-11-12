//! What the T0 tests share: a llama-server that answers with prepared generations in the
//! layout of the recorded streams and keeps every request body, an agent on the Generic
//! driver against it, and the check of what the requests did to the server's cache.

#![allow(dead_code)] // each test file uses its own part

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ognivo_core::agent::{Agent, AgentOptions, AllowAll};
use ognivo_core::approval::WorkspaceState;
use ognivo_core::instructions::UserInstructions;
use ognivo_core::rules::Rules;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{ChatDriver, Family, ModelProfile, profile};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub fn profile(id: &str) -> ModelProfile {
    profile::builtin()
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap()
}

/// One generation as the server streams it.
#[derive(Debug, Clone, Default)]
pub struct Gen {
    pub reasoning: String,
    pub content: String,
    /// Id, name and the arguments' exact text.
    pub calls: Vec<(String, String, String)>,
    /// Cut by `max_tokens`: the stream ends with `finish_reason` length.
    pub cut: bool,
}

impl Gen {
    pub fn calls(reasoning: &str, calls: &[(&str, &str, Value)]) -> Self {
        Self {
            reasoning: reasoning.into(),
            content: String::new(),
            calls: calls
                .iter()
                .map(|(id, name, args)| ((*id).into(), (*name).into(), args.to_string()))
                .collect(),
            cut: false,
        }
    }

    pub fn answer(reasoning: &str, text: &str) -> Self {
        Self {
            reasoning: reasoning.into(),
            content: text.into(),
            calls: Vec::new(),
            cut: false,
        }
    }

    /// The SSE body, laid out as the recorded runs show it (evals/t0/corpus): the role
    /// chunk, reasoning and text a few bytes per chunk, each call's header with `{` and
    /// its arguments in fragments, the finish chunk, the usage chunk for a prompt of
    /// `prompt` tokens, `[DONE]`.
    pub fn sse(&self, prompt: u64) -> String {
        let mut chunks = vec![delta(json!({"role": "assistant", "content": null}), None)];
        for p in pieces(&self.reasoning) {
            chunks.push(delta(json!({"reasoning_content": p}), None));
        }
        for p in pieces(&self.content) {
            chunks.push(delta(json!({"content": p}), None));
        }
        for (i, (id, name, args)) in self.calls.iter().enumerate() {
            let (head, rest) = args.split_at(1);
            chunks.push(delta(
                json!({"tool_calls": [{"index": i, "id": id, "type": "function", "function": {"name": name, "arguments": head}}]}),
                None,
            ));
            for p in pieces(rest) {
                chunks.push(delta(
                    json!({"tool_calls": [{"index": i, "function": {"arguments": p}}]}),
                    None,
                ));
            }
        }
        let finish = match (self.cut, self.calls.is_empty()) {
            (true, _) => "length",
            (false, true) => "stop",
            (false, false) => "tool_calls",
        };
        chunks.push(delta(json!({}), Some(finish)));
        chunks.push(json!({"choices": [], "usage": {"completion_tokens": 20, "prompt_tokens": prompt, "total_tokens": prompt + 20}, "timings": {"cache_n": 0, "prompt_n": prompt, "predicted_n": 20}}));
        let mut body: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        body.push_str("data: [DONE]\n\n");
        body
    }

    /// The assistant message the next request must carry for this generation, as the
    /// driver sends it to `family`'s template.
    pub fn message(&self, family: Family) -> Value {
        let mut m = json!({"role": "assistant", "content": self.content});
        if family == Family::Gemma4 && !self.calls.is_empty() {
            m["content"] = json!("");
        }
        if !self.reasoning.is_empty() {
            m["reasoning_content"] = json!(self.reasoning);
        }
        if !self.calls.is_empty() {
            m["tool_calls"] = self
                .calls
                .iter()
                .map(|(id, name, args)| {
                    json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
                })
                .collect();
        }
        m
    }
}

fn delta(delta: Value, finish: Option<&str>) -> Value {
    json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}], "object": "chat.completion.chunk"})
}

/// Fragments of at most three characters, so every value is put together from pieces.
fn pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(3).map(|c| c.iter().collect()).collect()
}

/// What the fake server answers a request with.
pub enum Reply {
    Stream(Gen),
    /// The role chunk, then nothing until the client goes away.
    Hang,
}

/// A llama-server on a free port: one connection per request, answered in order.
pub struct Server {
    pub url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        tokio::spawn(async move {
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            for reply in replies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let body: Value = serde_json::from_slice(&read_request(&mut sock).await).unwrap();
                // three bytes a token, as the agent estimates
                let prompt =
                    (body["messages"].to_string().len() + body["tools"].to_string().len()) / 3;
                seen.lock().unwrap().push(body);
                match reply {
                    Reply::Stream(g) => {
                        sock.write_all(format!("{head}{}", g.sse(prompt as u64)).as_bytes())
                            .await
                            .unwrap();
                        sock.shutdown().await.unwrap();
                    }
                    Reply::Hang => {
                        let start = delta(json!({"role": "assistant", "content": null}), None);
                        sock.write_all(format!("{head}data: {start}\n\n").as_bytes())
                            .await
                            .unwrap();
                        tokio::spawn(async move {
                            let _held = sock;
                            std::future::pending::<()>().await;
                        });
                    }
                }
            }
            // more requests than the test prepared for: say so instead of refusing
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                read_request(&mut sock).await;
                let body = "the test prepared no more replies";
                let reply = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(reply.as_bytes()).await.unwrap();
                sock.shutdown().await.unwrap();
            }
        });
        Self { url, bodies }
    }

    /// The request bodies so far, in order.
    pub fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    loop {
        let mut chunk = [0u8; 8192];
        let n = sock.read(&mut chunk).await.unwrap();
        assert!(
            n > 0,
            "the client closed the connection before its request ended"
        );
        buf.extend_from_slice(&chunk[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
        let len = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
        if buf.len() >= end + 4 + len {
            return buf[end + 4..end + 4 + len].to_vec();
        }
    }
}

/// A project in a temporary directory.
pub struct Project {
    pub root: PathBuf,
    _dir: tempfile::TempDir,
}

impl Project {
    pub fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = ognivo_os::canonicalize(dir.path()).unwrap().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        for (name, text) in files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Self { root, _dir: dir }
    }
}

pub fn options(root: &Path, p: &ModelProfile) -> AgentOptions {
    AgentOptions {
        root: root.to_owned(),
        data_dir: root.parent().unwrap().join("data"),
        max_steps: 10,
        context: p.context,
        n_ctx: None,
        seed: 1,
        workspace: WorkspaceState::CLEAN,
        time_limit: None,
        user_instructions: UserInstructions::default(),
        rules: Rules::default(),
        layout: Default::default(),
        toolset: Default::default(),
        writable: None,
        subagents: None,
        mcp: None,
        skills: Default::default(),
        hooks: Default::default(),
        agents: Default::default(),
        only: None,
        mode: Default::default(),
        vision: false,
    }
}

pub fn driver(url: &str, p: &ModelProfile) -> ChatDriver {
    ChatDriver::new(
        LlamaServer::new(url, None).unwrap(),
        p.clone(),
        None,
        Some(0),
    )
    .unwrap()
}

/// An agent on the Generic driver against `url` that may run anything.
pub fn agent(url: &str, p: &ModelProfile, opts: AgentOptions) -> Agent {
    Agent::new(Box::new(driver(url, p)), p.family, Box::new(AllowAll), opts)
}

/// What a run's requests did to the server's prompt cache.
#[derive(Debug, PartialEq, Eq)]
pub struct Cache {
    /// Requests that start a new prompt: their tools, template arguments or messages do
    /// not begin with the previous request's.
    pub restarts: Vec<usize>,
    /// Requests that go on from the previous one without the generation it got, so the
    /// server recomputes from the end of the previous prompt.
    pub dropped: Vec<usize>,
}

/// Checks each request against the one before: within a prompt the messages only grow,
/// and the first new one is the generation the previous request got, byte for byte.
/// `got[k]` is what request `k` was answered with (`None`: no finished generation).
/// `tool_choice` is not compared: llama-server renders the tools either way (M1 live run:
/// the summary request, with `none`, took 14430 of its 14464 tokens from the cache).
pub fn cache(bodies: &[Value], got: &[Option<Gen>], family: Family) -> Cache {
    let mut out = Cache {
        restarts: Vec::new(),
        dropped: Vec::new(),
    };
    for k in 1..bodies.len() {
        let (prev, cur) = (&bodies[k - 1], &bodies[k]);
        let (pm, cm) = (
            prev["messages"].as_array().unwrap(),
            cur["messages"].as_array().unwrap(),
        );
        let same = ["tools", "chat_template_kwargs"]
            .iter()
            .all(|f| prev[f] == cur[f]);
        if !same || cm.len() <= pm.len() || cm[..pm.len()] != pm[..] {
            out.restarts.push(k);
            continue;
        }
        if let Some(g) = &got[k - 1]
            && cm[pm.len()] != g.message(family)
        {
            out.dropped.push(k);
        }
    }
    out
}
