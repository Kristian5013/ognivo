//! The one seam between the agent loop and a model transport (docs/architecture.md §5.1,
//! item 5). The loop sees [`ModelDriver`] only; [`ChatDriver`] is the Generic
//! implementation over `/v1/chat/completions`, the Native one over `/completion` follows
//! in M2.

use std::fmt;
use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};

use crate::Family;
use crate::engine::{EngineError, SamplerSpec, Usage};
use crate::generic::{
    ChatCompletion, ChatEvent, ChatMessage, ChatRequest, FinishReason, Outcome, ToolChoice,
    ToolDecl, chat_body, chat_events, is_context_exceeded, is_format_error,
};
use crate::llama::LlamaServer;
use crate::profile::ModelProfile;

/// Attempts per step: the first try and one retry after a format error (§5.2).
const MAX_ATTEMPTS: u8 = 2;
/// Seed step for the retry: a fixed odd constant, so a rerun of the same step is
/// reproducible and the retry never reuses the failed seed.
const RETRY_SEED_STEP: u32 = 0x9E37_79B9;

/// One generation step: the whole history and the tools offered to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDecl>,
    pub tool_choice: ToolChoice,
    /// Fixed per request class by the caller, never derived from the window remainder
    /// (M0: OpenCode's remainder formula fell to 1024 tokens and broke a compaction).
    pub max_tokens: u32,
    pub seed: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    Reasoning(String),
    Text(String),
    /// A streamed fragment of a tool call, for display only.
    ToolCallDelta {
        index: u32,
        name: Option<String>,
        arguments: String,
    },
    /// The server rejected the output's format and the step starts over with `seed`.
    /// Everything streamed before this event is void.
    Restarted {
        seed: u32,
        reason: String,
    },
    /// The last event of every successful step.
    Finished(Box<Finished>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub content: String,
    pub reasoning: String,
    pub outcome: Outcome,
    pub finish: Option<FinishReason>,
    pub usage: Usage,
    /// The seed of the attempt that produced this result.
    pub seed: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("the server could not parse the model output after {attempts} attempts: {message}")]
    Format { attempts: u8, message: String },
    #[error("the prompt does not fit the context: {0}")]
    ContextExceeded(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
}

pub trait ModelDriver: Send + Sync + fmt::Debug {
    /// Streams one generation. The stream ends with exactly one [`DriverEvent::Finished`]
    /// or one error, never silently; dropping it cancels the generation on the server.
    fn step(&self, req: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>>;
}

/// The Generic path: llama-server renders the prompt with its chat template and parses
/// tool calls itself; the driver owns the request rules and the per-family history rules.
#[derive(Debug, Clone)]
pub struct ChatDriver {
    server: LlamaServer,
    profile: Arc<ModelProfile>,
    level: String,
    slot: Option<u32>,
}

impl ChatDriver {
    /// `level` is the reasoning level for the session; `None` takes the profile default.
    pub fn new(
        server: LlamaServer,
        profile: ModelProfile,
        level: Option<&str>,
        slot: Option<u32>,
    ) -> Result<Self, DriverError> {
        let level = level.unwrap_or(&profile.reasoning.default).to_owned();
        if !profile.reasoning.levels.contains(&level) {
            return Err(DriverError::InvalidRequest(format!(
                "{}: reasoning level {level:?} is not one of {:?}",
                profile.id, profile.reasoning.levels
            )));
        }
        Ok(Self {
            server,
            profile: Arc::new(profile),
            level,
            slot,
        })
    }

    pub fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    /// The request body without its seed, which every attempt sets on its own.
    fn body(&self, req: StepRequest) -> Result<Value, DriverError> {
        let family = self.profile.family;
        let chat = ChatRequest {
            messages: history_for(family, req.messages)?,
            sampler: SamplerSpec::from(self.profile.sampling.main),
            template_kwargs: self.profile.chat_template_kwargs(Some(&self.level)),
            tools: req.tools,
            tool_choice: req.tool_choice,
            // gpt-oss's template renders only tool_calls[0]: a second call would vanish
            parallel_tool_calls: family != Family::Harmony,
            max_tokens: Some(req.max_tokens),
            slot: self.slot,
        };
        Ok(chat_body(&chat))
    }
}

impl ModelDriver for ChatDriver {
    fn step(&self, req: StepRequest) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
        let seed = req.seed;
        match self.body(req) {
            Ok(body) => steps(self.server.clone(), body, seed),
            Err(e) => stream::once(async { Err(e) }).boxed(),
        }
    }
}

