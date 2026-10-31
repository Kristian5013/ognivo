//! Agent core: the loop, sessions, subagents and approvals (docs/architecture.md §6–8).
//!
//! What exists so far: the agent loop ([`agent`]), the approval policy ([`approval`]) and
//! the record of what a turn verified ([`verify`]).

pub mod agent;
pub mod approval;
pub mod verify;
