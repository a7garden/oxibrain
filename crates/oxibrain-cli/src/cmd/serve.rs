//! `oxibrain serve` — start the MCP server on stdio (DESIGN §12.4).
//!
//! stdio is the transport Claude Desktop and most MCP clients expect. Socket
//! and HTTP transports (the daemon topology, §4.3) land with the daemon work.

use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    oxibrain_mcp::serve_stdio(brain).await
}
