//! The token-level backend seam (docs/architecture.md §5.1, layer 1).
//!
//! Codecs talk to a [`TokenEngine`] in token ids only, so a future in-process engine
//! (cforge, for instance) is just another implementation.

use std::future::Future;

use futures::stream::{self, BoxStream, StreamExt};
use serde_json::{Value, json};

use crate::caps::EngineCaps;
use crate::llama::LlamaServer;
use crate::profile::Sampling;

pub type TokenId = u32;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("HTTP: {0}")]
    Http(String),
    #[error("server returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("protocol: {0}")]
    Protocol(String),
}

impl EngineError {
    /// The server refused the request for its API key (HTTP 401 or 403): llama-server
    /// started with `--api-key` got none, or one it does not take. It answers so on every
    /// endpoint but `/health`, `/props` and `/models` too.
    pub fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: 401 | 403,
                ..
            }
        )
    }
}

/// What a client is told of the server at `url` that refused its API key
/// ([`EngineError::is_unauthorized`]), with what the server said.
pub fn key_refused(url: &str, e: &EngineError) -> String {
    let said = match e {
        EngineError::Status { status, body } => {
            let message = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| body.trim().to_owned());
            format!("HTTP {status}: {message}")
        }
        other => other.to_string(),
    };
    format!("{url} wants an API key, or does not take the one given ({said})")
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplerSpec {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,
    pub seed: Option<u32>,
}

impl From<Sampling> for SamplerSpec {
    fn from(s: Sampling) -> Self {
        SamplerSpec {
            temperature: s.temperature,
            top_p: s.top_p,
            top_k: s.top_k,
            min_p: s.min_p,
            seed: None,
        }
    }
}

/// A grammar that switches on at one of its triggers.
///
/// llama.cpp feeds the grammar from the trigger text itself (for pattern triggers, from
/// the first capture group), so `gbnf` must accept generation starting there (M0 finding).
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub(crate) gbnf: String,
    pub(crate) triggers: Vec<Trigger>,
    /// Token strings the grammar must keep as single tokens; word triggers need theirs.
    pub(crate) preserved_tokens: Vec<String>,
}

impl Constraint {
    pub fn new(gbnf: impl Into<String>, triggers: Vec<Trigger>) -> Self {
        // A word trigger that is one token is rejected unless preserved; a preserved token is
        // also printed into the streamed content. Prefer Trigger::Token for single tokens.
        let preserved_tokens = triggers
            .iter()
            .filter_map(|t| {
                if let Trigger::Word(w) = t {
                    Some(w.clone())
                } else {
                    None
                }
            })
            .collect();
        Constraint {
            gbnf: gbnf.into(),
            triggers,
            preserved_tokens,
        }
    }

    pub fn gbnf(&self) -> &str {
        &self.gbnf
    }

