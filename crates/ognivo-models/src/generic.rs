//! The Generic path: OpenAI `/v1/chat/completions` (docs/architecture.md §5.2, M1).
//!
//! This is the baseline transport and the path for any model without a codec. On someone
//! else's server the agent cannot replace the chat template, so correctness rests on the
//! request rules the llama.cpp maintainers require (docs/research/2026-10-29/llamacpp.md,
//! lines 66-68), which this module encodes:
//! - sampling is always explicit: the server's command-line defaults match no model;
//! - `tool_choice` is a string only (`auto`|`none`|`required`); to force one tool, send
//!   only that tool with `Required` (there is no object form here);
//! - tools and `response_format` are never mixed — [`ChatRequest`] has no response_format,
//!   so the two cannot be sent together;
//! - `parallel_tool_calls` is explicit; the driver sets it off for gpt-oss, whose template
//!   renders only `tool_calls[0]` (a second call would vanish from history);
//! - `reasoning_content` of an assistant message is sent back verbatim when the profile's
//!   history policy keeps it (llama.cpp #27720: 7/120 malformed turns without it, 0/120
//!   with it). Whether a turn carries reasoning is the driver's decision; this module sends
//!   exactly what it is given.
//!
//! Tool calls may run only through [`Outcome::Calls`], which [`ChatCompletion::outcome`]
//! grants once the generation ended with `finish_reason=tool_calls` and every call is
//! complete and free of the model's own control tokens ([`crate::guard`]); streamed
//! [`ChatEvent::ToolCall`] fragments are a preview. A stream that ends without `[DONE]`
//! never gets this far ([`crate::llama::LlamaServer::chat_stream`]).
//!
//! The server's own parser can reject the model output: before the first byte as HTTP 500,
//! after it as an in-band `{"error": ...}` chunk (M0: HTTP 200, then the error, no
//! `[DONE]`). Both carry the same message; see [`is_format_error`].

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::engine::{EngineError, SamplerSpec, Usage};
use crate::guard::Guard;
use crate::image::Image;

/// One message of the conversation, in OpenAI chat shape. The session store keeps it as
/// `{"role", "content"}` with `reasoning`, `tool_calls`, `tool_call_id` or `images` where
/// they apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(into = "StoredMessage", try_from = "StoredMessage")]
pub enum ChatMessage {
    System(String),
    /// The user's message, or ognivo's note to the model, with the user's images.
    User {
        text: String,
        images: Vec<Image>,
    },
    Assistant {
        /// Text shown to the user (a Harmony preamble or the final answer); may be empty.
        content: String,
        /// Verbatim chain of thought, sent back only when the profile's history policy
        /// keeps it. `None` omits the field entirely; `Some("")` still sends an empty
        /// `reasoning_content`, which some templates treat as meaningful.
        reasoning: Option<String>,
        /// gpt-oss keeps at most one call per assistant message (its template renders only
        /// the first); other families may carry several.
        tool_calls: Vec<ToolCall>,
        /// The ids the model generated for this message, for the Native path to send back as
        /// they came (§5.4); `None` on the Generic path, and where the Native path repaired
        /// the generation.
        tokens: Option<Vec<u32>>,
    },
    /// One tool result, answering the assistant's call with id `tool_call_id`: its text,
    /// and the image file it read.
    Tool {
        tool_call_id: String,
        content: String,
        images: Vec<Image>,
    },
}

impl ChatMessage {
    /// A message of the user's, or a note of ognivo's, without images.
    pub fn user(text: impl Into<String>) -> Self {
        ChatMessage::User {
            text: text.into(),
            images: Vec::new(),
        }
    }

    /// A tool result without images.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            images: Vec::new(),
        }
    }

    /// The images the message holds.
    pub fn images(&self) -> &[Image] {
        match self {
            ChatMessage::User { images, .. } | ChatMessage::Tool { images, .. } => images,
            ChatMessage::System(_) | ChatMessage::Assistant { .. } => &[],
        }
    }
}

/// A tool call the model emitted, or one replayed back into history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments exactly as the model produced them; not yet validated.
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// [`ChatMessage`] as the store keeps it.
#[derive(Debug, Serialize, Deserialize)]
struct StoredMessage {
    role: Role,
    #[serde(default)]
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokens: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    images: Vec<Image>,
}

