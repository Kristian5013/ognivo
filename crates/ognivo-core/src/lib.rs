//! Agent core: the loop, sessions, subagents and approvals (docs/architecture.md §6–8).
//!
//! What exists so far: the approval policy ([`approval`]) and the agent loop ([`agent`]).

pub mod agent;
pub mod approval;
