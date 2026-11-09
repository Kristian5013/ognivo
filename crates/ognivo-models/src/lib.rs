//! The model layer of ognivo (docs/architecture.md §4–5).
//!
//! What exists so far:
//! - [`Family`] and [`ModelProfile`]: per-model behaviour as data (`profiles/*.toml`);
//! - [`caps`]: what a connected server can do, including its class A/B/C;
//! - [`engine`]: the token-level backend seam ([`TokenEngine`]) and its first
//!   implementation over llama-server's `/completion` (the Native path, M2+);
//! - [`generic`]: the Generic path over `/v1/chat/completions` (M1): request building and
//!   streamed-chunk folding, with the maintainers' correctness rules baked in;
//! - [`driver`]: [`ModelDriver`], the agent loop's only view of a model, and
//!   [`ChatDriver`], its Generic implementation, with the special-token [`guard`] and the
//!   [`repeat`] guard on its streams;
//! - [`llama`]: the HTTP client for llama-server and ik_llama.cpp ([`sse`] parses its
//!   streams);
//! - [`doctor`]: server checks behind `ognivo doctor`;
//! - [`template`]: chat templates in the form servers report them;
//! - [`grammar`]: GBNF for llama.cpp, JSON Schema turned into rules;
//! - [`router`]: the servers and their models, and whose step goes next at their slots,
//!   by priority.
//!
//! - [`harmony`]: gpt-oss's format, rendered as its reference does and read back
//!   tolerantly, for the Native path (M2);
//! - [`native`]: [`NativeDriver`], the Native path for gpt-oss over `/completion`.
//!
//! The codecs of Gemma 4 and Qwen XML arrive in M3.

pub mod caps;
pub mod doctor;
pub mod draft;
pub mod driver;
pub mod engine;
pub mod family;
pub mod generic;
pub mod grammar;
pub mod guard;
pub mod harmony;
pub mod llama;
pub mod native;
pub mod profile;
pub mod qwen;
pub mod repeat;
pub mod router;
pub mod sse;
pub mod template;

pub use caps::{EngineCaps, ServerClass, ServerKind, StreamIds};
pub use driver::{ChatDriver, DriverError, DriverEvent, Finished, ModelDriver, StepRequest};
pub use engine::{CompletionRequest, EngineError, RawEngine, TokenEngine, TokenEvent, TokenId};
pub use family::Family;
pub use generic::{
    Channel, ChatCompletion, ChatEvent, ChatMessage, ChatRequest, ExecutableCalls, FinishReason,
    Outcome, ToolCall, ToolChoice, ToolDecl, chat_body, chat_events,
};
pub use guard::Guard;
pub use native::{AnswerNow, NativeDriver};
pub use profile::ModelProfile;

/// `tools` declared as `family` reads declarations in its prompt: Harmony's `functions`
/// namespace in TypeScript, Qwen's `<tools>` block of JSON; Gemma 4's declarations are
/// written with control tokens, so it gets the JSON lines. For a tool result that declares
/// tools the prompt does not (`tool_search`).
pub fn declarations(family: Family, tools: &[ToolDecl]) -> String {
    match family {
        Family::Harmony => harmony::functions_text(tools),
        Family::QwenXml => qwen::tools_text(tools),
        Family::Gemma4 => tools
            .iter()
            .map(|t| {
                serde_json::json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
pub use router::{Endpoint, Health, Lease, Priority, Router, RouterError, Scheduled};