impl From<ChatMessage> for StoredMessage {
    fn from(m: ChatMessage) -> Self {
        let plain = |role, content| StoredMessage {
            role,
            content,
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tokens: None,
            images: Vec::new(),
        };
        match m {
            ChatMessage::System(content) => plain(Role::System, content),
            ChatMessage::User { text, images } => StoredMessage {
                images,
                ..plain(Role::User, text)
            },
            ChatMessage::Assistant {
                content,
                reasoning,
                tool_calls,
                tokens,
            } => StoredMessage {
                reasoning,
                tool_calls,
                tokens,
                ..plain(Role::Assistant, content)
            },
            ChatMessage::Tool {
                tool_call_id,
                content,
                images,
            } => StoredMessage {
                tool_call_id: Some(tool_call_id),
                images,
                ..plain(Role::Tool, content)
            },
        }
    }
}

impl TryFrom<StoredMessage> for ChatMessage {
    type Error = String;

    fn try_from(m: StoredMessage) -> Result<Self, String> {
        Ok(match m.role {
            Role::System => ChatMessage::System(m.content),
            Role::User => ChatMessage::User {
                text: m.content,
                images: m.images,
            },
            Role::Assistant => ChatMessage::Assistant {
                content: m.content,
                reasoning: m.reasoning,
                tool_calls: m.tool_calls,
                tokens: m.tokens,
            },
            Role::Tool => ChatMessage::Tool {
                tool_call_id: m.tool_call_id.ok_or("a tool result without tool_call_id")?,
                content: m.content,
                images: m.images,
            },
        })
    }
}

/// A tool offered to the model, in OpenAI `tools` shape.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    /// JSON Schema of the parameters object.
    pub parameters: Value,
}

/// The string forms llama-server accepts; there is deliberately no object form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
}

impl ToolChoice {
    fn as_str(self) -> &'static str {
        match self {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
            ToolChoice::Required => "required",
        }
    }
}

/// One `/v1/chat/completions` request on the Generic path.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub sampler: SamplerSpec,
    /// `chat_template_kwargs` for the chosen reasoning level
    /// ([`crate::ModelProfile::chat_template_kwargs`]).
    pub template_kwargs: Map<String, Value>,
    pub tools: Vec<ToolDecl>,
    pub tool_choice: ToolChoice,
    pub parallel_tool_calls: bool,
    /// Hard cap on generated tokens (the per-step reasoning budget, M0 finding #2).
    pub max_tokens: Option<u32>,
    /// Pinned server slot, so the session keeps its KV cache.
    pub slot: Option<u32>,
}

/// Builds the JSON body. Every sampler the server might apply from its command line is set
/// here, so the request does not inherit the server's defaults (same discipline as the
/// Native path, see `engine::completion_body`).
pub fn chat_body(req: &ChatRequest) -> Value {
    let s = &req.sampler;
    let mut body = json!({
        "messages": req.messages.iter().map(message_json).collect::<Vec<_>>(),
        "stream": true,
        // ask for the final usage/timings chunk, so cache-hit telemetry is available
        "stream_options": { "include_usage": true },
        "cache_prompt": true,
        "temperature": s.temperature,
        "top_p": s.top_p,
        "top_k": s.top_k,
        "min_p": s.min_p,
        // neutralise every other sampler the server could carry from its command line
        "typical_p": 1.0,
        "repeat_penalty": 1.0,
        "presence_penalty": 0.0,
        "frequency_penalty": 0.0,
        "dry_multiplier": 0.0,
        "xtc_probability": 0.0,
        "mirostat": 0,
        "top_n_sigma": -1.0,
        "dynatemp_range": 0.0,
        "adaptive_target": -1.0,
        "ignore_eos": false,
    });
    if let Some(seed) = s.seed {
        body["seed"] = json!(seed);
    }
    if let Some(slot) = req.slot {
        body["id_slot"] = json!(slot);
    }
    if let Some(n) = req.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if !req.template_kwargs.is_empty() {
        body["chat_template_kwargs"] = Value::Object(req.template_kwargs.clone());
    }
    if !req.tools.is_empty() {
        body["tools"] = req.tools.iter().map(tool_json).collect();
        body["tool_choice"] = json!(req.tool_choice.as_str());
        body["parallel_tool_calls"] = json!(req.parallel_tool_calls);
    }
    body
}

