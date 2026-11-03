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
    /// The model every request names, for llama-server in router mode.
    model: Option<String>,
}

impl fmt::Debug for LlamaServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlamaServer")
            .field("base", &self.base)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// A model of llama-server in router mode, as its `/models` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterModel {
    pub id: String,
    pub aliases: Vec<String>,
    /// `loaded`, `loading`, `unloaded`, `sleeping`, ...
    pub status: String,
    /// Its last start failed.
    pub failed: bool,
    /// The GGUF file the router runs it from, when its arguments say (once it was loaded).
    pub path: Option<String>,
}

impl RouterModel {
    fn from_json(v: &Value) -> Option<Self> {
        let strings = |v: &Value| -> Vec<String> {
            v.as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let args = strings(&v["status"]["args"]);
        let path = args
            .iter()
            .position(|a| a == "-m" || a == "--model")
            .and_then(|i| args.get(i + 1))
            .cloned();
        Some(Self {
            id: v["id"].as_str()?.to_owned(),
            aliases: strings(&v["aliases"]),
            status: v["status"]["value"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned(),
            failed: v["status"]["failed"].as_bool() == Some(true),
            path,
        })
    }

    /// The names it goes by, lowercase: its id, aliases and file name.
    pub fn names(&self) -> Vec<String> {
        let file = self.path.as_deref().and_then(|p| {
            std::path::Path::new(p)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        });
        std::iter::once(&self.id)
            .chain(&self.aliases)
            .chain(file.as_ref())
            .map(|n| n.to_lowercase())
            .collect()
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

/// The error with its causes: reqwest's own message names only the URL, and the cause (a
/// refused connection, one closed under the request) is what tells failures apart.
pub(crate) fn http_err(e: reqwest::Error) -> EngineError {
    let mut text = e.to_string();
    let mut cause = std::error::Error::source(&e);
    while let Some(c) = cause {
        let c_text = c.to_string();
        if !text.contains(&c_text) {
            text.push_str(": ");
            text.push_str(&c_text);
        }
        cause = c.source();
    }
    EngineError::Http(text)
}

/// A chat stream that ended without `[DONE]`: cut off, so nothing in it may run.
pub(crate) fn no_done() -> EngineError {
    EngineError::Protocol("stream ended without [DONE]".into())
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
        // otherwise route even loopback traffic through it. No idle connections kept:
        // llama-server closes the connection after every stream although its headers offer
        // keep-alive (its stream callback ends with `false`, which cpp-httplib takes for a
        // failed write), so a kept one can be gone when the next request goes out. A new
        // connection to such a server costs nothing next to a generation.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(0)
            .no_proxy();
        if let Some(idle) = idle {
            builder = builder.read_timeout(idle);
        }
        let http = builder.build().map_err(http_err)?;
        Ok(Self {
            base: base_url.trim_end_matches('/').to_owned(),
            http,
            api_key,
            model: None,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// This server's `model` behind its router: every request names it, a GET with
    /// `?model=` and a POST in its body, and the router passes it to that model's server.
    /// Only `/health` stays the router's own.
    pub fn for_model(&self, model: &str) -> Self {
        Self {
            model: Some(model.to_owned()),
            ..self.clone()
        }
    }

    /// The model every request names (router mode).
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base, path);
        let rb = match (&self.model, reqwest::Url::parse(&url)) {
            (Some(model), Ok(mut named)) if method == Method::GET && path != "/health" => {
                named.query_pairs_mut().append_pair("model", model);
                self.http.request(method, named)
            }
            _ => self.http.request(method, url),
        };
        match &self.api_key {
            Some(key) => rb.bearer_auth(key),
            None => rb,
        }
    }

    /// `body` naming this server's model, if it has one.
    fn named<'b>(&self, body: &'b Value) -> std::borrow::Cow<'b, Value> {
        match (&self.model, body) {
            (Some(model), Value::Object(fields)) if !fields.contains_key("model") => {
                let mut body = body.clone();
                body["model"] = json!(model);
                std::borrow::Cow::Owned(body)
            }
            _ => std::borrow::Cow::Borrowed(body),
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
            .json(&self.named(body))
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

    /// The models of llama-server in router mode.
    pub async fn models(&self) -> Result<Vec<RouterModel>, EngineError> {
        match self.get("/models").await? {
            (200, v) => Ok(v["data"]
                .as_array()
                .map(|a| a.iter().filter_map(RouterModel::from_json).collect())
                .unwrap_or_default()),
            (status, v) => Err(status_err(status, &v)),
        }
    }

    /// Asks llama-server in router mode to start `model`; [`models`](Self::models) says
    /// when it runs.
    pub async fn load(&self, model: &str) -> Result<(), EngineError> {
        match self
            .post("/models/load", &json!({ "model": model }), META_TIMEOUT)
            .await?
        {
            (200, _) => Ok(()),
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

    /// `/tokenize` of a list of texts and ids, read as llama-server reads a mixed prompt: each
    /// text on its own, here without special parsing, and each id as it is.
    pub async fn tokenize_mixed(&self, content: &[Value]) -> Result<Vec<u32>, EngineError> {
        let body = json!({ "content": content, "parse_special": false, "add_special": false });
        match self.post("/tokenize", &body, TOKENIZE_TIMEOUT).await? {
            (200, v) => v["tokens"]
                .as_array()
                .ok_or_else(|| EngineError::Protocol("/tokenize: no tokens array".into()))?
                .iter()
                .map(|t| {
                    t.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| EngineError::Protocol(format!("/tokenize: bad token {t}")))
                })
                .collect(),
            (status, v) => Err(status_err(status, &v)),
        }
    }

    /// The bytes of each of `ids` in the server's vocabulary: `/tokenize` of the ids with their
    /// pieces, where a piece that is no UTF-8 on its own comes as its bytes.
    pub async fn pieces(&self, ids: &[u32]) -> Result<Vec<Vec<u8>>, EngineError> {
        let body = json!({ "content": ids, "with_pieces": true, "add_special": false });
        let tokens = match self.post("/tokenize", &body, TOKENIZE_TIMEOUT).await? {
            (200, mut v) => match v["tokens"].take() {
                Value::Array(a) => a,
                _ => return Err(EngineError::Protocol("/tokenize: no tokens array".into())),
            },
            (status, v) => return Err(status_err(status, &v)),
        };
        if tokens.len() != ids.len() {
            return Err(EngineError::Protocol(format!(
                "/tokenize: {} pieces for {} ids",
                tokens.len(),
                ids.len()
            )));
        }
        let bad = |t: &Value| EngineError::Protocol(format!("/tokenize: bad piece {t}"));
        tokens
            .iter()
            .zip(ids)
            .map(|(t, id)| {
                if t["id"].as_u64() != Some(u64::from(*id)) {
                    return Err(bad(t));
                }
                match &t["piece"] {
                    Value::String(s) => Ok(s.as_bytes().to_vec()),
                    Value::Array(bytes) => bytes
                        .iter()
                        .map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                        .collect::<Option<Vec<u8>>>()
                        .ok_or_else(|| bad(t)),
                    _ => Err(bad(t)),
                }
            })
            .collect()
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
                    None => Some((Err(no_done()), (events, true))),
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
        let mut rb = self.request(Method::POST, path).json(&self.named(body));
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

    /// Reads one request, headers and a Content-Length body: its request line and body;
    /// `None` at the end of the connection.
    async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
        use tokio::io::AsyncReadExt;
        let (mut buf, mut chunk) = (Vec::new(), [0u8; 4096]);
        loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                let len = head
                    .to_ascii_lowercase()
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(str::to_owned))
                    .map_or(0, |v| v.trim().parse().unwrap());
                if buf.len() >= end + 4 + len {
                    let line = head.lines().next().unwrap_or_default().to_owned();
                    return Some((line, buf[end + 4..end + 4 + len].to_vec()));
                }
            }
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    /// llama-server in router mode reads the model from a GET's `?model=` and from a POST's
    /// body; its `/health` is its own.
    #[tokio::test]
    async fn a_server_behind_a_router_names_its_model_in_every_request() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let Some((line, body)) = read_request(&mut sock).await else {
                    continue;
                };
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                log.lock().unwrap().push((line, body));
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let server = LlamaServer::new(&url, None)
            .unwrap()
            .for_model("gpt oss/20b");
        assert_eq!(server.model(), Some("gpt oss/20b"));
        let limit = Duration::from_secs(5);
        server.get("/health").await.unwrap();
        server.get("/props").await.unwrap();
        server
            .post("/tokenize", &json!({"content": "x"}), limit)
            .await
            .unwrap();
        // a body that names a model keeps it
        let other = json!({"content": "x", "model": "other"});
        server.post("/tokenize", &other, limit).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "GET /health HTTP/1.1");
        assert_eq!(seen[1].0, "GET /props?model=gpt+oss%2F20b HTTP/1.1");
        assert_eq!(seen[2].1, json!({"content": "x", "model": "gpt oss/20b"}));
        assert_eq!(seen[3].1, other);
    }

    #[test]
    fn a_router_lists_its_models_with_the_files_they_run_from() {
        let m = RouterModel::from_json(&json!({
            "id": "oss",
            "aliases": ["GPT-OSS-20B"],
            "status": {"value": "loaded", "args": ["llama-server", "-m", "/m/gpt-oss-20b-F16.gguf", "--port", "1"]}
        }))
        .unwrap();
        assert_eq!(
            (m.status.as_str(), m.path.as_deref()),
            ("loaded", Some("/m/gpt-oss-20b-F16.gguf"))
        );
        assert_eq!(m.names(), ["oss", "gpt-oss-20b", "gpt-oss-20b-f16.gguf"]);
        // before its first load a model has no arguments
        let m = RouterModel::from_json(
            &json!({"id": "q", "status": {"value": "unloaded", "args": []}}),
        )
        .unwrap();
        assert_eq!((m.path.as_deref(), m.names()), (None, vec!["q".to_owned()]));
    }

    /// llama-server closes the connection after every stream although its headers offer
    /// keep-alive, so a pooled connection can be gone when the next request goes out (2 of
    /// 525 requests in the T2 night of 2026-11-02, each right after a quick tool). This
    /// server answers the way llama-server does, then keeps the socket open and drops a
    /// second request on it unanswered: a reused connection fails every time, not now and
    /// then.
    #[tokio::test]
    async fn every_stream_gets_a_connection_of_its_own() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    if read_request(&mut sock).await.is_none() {
                        return;
                    }
                    let chunk = json!({"choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": "stop"}]});
                    let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nKeep-Alive: timeout=5, max=100\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n",
                        body.len()
                    );
                    sock.write_all(resp.as_bytes()).await.unwrap();
                    read_request(&mut sock).await;
                });
            }
        });
        let server = LlamaServer::new(&url, None).unwrap();
        for i in 0..3 {
            let stream = server
                .chat_stream(&json!({"stream": true}), None)
                .await
                .unwrap_or_else(|e| panic!("request {i}: {e}"));
            let events: Vec<_> = stream.collect().await;
            assert!(
                events.len() == 1 && events[0].is_ok(),
                "request {i}: {events:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_transport_error_names_its_cause() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let e = LlamaServer::new(&url, None)
            .unwrap()
            .get("/props")
            .await
            .unwrap_err();
        let text = e.to_string();
        assert!(
            text.contains("error sending request for url") && text.contains("refused"),
            "{text}"
        );
    }
}