    pub fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Token {
        id: TokenId,
        text: String,
    },
    Word(String),
    /// Regex matched anywhere in the output; the grammar starts at its first group.
    Pattern(String),
    /// Regex that must match the whole output so far.
    PatternFull(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    pub prompt: Vec<TokenId>,
    pub sampler: SamplerSpec,
    pub n_predict: u32,
    /// Pinned server slot, so the session keeps its KV cache.
    pub slot: Option<u32>,
    pub constraint: Option<Constraint>,
    /// Tokens the model must never produce (logit bias −inf).
    pub banned: Vec<TokenId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stop {
    Eog,
    /// `n_predict` reached.
    Limit,
    /// The slot's context is full: the generation was cut, not finished.
    ContextFull,
    Word,
    Other(String),
}

/// Server-side counters of one generation (llama-server `timings`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[non_exhaustive]
pub struct Usage {
    pub prompt_n: Option<u32>,
    /// Prompt tokens taken from the KV cache instead of being re-processed.
    pub cache_n: Option<u32>,
    pub predicted_n: Option<u32>,
    pub prompt_ms: Option<f64>,
    pub predicted_ms: Option<f64>,
}

/// How far the server got with a prompt, as upstream llama-server says while it reads one
/// when asked (`return_progress`, since b6399): at the start, then after each batch.
/// ik_llama.cpp does not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptProgress {
    /// The prompt's tokens in all.
    pub total: u32,
    /// Of them, those the server took from its KV cache.
    pub cache: u32,
    /// Of them, those in the slot so far, the cached ones included: it starts at `cache`,
    /// so the share read is `(processed - cache) / (total - cache)`.
    pub processed: u32,
    /// Milliseconds the server spent on it so far.
    pub time_ms: Option<u64>,
}

impl PromptProgress {
    /// The `prompt_progress` of a stream payload, when it has all its counts.
    pub(crate) fn of(v: &Value) -> Option<Self> {
        let p = v.get("prompt_progress")?;
        let n = |k: &str| p[k].as_u64().and_then(|n| u32::try_from(n).ok());
        Some(Self {
            total: n("total")?,
            cache: n("cache")?,
            processed: n("processed")?,
            time_ms: p["time_ms"].as_u64(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TokenEvent {
    /// Newly sampled ids and the text the server released with them. Text may lag ids
    /// while UTF-8 is incomplete; on class-A servers the ids are complete (the stop
    /// token's id included), on class-B servers some may be missing.
    Tokens { ids: Vec<TokenId>, text: String },
    /// How far the server read the prompt, before the generation's first id.
    Progress(PromptProgress),
    /// End of generation. Always the last event of a successful stream.
    Done { stop: Stop, usage: Usage },
}

pub trait TokenEngine: Send + Sync {
    fn caps(&self) -> &EngineCaps;

    fn tokenize(
        &self,
        text: &str,
        parse_special: bool,
    ) -> impl Future<Output = Result<Vec<TokenId>, EngineError>> + Send;

    /// Streams the generation. The stream ends with `Done` or with an error, never
    /// silently. Dropping the stream cancels the generation.
    fn complete(
        &self,
        req: CompletionRequest,
    ) -> impl Future<
        Output = Result<BoxStream<'static, Result<TokenEvent, EngineError>>, EngineError>,
    > + Send;
}

/// [`TokenEngine`] over llama-server's raw `/completion` with token-array prompts.
#[derive(Debug, Clone)]
pub struct RawEngine {
    server: LlamaServer,
    caps: EngineCaps,
}

impl RawEngine {
    pub fn new(server: LlamaServer, caps: EngineCaps) -> Self {
        Self { server, caps }
    }
}

impl TokenEngine for RawEngine {
    fn caps(&self) -> &EngineCaps {
        &self.caps
    }

    async fn tokenize(&self, text: &str, parse_special: bool) -> Result<Vec<TokenId>, EngineError> {
        self.server.tokenize(text, parse_special).await
    }

    async fn complete(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<TokenEvent, EngineError>>, EngineError> {
        let payloads = self
            .server
            .completion_stream(&completion_body(&req), None)
            .await?;
        Ok(terminated(payloads))
    }
}

/// Maps payloads to events and turns "the connection just ended" into an error unless a
/// final payload arrived first (/completion never sends `[DONE]`).
pub(crate) fn terminated(
    payloads: BoxStream<'static, Result<Value, EngineError>>,
) -> BoxStream<'static, Result<TokenEvent, EngineError>> {
    stream::unfold(
        (payloads, false, false),
        |(mut payloads, saw_done, finished)| async move {
            if finished {
                return None;
            }
            match payloads.next().await {
                Some(Ok(v)) => match token_events(&v) {
                    Ok(events) => {
                        let done =
                            saw_done || events.iter().any(|e| matches!(e, TokenEvent::Done { .. }));
                        Some((
                            events.into_iter().map(Ok).collect::<Vec<_>>(),
                            (payloads, done, done),
                        ))
                    }
                    Err(e) => Some((vec![Err(e)], (payloads, saw_done, true))),
                },
                Some(Err(e)) => Some((vec![Err(e)], (payloads, saw_done, true))),
                None if saw_done => None,
                None => Some((
                    vec![Err(EngineError::Protocol(
                        "stream ended without a final payload".into(),
                    ))],
                    (payloads, saw_done, true),
                )),
            }
        },
    )
    .flat_map(stream::iter)
    .boxed()
}

pub(crate) fn completion_body(req: &CompletionRequest) -> Value {
    let s = &req.sampler;
    let mut body = json!({
        "prompt": req.prompt,
        "stream": true,
        "return_tokens": true,
        "cache_prompt": true,
        "n_predict": req.n_predict,
        "temperature": s.temperature,
        "top_p": s.top_p,
        "top_k": s.top_k,
        "min_p": s.min_p,
        // every request starts from the server's command-line sampling: neutralise the rest
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
        // always sent: an empty list also clears biases inherited from the server (--ignore-eos)
        "logit_bias": req.banned.iter().map(|id| json!([id, false])).collect::<Vec<_>>(),
        // how far the prompt is read, and the server's counts with each token, for the user
        // who waits on the step; a server that has neither ignores them
        "return_progress": true,
        "timings_per_token": true,
    });
    if let Some(seed) = s.seed {
        body["seed"] = json!(seed);
    }
    if let Some(slot) = req.slot {
        body["id_slot"] = json!(slot);
    }
    if let Some(c) = &req.constraint {
        body["grammar"] = json!(c.gbnf);
        body["grammar_lazy"] = json!(true);
        // trigger types follow common_grammar_trigger_type: TOKEN, WORD, PATTERN, PATTERN_FULL
        body["grammar_triggers"] = c
            .triggers
            .iter()
            .map(|t| match t {
                Trigger::Token { id, text } => json!({ "type": 0, "value": text, "token": id }),
                Trigger::Word(w) => json!({ "type": 1, "value": w }),
                Trigger::Pattern(p) => json!({ "type": 2, "value": p }),
                Trigger::PatternFull(p) => json!({ "type": 3, "value": p }),
            })
            .collect();
        if !c.preserved_tokens.is_empty() {
            body["preserved_tokens"] = json!(c.preserved_tokens);
        }
    }
    body
}

/// Maps one `/completion` stream payload to events. The final payload may carry trailing
/// ids and the stop together; an `error` payload becomes an error.
fn token_events(v: &Value) -> Result<Vec<TokenEvent>, EngineError> {
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
    if v.get("prompt_progress").is_some() {
        // no ids of the generation come with it: upstream sends the id 0 of a token it never
        // sampled, which read as one would put a token in front of the model's output
        out.extend(PromptProgress::of(v).map(TokenEvent::Progress));
    } else {
        let ids: Vec<TokenId> = v["tokens"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_u64().and_then(|n| u32::try_from(n).ok()))
                    .collect()
            })
            .unwrap_or_default();
        let text = v["content"].as_str().unwrap_or_default().to_owned();
        if !ids.is_empty() || !text.is_empty() {
            out.push(TokenEvent::Tokens { ids, text });
        }
    }
    if v["stop"].as_bool() == Some(true) {
        let flag = |k: &str| v[k].as_bool() == Some(true);
        let stop = match v["stop_type"].as_str() {
            // upstream: `limit` plus `truncated` means the slot's context is full
            Some("limit") if flag("truncated") => Stop::ContextFull,
            Some("limit") => Stop::Limit,
            Some("eos") => Stop::Eog,
            Some("word") => Stop::Word,
            Some(other) => Stop::Other(other.to_owned()),
            // ik_llama.cpp: stopped_* booleans in its own priority order; its `truncated`
            // means a context shift or prompt cut, not a full context
            None if flag("stopped_word") => Stop::Word,
            None if flag("stopped_eos") => Stop::Eog,
            None if flag("stopped_limit") => Stop::Limit,
            None => Stop::Other("none".into()),
        };
        let t = &v["timings"];
        let n = |x: &Value| x.as_u64().and_then(|n| u32::try_from(n).ok());
        let usage = Usage {
            prompt_n: n(&t["prompt_n"]),
            // no fallback to `tokens_cached`: on ik_llama.cpp it is n_past, not the cache hit
            cache_n: n(&t["cache_n"]),
            predicted_n: n(&t["predicted_n"]).or_else(|| n(&v["tokens_predicted"])),
            prompt_ms: t["prompt_ms"].as_f64(),
            predicted_ms: t["predicted_ms"].as_f64(),
        };
        out.push(TokenEvent::Done { stop, usage });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(stop: Stop, predicted: u32) -> TokenEvent {
        TokenEvent::Done {
            stop,
            usage: Usage {
                predicted_n: Some(predicted),
                ..Usage::default()
            },
        }
    }

    #[test]
    fn upstream_final_payload_carries_ids_and_stop() {
        let v = json!({"content": "", "tokens": [200002], "stop": true, "stop_type": "eos", "timings": {"predicted_n": 72}});
        assert_eq!(
            token_events(&v).unwrap(),
            vec![
                TokenEvent::Tokens {
                    ids: vec![200002],
                    text: String::new()
                },
                done(Stop::Eog, 72)
            ]
        );
    }

    #[test]
    fn ik_final_payload_uses_booleans() {
        let v = json!({"content": "", "stop": true, "stopped_eos": false, "stopped_limit": true, "timings": {"predicted_n": 160}});
        assert_eq!(token_events(&v).unwrap(), vec![done(Stop::Limit, 160)]);
        // ik's priority: word before eos; its `truncated` is a context shift, not a full context
        let v = json!({"stop": true, "stopped_word": true, "stopped_eos": true, "timings": {"predicted_n": 5}});
        assert_eq!(token_events(&v).unwrap(), vec![done(Stop::Word, 5)]);
        let v = json!({"stop": true, "stopped_limit": true, "truncated": true, "timings": {"predicted_n": 7}});
        assert_eq!(token_events(&v).unwrap(), vec![done(Stop::Limit, 7)]);
    }

    #[test]
    fn full_context_is_not_a_normal_limit() {
        let v = json!({"stop": true, "stop_type": "limit", "truncated": true, "timings": {"predicted_n": 3}});
        assert_eq!(token_events(&v).unwrap(), vec![done(Stop::ContextFull, 3)]);
    }

    /// Upstream llama-server with `return_progress` (b6399 on), as its `/completion` stream
    /// says how far it read the prompt: a payload with the id 0 of a token it never sampled.
    #[test]
    fn a_payload_of_the_prompts_progress_has_no_ids() {
        let v = json!({"index": 0, "content": "", "tokens": [0], "stop": false, "id_slot": 0,
            "tokens_predicted": 0, "tokens_evaluated": 12400,
            "prompt_progress": {"total": 12400, "cache": 4000, "processed": 9300, "time_ms": 812}});
        assert_eq!(
            token_events(&v).unwrap(),
            vec![TokenEvent::Progress(PromptProgress {
                total: 12400,
                cache: 4000,
                processed: 9300,
                time_ms: Some(812),
            })]
        );
        // one that does not say it all is not read as ids either
        let v =
            json!({"content": "", "tokens": [0], "stop": false, "prompt_progress": {"total": 5}});
        assert_eq!(token_events(&v).unwrap(), vec![]);
        // the counts that come with each token change nothing of what it is
        let v = json!({"content": "hi", "tokens": [1, 2], "stop": false,
            "timings": {"prompt_n": 8400, "cache_n": 4000, "predicted_n": 2, "predicted_per_second": 31.5}});
        assert_eq!(
            token_events(&v).unwrap(),
            vec![TokenEvent::Tokens {
                ids: vec![1, 2],
                text: "hi".into()
            }]
        );
    }

    #[test]
    fn error_payload_is_an_error() {
        let v = json!({"error": {"code": 500, "message": "boom"}});
        assert!(matches!(
            token_events(&v),
            Err(EngineError::Status { status: 500, .. })
        ));
    }

    #[test]
    fn stream_without_final_payload_fails() {
        let payloads = stream::iter(vec![Ok(json!({"content": "hi", "tokens": [1]}))]).boxed();
        let events: Vec<_> = futures::executor::block_on(terminated(payloads).collect());
        assert!(matches!(events.last(), Some(Err(EngineError::Protocol(_)))));
    }

    #[test]
    fn body_neutralises_samplers_and_sets_triggers() {
        let req = CompletionRequest {
            prompt: vec![2, 105],
            sampler: SamplerSpec {
                temperature: 1.0,
                top_p: 0.95,
                top_k: 64,
                min_p: 0.0,
                seed: Some(1),
            },
            n_predict: 32,
            slot: Some(0),
            constraint: Some(Constraint::new(
                "root ::= <[48]> \"call:\" [a-z]+",
                vec![
                    Trigger::Token {
                        id: 48,
                        text: "<|tool_call>".into(),
                    },
                    Trigger::Word("ANSWER:".into()),
                ],
            )),
            banned: vec![105],
        };
        let b = completion_body(&req);
        assert_eq!(
            b["grammar_triggers"],
            json!([{"type": 0, "value": "<|tool_call>", "token": 48}, {"type": 1, "value": "ANSWER:"}])
        );
        assert_eq!(b["preserved_tokens"], json!(["ANSWER:"]));
        assert_eq!(b["logit_bias"], json!([[105, false]]));
        assert_eq!(b["mirostat"], json!(0));
        // the user sees how far the step got
        assert_eq!(
            (&b["return_progress"], &b["timings_per_token"]),
            (&json!(true), &json!(true))
        );
        let unbiased = CompletionRequest {
            banned: vec![],
            constraint: None,
            ..req
        };
        assert_eq!(completion_body(&unbiased)["logit_bias"], json!([]));
    }
}
