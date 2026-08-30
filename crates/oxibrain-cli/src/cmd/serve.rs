//! `oxibrain serve` — start an MCP session (DESIGN §12.4).
//!
//! Daemonless transports only (two-plane design):
//!
//! - Default (or `--stdio`): a newline-delimited JSON-RPC session on
//!   stdin/stdout — a caller-owned child. `BrainClient::spawn_local` spawns
//!   exactly this shape; MCP hosts (Claude Desktop) launch it directly. An
//!   optional `auth` request as the first message switches the session into
//!   token-scoped mode (DESIGN §11.2).
//! - `--http <addr>`: loopback HTTP with the operations console. Foreground
//!   only; a reverse proxy provides TLS for anything beyond loopback.
//!
//! There is no socket, no daemon, and no PID file. Diagnostics go to stderr
//! — stdout is the protocol channel.

// ADR-013: `space` is a required argument on every space-scoped call —
// there is no config-file default and serve loads no user config.

use anyhow::Context;
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::BrainError;
use std::path::Path;

pub async fn run(
    dir: &Path,
    http: Option<String>,
    ui_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let brain = match Brain::open(BrainConfig::at(dir)).await {
        Ok(b) => b,
        Err(BrainError::Locked { holder }) => {
            // §4.3: fail fast with a clear error when another process owns
            // the write lock.
            anyhow::bail!(
                "store is locked — another oxibrain process owns it ({holder}).\n\
                 Wait for it to finish, or point --dir at a different store."
            );
        }
        Err(e) => return Err(e.into()),
    };

    if let Some(addr_str) = http {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --http address '{addr_str}': {e}"))?;
        return oxibrain_mcp::serve_http(brain, addr, ui_dir).await;
    }

    // Stdio: one session per process, optionally token-gated when the first
    // message on stdin is an `auth` request.
    oxibrain_mcp::serve_stdio(brain)
        .await
        .context("stdio session")
}
