//! MCP server implementation: exposes Brain facade as MCP tools (DESIGN §12.2).

use oxibrain::{Brain, BrainConfig};
use oxibrain_core::retrieval::Query;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ContentBlock;
use rmcp::schemars;
use rmcp::{tool, tool_router, ErrorData as McpError, ServiceExt};
use serde::Deserialize;
use std::sync::Arc;

fn default_space() -> String {
    "personal".to_string()
}

// ── Parameter types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// The search query text.
    pub query: String,
    /// Space name to search in (default: "personal").
    #[serde(default = "default_space")]
    pub space: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallParams {
    /// The context query — what information to assemble.
    pub query: String,
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
    /// Maximum tokens for the assembled context.
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
}

fn default_token_budget() -> usize {
    3000
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetEntityParams {
    /// Entity ID.
    pub entity_id: String,
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IngestParams {
    /// Content text to ingest.
    pub content: String,
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
    /// Optional source path (e.g. "notes/meeting.md").
    pub source_path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeclareParams {
    /// Declaration JSON (the canonical declaration format).
    pub declaration_json: String,
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WhyParams {
    /// Statement ID.
    pub statement_id: String,
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ContradictionsParams {
    /// Space name.
    #[serde(default = "default_space")]
    pub space: String,
}

// ── Server ──────────────────────────────────────────────────────────────

/// MCP server wrapping a Brain instance. Clone is cheap — Brain is behind Arc.
#[derive(Clone)]
pub struct BrainServer {
    brain: Arc<Brain>,
}

impl BrainServer {
    /// Open a Brain from a config directory and wrap it in an MCP server.
    pub async fn open(dir: &std::path::Path) -> anyhow::Result<Self> {
        let brain = Brain::open(BrainConfig::at(dir)).await?;
        Ok(Self {
            brain: Arc::new(brain),
        })
    }

    /// Wrap an existing Brain.
    pub fn from_brain(brain: Brain) -> Self {
        Self {
            brain: Arc::new(brain),
        }
    }

    /// Resolve a space name to its content-derived ID.
    async fn space_id(&self, name: &str) -> Result<String, McpError> {
        self.brain
            .ensure_space(name)
            .await
            .map_err(|e| McpError::internal_error(format!("ensure_space: {e}"), None))
    }
}

#[tool_router]
impl BrainServer {
    #[tool(description = "Search the brain using hybrid (lexical + semantic) retrieval. Returns ranked results with provenance.")]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<String, McpError> {
        let _space_id = self.space_id(&params.space).await?;
        let mut q = Query::hybrid(&params.query);
        q.space = params.space.clone();
        let result = self
            .brain
            .query(q)
            .await
            .map_err(|e| McpError::internal_error(format!("query: {e}"), None))?;

        let mut lines = Vec::new();
        for (i, hit) in result.ranked.iter().take(20).enumerate() {
            lines.push(format!(
                "{}. [score {:.3}] {} ({})",
                i + 1,
                hit.score,
                hit.target_id,
                hit.target_kind
            ));
        }
        if lines.is_empty() {
            Ok("No results found.".into())
        } else {
            Ok(lines.join("\n"))
        }
    }

    #[tool(description = "Assemble context for a query — the per-turn call for agents. Returns relevant passages within a token budget.")]
    async fn recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let ctx = self
            .brain
            .assemble_context(&space_id, &params.query, params.token_budget)
            .await
            .map_err(|e| McpError::internal_error(format!("assemble_context: {e}"), None))?;

        let mut lines = Vec::new();
        for layer in &ctx.layers {
            lines.push(format!("## {} ({} tokens)", layer.kind, layer.token_count));
            for item in &layer.items {
                lines.push(format!("  - {}", item));
            }
        }
        lines.push(format!("\nTotal tokens: {}", ctx.total_tokens));
        Ok(lines.join("\n"))
    }

    #[tool(description = "Get an entity's current beliefs — all statements about it with status and confidence.")]
    async fn get_entity(
        &self,
        Parameters(params): Parameters<GetEntityParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let beliefs = self
            .brain
            .beliefs(&space_id, &params.entity_id)
            .await
            .map_err(|e| McpError::internal_error(format!("beliefs: {e}"), None))?;

        if beliefs.is_empty() {
            return Ok(format!("No beliefs found for entity {}.", params.entity_id));
        }

        let mut lines = Vec::new();
        for b in &beliefs {
            lines.push(format!(
                "  {} | status={} confidence={:.3} [{}..{}]",
                b.statement,
                b.status.as_str(),
                b.confidence,
                b.valid_from.millis(),
                b.valid_to.millis()
            ));
        }
        Ok(lines.join("\n"))
    }

    #[tool(description = "Ingest text content as a new episode. Returns the episode ID.")]
    async fn ingest(
        &self,
        Parameters(params): Parameters<IngestParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let path = params.source_path.unwrap_or_else(|| "mcp".into());
        let now = self.brain.now();
        let ep_id = self
            .brain
            .ingest_note(&space_id, &path, params.content, now)
            .await
            .map_err(|e| McpError::internal_error(format!("ingest: {e}"), None))?;
        Ok(format!("Ingested as episode: {ep_id}"))
    }

    #[tool(description = "Declare a statement (deterministic, no LLM). Takes a declaration JSON with subject, predicate, object, polarity, valid_from, valid_to.")]
    async fn declare(
        &self,
        Parameters(params): Parameters<DeclareParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let decl: oxibrain::Declaration = serde_json::from_str(&params.declaration_json)
            .map_err(|e| McpError::invalid_params(format!("declaration parse: {e}"), None))?;
        let ep_id = self
            .brain
            .declare(&space_id, decl)
            .await
            .map_err(|e| McpError::internal_error(format!("declare: {e}"), None))?;
        Ok(format!("Declared as episode: {ep_id}"))
    }

    #[tool(description = "Get provenance for a statement — which episodes and extractors support it, with confidence breakdown.")]
    async fn why(
        &self,
        Parameters(params): Parameters<WhyParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let explain = self
            .brain
            .why(&space_id, &params.statement_id)
            .await
            .map_err(|e| McpError::internal_error(format!("why: {e}"), None))?;
        Ok(serde_json::to_string_pretty(&explain).unwrap_or_default())
    }

    #[tool(description = "List all contradicted statements in a space.")]
    async fn contradictions(
        &self,
        Parameters(params): Parameters<ContradictionsParams>,
    ) -> Result<String, McpError> {
        let space_id = self.space_id(&params.space).await?;
        let stmts = self
            .brain
            .contradictions(&space_id)
            .await
            .map_err(|e| McpError::internal_error(format!("contradictions: {e}"), None))?;

        if stmts.is_empty() {
            Ok("No contradictions found.".into())
        } else {
            let lines: Vec<String> = stmts
                .iter()
                .map(|s| format!("  {} ({} {} {:?})", s.id, s.subject, s.predicate, s.object))
                .collect();
            Ok(format!("{} contradiction(s):\n{}", stmts.len(), lines.join("\n")))
        }
    }
}

// ── Startup ────────────────────────────────────────────────────────────

/// Start the MCP server on stdio (for Claude Desktop and other MCP clients).
pub async fn serve_stdio(brain: Brain) -> anyhow::Result<()> {
    let server = BrainServer::from_brain(brain);
    let transport = rmcp::transport::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;
    Ok(())
}

/// Start the MCP server from a config directory.
pub async fn serve_stdio_at(dir: &std::path::Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    serve_stdio(brain).await
}
