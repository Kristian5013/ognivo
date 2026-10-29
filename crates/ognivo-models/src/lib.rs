//! The model layer of ognivo (docs/architecture.md §4–5).
//!
//! What exists so far:
//! - [`Family`] and [`ModelProfile`]: per-model behaviour as data (`profiles/*.toml`);
//! - [`caps`]: what a connected server can do, including its class A/B/C;
//! - [`engine`]: the token-level backend seam ([`TokenEngine`]) and its first
//!   implementation over llama-server's `/completion`;
//! - [`llama`]: the HTTP client for llama-server and ik_llama.cpp;
//! - [`doctor`]: server checks behind `ognivo doctor`;
//! - [`template`]: chat templates in the form servers report them.
//!
//! Format codecs (Harmony, Gemma 4, Qwen XML) and the `ModelDriver` seam arrive with their
//! first implementations in M1–M3.

pub mod caps;
pub mod doctor;
pub mod engine;
pub mod family;
pub mod llama;
pub mod profile;
pub mod template;

pub use caps::{EngineCaps, ServerClass, ServerKind, StreamIds};
pub use engine::{CompletionRequest, EngineError, RawEngine, TokenEngine, TokenEvent, TokenId};
pub use family::Family;
pub use profile::ModelProfile;
