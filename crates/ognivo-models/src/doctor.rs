//! Checks behind `ognivo doctor` (docs/architecture.md §4; the M0 probes in
//! scripts/probe/ are the reference these checks were ported from).

use std::path::Path;
use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;

use crate::Family;
use crate::caps::{self, ServerClass, ServerKind, StreamIds};
use crate::engine::{self, CompletionRequest, EngineError, SamplerSpec, TokenEvent};
use crate::llama::{LlamaServer, Props};
use crate::profile::{ModelProfile, TemplateVerdict};
use crate::template::sha256_hex;

/// Upper bound for the stream probe, queueing included.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Default)]
pub struct DoctorOptions {
    /// Run a short generation to see whether every token id is streamed (uses the GPU).
    pub probe_stream: bool,
    /// The model to check behind llama-server in router mode.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub level: Level,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecialTokenCheck {
    pub text: String,
    pub id: Option<u32>,
    /// The server's tokenizer turns this text into the control token even with
    /// `parse_special=false`: content containing it must be split before tokenizing.
    pub recognised_without_parse_special: bool,
    /// The text as the Generic path's guard sends it still yields the control token with
    /// `parse_special=true` (§5.3); `None` when the server did not tokenize it.
    pub survives_guard: Option<bool>,
}

/// A model of llama-server in router mode, and the profile its names point to.
#[derive(Debug, Clone, Serialize)]
pub struct RoutedModel {
    pub id: String,
    pub status: String,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub url: String,
    pub reachable: bool,
    pub kind: Option<ServerKind>,
    /// The models of a router, when the server is one.
    pub router_models: Vec<RoutedModel>,
    /// The router's model these checks are of.
    pub model: Option<String>,
    pub build_info: Option<String>,
    pub model_path: Option<String>,
    pub n_ctx: Option<u32>,
    pub total_slots: Option<u32>,
    pub chat_template_sha256: Option<String>,
    pub family: Option<Family>,
    pub profile: Option<String>,
    pub special_tokens: Vec<SpecialTokenCheck>,
    pub stream_ids: StreamIds,
    pub class: Option<ServerClass>,
    pub recommended_flags: Option<String>,
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    pub fn new(url: &str) -> Self {
        DoctorReport {
            url: url.to_owned(),
            reachable: false,
            kind: None,
            router_models: Vec::new(),
            model: None,
            build_info: None,
            model_path: None,
            n_ctx: None,
            total_slots: None,
            chat_template_sha256: None,
            family: None,
            profile: None,
            special_tokens: Vec::new(),
            stream_ids: StreamIds::NotProbed,
            class: None,
            recommended_flags: None,
            findings: Vec::new(),
        }
    }

    pub fn note(&mut self, level: Level, message: impl Into<String>) {
        self.findings.push(Finding {
            level,
            message: message.into(),
        });
    }

    pub fn has_errors(&self) -> bool {
        !self.reachable || self.findings.iter().any(|f| f.level == Level::Error)
    }
}

fn auth_failed(status: u16) -> String {
    format!("authentication failed (HTTP {status}): pass --api-key or set OGNIVO_API_KEY")
}