fn message_json(m: &ChatMessage) -> Value {
    match m {
        ChatMessage::System(content) => json!({ "role": "system", "content": content }),
        // the images before the text, as the models' makers advise
        ChatMessage::User { text, images } => json!({
            "role": "user",
            "content": content_json(images, text, false),
        }),
        ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
        } => {
            let mut o = Map::new();
            o.insert("role".into(), json!("assistant"));
            o.insert("content".into(), json!(content));
            if let Some(r) = reasoning {
                o.insert("reasoning_content".into(), json!(r));
            }
            if !tool_calls.is_empty() {
                o.insert(
                    "tool_calls".into(),
                    tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": { "name": tc.name, "arguments": tc.arguments },
                            })
                        })
                        .collect(),
                );
            }
            Value::Object(o)
        }
        // what the call says of the image, then the image
        ChatMessage::Tool {
            tool_call_id,
            content,
            images,
        } => json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": content_json(images, content, true),
        }),
    }
}

/// A message's content: its text alone, or with images the parts llama-server reads them
/// from, each image a data URL. The server joins the parts into one text with its media
/// marker where each image stands, and no line break next to one.
fn content_json(images: &[Image], text: &str, text_first: bool) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut parts: Vec<Value> = images
        .iter()
        .map(|i| json!({"type": "image_url", "image_url": {"url": i.data_url()}}))
        .collect();
    if !text.is_empty() {
        let at = if text_first { 0 } else { parts.len() };
        parts.insert(at, json!({"type": "text", "text": text}));
    }
    Value::Array(parts)
}

fn tool_json(t: &ToolDecl) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        },
    })
}

/// Why a generation stopped (`finish_reason`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// A plain final answer.
    Stop,
    /// The model wants its tool calls run; only then may they execute.
    ToolCalls,
    /// The token limit was hit mid-generation: a tool call here is truncated and must not
    /// run (docs/research/2026-10-29/llamacpp.md:68).
    Length,
    Other(String),
}

impl FinishReason {
    fn parse(s: &str) -> Self {
        match s {
            "stop" => FinishReason::Stop,
            "tool_calls" => FinishReason::ToolCalls,
            "length" => FinishReason::Length,
            other => FinishReason::Other(other.to_owned()),
        }
    }
}

/// One piece of a streamed chat response. `id`/`name` of a tool call arrive once; its
/// `arguments` accrete across fragments at the same `index`.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    Text(String),
    Reasoning(String),
    ToolCall {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Finish(FinishReason),
    Usage(Usage),
}

/// Maps one SSE chunk to its events. An `error` chunk becomes an error, as the Native path
/// does; the final `include_usage` chunk has empty `choices` and carries only usage.
pub fn chat_events(v: &Value) -> Result<Vec<ChatEvent>, EngineError> {
    if let Some(err) = v.get("error") {
        let status = err["code"]
            .as_u64()
            .and_then(|c| u16::try_from(c).ok())
            .unwrap_or(500);
        let body = err["message"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| err.to_string());
        return Err(EngineError::Status { status, body });
    }
    let mut out = Vec::new();
    for choice in v["choices"].as_array().into_iter().flatten() {
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            out.push(ChatEvent::Text(text.to_owned()));
        }
        if let Some(r) = delta["reasoning_content"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            out.push(ChatEvent::Reasoning(r.to_owned()));
        }
        for call in delta["tool_calls"].as_array().into_iter().flatten() {
            // the index ties fragments of one call together; default to 0 when a server omits it
            let index = call["index"].as_u64().and_then(|n| u32::try_from(n).ok());
            let f = &call["function"];
            out.push(ChatEvent::ToolCall {
                index: index.unwrap_or(0),
                id: call["id"].as_str().map(str::to_owned),
                name: f["name"].as_str().map(str::to_owned),
                arguments: f["arguments"].as_str().unwrap_or_default().to_owned(),
            });
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            out.push(ChatEvent::Finish(FinishReason::parse(reason)));
        }
    }
    if v.get("usage").is_some() || v.get("timings").is_some() {
        out.push(ChatEvent::Usage(usage_of(v)));
    }
    Ok(out)
}

