//! oxibrain MCP server: exposes the Brain facade over the Model Context
//! Protocol (DESIGN §12.2).
//!
//! This is the in-house JSON-RPC implementation (DESIGN §18 fallback for the
//! `rmcp` risk). It speaks MCP `2025-11-25` (with `2026-07-28` negotiation) over
//! newline-delimited JSON-RPC 2.0 on stdio. No external protocol crate — the
//! surface is small and the MSRV stays at 1.85 (rmcp 0.12+ requires 1.88 via
//! darling 0.23).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod protocol;
pub mod server;

#[cfg(unix)]
pub use server::serve_socket;
pub use server::{BrainServer, run_session, serve_stdio, serve_stdio_at};
