//! Evaluation harness for model profiles (docs/architecture.md §11).
//!
//! T0 runs before every commit (`scripts/check.sh`), on the CPU:
//! - the parse corpus: every chat completion the M0 and M1 runs recorded, read again by
//!   the Generic driver's own code ([`replay`], the `replay` binary) against a snapshot in
//!   git, and the failure shapes among them with neutral text (`evals/t0/corpus`,
//!   `tests/corpus.rs`), cut anywhere;
//! - the injection corpus: every control token of every profile, wherever text reaches a
//!   request, never whole on the wire and whole again in what the tools and the user get
//!   (`tests/injection.rs`);
//! - the KV-fidelity simulation: within a prompt every request repeats the one before and
//!   goes on with the generation it got (`tests/kv.rs`).
//!
//! T2 (a live micro-suite on mock tools, nightly) and T3 (agentic tasks through Harbor)
//! come next.

pub mod replay;