/// History rules of the Generic path, per family:
/// - every system text goes into one leading system message: Qwen's template accepts
///   only one, and none of the three models was trained on mid-conversation system turns;
/// - Gemma: an assistant message with tool calls carries no content (its template
///   renders the content after the calls, which the model then imitates);
/// - gpt-oss: at most one call per assistant message, the only form its template renders.
fn history_for(
    family: Family,
    messages: Vec<ChatMessage>,
) -> Result<Vec<ChatMessage>, DriverError> {
    let mut system = Vec::new();
    let mut rest = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            ChatMessage::System(text) => system.push(text),
            ChatMessage::Assistant {
                content,
                reasoning,
                tool_calls,
            } => {
                if family == Family::Harmony && tool_calls.len() > 1 {
                    return Err(DriverError::InvalidRequest(format!(
                        "gpt-oss history holds {} calls in one assistant message",
                        tool_calls.len()
                    )));
                }
                let content = if family == Family::Gemma4 && !tool_calls.is_empty() {
                    String::new()
                } else {
                    content
                };
                rest.push(ChatMessage::Assistant {
                    content,
                    reasoning,
                    tool_calls,
                });
            }
            other => rest.push(other),
        }
    }
    let mut out = Vec::with_capacity(rest.len() + 1);
    if !system.is_empty() {
        out.push(ChatMessage::System(system.join("\n\n")));
    }
    out.extend(rest);
    Ok(out)
}

enum State {
    Connect {
        attempt: u8,
        seed: u32,
    },
    Stream {
        attempt: u8,
        seed: u32,
        chunks: BoxStream<'static, Result<Value, EngineError>>,
        acc: ChatCompletion,
    },
    End,
}

type Batch = Vec<Result<DriverEvent, DriverError>>;

/// Runs the attempts of one step as a single stream of events.
fn steps(
    server: LlamaServer,
    body: Value,
    seed: u32,
) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
    let ctx = Arc::new((server, body));
    stream::unfold(
        (ctx, State::Connect { attempt: 1, seed }),
        |(ctx, state)| async move {
            let (batch, next) = match state {
                State::End => return None,
                State::Connect { attempt, seed } => {
                    let (server, body) = &*ctx;
                    let mut body = body.clone();
                    body["seed"] = json!(seed);
                    match server.chat_stream(&body, None).await {
                        Ok(chunks) => (
                            Vec::new(),
                            State::Stream {
                                attempt,
                                seed,
                                chunks,
                                acc: ChatCompletion::default(),
                            },
                        ),
                        Err(e) => failed(e, attempt, seed),
                    }
                }
                State::Stream {
                    attempt,
                    seed,
                    mut chunks,
                    mut acc,
                } => match chunks.next().await {
                    Some(Ok(v)) => match chat_events(&v) {
                        Ok(events) => {
                            let shown = events.iter().filter_map(shown).map(Ok).collect();
                            for ev in events {
                                acc.apply(ev);
                            }
                            (
                                shown,
                                State::Stream {
                                    attempt,
                                    seed,
                                    chunks,
                                    acc,
                                },
                            )
                        }
                        Err(e) => failed(e, attempt, seed),
                    },
                    Some(Err(e)) => failed(e, attempt, seed),
                    None => {
                        let done = Finished {
                            outcome: acc.outcome(),
                            content: acc.content,
                            reasoning: acc.reasoning,
                            finish: acc.finish,
                            usage: acc.usage,
                            seed,
                        };
                        (vec![Ok(DriverEvent::Finished(Box::new(done)))], State::End)
                    }
                },
            };
            Some((batch, (ctx, next)))
        },
    )
    .flat_map(stream::iter)
    .boxed()
}

/// An attempt failed: retry once on a format error, otherwise end the step with the error.
fn failed(e: EngineError, attempt: u8, seed: u32) -> (Batch, State) {
    if is_format_error(&e) {
        let message = match &e {
            EngineError::Status { body, .. } => body.clone(),
            other => other.to_string(),
        };
        if attempt < MAX_ATTEMPTS {
            let seed = seed.wrapping_add(RETRY_SEED_STEP);
            let ev = DriverEvent::Restarted {
                seed,
                reason: message,
            };
            return (
                vec![Ok(ev)],
                State::Connect {
                    attempt: attempt + 1,
                    seed,
                },
            );
        }
        let err = DriverError::Format {
            attempts: attempt,
            message,
        };
        return (vec![Err(err)], State::End);
    }
    let err = if is_context_exceeded(&e) {
        DriverError::ContextExceeded(e.to_string())
    } else {
        DriverError::Engine(e)
    };
    (vec![Err(err)], State::End)
}

