//! `oxibrain serve` — start the MCP server (DESIGN §12.4).
//!
//! Default transport is stdio (what Claude Desktop expects). `--socket <path>`
//! listens on a Unix-domain socket for the daemon topology (§4.3): several apps
//! share one brain through the single-writer store actor (P8).
//! `--socket <path> --require-token` gates each connection behind a token
//! handshake (§11.2). `--http <addr>` serves loopback HTTP.

use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(
    dir: &Path,
    socket: Option<std::path::PathBuf>,
    http: Option<String>,
    require_token: bool,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;

    if let Some(addr_str) = http {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --http address '{addr_str}': {e}"))?;
        return oxibrain_mcp::serve_http(brain, addr).await;
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
            oxibrain_mcp::serve_stdio(brain).await
        }
    }
}