pub async fn diagnose(
    url: &str,
    api_key: Option<String>,
    profiles: &[ModelProfile],
    opts: DoctorOptions,
) -> DoctorReport {
    let mut report = DoctorReport::new(url);
    let mut server = match LlamaServer::new(url, api_key) {
        Ok(s) => s,
        Err(e) => {
            report.note(Level::Error, format!("cannot create HTTP client: {e}"));
            return report;
        }
    };

    match server.get("/health").await {
        Ok((200, _)) => report.reachable = true,
        Ok((status @ (401 | 403), _)) => {
            report.note(Level::Error, auth_failed(status));
            return report;
        }
        Ok((404, _)) => {
            report.note(
                Level::Error,
                format!("no llama-server API at {url} (/health returned 404); use the server root URL, without /v1"),
            );
            return report;
        }
        Ok((503, _)) => {
            report.note(
                Level::Error,
                "server is up but still loading the model (/health returned 503)",
            );
            return report;
        }
        Ok((status, body)) => {
            report.note(Level::Error, format!("/health returned {status}: {body}"));
            return report;
        }
        Err(e) => {
            report.note(Level::Error, format!("no server at {url}: {e}"));
            return report;
        }
    }

    // ik_llama.cpp is inferred only from a successful /props without build_info
    let mut props = match server.get("/props").await {
        Ok((200, v)) if v.is_object() => {
            let p = Props::from_json(&v);
            report.kind = Some(ServerKind::from_props(
                p.build_info.as_deref(),
                p.role.as_deref(),
            ));
            p
        }
        Ok((status @ (401 | 403), _)) => {
            report.note(Level::Error, auth_failed(status));
            return report;
        }
        Ok((404, _)) if url.trim_end_matches('/').ends_with("/v1") => {
            // llama-server answers /v1/health too, so a /v1 base URL gets this far
            report.note(
                Level::Error,
                "use the server root URL, without /v1 (/v1/props returned 404)",
            );
            return report;
        }
        // without /props nothing about the server can be trusted: stop, guess nothing
        Ok((status, _)) => {
            report.note(
                Level::Error,
                format!("/props returned {status}: not a llama-server API (use the server root URL, without /v1)"),
            );
            return report;
        }
        Err(e) => {
            report.note(Level::Error, format!("/props failed: {e}"));
            return report;
        }
    };
    let kind = report.kind.clone().unwrap_or(ServerKind::Unknown);
    report.build_info = props.build_info.clone();

    if matches!(kind, ServerKind::Router { .. }) {
        match server.models().await {
            Ok(models) => {
                report.router_models = models
                    .iter()
                    .map(|m| RoutedModel {
                        id: m.id.clone(),
                        status: m.status.clone(),
                        profile: profile_by_name(&m.names(), profiles).map(|p| p.id.clone()),
                    })
                    .collect();
            }
            Err(e) => report.note(Level::Error, format!("/models failed: {e}")),
        }
        let Some(model) = opts.model.clone() else {
            report.note(
                Level::Info,
                "llama-server router: every request names a model; pass --model to check one",
            );
            report.class = Some(caps::classify(&kind, StreamIds::NotProbed));
            return report;
        };
        // the model's own server behind the router, which starts it if it loads on demand
        server = server.for_model(&model);
        props = match server.props().await {
            Ok(p) => p,
            Err(e) => {
                report.note(Level::Error, format!("{model}: /props failed: {e}"));
                return report;
            }
        };
        report.model = Some(model);
    }
    report.model_path = props.model_path.clone();
    report.n_ctx = props.n_ctx;
    report.total_slots = props.total_slots;
    report.chat_template_sha256 = props.chat_template.as_deref().map(sha256_hex);

    let (profile, by_name) = match detect_profile(&server, &props, profiles).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            report.note(Level::Warn, "model family not recognised: only the Generic path (llama.cpp's own templates) is available");
            report.class = Some(caps::classify(&kind, StreamIds::NotProbed));
            return report;
        }
        Err(e) => {
            report.note(Level::Error, format!("/tokenize failed: {e}"));
            return report;
        }
    };
    report.family = Some(profile.family);
    report.profile = Some(profile.id.clone());

    check_special_tokens(&server, profile, &mut report).await;
    if by_name {
        // model-specific values only when the file name says it is this model
        report.recommended_flags = Some(repo_paths(&profile.server.recommended_flags));
        check_template(profile, &mut report);
    } else {
        report.note(
            Level::Warn,
            format!(
                "family recognised by tokenizer only: the {} profile's context budget, server flags and template \
                 are not verified for this model",
                profile.display_name
            ),
        );
    }

    if opts.probe_stream {
        if server.slots_busy().await == Some(true) {
            report.note(
                Level::Warn,
                "server is busy (a slot is processing): stream probe skipped",
            );
        } else {
            match probe_stream_ids(&server, profile).await {
                Ok(s) => report.stream_ids = s,
                Err(e) => report.note(Level::Warn, format!("stream probe failed: {e}")),
            }
        }
    }
    let class = caps::classify(&kind, report.stream_ids);
    report.class = Some(class);
    note_class(&mut report, &kind, class, opts.probe_stream);
    if by_name {
        check_context_and_sampling(&props, profile, &mut report);
    }
    report
}

