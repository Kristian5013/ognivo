//! Agent core: the loop, sessions, subagents and approvals (docs/architecture.md §6–8).
//!
//! Only the approval policy exists so far; the loop lands in M1.

pub mod approval;
