//! The one seam between the agent loop and a model transport (docs/architecture.md §5.1,
//! item 5). The loop sees [`ModelDriver`] only; [`ChatDriver`] is the Generic
//! implementation over `/v1/chat/completions`, the Native one over `/completion` follows
//! in M2.
//!
//! Both guards of the Generic path live here, so the loop and the tools only ever see plain
//! text: the special-token [`guard`](crate::guard) breaks control-token strings in every
//! request and restores them in every result, and the [`repeat`](crate::repeat) guard
//! stops a stream that loops by dropping it, which makes llama-server cancel the task.

use std::fmt;
use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};

use crate::Family;
use crate::engine::{EngineError, SamplerSpec, Usage};
use crate::generic::{
    Channel, ChatCompletion, ChatEvent, ChatMessage, ChatRequest, FinishReason, Outcome, ToolCall,
    ToolChoice, ToolDecl, chat_body, chat_events, is_context_exceeded, is_format_error,
};
use crate::guard::{Guard, unescape};
use crate::llama::{LlamaServer, no_done};
use crate::profile::ModelProfile;
use crate::repeat::{Loop, Repeats};
use crate::sse::{self, SseEvent};

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
    /// The last event of every successful step, a step the repetition guard stopped
    /// included.
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
    guard: Arc<Guard>,
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
            guard: Arc::new(Guard::new(&profile.tokens)),
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
        let messages = history_for(family, req.messages)?;
        let chat = ChatRequest {
            messages: messages
                .into_iter()
                .map(|m| guard_message(&self.guard, m))
                .collect(),
            sampler: SamplerSpec::from(self.profile.sampling.main),
            template_kwargs: self.profile.chat_template_kwargs(Some(&self.level)),
            tools: req
                .tools
                .into_iter()
                .map(|t| guard_tool(&self.guard, t))
                .collect(),
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
            Ok(body) => steps(self.server.clone(), self.guard.clone(), body, seed),
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

/// Every text of a message as the server must get it: the user's words and the model's own
/// earlier output too, since the server re-tokenizes all of it with special parsing.
fn guard_message(g: &Guard, m: ChatMessage) -> ChatMessage {
    let esc = |t: String| g.escape(&t).into_owned();
    match m {
        ChatMessage::System(t) => ChatMessage::System(esc(t)),
        ChatMessage::User(t) => ChatMessage::User(esc(t)),
        ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
        } => ChatMessage::Assistant {
            content: esc(content),
            reasoning: reasoning.map(esc),
            tool_calls: tool_calls
                .into_iter()
                .map(|c| ToolCall {
                    arguments: guard_arguments(g, &c.arguments),
                    ..c
                })
                .collect(),
        },
        ChatMessage::Tool {
            tool_call_id,
            content,
        } => ChatMessage::Tool {
            tool_call_id,
            content: esc(content),
        },
    }
}

/// JSON arguments are guarded string by string, so an escape sequence in the raw text
/// cannot hide a control token; unchanged arguments keep their bytes.
fn guard_arguments(g: &Guard, raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(mut v) => {
            if g.escape_json(&mut v) {
                v.to_string()
            } else {
                raw.to_owned()
            }
        }
        Err(_) => g.escape(raw).into_owned(),
    }
}

fn guard_tool(g: &Guard, mut t: ToolDecl) -> ToolDecl {
    t.description = g.escape(&t.description).into_owned();
    g.escape_json(&mut t.parameters);
    t
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
        watch: Box<Watch>,
    },
    End,
}

/// The repetition guard over every stream of one attempt.
#[derive(Debug, Default)]
struct Watch {
    reasoning: Repeats,
    text: Repeats,
    arguments: Vec<Repeats>,
}

impl Watch {
    fn push(&mut self, ev: &ChatEvent) -> Option<(Channel, Loop)> {
        match ev {
            ChatEvent::Reasoning(r) => self.reasoning.push(r).map(|l| (Channel::Reasoning, l)),
            ChatEvent::Text(t) => self.text.push(t).map(|l| (Channel::Text, l)),
            ChatEvent::ToolCall {
                index, arguments, ..
            } => {
                let i = *index as usize;
                if self.arguments.len() <= i {
                    self.arguments.resize_with(i + 1, Repeats::default);
                }
                self.arguments[i].push(arguments).map(|l| {
                    let channel = Channel::Arguments {
                        index: *index,
                        name: None,
                    };
                    (channel, l)
                })
            }
            ChatEvent::Finish(_) | ChatEvent::Usage(_) => None,
        }
    }
}

