//! Agent core: the loop, sessions, subagents and approvals (docs/architecture.md §6–8).
//!
//! What exists so far: the agent loop ([`agent`]), the approval policy ([`approval`]), the
//! edit journal ([`journal`]), AGENTS.md and CLAUDE.md ([`instructions`]), the session in
//! the store ([`session`]), compaction in the middle of a turn ([`compact`]), permission
//! rules ([`rules`]), traces in ATIF ([`trace`]), the record of what a turn verified
//! ([`verify`]), the detector of repeated calls ([`repeats`]) and the clients' event model
//! made of the agent's events ([`protocol`]).

pub mod agent;
pub mod approval;
pub mod compact;
pub mod instructions;
pub mod journal;
pub mod protocol;
pub mod repeats;
pub mod rules;
pub mod session;
pub mod trace;
pub mod verify;