/// Makes the repository-relative paths in advice absolute when this binary runs from a
/// checkout (installed builds ship the templates elsewhere: M6).
fn repo_paths(text: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match root.join("profiles").canonicalize() {
        Ok(profiles) => text.replace("profiles/", &format!("{}/", profiles.display())),
        Err(_) => text.to_owned(),
    }
}

fn note_class(
    report: &mut DoctorReport,
    kind: &ServerKind,
    class: ServerClass,
    probe_requested: bool,
) {
    let stream = report.stream_ids;
    match (class, stream) {
        (ServerClass::A, StreamIds::Complete) => {
            report.note(Level::Ok, "every sampled token id is streamed: verbatim token replay works")
        }
        (ServerClass::A, _) if probe_requested => {
            report.note(Level::Info, "known build with complete token-id streaming (the probe gave no verdict)")
        }
        (ServerClass::A, _) => {
            report.note(Level::Info, "known build with complete token-id streaming (run with --probe to verify)")
        }
        (ServerClass::B, StreamIds::Lossy { received, predicted }) => report.note(
            Level::Warn,
            format!(
                "{received} of {predicted} sampled token ids were streamed (lossy on incomplete UTF-8): ognivo will \
                 re-tokenize; for your own server build llama.cpp with PR #31244 (scripts/llama/build-pinned.sh)"
            ),
        ),
        (ServerClass::B, _) => report.note(
            Level::Warn,
            "token ids are expected to be lossy on incomplete UTF-8 (not verified: run with --probe); for your own \
             server build llama.cpp with PR #31244 (scripts/llama/build-pinned.sh)",
        ),
        (ServerClass::C, _) if *kind == ServerKind::IkLlama || stream == StreamIds::Missing => report.note(
            Level::Warn,
            "server streams no token ids: Native path only in degraded mode, Generic is the full path",
        ),
        (ServerClass::C, _) => {
            report.note(Level::Warn, "server type unknown: assuming no token ids until a probe shows otherwise")
        }
    }
    if stream == StreamIds::Inconclusive {
        report.note(
            Level::Info,
            "stream probe inconclusive: the reply never needed a multi-token character",
        );
    }
}

/// The profile for the model a server runs, as `doctor` finds it; `None` when no profile
/// matches. The bool says whether the GGUF file name confirmed the choice.
pub async fn identify<'p>(
    server: &LlamaServer,
    profiles: &'p [ModelProfile],
) -> Result<Option<(&'p ModelProfile, bool)>, EngineError> {
    let props = server.props().await?;
    detect_profile(server, &props, profiles).await
}