/// Reads token counts from the OpenAI `usage` object and the richer llama.cpp `timings`.
fn usage_of(v: &Value) -> Usage {
    let u = &v["usage"];
    let t = &v["timings"];
    let n = |x: &Value| x.as_u64().and_then(|n| u32::try_from(n).ok());
    Usage {
        prompt_n: n(&t["prompt_n"]).or_else(|| n(&u["prompt_tokens"])),
        cache_n: n(&t["cache_n"]),
        predicted_n: n(&t["predicted_n"]).or_else(|| n(&u["completion_tokens"])),
        prompt_ms: t["prompt_ms"].as_f64(),
        predicted_ms: t["predicted_ms"].as_f64(),
    }
}

/// The folded result of a chat generation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatCompletion {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: Option<FinishReason>,
    pub usage: Usage,
}

impl ChatCompletion {
    /// Folds one event in, growing tool calls by index and concatenating their arguments.
    pub fn apply(&mut self, ev: ChatEvent) {
        match ev {
            ChatEvent::Text(t) => self.content.push_str(&t),
            ChatEvent::Reasoning(r) => self.reasoning.push_str(&r),
            ChatEvent::ToolCall {
                index,
                id,
                name,
                arguments,
            } => {
                let i = index as usize;
                if self.tool_calls.len() <= i {
                    self.tool_calls.resize(
                        i + 1,
                        ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        },
                    );
                }
                let call = &mut self.tool_calls[i];
                if let Some(id) = id {
                    call.id = id;
                }
                if let Some(name) = name {
                    call.name = name;
                }
                call.arguments.push_str(&arguments);
            }
            ChatEvent::Finish(reason) => self.finish = Some(reason),
            ChatEvent::Usage(u) => self.usage = u,
        }
    }

    /// What the finished generation allows. Call it only after the stream ended normally.
    /// Executable calls carry the arguments with the guard's joiners removed.
    pub fn outcome(&self, guard: &Guard) -> Outcome {
        let calls = &self.tool_calls;
        match &self.finish {
            Some(FinishReason::ToolCalls) => match check_calls(calls, guard) {
                Ok(calls) => Outcome::Calls(ExecutableCalls(calls)),
                Err(reason) => Outcome::Malformed {
                    calls: calls.clone(),
                    reason,
                },
            },
            Some(FinishReason::Stop) if calls.is_empty() => Outcome::Answer,
            Some(FinishReason::Stop) => Outcome::Malformed {
                calls: calls.clone(),
                reason: "tool calls with finish_reason stop".into(),
            },
            Some(FinishReason::Length) => Outcome::Truncated {
                partial: calls.clone(),
            },
            Some(FinishReason::Other(r)) => Outcome::Malformed {
                calls: calls.clone(),
                reason: format!("finish_reason {r}"),
            },
            None => Outcome::Malformed {
                calls: calls.clone(),
                reason: "no finish_reason".into(),
            },
        }
    }
}

/// What a finished generation allows the agent to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Complete calls the model asked to run.
    Calls(ExecutableCalls),
    /// A final answer without calls.
    Answer,
    /// Cut by the token limit: nothing from this generation may run.
    Truncated { partial: Vec<ToolCall> },
    /// Stopped by the driver because a stream fell into a loop ([`crate::repeat`]):
    /// nothing from this generation may run.
    Repeating {
        partial: Vec<ToolCall>,
        /// Where the loop was.
        channel: Channel,
        /// One period of it.
        sample: String,
    },
    /// The generation ended, but its calls cannot run as they are.
    Malformed {
        calls: Vec<ToolCall>,
        reason: String,
    },
}

/// A stream of one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Reasoning,
    Text,
    /// The arguments of the call at `index`; `name` once its header arrived.
    Arguments {
        index: u32,
        name: Option<String>,
    },
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::Reasoning => f.write_str("the reasoning"),
            Channel::Text => f.write_str("the text"),
            Channel::Arguments { name: Some(n), .. } => write!(f, "the arguments of {n}"),
            Channel::Arguments { index, .. } => write!(f, "the arguments of call {index}"),
        }
    }
}

