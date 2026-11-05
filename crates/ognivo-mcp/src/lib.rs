//! MCP servers for the ognivo agent (docs/architecture.md §9): where they are configured,
//! a client for each, and the gateway that shows their tools to the model under names it
//! can write and runs its calls.

pub mod config;
mod gateway;
pub mod oauth;

pub use config::{Loaded, OAuth, ServerConfig, Transport};
pub use gateway::{
    DECLARED_CHARS, DECLARED_MAX, Found, Gateway, Listing, RESULT_CHARS, Started, Tool, name_of,
    search, text_of,
};
