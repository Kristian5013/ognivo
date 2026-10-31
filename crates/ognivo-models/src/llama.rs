//! HTTP client for llama-server and ik_llama.cpp.
//!
//! ognivo never launches servers (owner's decision 4); it attaches to whatever runs at a
//! URL and learns what that server can do (see [`crate::doctor`]).

use std::fmt;
use std::time::Duration;

use futures::stream::{self, BoxStream, StreamExt};
use reqwest::Method;
use serde_json::{Value, json};

use crate::engine::EngineError;
use crate::sse::{self, SseEvent};

/// Quick metadata calls (/health, /props, /slots, /models).
const META_TIMEOUT: Duration = Duration::from_secs(15);
/// /tokenize and /detokenize; long prompts take a moment, nothing takes minutes.
const TOKENIZE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct LlamaServer {
    base: String,
    http: reqwest::Client,
    api_key: Option<String>,
}

impl fmt::Debug for LlamaServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlamaServer")
            .field("base", &self.base)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// The parts of `/props` ognivo relies on. Every field is optional: servers differ.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Props {
    pub build_info: Option<String>,
    /// `"router"` for llama-server in router mode.
    pub role: Option<String>,
    pub model_path: Option<String>,
    pub total_slots: Option<u32>,
    pub n_ctx: Option<u32>,
    pub chat_template: Option<String>,
    pub default_sampling: DefaultSampling,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DefaultSampling {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<f64>,
    pub min_p: Option<f64>,
}

impl Props {
    pub fn from_json(v: &Value) -> Self {
        let settings = &v["default_generation_settings"];
        // upstream nests sampling under `params`; ik_llama.cpp keeps it flat
        let params = if settings["params"].is_object() {
            &settings["params"]
        } else {
            settings
        };
        let u32_of = |x: &Value| x.as_u64().and_then(|n| u32::try_from(n).ok());
        Props {
            build_info: v["build_info"].as_str().map(str::to_owned),
            role: v["role"].as_str().map(str::to_owned),
            model_path: v["model_path"].as_str().map(str::to_owned),
            total_slots: u32_of(&v["total_slots"]),
            n_ctx: u32_of(&settings["n_ctx"]).filter(|&n| n > 0),
            chat_template: v["chat_template"].as_str().map(str::to_owned),
            default_sampling: DefaultSampling {
                temperature: params["temperature"].as_f64(),
                top_p: params["top_p"].as_f64(),
                top_k: params["top_k"].as_f64(),
                min_p: params["min_p"].as_f64(),
            },
        }
    }
}

fn http_err(e: reqwest::Error) -> EngineError {
    EngineError::Http(e.to_string())
}

fn status_err(status: u16, body: &Value) -> EngineError {
    EngineError::Status {
        status,
        body: body.to_string(),
    }
}

impl LlamaServer {
    /// A client without an idle limit on streams: cancelling (dropping the stream) is the
    /// main way to stop a generation, and ik_llama.cpp sends nothing until its first token,
    /// so a long prefill would trip any fixed limit.
    pub fn new(base_url: &str, api_key: Option<String>) -> Result<Self, EngineError> {
        Self::build(base_url, api_key, None)
    }

    /// A client whose streams fail after `idle` without any bytes.
    pub fn with_idle_timeout(
        base_url: &str,
        api_key: Option<String>,
        idle: Duration,
    ) -> Result<Self, EngineError> {
        Self::build(base_url, api_key, Some(idle))
    }