fn shown(ev: &ChatEvent) -> Option<DriverEvent> {
    match ev {
        ChatEvent::Text(t) => Some(DriverEvent::Text(t.clone())),
        ChatEvent::Reasoning(r) => Some(DriverEvent::Reasoning(r.clone())),
        ChatEvent::ToolCall {
            index,
            name,
            arguments,
            ..
        } => Some(DriverEvent::ToolCallDelta {
            index: *index,
            name: name.clone(),
            arguments: arguments.clone(),
        }),
        ChatEvent::Finish(_) | ChatEvent::Usage(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generic::ToolCall;
    use crate::profile;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A one-request-per-connection HTTP server answering with canned responses in order
    /// and recording every request body.
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
                    let mut chunk = [0u8; 4096];
                    let n = sock.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
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

    fn sse(status: &str, events: &[Value], done: bool) -> String {
        let mut body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        if done {
            body.push_str("data: [DONE]\n\n");
        }
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
        )
    }

    fn plain(status: &str, body: &Value) -> String {
        let body = body.to_string();
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn chunk(delta: Value, finish: Option<&str>) -> Value {
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    /// The shape of a real pinned-server tool-call stream (M0 records): role chunk, header
    /// with `{`, argument fragments, finish chunk, usage chunk with empty choices.
    fn tool_call_stream() -> Vec<Value> {
        vec![
            chunk(json!({"role": "assistant", "content": null}), None),
            chunk(json!({"reasoning_content": "read it"}), None),
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "abc", "type": "function", "function": {"name": "read", "arguments": "{"}}]}),
                None,
            ),
            chunk(
                json!({"tool_calls": [{"index": 0, "function": {"arguments": "\"path\":\"a.rs\"}"}}]}),
                None,
            ),
            chunk(json!({}), Some("tool_calls")),
            json!({"choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5}, "timings": {"cache_n": 9, "prompt_n": 1, "predicted_n": 5}}),
        ]
    }

    fn format_error() -> Value {
        json!({"error": {"code": 500, "message": "The model produced output that does not match the expected peg-native format", "type": "server_error"}})
    }

    fn driver(url: &str, family: Family) -> ChatDriver {
        let p = profile::builtin()
            .unwrap()
            .into_iter()
            .find(|p| p.family == family)
            .unwrap();
        ChatDriver::new(LlamaServer::new(url, None).unwrap(), p, None, Some(0)).unwrap()
    }

    fn request(seed: u32) -> StepRequest {
        StepRequest {
            messages: vec![
                ChatMessage::System("be brief".into()),
                ChatMessage::User("read a.rs".into()),
            ],
            tools: vec![ToolDecl {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            }],
            tool_choice: ToolChoice::Auto,
            max_tokens: 16384,
            seed,
        }
    }

    async fn run(d: &ChatDriver, req: StepRequest) -> Vec<Result<DriverEvent, DriverError>> {
        d.step(req).collect().await
    }

    fn finished(events: &[Result<DriverEvent, DriverError>]) -> &Finished {
        match events.last() {
            Some(Ok(DriverEvent::Finished(f))) => f,
            other => panic!("last event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_call_becomes_executable() {
        let (url, bodies) = fake_server(vec![sse("200 OK", &tool_call_stream(), true)]).await;
        let events = run(&driver(&url, Family::Harmony), request(7)).await;
        let f = finished(&events);
        let Outcome::Calls(calls) = &f.outcome else {
            panic!("{:?}", f.outcome)
        };
        assert_eq!(
            calls.calls(),
            [ToolCall {
                id: "abc".into(),
                name: "read".into(),
                arguments: "{\"path\":\"a.rs\"}".into()
            }]
        );
        assert_eq!(
            (f.reasoning.as_str(), f.usage.cache_n, f.seed),
            ("read it", Some(9), 7)
        );
        let b = &bodies.lock().unwrap()[0];
        assert_eq!(b["seed"], json!(7));
        assert_eq!(b["parallel_tool_calls"], json!(false)); // gpt-oss
        assert_eq!(b["max_tokens"], json!(16384));
        assert_eq!(b["id_slot"], json!(0));
        assert_eq!(
            b["chat_template_kwargs"],
            json!({"reasoning_effort": "high"})
        );
        // the declared property order survives (serde_json preserve_order)
        let keys: Vec<_> = b["tools"][0]["function"]["parameters"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["type", "properties", "required"]);
    }

    #[tokio::test]
    async fn in_band_format_error_retries_once_with_a_new_seed() {
        let bad = vec![
            chunk(json!({"role": "assistant", "content": null}), None),
            format_error(),
        ];
        let (url, bodies) = fake_server(vec![
            sse("200 OK", &bad, false),
            sse("200 OK", &tool_call_stream(), true),
        ])
        .await;
        let events = run(&driver(&url, Family::QwenXml), request(7)).await;
        let retry_seed = 7u32.wrapping_add(RETRY_SEED_STEP);
        assert!(
            events.iter().any(
                |e| matches!(e, Ok(DriverEvent::Restarted { seed, .. }) if *seed == retry_seed)
            )
        );
        let f = finished(&events);
        assert!(matches!(f.outcome, Outcome::Calls(_)));
        assert_eq!(f.seed, retry_seed);
        let seeds: Vec<_> = bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| b["seed"].clone())
            .collect();
        assert_eq!(seeds, [json!(7), json!(retry_seed)]);
    }

    #[tokio::test]
    async fn second_format_error_ends_the_step() {
        let (url, bodies) = fake_server(vec![
            plain("500 Internal Server Error", &format_error()),
            plain("500 Internal Server Error", &format_error()),
        ])
        .await;
        let events = run(&driver(&url, Family::Gemma4), request(1)).await;
        assert!(matches!(
            events.last(),
            Some(Err(DriverError::Format { attempts: 2, .. }))
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(DriverEvent::Finished(_))))
        );
        assert_eq!(bodies.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn stream_without_done_executes_nothing() {
        let (url, _) = fake_server(vec![sse("200 OK", &tool_call_stream(), false)]).await;
        let events = run(&driver(&url, Family::Harmony), request(1)).await;
        assert!(matches!(
            events.last(),
            Some(Err(DriverError::Engine(EngineError::Protocol(_))))
        ));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(DriverEvent::Finished(_))))
        );
    }

    #[tokio::test]
    async fn length_cut_is_truncated() {
        let cut = vec![
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "w", "type": "function", "function": {"name": "write", "arguments": "{\"path\":\"a\",\"cont"}}]}),
                None,
            ),
            chunk(json!({}), Some("length")),
        ];
        let (url, _) = fake_server(vec![sse("200 OK", &cut, true)]).await;
        let events = run(&driver(&url, Family::QwenXml), request(1)).await;
        assert!(
            matches!(&finished(&events).outcome, Outcome::Truncated { partial } if partial.len() == 1)
        );
    }

    #[tokio::test]
    async fn context_overflow_is_reported() {
        let err = json!({"error": {"code": 400, "message": "request (99000 tokens) exceeds the available context size (98304 tokens)", "type": "exceed_context_size_error"}});
        let (url, _) = fake_server(vec![plain("400 Bad Request", &err)]).await;
        let events = run(&driver(&url, Family::QwenXml), request(1)).await;
        assert!(matches!(
            events.as_slice(),
            [Err(DriverError::ContextExceeded(_))]
        ));
    }

    #[test]
    fn history_rules_per_family() {
        let call = |id: &str| ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: "{}".into(),
        };
        let history = vec![
            ChatMessage::System("a".into()),
            ChatMessage::User("q".into()),
            ChatMessage::System("b".into()),
            ChatMessage::Assistant {
                content: "let me look".into(),
                reasoning: Some("r".into()),
                tool_calls: vec![call("1")],
            },
        ];
        let gemma = history_for(Family::Gemma4, history.clone()).unwrap();
        assert_eq!(gemma[0], ChatMessage::System("a\n\nb".into()));
        assert!(
            matches!(&gemma[2], ChatMessage::Assistant { content, reasoning: Some(r), .. } if content.is_empty() && r == "r")
        );
        let qwen = history_for(Family::QwenXml, history).unwrap();
        assert!(
            matches!(&qwen[2], ChatMessage::Assistant { content, .. } if content == "let me look")
        );
        let two = vec![ChatMessage::Assistant {
            content: String::new(),
            reasoning: None,
            tool_calls: vec![call("1"), call("2")],
        }];
        assert!(history_for(Family::Harmony, two.clone()).is_err());
        assert!(history_for(Family::Gemma4, two).is_ok());
    }

    #[test]
    fn unknown_reasoning_level_is_rejected() {
        let qwen = profile::builtin().unwrap().remove(2);
        let server = LlamaServer::new("http://127.0.0.1:1", None).unwrap();
        // "high" is an error in Qwen's official template
        assert!(ChatDriver::new(server, qwen, Some("high"), None).is_err());
    }
}
