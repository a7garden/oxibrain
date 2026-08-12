//! `oxibrain serve` — start the MCP server (DESIGN §12.4).
//!
//! Default transport is stdio (what Claude Desktop expects). `--socket <path>`
//! listens on a Unix-domain socket for the daemon topology (§4.3): several apps
//! share one brain through the single-writer store actor (P8).

use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, socket: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    match socket {
        #[cfg(unix)]
        Some(path) => oxibrain_mcp::serve_socket(brain, &path).await,
        #[cfg(not(unix))]
        Some(_) => anyhow::bail!("--socket is only supported on Unix"),
        None => oxibrain_mcp::serve_stdio(brain).await,
    }
}