/// Calls that passed [`ChatCompletion::outcome`]: the generation ended with
/// `finish_reason=tool_calls`, and every call has an id, a name and arguments that parse
/// as a JSON object free of the model's own control tokens, with no id repeated. Nothing
/// outside this crate can build one.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutableCalls(Vec<ToolCall>);

impl ExecutableCalls {
    /// The calls in the order the model emitted them; never empty.
    pub fn calls(&self) -> &[ToolCall] {
        &self.0
    }

    /// Calls the Native path read and checked itself: complete, each with an id, a name and an
    /// object of arguments.
    pub(crate) fn checked(calls: Vec<ToolCall>) -> Self {
        ExecutableCalls(calls)
    }
}

/// The calls as the tools must see them, or why they cannot run.
fn check_calls(calls: &[ToolCall], guard: &Guard) -> Result<Vec<ToolCall>, String> {
    if calls.is_empty() {
        return Err("finish_reason tool_calls without calls".into());
    }
    let mut ids = HashSet::new();
    let mut out = Vec::with_capacity(calls.len());
    for (i, c) in calls.iter().enumerate() {
        if c.id.is_empty() || c.name.is_empty() {
            return Err(format!("call {i} has no id or name"));
        }
        if !ids.insert(c.id.as_str()) {
            return Err(format!("call id {} repeats", c.id));
        }
        let mut args = match serde_json::from_str::<Value>(&c.arguments) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => return Err(format!("{}: arguments are not a JSON object", c.name)),
            Err(e) => return Err(format!("{}: arguments are not JSON: {e}", c.name)),
        };
        if let Some(token) = guard.framing_in_json(&args) {
            return Err(format!(
                "{}: the arguments contain {token}, a control token of your own format, so the call ran past its end. \
                 To write such text on purpose, put U+2060 after its first character (\\u2060 in JSON); ognivo removes it",
                c.name
            ));
        }
        let arguments = if guard.restore_json(&mut args) {
            args.to_string()
        } else {
            c.arguments.clone()
        };
        out.push(ToolCall {
            arguments,
            ..c.clone()
        });
    }
    Ok(out)
}

/// The server's parser rejected the model output (llama.cpp common/chat.cpp: "The model
/// produced output that does not match the expected <format> format"), as HTTP 500 or as
/// an in-band error chunk. One retry with a new seed is the documented remedy (§5.2).
pub fn is_format_error(e: &EngineError) -> bool {
    matches!(e, EngineError::Status { status: 500, body } if body.contains("does not match the expected"))
}

