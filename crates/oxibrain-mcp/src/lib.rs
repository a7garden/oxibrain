//! oxibrain MCP server: exposes Brain facade methods over MCP (DESIGN §12.2).
//!
//! Wraps the Brain facade as MCP tools. Token authentication is enforced
//! per-call by checking the token against the store before dispatching.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod server;

pub use server::{BrainServer, serve_stdio};