    fn build(
        base_url: &str,
        api_key: Option<String>,
        idle: Option<Duration>,
    ) -> Result<Self, EngineError> {
        // No system proxy: servers are local or on the LAN, and HTTP(S)_PROXY would
        // otherwise route even loopback traffic through it.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .no_proxy();
        if let Some(idle) = idle {
            builder = builder.read_timeout(idle);
        }
        let http = builder.build().map_err(http_err)?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_owned(),
            http,
            api_key,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, format!("{}{}", self.base, path));
        match &self.api_key {
            Some(key) => rb.bearer_auth(key),
            None => rb,
        }
    }

    /// GET returning the status and the body parsed as JSON (`Null` when it is not JSON).
    pub async fn get(&self, path: &str) -> Result<(u16, Value), EngineError> {
        let resp = self
            .request(Method::GET, path)
            .timeout(META_TIMEOUT)
            .send()
            .await
            .map_err(http_err)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(http_err)?;
        Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
    }

    /// POST with a total timeout; the body comes back as JSON, or as a JSON string.
    pub async fn post(
        &self,
        path: &str,
        body: &Value,
        timeout: Duration,
    ) -> Result<(u16, Value), EngineError> {
        let resp = self
            .request(Method::POST, path)
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(http_err)?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(http_err)?;
        Ok((
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        ))
    }

    pub async fn props(&self) -> Result<Props, EngineError> {
        match self.get("/props").await? {
            (200, v) if v.is_object() => Ok(Props::from_json(&v)),
            (status, v) => Err(status_err(status, &v)),
        }
    }

    /// Whether any slot is busy right now (`None` when /slots is disabled).
    pub async fn slots_busy(&self) -> Option<bool> {
        match self.get("/slots").await {
            Ok((200, Value::Array(slots))) => Some(
                slots
                    .iter()
                    .any(|s| s["is_processing"].as_bool() == Some(true)),
            ),
            _ => None,
        }
    }

    pub async fn tokenize(&self, text: &str, parse_special: bool) -> Result<Vec<u32>, EngineError> {
        let body = json!({ "content": text, "parse_special": parse_special, "add_special": false });
        match self.post("/tokenize", &body, TOKENIZE_TIMEOUT).await? {
            (200, v) => v["tokens"]
                .as_array()
                .ok_or_else(|| EngineError::Protocol("/tokenize: no tokens array".into()))?
                .iter()
                .map(|t| {
                    // plain ids, or {id, piece} objects when pieces are requested
                    t.as_u64()
                        .or_else(|| t["id"].as_u64())
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| EngineError::Protocol(format!("/tokenize: bad token {t}")))
                })
                .collect(),
            (status, v) => Err(status_err(status, &v)),
        }
    }

    pub async fn detokenize(&self, ids: &[u32]) -> Result<String, EngineError> {
        match self
            .post("/detokenize", &json!({ "tokens": ids }), TOKENIZE_TIMEOUT)
            .await?
        {
            (200, v) => Ok(v["content"].as_str().unwrap_or_default().to_owned()),
            (status, v) => Err(status_err(status, &v)),
        }
    }

    /// POST `/completion` with `stream: true`; yields the JSON payload of every SSE event.
    /// `total` bounds the whole request (headers arrive only once a slot takes the task);
    /// without it only the idle limit between chunks applies. `/completion` sends no
    /// `[DONE]`: whether the generation finished is read from its final payload
    /// ([`crate::engine`]).
    pub async fn completion_stream(
        &self,
        body: &Value,
        total: Option<Duration>,
    ) -> Result<BoxStream<'static, Result<Value, EngineError>>, EngineError> {
        let events = self.stream_post("/completion", body, total).await?;
        Ok(events
            .filter_map(|e| async move {
                match e {
                    Ok(SseEvent::Data(v)) => Some(Ok(v)),
                    Ok(SseEvent::Done) => None,
                    Err(e) => Some(Err(e)),
                }
            })
            .boxed())
    }

    /// POST `/v1/chat/completions` with `stream: true`; yields the JSON payload of every SSE
    /// chunk (the Generic path, docs/architecture.md §5.2). The server sends `[DONE]` only
    /// when the generation ends normally, so a stream that ends without it fails here: a
    /// tool call from a cut-off stream must never look complete.
    pub async fn chat_stream(
        &self,
        body: &Value,
        total: Option<Duration>,
    ) -> Result<BoxStream<'static, Result<Value, EngineError>>, EngineError> {
        let events = self
            .stream_post("/v1/chat/completions", body, total)
            .await?;
        Ok(
            stream::unfold((events, false), |(mut events, finished)| async move {
                if finished {
                    return None;
                }
                match events.next().await {
                    Some(Ok(SseEvent::Data(v))) => Some((Ok(v), (events, false))),
                    Some(Ok(SseEvent::Done)) => None,
                    Some(Err(e)) => Some((Err(e), (events, true))),
                    None => {
                        let err = EngineError::Protocol("stream ended without [DONE]".into());
                        Some((Err(err), (events, true)))
                    }
                }
            })
            .boxed(),
        )
    }

    /// POST a streaming endpoint. A status other than 200 is an error carrying the body.
    async fn stream_post(
        &self,
        path: &str,
        body: &Value,
        total: Option<Duration>,
    ) -> Result<BoxStream<'static, Result<SseEvent, EngineError>>, EngineError> {
        let mut rb = self.request(Method::POST, path).json(body);
        if let Some(t) = total {
            rb = rb.timeout(t);
        }
        let resp = rb.send().await.map_err(http_err)?;
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(EngineError::Status { status, body });
        }
        Ok(sse::events(resp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_key() {
        let s = LlamaServer::new("http://127.0.0.1:8090/", Some("secret".into())).unwrap();
        let d = format!("{s:?}");
        assert!(!d.contains("secret") && d.contains("redacted"), "{d}");
        assert_eq!(s.base_url(), "http://127.0.0.1:8090");
    }

    #[test]
    fn props_upstream_and_ik_layouts() {
        let upstream = json!({
            "build_info": "b11924-12cc59ba4",
            "model_path": "/m/gpt-oss-20b-F16.gguf",
            "total_slots": 1,
            "chat_template": "{{ x }}",
            "default_generation_settings": {"n_ctx": 16384, "params": {"temperature": 0.8, "top_k": 40, "top_p": 0.95, "min_p": 0.05}}
        });
        let p = Props::from_json(&upstream);
        assert_eq!(p.build_info.as_deref(), Some("b11924-12cc59ba4"));
        assert_eq!(p.n_ctx, Some(16384));
        assert_eq!(p.default_sampling.top_k, Some(40.0));

        let ik = json!({"default_generation_settings": {"n_ctx": 16384, "temperature": 0.8, "top_k": 40}});
        assert_eq!(Props::from_json(&ik).default_sampling.top_k, Some(40.0));

        let router = json!({"role": "router", "build_info": "b11924-12cc59ba4", "default_generation_settings": {"n_ctx": 0, "params": {}}});
        let r = Props::from_json(&router);
        assert_eq!((r.role.as_deref(), r.n_ctx), (Some("router"), None));
    }
}
