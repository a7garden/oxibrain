//! oxibrain MCP server: exposes the Brain facade over the Model Context
//! Protocol (DESIGN §12.2).
//!
//! This is the in-house JSON-RPC implementation (DESIGN §18 fallback for the
//! `rmcp` risk). It speaks MCP `2025-11-25` (with `2026-07-28` negotiation) over
//! newline-delimited JSON-RPC 2.0. No external protocol crate — the surface is
//! small and the in-house implementation keeps the DESIGN §18 `rmcp` risk at
//! zero (rmcp 0.12+ needs rustc ≥1.88 via darling 0.23, below our MSRV 1.96).
//!
//! Daemonless transports only (two-plane design): a caller-owned stdio session
//! (`serve --stdio`, optionally token-gated via a leading `auth` request) or a
//! foreground loopback HTTP console. There is no socket, no daemon, and no
//! PID file.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod protocol;
pub mod sampling;
pub mod server;

pub use server::{
    BrainServer, run_session, run_session_gated, serve_http, serve_stdio, serve_stdio_at,
};