type Batch = Vec<Result<DriverEvent, DriverError>>;

/// What every attempt of a step shares.
struct Attempts {
    server: LlamaServer,
    /// The request without its seed.
    body: Value,
    guard: Arc<Guard>,
}

/// Runs the attempts of one step as a single stream of events.
fn steps(
    server: LlamaServer,
    guard: Arc<Guard>,
    body: Value,
    seed: u32,
) -> BoxStream<'static, Result<DriverEvent, DriverError>> {
    let ctx = Arc::new(Attempts {
        server,
        body,
        guard,
    });
    stream::unfold(
        (ctx, State::Connect { attempt: 1, seed }),
        |(ctx, state)| async move {
            let (batch, next) = match state {
                State::End => return None,
                State::Connect { attempt, seed } => {
                    let mut body = ctx.body.clone();
                    body["seed"] = json!(seed);
                    match ctx.server.chat_stream(&body, None).await {
                        Ok(chunks) => (
                            Vec::new(),
                            State::Stream {
                                attempt,
                                seed,
                                chunks,
                                acc: ChatCompletion::default(),
                                watch: Box::default(),
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
                    mut watch,
                } => match chunks.next().await {
                    Some(Ok(v)) => match chat_events(&v) {
                        Ok(events) => {
                            let mut batch: Batch =
                                events.iter().filter_map(shown).map(Ok).collect();
                            match fold(&mut acc, &mut watch, events) {
                                // returning without `chunks` drops the connection, and
                                // llama-server cancels the generation
                                Some((channel, l)) => {
                                    let done = stopped(acc, channel, &l, seed);
                                    batch.push(Ok(DriverEvent::Finished(Box::new(done))));
                                    (batch, State::End)
                                }
                                None => (
                                    batch,
                                    State::Stream {
                                        attempt,
                                        seed,
                                        chunks,
                                        acc,
                                        watch,
                                    },
                                ),
                            }
                        }
                        Err(e) => failed(e, attempt, seed),
                    },
                    Some(Err(e)) => failed(e, attempt, seed),
                    None => {
                        let done = finished(acc, &ctx.guard, seed);
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

/// Folds one chunk's events into the attempt while the repetition guard watches each
/// channel; the first loop it sees, if any.
fn fold(
    acc: &mut ChatCompletion,
    watch: &mut Watch,
    events: Vec<ChatEvent>,
) -> Option<(Channel, Loop)> {
    let mut looped = None;
    for ev in events {
        if looped.is_none() {
            looped = watch.push(&ev);
        }
        acc.apply(ev);
    }
    looped
}

/// The result of an attempt whose stream ended with `[DONE]`.
fn finished(acc: ChatCompletion, guard: &Guard, seed: u32) -> Finished {
    Finished {
        outcome: acc.outcome(guard),
        content: unescape(&acc.content).into_owned(),
        reasoning: unescape(&acc.reasoning).into_owned(),
        finish: acc.finish,
        usage: acc.usage,
        seed,
    }
}

/// A recorded `/v1/chat/completions` response read the way an attempt reads the live one:
/// the status, the SSE lines, the chunks, the repetition guard and the end of the stream,
/// so recorded generations can be judged again by today's code (T0, docs/architecture.md
/// §11). An error is the one the attempt would have failed with.
pub fn read_recorded(
    status: u16,
    body: &[u8],
    guard: &Guard,
    seed: u32,
) -> Result<Finished, EngineError> {
    if status != 200 {
        let body = String::from_utf8_lossy(body).into_owned();
        return Err(EngineError::Status { status, body });
    }
    let mut acc = ChatCompletion::default();
    let mut watch = Watch::default();
    for event in sse::decode(body) {
        let SseEvent::Data(v) = event? else {
            return Ok(finished(acc, guard, seed));
        };
        if let Some((channel, l)) = fold(&mut acc, &mut watch, chat_events(&v)?) {
            return Ok(stopped(acc, channel, &l, seed));
        }
    }
    Err(no_done())
}

/// The result of an attempt the repetition guard stopped: nothing in it may run.
fn stopped(acc: ChatCompletion, mut channel: Channel, l: &Loop, seed: u32) -> Finished {
    if let Channel::Arguments { index, name } = &mut channel {
        *name = acc
            .tool_calls
            .get(*index as usize)
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty());
    }
    Finished {
        outcome: Outcome::Repeating {
            partial: acc.tool_calls,
            channel,
            sample: unescape(&l.sample).into_owned(),
        },
        content: unescape(&acc.content).into_owned(),
        reasoning: unescape(&acc.reasoning).into_owned(),
        finish: None,
        usage: acc.usage,
        seed,
    }
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

/// A fragment for display. A joiner pair split across two fragments shows as nothing; the
/// [`Finished`] texts are exact.
fn shown(ev: &ChatEvent) -> Option<DriverEvent> {
    let plain = |t: &str| unescape(t).into_owned();
    match ev {
        ChatEvent::Text(t) => Some(DriverEvent::Text(plain(t))),
        ChatEvent::Reasoning(r) => Some(DriverEvent::Reasoning(plain(r))),
        ChatEvent::ToolCall {
            index,
            name,
            arguments,
            ..
        } => Some(DriverEvent::ToolCallDelta {
            index: *index,
            name: name.clone(),
            arguments: plain(arguments),
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

    /// What `llm_tap.py` keeps of a response: the body without the HTTP head.
    fn recorded(events: &[Value], done: bool) -> String {
        let r = sse("200 OK", events, done);
        r[r.find("\r\n\r\n").unwrap() + 4..].to_owned()
    }

    #[test]
    fn a_recorded_stream_reads_like_a_live_one() {
        let guard = Guard::default();
        let body = recorded(&tool_call_stream(), true);
        let f = read_recorded(200, body.as_bytes(), &guard, 7).unwrap();
        let Outcome::Calls(calls) = &f.outcome else {
            panic!("{f:?}");
        };
        assert_eq!(calls.calls()[0].arguments, r#"{"path":"a.rs"}"#);
        assert_eq!((f.reasoning.as_str(), f.seed), ("read it", 7));
        assert_eq!(f.usage.cache_n, Some(9));

        // cut anywhere before the end of `[DONE]`, the stream allows nothing
        let end = body.find("data: [DONE]").unwrap() + "data: [DONE]".len();
        for cut in 0..=end {
            let r = read_recorded(200, &body.as_bytes()[..cut], &guard, 7);
            assert!(
                matches!(r, Err(EngineError::Protocol(_))),
                "cut at {cut}: {r:?}"
            );
        }

        let error = recorded(
            &[chunk(json!({"role": "assistant"}), None), format_error()],
            false,
        );
        let e = read_recorded(200, error.as_bytes(), &guard, 7).unwrap_err();
        assert!(is_format_error(&e), "{e:?}");
        let full = br#"{"error":{"code":400,"type":"exceed_context_size_error"}}"#;
        let e = read_recorded(400, full, &guard, 7).unwrap_err();
        assert!(is_context_exceeded(&e), "{e:?}");

        // the driver dropped this stream once it looped, so it has no `[DONE]`
        let looping: Vec<Value> = (0..300)
            .map(|_| chunk(json!({"reasoning_content": "over and over. "}), None))
            .collect();
        let f = read_recorded(200, recorded(&looping, false).as_bytes(), &guard, 7).unwrap();
        assert!(
            matches!(
                &f.outcome,
                Outcome::Repeating {
                    channel: Channel::Reasoning,
                    ..
                }
            ),
            "{:?}",
            f.outcome
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

    /// Streams `chunks` one write at a time and reports the chunk at which the client was
    /// gone (`None`: it read everything).
    async fn streaming_server(chunks: Vec<Value>) -> (String, Arc<Mutex<Option<usize>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let gone = Arc::new(Mutex::new(None));
        let seen = gone.clone();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1 << 16];
            let _ = sock.read(&mut buf).await.unwrap();
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            sock.write_all(head.as_bytes()).await.unwrap();
            let mut frames: Vec<String> = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
            frames.push("data: [DONE]\n\n".into());
            for (i, f) in frames.iter().enumerate() {
                let frame = format!("{:x}\r\n{f}\r\n", f.len());
                if sock.write_all(frame.as_bytes()).await.is_err() || sock.flush().await.is_err() {
                    *seen.lock().unwrap() = Some(i);
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });
        (url, gone)
    }

    #[tokio::test]
    async fn a_looping_stream_is_dropped_and_nothing_runs() {
        // M1 stamp A, Gemma, misc-ai-chat-multiturn: a write argument stuck on 56 bytes
        let period = "tok\":256,\"i for i, tok in enumerate(SPECIALS) if False,\n";
        let mut chunks = vec![chunk(
            json!({"tool_calls": [{"index": 0, "id": "w", "type": "function", "function": {"name": "write", "arguments": "{\"file\":\"a.py\",\"text\":\""}}]}),
            None,
        )];
        for _ in 0..600 {
            for piece in [&period[..20], &period[20..]] {
                chunks.push(chunk(
                    json!({"tool_calls": [{"index": 0, "function": {"arguments": piece}}]}),
                    None,
                ));
            }
        }
        chunks.push(chunk(json!({}), Some("length")));
        let total = chunks.len();
        let (url, gone) = streaming_server(chunks).await;
        let events = run(&driver(&url, Family::Gemma4), request(1)).await;
        let f = finished(&events);
        let Outcome::Repeating {
            partial,
            channel,
            sample,
        } = &f.outcome
        else {
            panic!("{:?}", f.outcome)
        };
        assert_eq!(partial.len(), 1);
        assert_eq!(channel.to_string(), "the arguments of write");
        assert!(period.repeat(2).contains(sample.as_str()), "{sample:?}");
        assert_eq!(f.finish, None);
        // the guard fired near 2 KB of loop, and the server saw the client go long before
        // the end of the stream
        for _ in 0..200 {
            if gone.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let at = gone.lock().unwrap().expect("the client kept reading");
        assert!(at < 200 && at < total, "dropped at chunk {at} of {total}");
    }

    #[tokio::test]
    async fn control_token_text_is_broken_in_requests_and_restored_in_results() {
        let p = profile::builtin()
            .unwrap()
            .into_iter()
            .find(|p| p.family == Family::Gemma4)
            .unwrap();
        // control-token texts are built from parts here (§5.3 repository rule)
        let bos = ["<", "bos>"].concat();
        let quote = ["<|", "\"|>"].concat();
        let shown = Guard::new(&p.tokens).escape(&bos).into_owned();
        let reply = vec![
            chunk(json!({"content": format!("the list holds {shown}")}), None),
            chunk(
                json!({"tool_calls": [{"index": 0, "id": "e", "type": "function", "function": {"name": "edit", "arguments": json!({"file": "a.py", "find": shown, "replace": "\"x\""}).to_string()}}]}),
                None,
            ),
            chunk(json!({}), Some("tool_calls")),
        ];
        let (url, bodies) = fake_server(vec![sse("200 OK", &reply, true)]).await;
        let d =
            ChatDriver::new(LlamaServer::new(&url, None).unwrap(), p.clone(), None, None).unwrap();
        let mut req = request(1);
        req.messages = vec![
            ChatMessage::User(format!("Render {bos} first.")),
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: Some(format!("the file starts with {bos}")),
                tool_calls: vec![ToolCall {
                    id: "r".into(),
                    name: "read".into(),
                    arguments: json!({"file": "a.py"}).to_string(),
                }],
            },
            ChatMessage::Tool {
                tool_call_id: "r".into(),
                content: format!("1│SPECIALS = [\"{bos}\"]  # {quote}"),
            },
        ];
        let events = run(&d, req).await;
        let sent = bodies.lock().unwrap()[0].to_string();
        for g in p.tokens.guarded() {
            let g = serde_json::to_string(g).unwrap();
            assert!(!sent.contains(&g[1..g.len() - 1]), "{g} reached the server");
        }
        assert!(
            sent.contains("1│SPECIALS = [\\\"<\u{2060}bos>\\\"]"),
            "{sent}"
        );
        let f = finished(&events);
        assert_eq!(f.content, format!("the list holds {bos}"));
        let Outcome::Calls(calls) = &f.outcome else {
            panic!("{:?}", f.outcome)
        };
        let args: Value = serde_json::from_str(&calls.calls()[0].arguments).unwrap();
        assert_eq!(args["find"], json!(bos));
    }

    #[test]
    fn unknown_reasoning_level_is_rejected() {
        let qwen = profile::builtin().unwrap().remove(2);
        let server = LlamaServer::new("http://127.0.0.1:1", None).unwrap();
        // "high" is an error in Qwen's official template
        assert!(ChatDriver::new(server, qwen, Some("high"), None).is_err());
    }
}