/// HTTP 400 `exceed_context_size_error`: the prompt does not fit the slot's context.
pub fn is_context_exceeded(e: &EngineError) -> bool {
    matches!(e, EngineError::Status { status: 400, body } if body.contains("exceed_context_size_error"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineError;

    #[test]
    fn messages_are_stored_as_they_are_and_read_back() {
        let messages = [
            ChatMessage::System("be brief".into()),
            ChatMessage::user("hi"),
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                }],
                tokens: None,
            },
            ChatMessage::Assistant {
                content: "done".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                tokens: None,
            },
            ChatMessage::tool("c1", "[read: a]"),
        ];
        let stored: Vec<String> = messages
            .iter()
            .map(|m| serde_json::to_string(m).unwrap())
            .collect();
        assert_eq!(stored[0], r#"{"role":"system","content":"be brief"}"#);
        // an empty reasoning is not a missing one
        assert_eq!(
            stored[2],
            r#"{"role":"assistant","content":"","reasoning":"","tool_calls":[{"id":"c1","name":"read","arguments":"{\"path\":\"a\"}"}]}"#
        );
        assert_eq!(stored[3], r#"{"role":"assistant","content":"done"}"#);
        assert_eq!(
            stored[4],
            r#"{"role":"tool","content":"[read: a]","tool_call_id":"c1"}"#
        );
        let back: Vec<ChatMessage> = stored
            .iter()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(back, messages);
        assert!(serde_json::from_str::<ChatMessage>(r#"{"role":"tool","content":"x"}"#).is_err());
        assert!(serde_json::from_str::<ChatMessage>(r#"{"role":"developer"}"#).is_err());
    }

    #[test]
    fn images_go_as_parts_the_users_before_the_text_a_results_after_it() {
        let image = Image::new(&crate::image::tests::png(2, 2)).unwrap();
        let url = image.data_url();
        let user = ChatMessage::User {
            text: "What is this?".into(),
            images: vec![image.clone()],
        };
        assert_eq!(
            message_json(&user),
            json!({"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": url}},
                {"type": "text", "text": "What is this?"},
            ]})
        );
        let result = ChatMessage::Tool {
            tool_call_id: "c1".into(),
            content: "[read: a.png]".into(),
            images: vec![image.clone()],
        };
        assert_eq!(
            message_json(&result)["content"],
            json!([
                {"type": "text", "text": "[read: a.png]"},
                {"type": "image_url", "image_url": {"url": url}},
            ])
        );
        // no text, no text part; without images the content is the text
        let bare = ChatMessage::User {
            text: String::new(),
            images: vec![image.clone()],
        };
        assert_eq!(message_json(&bare)["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            message_json(&ChatMessage::user("hi"))["content"],
            json!("hi")
        );
        // the store keeps the images with the message, and reads them back
        let stored = serde_json::to_value(&user).unwrap();
        assert_eq!(stored["images"][0]["mime"], "image/png");
        assert_eq!(serde_json::from_value::<ChatMessage>(stored).unwrap(), user);
        assert_eq!(user.images(), [image]);
    }

    fn sampler() -> SamplerSpec {
        SamplerSpec {
            temperature: 1.0,
            top_p: 0.95,
            top_k: 64,
            min_p: 0.0,
            seed: Some(7),
        }
    }

    fn read_tool() -> ToolDecl {
        ToolDecl {
            name: "Read".into(),
            description: "Read a file".into(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }
    }

    #[test]
    fn body_sets_explicit_sampling_and_string_tool_choice() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("hi")],
            sampler: sampler(),
            template_kwargs: serde_json::from_value(json!({"reasoning_effort": "high"})).unwrap(),
            tools: vec![read_tool()],
            tool_choice: ToolChoice::Required,
            parallel_tool_calls: false,
            max_tokens: Some(2048),
            slot: Some(3),
        };
        let b = chat_body(&req);
        assert_eq!(b["stream"], json!(true));
        assert_eq!(b["stream_options"], json!({ "include_usage": true }));
        assert_eq!(b["temperature"], json!(1.0));
        assert_eq!(b["top_k"], json!(64));
        assert_eq!(b["mirostat"], json!(0));
        assert_eq!(b["seed"], json!(7));
        assert_eq!(b["id_slot"], json!(3));
        assert_eq!(b["max_tokens"], json!(2048));
        assert_eq!(
            b["chat_template_kwargs"],
            json!({"reasoning_effort": "high"})
        );
        // tool_choice is a bare string, never an object
        assert_eq!(b["tool_choice"], json!("required"));
        assert!(b["tool_choice"].is_string());
        assert_eq!(b["parallel_tool_calls"], json!(false));
        assert_eq!(b["tools"][0]["type"], json!("function"));
        assert_eq!(b["tools"][0]["function"]["name"], json!("Read"));
        // response_format cannot be set: the type has no such field
        assert!(b.get("response_format").is_none());
    }

    #[test]
    fn body_without_tools_omits_tool_fields() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("hi")],
            sampler: sampler(),
            template_kwargs: Map::new(),
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: true,
            max_tokens: None,
            slot: None,
        };
        let b = chat_body(&req);
        assert!(b.get("tools").is_none());
        assert!(b.get("tool_choice").is_none());
        assert!(b.get("parallel_tool_calls").is_none());
        assert!(b.get("chat_template_kwargs").is_none());
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("id_slot").is_none());
    }

    #[test]
    fn assistant_history_carries_reasoning_and_calls_verbatim() {
        let msg = ChatMessage::Assistant {
            content: "preamble".into(),
            reasoning: Some("because".into()),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "Read".into(),
                arguments: "{\"path\":\"a.rs\"}".into(),
            }],
            tokens: None,
        };
        let v = message_json(&msg);
        assert_eq!(v["role"], json!("assistant"));
        assert_eq!(v["content"], json!("preamble"));
        assert_eq!(v["reasoning_content"], json!("because"));
        assert_eq!(v["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(v["tool_calls"][0]["type"], json!("function"));
        assert_eq!(
            v["tool_calls"][0]["function"]["arguments"],
            json!("{\"path\":\"a.rs\"}")
        );
        // None omits the field; Some("") keeps an explicit empty string
        let none = message_json(&ChatMessage::Assistant {
            content: "x".into(),
            reasoning: None,
            tool_calls: vec![],
            tokens: None,
        });
        assert!(none.get("reasoning_content").is_none());
        assert!(none.get("tool_calls").is_none());
        let empty = message_json(&ChatMessage::Assistant {
            content: "x".into(),
            reasoning: Some(String::new()),
            tool_calls: vec![],
            tokens: None,
        });
        assert_eq!(empty["reasoning_content"], json!(""));
    }

    #[test]
    fn folds_a_tool_call_split_across_fragments() {
        // name/id in the first fragment, arguments split across the next two
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {"reasoning_content": "think"}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "Read", "arguments": "{\"pa"}}]}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "th\":\"a.rs\"}"}}]}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 12}, "timings": {"cache_n": 80, "prompt_ms": 5.0}}),
        ];
        let mut c = ChatCompletion::default();
        for chunk in &chunks {
            for ev in chat_events(chunk).unwrap() {
                c.apply(ev);
            }
        }
        assert_eq!(c.reasoning, "think");
        assert_eq!(c.content, "");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].id, "call_a");
        assert_eq!(c.tool_calls[0].name, "Read");
        assert_eq!(c.tool_calls[0].arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(c.finish, Some(FinishReason::ToolCalls));
        assert_eq!(c.usage.prompt_n, Some(100));
        assert_eq!(c.usage.predicted_n, Some(12));
        assert_eq!(c.usage.cache_n, Some(80));
        let Outcome::Calls(calls) = c.outcome(&Guard::default()) else {
            panic!("{:?}", c.outcome(&Guard::default()))
        };
        assert_eq!(calls.calls(), c.tool_calls.as_slice());
    }

    #[test]
    fn two_parallel_calls_accrete_by_index() {
        let chunk = json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "a", "function": {"name": "Read", "arguments": "{}"}},
            {"index": 1, "id": "b", "function": {"name": "Grep", "arguments": "{\"q\":\"x\"}"}}
        ]}, "finish_reason": null}]});
        let mut c = ChatCompletion::default();
        for ev in chat_events(&chunk).unwrap() {
            c.apply(ev);
        }
        assert_eq!(c.tool_calls.len(), 2);
        assert_eq!(c.tool_calls[1].name, "Grep");
        assert_eq!(c.tool_calls[1].arguments, "{\"q\":\"x\"}");
    }

    #[test]
    fn plain_text_answer_folds_to_content() {
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"content": "Hel"}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {"content": "lo"}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let mut c = ChatCompletion::default();
        for chunk in &chunks {
            for ev in chat_events(chunk).unwrap() {
                c.apply(ev);
            }
        }
        assert_eq!(c.content, "Hello");
        assert_eq!(c.finish, Some(FinishReason::Stop));
        assert_eq!(c.outcome(&Guard::default()), Outcome::Answer);
    }

    #[test]
    fn truncated_tool_call_is_not_runnable() {
        let chunks = [
            json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "a", "function": {"name": "Write", "arguments": "{\"path\":\"a\",\"cont"}}]}, "finish_reason": null}]}),
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "length"}]}),
        ];
        let mut c = ChatCompletion::default();
        for chunk in &chunks {
            for ev in chat_events(chunk).unwrap() {
                c.apply(ev);
            }
        }
        assert_eq!(c.finish, Some(FinishReason::Length));
        // a length-truncated call must never be executed
        assert!(
            matches!(c.outcome(&Guard::default()), Outcome::Truncated { partial } if partial.len() == 1)
        );
    }

    fn finished(calls: Vec<ToolCall>, finish: FinishReason) -> ChatCompletion {
        ChatCompletion {
            tool_calls: calls,
            finish: Some(finish),
            ..ChatCompletion::default()
        }
    }

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args.into(),
        }
    }

    #[test]
    fn only_complete_calls_are_executable() {
        let bad = [
            (vec![], "without calls"),
            (vec![call("", "Read", "{}")], "no id or name"),
            (vec![call("a", "", "{}")], "no id or name"),
            (vec![call("a", "Read", "{\"path\":")], "not JSON"),
            (vec![call("a", "Read", "[1]")], "not a JSON object"),
            (
                vec![call("a", "Read", "{}"), call("a", "Grep", "{}")],
                "repeats",
            ),
        ];
        for (calls, why) in bad {
            match finished(calls, FinishReason::ToolCalls).outcome(&Guard::default()) {
                Outcome::Malformed { reason, .. } => assert!(reason.contains(why), "{reason}"),
                other => panic!("{why}: {other:?}"),
            }
        }
        let stop_with_calls = finished(vec![call("a", "Read", "{}")], FinishReason::Stop);
        assert!(matches!(
            stop_with_calls.outcome(&Guard::default()),
            Outcome::Malformed { .. }
        ));
        let no_finish = ChatCompletion::default();
        assert!(matches!(
            no_finish.outcome(&Guard::default()),
            Outcome::Malformed { .. }
        ));
    }

    #[test]
    fn the_guard_rejects_leaked_format_and_unbreaks_copied_text() {
        let profiles = crate::profile::builtin().unwrap();
        let gemma = profiles.iter().find(|p| p.id == "gemma-4-26b-a4b").unwrap();
        let guard = Guard::new(&gemma.tokens);
        // control-token texts are built from parts here (§5.3 repository rule)
        let (bos, eos, end_thought) = (
            ["<", "bos>"].concat(),
            ["<", "eos>"].concat(),
            ["<", "channel|>"].concat(),
        );
        // M1 stamp A, cforge-weighted-stripe-move: the end of the thought channel and a new
        // call header inside the content of a write
        let call_header = ["<|", "tool_call>"].concat();
        let leak = format!("I will rewrite it.\n{end_thought}{call_header}call:write{{file:");
        let args = json!({"file": "src/stripe.rs", "text": leak}).to_string();
        match finished(vec![call("a", "write", &args)], FinishReason::ToolCalls).outcome(&guard) {
            Outcome::Malformed { reason, .. } => assert!(
                reason.starts_with(&format!("write: the arguments contain {call_header}")),
                "{reason}"
            ),
            other => panic!("{other:?}"),
        }
        // a copy of guarded file text, joiner and all, runs with the file's own text
        let shown = |t: &str| guard.escape(t).into_owned();
        let copied = json!({"file": "a.py", "find": shown(&bos), "replace": shown(&eos)});
        let Outcome::Calls(calls) = finished(
            vec![call("b", "edit", &copied.to_string())],
            FinishReason::ToolCalls,
        )
        .outcome(&guard) else {
            panic!()
        };
        let restored: Value = serde_json::from_str(&calls.calls()[0].arguments).unwrap();
        assert_eq!(
            restored,
            json!({"file": "a.py", "find": bos, "replace": eos})
        );
        // untouched arguments keep their exact bytes
        let raw = "{\"file\": \"a.py\",  \"limit\": 5}";
        let Outcome::Calls(calls) =
            finished(vec![call("c", "read", raw)], FinishReason::ToolCalls).outcome(&guard)
        else {
            panic!()
        };
        assert_eq!(calls.calls()[0].arguments, raw);
    }

    #[test]
    fn format_errors_in_both_shapes() {
        // in-band chunk, as gpt-oss produced it in M0 (ct-memeq, request 18)
        let v = json!({"error": {"code": 500, "message": "The model produced output that does not match the expected peg-native format", "type": "server_error"}});
        let e = chat_events(&v).unwrap_err();
        assert!(is_format_error(&e), "{e:?}");
        // plain HTTP 500 before streaming: the body is the same error object as text
        let plain = EngineError::Status {
            status: 500,
            body: v.to_string(),
        };
        assert!(is_format_error(&plain));
        let other = EngineError::Status {
            status: 500,
            body: "slot unavailable".into(),
        };
        assert!(!is_format_error(&other));
        let ctx = EngineError::Status {
            status: 400,
            body: json!({"error": {"code": 400, "type": "exceed_context_size_error"}}).to_string(),
        };
        assert!(is_context_exceeded(&ctx) && !is_format_error(&ctx));
    }
}