/// The profile with the longest name hint in one of `names` (lowercase), as a model of a
/// router is matched before anything of it is loaded.
pub fn profile_by_name<'p>(
    names: &[String],
    profiles: &'p [ModelProfile],
) -> Option<&'p ModelProfile> {
    profiles
        .iter()
        .filter_map(|p| {
            let best = p
                .detect
                .name_hints
                .iter()
                .filter(|h| names.iter().any(|n| n.contains(h.as_str())))
                .map(|h| h.len())
                .max()?;
            Some((best, p))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

/// Picks the profile whose probe text is a single token with the expected id. Within a
/// family the longest name hint found in the GGUF file name wins; the bool says whether a
/// hint matched at all.
async fn detect_profile<'p>(
    server: &LlamaServer,
    props: &Props,
    profiles: &'p [ModelProfile],
) -> Result<Option<(&'p ModelProfile, bool)>, EngineError> {
    let mut matches = Vec::new();
    let mut last_err = None;
    let mut answered = false;
    for p in profiles {
        match server.tokenize(&p.detect.probe.text, true).await {
            Ok(ids) => {
                answered = true;
                if ids == [p.detect.probe.id] {
                    matches.push(p);
                }
            }
            Err(e) => last_err = Some(e),
        }
    }
    if !answered && let Some(e) = last_err {
        return Err(e);
    }
    let file = props
        .model_path
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_lowercase();
    let by_name = matches
        .iter()
        .filter_map(|p| {
            let best = p
                .detect
                .name_hints
                .iter()
                .filter(|h| file.contains(h.as_str()))
                .map(|h| h.len())
                .max()?;
            Some((best, *p))
        })
        .max_by_key(|(len, _)| *len);
    Ok(match by_name {
        Some((_, p)) => Some((p, true)),
        None => matches.first().map(|p| (*p, false)),
    })
}

async fn check_special_tokens(
    server: &LlamaServer,
    profile: &ModelProfile,
    report: &mut DoctorReport,
) {
    let guard = crate::guard::Guard::new(&profile.tokens);
    for text in profile.tokens.guarded() {
        let special = server.tokenize(text, true).await.ok();
        let plain = server.tokenize(text, false).await.ok();
        let id = special
            .as_deref()
            .and_then(|s| if let [one] = s { Some(*one) } else { None });
        let recognised = id.is_some() && plain.as_deref() == special.as_deref();
        let survives_guard = match id {
            Some(id) => server
                .tokenize(&guard.escape(text), true)
                .await
                .ok()
                .map(|ids| ids.contains(&id)),
            None => None,
        };
        report.special_tokens.push(SpecialTokenCheck {
            text: text.to_owned(),
            id,
            recognised_without_parse_special: recognised,
            survives_guard,
        });
    }
    let survivors: Vec<&str> = report
        .special_tokens
        .iter()
        .filter(|c| c.survives_guard == Some(true))
        .map(|c| c.text.as_str())
        .collect();
    if !survivors.is_empty() {
        report.note(
            Level::Error,
            format!(
                "the special-token guard does not break {}: file text can still forge control tokens",
                survivors.join(" ")
            ),
        );
    }
    let recognised: Vec<&str> = report
        .special_tokens
        .iter()
        .filter(|c| c.recognised_without_parse_special)
        .map(|c| c.text.as_str())
        .collect();
    if !recognised.is_empty() {
        report.note(
            Level::Info,
            format!(
                "{} special tokens are recognised even with parse_special=false ({}): ognivo splits them in content \
                 before tokenizing",
                recognised.len(),
                recognised.join(" ")
            ),
        );
    }
}

/// Generates a reply full of characters that most vocabularies split into several tokens
/// and compares the streamed ids with the server's own count of sampled tokens.
async fn probe_stream_ids(
    server: &LlamaServer,
    profile: &ModelProfile,
) -> Result<StreamIds, EngineError> {
    const LINE: &str = "🚀 ⚠️ └── ├── │ ✅ 🔥 привет 日本語 𓂀 𝔘𝔫𝔦𝔠𝔬𝔡𝔢 🜁";
    let text =
        format!("Repeat the following line exactly three times and output nothing else:\n{LINE}");
    let prompt = server
        .tokenize(&probe_prompt(profile.family, &text), true)
        .await?;
    let req = CompletionRequest {
        prompt,
        sampler: SamplerSpec {
            temperature: 0.0,
            top_p: 1.0,
            top_k: 1,
            min_p: 0.0,
            seed: Some(1),
        },
        n_predict: 200,
        slot: None,
        constraint: None,
        banned: Vec::new(),
    };
    let payloads = server
        .completion_stream(&engine::completion_body(&req), Some(PROBE_TIMEOUT))
        .await?;
    let mut events = engine::terminated(payloads);
    let (mut received, mut predicted, mut output) = (0usize, None, String::new());
    while let Some(ev) = events.next().await {
        match ev? {
            TokenEvent::Tokens { ids, text } => {
                received += ids.len();
                output.push_str(&text);
            }
            TokenEvent::Done { usage, .. } => predicted = usage.predicted_n.map(|n| n as usize),
        }
    }
    let Some(predicted) = predicted else {
        return Err(EngineError::Protocol(
            "final payload without a sampled-token count".into(),
        ));
    };
    // only characters outside the BMP reliably span several tokens
    let exercised = output.chars().any(|c| u32::from(c) >= 0x10000);
    Ok(match received {
        0 if predicted > 0 => StreamIds::Missing,
        r if r > predicted || !exercised => StreamIds::Inconclusive,
        r if r == predicted => StreamIds::Complete,
        r => StreamIds::Lossy {
            received: r,
            predicted,
        },
    })
}

/// Minimal single-turn prompts for the stream probe only; the real renderers are the
/// family codecs (M2–M3).
fn probe_prompt(family: Family, text: &str) -> String {
    match family {
        Family::Harmony => format!(
            "<|start|>system<|message|>You are ChatGPT, a large language model trained by OpenAI.\n\
             Knowledge cutoff: 2024-06\n\nReasoning: low\n\n\
             # Valid channels: analysis, commentary, final. Channel must be included for every message.<|end|>\
             <|start|>user<|message|>{text}<|end|><|start|>assistant"
        ),
        Family::Gemma4 => {
            format!("<bos><|turn>user\n{text}<turn|>\n<|turn>model\n<|channel>thought\n<channel|>")
        }
        Family::QwenXml => format!(
            "<|im_start|>user\n{text}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        ),
    }
}

/// Matters for the Generic path only: the Native path renders prompts itself.
fn check_template(profile: &ModelProfile, report: &mut DoctorReport) {
    let Some(sha) = report.chat_template_sha256.clone() else {
        return;
    };
    let canonical = repo_paths(&format!("{}/canonical.jinja", profile.template.dir));
    match profile.template.lookup(&sha) {
        Some(k) if k.verdict == TemplateVerdict::Canonical => {
            report.note(Level::Ok, "chat template is the vetted canonical one")
        }
        Some(k) if k.verdict == TemplateVerdict::Ok => report.note(
            Level::Info,
            format!("chat template is a known variant ({}); canonical: --chat-template-file {canonical}", k.note),
        ),
        Some(k) => report.note(
            Level::Warn,
            format!(
                "chat template is a known-bad variant: {}. The Generic path is affected; start the server with \
                 --chat-template-file {canonical}",
                k.note
            ),
        ),
        None => report.note(
            Level::Info,
            format!(
                "chat template not in the vetted list (sha256 {}…): the Generic path may differ from the oracle; \
                 canonical: --chat-template-file {canonical}",
                &sha[..12.min(sha.len())]
            ),
        ),
    }
}

fn check_context_and_sampling(props: &Props, profile: &ModelProfile, report: &mut DoctorReport) {
    if let Some(n_ctx) = props.n_ctx
        && n_ctx < profile.context.working_max
    {
        report.note(
            Level::Warn,
            format!(
                "n_ctx per slot is {n_ctx}, below the {} profile's working context {}",
                profile.id, profile.context.working_max
            ),
        );
    }
    let d = props.default_sampling;
    let want = profile.sampling.main;
    let differs = |server: Option<f64>, ours: f64| server.is_some_and(|s| (s - ours).abs() > 1e-3);
    if differs(d.temperature, want.temperature.into())
        || differs(d.top_p, want.top_p.into())
        || differs(d.top_k, want.top_k.into())
        || differs(d.min_p, want.min_p.into())
    {
        report.note(
            Level::Info,
            format!(
                "server sampling defaults (temperature {}, top_k {}, top_p {}, min_p {}) differ from the profile \
                 (temperature {}, top_k {}, top_p {}, min_p {}); ognivo always sends its own sampling",
                num(d.temperature),
                num(d.top_k),
                num(d.top_p),
                num(d.min_p),
                num(Some(want.temperature.into())),
                want.top_k,
                num(Some(want.top_p.into())),
                num(Some(want.min_p.into())),
            ),
        );
    }
}

/// Short human form of a sampler value: f32 noise rounded away, `?` when absent.
fn num(v: Option<f64>) -> String {
    match v {
        Some(x) => {
            let s = format!("{x:.3}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        None => "?".into(),
    }
}
