//! `oxibrain serve` — start the MCP server (DESIGN §12.4).
//!
//! Default transport is stdio (what Claude Desktop expects). `--socket <path>`
//! listens on a Unix-domain socket for the daemon topology (§4.3): several apps
//! share one brain through the single-writer store actor (P8).
//! `--socket <path> --require-token` gates each connection behind a token
//! handshake (§11.2). `--http <addr>` serves loopback HTTP.
//!
//! `--daemon` writes a PID file to `<dir>/.oxibrain.pid` so external supervisors
//! (launchd) can manage the process. The binary never forks — backgrounding is
//! the supervisor's job (§15). All socket/HTTP listeners shut down gracefully on
//! SIGINT/SIGTERM.

use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::BrainError;
use std::path::Path;

pub async fn run(
    dir: &Path,
    socket: Option<std::path::PathBuf>,
    http: Option<String>,
    require_token: bool,
    daemon: bool,
    ui_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let brain = match Brain::open(BrainConfig::at(dir)).await {
        Ok(b) => b,
        Err(BrainError::Locked { holder }) => {
            // §4.3: "fails fast with a clear error if a daemon holds the lock,
            // and prints the command to attach instead."
            anyhow::bail!(
                "store is locked — another oxibrain process owns it ({holder}).\n\
                 If a daemon is already running, connect to it (e.g. via its socket) \
                 instead of starting a second one.\n\
                 To start a new daemon, ensure no other oxibrain process is running."
            );
        }
        Err(e) => return Err(e.into()),
    };

    // Write the PID file in daemon mode. RAII: removed on drop when `run`
    // returns (after graceful shutdown or error). The advisory lock inside the
    // Brain is the real single-writer guard (P8); the PID file is informational.
    let _pid = if daemon {
        let pid = oxibrain_mcp::PidFile::acquire(dir)
            .map_err(|e| anyhow::anyhow!("write PID file: {e}"))?;
        tracing::info!(
            "daemon PID {} → {}",
            std::process::id(),
            pid.path().display()
        );
        Some(pid)
    } else {
        None
    };

    if let Some(addr_str) = http {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --http address '{addr_str}': {e}"))?;
        return oxibrain_mcp::serve_http(brain, addr, ui_dir).await;
    }

    match socket {
        #[cfg(unix)]
        Some(path) => {
            if require_token {
                oxibrain_mcp::serve_socket_auth(brain, &path).await
            } else {
                tracing::warn!(
                    "serving on socket without --require-token: relying on filesystem \
                     permissions alone (DESIGN §11.2). Pass --require-token for token auth."
                );
                oxibrain_mcp::serve_socket(brain, &path).await
            }
        }
        #[cfg(not(unix))]
        Some(_) => anyhow::bail!("--socket is only supported on Unix"),
        None => {
            if require_token {
                anyhow::bail!("--require-token requires --socket");
            }
            if daemon {
                tracing::warn!(
                    "--daemon has no effect over stdio (no PID file needed; the \
                     MCP client manages the process lifecycle)"
                );
            }
            oxibrain_mcp::serve_stdio(brain).await
        }
    }
}
