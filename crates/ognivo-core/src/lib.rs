//! Agent core: the loop, sessions, subagents and approvals (docs/architecture.md §6–8).
//!
//! What exists so far: the agent loop ([`agent`]), the approval policy ([`approval`]), the
//! edit journal ([`journal`]), AGENTS.md and CLAUDE.md ([`instructions`]), the session in
//! the store ([`session`]), compaction in the middle of a turn ([`compact`]), the record
//! of what a turn verified ([`verify`]) and the detector of repeated calls ([`repeats`]).

pub mod agent;
pub mod approval;
pub mod compact;
pub mod instructions;
pub mod journal;
pub mod repeats;
pub mod session;
pub mod verify;
