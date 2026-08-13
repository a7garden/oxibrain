//! MCP server: exposes the Brain facade as MCP tools (DESIGN §12.2).
//!
//! In-house JSON-RPC implementation (DESIGN §18 fallback). The tool set mirrors
//! the design's MCP surface table: `search`, `recall`, `get_entity`, `ingest`,
//! `declare`, `why`, `contradictions`. Structured results are returned as JSON
//! text so agents can parse them; write tools return short confirmations.

use crate::protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, Message, UNAUTHORIZED, error, success,
    text_result, tool_error,
};
use oxibrain::{
    Brain, BrainConfig, Capability, DeclObject, Declaration, EntityRef, RedactTarget, Scope,
};
use oxibrain_core::retrieval::{
    Direction, PredicateFilter, Query, QueryMode, Strategy, TraversalSpec,
};
use oxibrain_ports::{ClockPort, SystemClock, Timestamp};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
};

/// MCP protocol version advertised to clients that do not request 2026-07-28.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Clone)]
pub struct BrainServer {
    brain: Arc<Brain>,
    /// When set (authenticated transport), tool calls are gated by capability +
    /// space membership (DESIGN §11.2). `None` = trusted local channel.
    scope: Option<Scope>,
}

impl BrainServer {
    /// Open a Brain from a config directory as a trusted local server (no scope).
    pub async fn open(dir: &std::path::Path) -> anyhow::Result<Self> {
        let brain = Brain::open(BrainConfig::at(dir)).await?;
        Ok(Self {
            brain: Arc::new(brain),
            scope: None,
        })
    }

    /// Wrap an existing Brain as a trusted local server (no scope).
    pub fn from_brain(brain: Brain) -> Self {
        Self {
            brain: Arc::new(brain),
            scope: None,
        }
    }

    /// Wrap a Brain with an authorization scope — tool calls are then gated by
    /// the scope's capabilities and space membership (DESIGN §11.2). Used by
    /// authenticated transports (daemon, token-bearing MCP sessions).
    pub fn from_brain_scoped(brain: Brain, scope: Scope) -> Self {
        Self {
            brain: Arc::new(brain),
            scope: Some(scope),
        }
    }

    /// Wrap a shared `Arc<Brain>` as a trusted local server (no scope).
    ///
    /// Used by authenticated transports that share one brain across many
    /// connections, each resolved to its own scope.
    pub fn from_arc(brain: Arc<Brain>) -> Self {
        Self { brain, scope: None }
    }

    /// Wrap a shared `Arc<Brain>` with an authorization scope.
    pub fn from_arc_scoped(brain: Arc<Brain>, scope: Scope) -> Self {
        Self {
            brain,
            scope: Some(scope),
        }
    }

    /// Resolve a space name to its content-derived ID, creating it if absent.
    async fn ensure_space(&self, name: &str) -> Result<String, ToolErr> {
        self.brain.ensure_space(name).await.map_err(ToolErr::run)
    }

    /// Capability required to invoke a tool (DESIGN §12.2 surface table).
    /// `None` for unknown tools — dispatch then returns method-not-found.
    fn required_capability(tool: &str) -> Option<Capability> {
        match tool {
            "search" | "recall" | "get_entity" | "why" | "contradictions" | "traverse"
            | "timeline" | "review_merges" | "stats" => Some(Capability::Read),
            "ingest" => Some(Capability::Ingest),
            "declare" | "remember" | "retract" | "merge_entities" => Some(Capability::Write),
            "redact" => Some(Capability::Redact),
            _ => None,
        }
    }

    /// Enforce the attached scope (if any) before dispatching a tool.
    ///
    /// A trusted local server (`scope == None`) skips this entirely. An
    /// authenticated server checks capability + expiry first (no side effects),
    /// then space membership. Returns `UNAUTHORIZED` on denial.
    async fn enforce_scope(&self, tool: &str, args: &Value) -> Result<(), (i64, String)> {
        let Some(scope) = &self.scope else {
            return Ok(());
        };
        let Some(cap) = Self::required_capability(tool) else {
            return Ok(()); // unknown tool — dispatch returns method-not-found.
        };
        let now = SystemClock.now();
        let expired = scope.expires_at.is_some_and(|exp| now >= exp);
        if expired || !scope.caps.contains(&cap) {
            return Err((
                UNAUTHORIZED,
                format!("token lacks '{}' (expired={expired})", cap.as_str()),
            ));
        }
        // Space membership: resolve the content-derived id, then check. The
        // space is created only after the capability gate passes.
        let space = args
            .get("space")
            .and_then(|v| v.as_str())
            .unwrap_or("personal");
        let space_id = self
            .brain
            .ensure_space(space)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("ensure_space: {e}")))?;
        if !scope.spaces.iter().any(|s| s == &space_id) {
            return Err((UNAUTHORIZED, format!("token not scoped to space '{space}'")));
        }
        Ok(())
    }

    /// Handle one JSON-RPC message without a sampling session (for HTTP and
    /// direct unit tests). Equivalent to `handle_with(msg, None)`.
    pub async fn handle(&self, msg: Message) -> Option<Value> {
        self.handle_with(msg, None).await
    }

    /// Handle one JSON-RPC message with an optional sampling session.
    ///
    /// When `session` is `Some`, tools that need the LLM (e.g. `ingest` with
    /// `extract: true`) can delegate `complete()` to the client via
    /// `sampling/createMessage` (§12.3). Returns a response value for requests,
    /// `None` for notifications.
    async fn handle_with(
        &self,
        msg: Message,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Option<Value> {
        match msg.method.as_str() {
            "initialize" => msg
                .id
                .map(|id| success(id, self.initialize(msg.params.as_ref()))),
            "notifications/initialized" | "initialized" => None,
            "ping" => msg.id.map(|id| success(id, json!({}))),
            "tools/list" => msg.id.map(|id| success(id, tool_list())),
            "resources/list" => msg
                .id
                .map(|id| success(id, self.resources_list(msg.params.as_ref()))),
            "resources/read" => match msg.id {
                Some(id) => match self.resources_read(msg.params.as_ref()).await {
                    Ok(v) => Some(success(id, v)),
                    Err((code, m)) => Some(error(id, code, m)),
                },
                None => None,
            },
            "tools/call" => match msg.id {
                Some(id) => match self.call_tool(msg.params.as_ref(), session).await {
                    Ok(v) => Some(success(id, v)),
                    Err((code, m)) => Some(error(id, code, m)),
                },
                None => None,
            },
            other => msg
                .id
                .map(|id| error(id, METHOD_NOT_FOUND, format!("unknown method: {other}"))),
        }
    }

    /// MCP `initialize` — negotiate protocol version, advertise tools capability.
    fn initialize(&self, params: Option<&Value>) -> Value {
        let client_version = params
            .and_then(|p| p.get("protocolVersion"))
            .and_then(|v| v.as_str());
        // Negotiate: echo the 2026-07-28 revision if the client asks for it,
        // otherwise advertise our default. Both are served by the same code.
        let version = if matches!(client_version, Some("2026-07-28")) {
            "2026-07-28"
        } else {
            DEFAULT_PROTOCOL_VERSION
        };
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "listChanged": false, "subscribe": false, "read": true }
            },
            "serverInfo": { "name": "oxibrain", "version": env!("CARGO_PKG_VERSION") }
        })
    }

    async fn call_tool(
        &self,
        params: Option<&Value>,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<Value, (i64, String)> {
        let p = params.ok_or((INVALID_PARAMS, "missing 'params'".into()))?;
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((INVALID_PARAMS, "missing tool 'name'".into()))?;
        let args = p.get("arguments").cloned().unwrap_or(json!({}));
        self.enforce_scope(name, &args).await?;
        let outcome = match name {
            "search" => self.tool_search(&args).await,
            "recall" => self.tool_recall(&args).await,
            "get_entity" => self.tool_get_entity(&args).await,
            "traverse" => self.tool_traverse(&args).await,
            "timeline" => self.tool_timeline(&args).await,
            "why" => self.tool_why(&args).await,
            "contradictions" => self.tool_contradictions(&args).await,
            "stats" => self.tool_stats(&args).await,
            "review_merges" => self.tool_review_merges(&args).await,
            "ingest" => self.tool_ingest(&args, session).await,
            "remember" => self.tool_remember(&args, session).await,
            "declare" => self.tool_declare(&args).await,
            "retract" => self.tool_retract(&args).await,
            "merge_entities" => self.tool_merge_entities(&args).await,
            "redact" => self.tool_redact(&args).await,
            other => return Err((METHOD_NOT_FOUND, format!("unknown tool: {other}"))),
        };
        match outcome {
            Ok(text) => Ok(text_result(text)),
            Err(ToolErr::Params(m)) => Err((INVALID_PARAMS, m)),
            Err(ToolErr::Run(m)) => Ok(tool_error(m)),
        }
    }

    // ── Tools ──────────────────────────────────────────────────────────────

    async fn tool_search(&self, args: &Value) -> Result<String, ToolErr> {
        let query = str_arg(args, "query")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let mode = parse_mode(&str_arg_or(args, "mode", "hybrid"));
        let limit = u_arg_or(args, "limit", 20);
        let q = Query {
            text: query.to_string(),
            mode,
            space: space_id,
            as_of: None,
            limit,
            min_confidence: 0.0,
        };
        let result = self.brain.query(q).await.map_err(ToolErr::run)?;
        to_json(&result)
    }

    async fn tool_recall(&self, args: &Value) -> Result<String, ToolErr> {
        let query = str_arg(args, "query")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let budget = u_arg_or(args, "token_budget", 3000);
        let ctx = self
            .brain
            .assemble_context(&space_id, query, budget)
            .await
            .map_err(ToolErr::run)?;
        to_json(&ctx)
    }

    async fn tool_get_entity(&self, args: &Value) -> Result<String, ToolErr> {
        let entity_id = str_arg(args, "entity_id")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let beliefs = self
            .brain
            .beliefs(&space_id, entity_id)
            .await
            .map_err(ToolErr::run)?;
        to_json(&beliefs)
    }

    async fn tool_ingest(
        &self,
        args: &Value,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<String, ToolErr> {
        let content = str_arg(args, "content")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let path = str_arg_or(args, "source_path", "mcp");
        let extract = args
            .get("extract")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let now = SystemClock.now();
        let id = self
            .brain
            .ingest_note(&space_id, &path, content.to_string(), now)
            .await
            .map_err(ToolErr::run)?;

        if extract {
            return self.try_sample_extract(&space_id, &id, session).await;
        }
        Ok(format!("Ingested as episode: {id}"))
    }

    /// Whether sampling is available for this session. A trusted local channel
    /// (scope == None, e.g. stdio with Claude Desktop) gets sampling by default;
    /// an authenticated session requires the `Sample` capability (§12.3).
    fn sampling_available(&self) -> bool {
        match &self.scope {
            None => true,
            Some(s) => s.caps.contains(&Capability::Sample),
        }
    }

    /// Attempt realtime extraction via client sampling (§12.3). If sampling is
    /// not available (no session, no `Sample` cap), returns a skip message —
    /// the episode is still ingested, just not extracted yet.
    async fn try_sample_extract(
        &self,
        space_id: &str,
        episode_id: &str,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<String, ToolErr> {
        let Some(handle) = session else {
            return Ok(format!(
                "Ingested as episode: {episode_id} (extraction skipped: no sampling session)"
            ));
        };
        if !self.sampling_available() {
            return Ok(format!(
                "Ingested as episode: {episode_id} \
                 (extraction skipped: token lacks Sample capability)"
            ));
        }

        let llm = std::sync::Arc::new(crate::sampling::SamplingLlmPort::new(handle.clone()));
        let config = oxibrain_core::extraction::ExtractorConfig {
            model_id: "mcp-sampling".into(),
            prompt_version: 1,
            registry_major: oxibrain_core::registry::CORE_V1_MAJOR,
            mechanism: oxibrain_core::extraction::ExtractMechanism::ToolCall,
            max_tokens: 8192,
            model_digest: None,
        };
        match self
            .brain
            .extract_one_with(space_id, episode_id, &config, llm)
            .await
        {
            Ok(summary) => Ok(format!(
                "Ingested + extracted: {episode_id} ({} extracted, {} quarantined)",
                summary.extracted, summary.quarantined
            )),
            Err(e) => Ok(format!(
                "Ingested as episode: {episode_id} (extraction failed: {e})"
            )),
        }
    }

    async fn tool_declare(&self, args: &Value) -> Result<String, ToolErr> {
        let decl_json = str_arg(args, "declaration_json")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let decl: Declaration = serde_json::from_str(decl_json)
            .map_err(|e| ToolErr::Params(format!("declaration parse: {e}")))?;
        let id = self
            .brain
            .declare(&space_id, decl)
            .await
            .map_err(ToolErr::run)?;
        Ok(format!("Declared as episode: {id}"))
    }

    async fn tool_why(&self, args: &Value) -> Result<String, ToolErr> {
        let statement_id = str_arg(args, "statement_id")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let explain = self
            .brain
            .why(&space_id, statement_id)
            .await
            .map_err(ToolErr::run)?;
        to_json(&explain)
    }

    async fn tool_contradictions(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let stmts = self
            .brain
            .contradictions(&space_id)
            .await
            .map_err(ToolErr::run)?;
        to_json(&stmts)
    }

    async fn tool_stats(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let stats = self.brain.stats(&space_id).await.map_err(ToolErr::run)?;
        to_json(&stats)
    }

    // ── Read tools: traverse, timeline, review_merges ────────────────────

    async fn tool_traverse(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let start = args
            .get("start")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolErr::Params("missing required argument 'start'".into()))?;
        let start_ids: Vec<String> = start
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if start_ids.is_empty() {
            return Err(ToolErr::Params(
                "'start' must contain at least one entity ID".into(),
            ));
        }
        let max_depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(3) as u8;
        let max_nodes = args
            .get("max_nodes")
            .and_then(|v| v.as_u64())
            .unwrap_or(256) as u32;
        let direction = match str_arg_or(args, "direction", "both").as_str() {
            "out" => Direction::Out,
            "in" => Direction::In,
            _ => Direction::Both,
        };
        let spec = TraversalSpec {
            start: start_ids,
            max_depth,
            max_nodes,
            predicates: PredicateFilter::AllowAll,
            direction,
            valid_at: None,
            min_confidence: 0.0,
            strategy: Strategy::Bfs,
        };
        let result = self
            .brain
            .traverse(&space_id, spec)
            .await
            .map_err(ToolErr::run)?;
        to_json(&result)
    }

    async fn tool_timeline(&self, args: &Value) -> Result<String, ToolErr> {
        let entity_id = str_arg(args, "entity_id")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let from = args.get("from").and_then(|v| v.as_i64()).map(Timestamp);
        let to = args.get("to").and_then(|v| v.as_i64()).map(Timestamp);
        let entries = self
            .brain
            .timeline(&space_id, entity_id, from, to)
            .await
            .map_err(ToolErr::run)?;
        to_json(&entries)
    }

    async fn tool_review_merges(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let merges = self
            .brain
            .list_merges(&space_id)
            .await
            .map_err(ToolErr::run)?;
        to_json(&merges)
    }

    // ── Write tools: remember, retract, merge_entities ───────────────────

    async fn tool_remember(
        &self,
        args: &Value,
        session: Option<&std::sync::Arc<crate::sampling::SessionHandle>>,
    ) -> Result<String, ToolErr> {
        let content = str_arg(args, "content")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let path = str_arg_or(args, "source_path", "remember");
        let now = SystemClock.now();
        let id = self
            .brain
            .ingest_note(&space_id, &path, content.to_string(), now)
            .await
            .map_err(ToolErr::run)?;
        // remember always extracts synchronously (DESIGN §12.2).
        self.try_sample_extract(&space_id, &id, session).await
    }

    async fn tool_retract(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let subject: EntityRef =
            serde_json::from_value(args.get("subject").cloned().unwrap_or_default())
                .map_err(|e| ToolErr::Params(format!("parse subject: {e}")))?;
        let predicate = str_arg(args, "predicate")?;
        let object: DeclObject =
            serde_json::from_value(args.get("object").cloned().unwrap_or_default())
                .map_err(|e| ToolErr::Params(format!("parse object: {e}")))?;
        let episode = str_arg(args, "episode")?;
        let decl = Declaration::Retract {
            subject,
            predicate: predicate.to_string(),
            object,
            episode: episode.to_string(),
        };
        let id = self
            .brain
            .declare(&space_id, decl)
            .await
            .map_err(ToolErr::run)?;
        Ok(format!("Retracted as episode: {id}"))
    }

    async fn tool_merge_entities(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let loser: EntityRef =
            serde_json::from_value(args.get("loser").cloned().unwrap_or_default())
                .map_err(|e| ToolErr::Params(format!("parse loser: {e}")))?;
        let winner: EntityRef =
            serde_json::from_value(args.get("winner").cloned().unwrap_or_default())
                .map_err(|e| ToolErr::Params(format!("parse winner: {e}")))?;
        let decl = Declaration::Merge { loser, winner };
        let id = self
            .brain
            .declare(&space_id, decl)
            .await
            .map_err(ToolErr::run)?;
        Ok(format!("Merged as episode: {id}"))
    }

    // ── Redact tool ──────────────────────────────────────────────────────

    async fn tool_redact(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let kind = str_arg(args, "target_kind")?;
        let target_id = str_arg(args, "target_id")?;
        let reason = str_arg_or(args, "reason", "mcp redact");
        let dry_run = args
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let target = match kind {
            "episode" => RedactTarget::Episode {
                id: target_id.to_string(),
            },
            "entity" => RedactTarget::Entity {
                space: space_id,
                entity_id: target_id.to_string(),
            },
            "predicate" => {
                let (entity_id, predicate) = target_id.split_once('/').ok_or_else(|| {
                    ToolErr::Params("predicate target_id must be 'entity_id/predicate'".into())
                })?;
                RedactTarget::PredicateScoped {
                    space: space_id,
                    entity_id: entity_id.to_string(),
                    predicate: predicate.to_string(),
                }
            }
            other => {
                return Err(ToolErr::Params(format!(
                    "unknown target_kind '{other}' (expected episode|entity|predicate)"
                )));
            }
        };
        if dry_run {
            let closure = self
                .brain
                .redact_dry_run(&target)
                .await
                .map_err(ToolErr::run)?;
            to_json(&closure)
        } else {
            let result = self
                .brain
                .redact(&target, &reason, "mcp")
                .await
                .map_err(ToolErr::run)?;
            to_json(&result)
        }
    }

    // ── Resources (DESIGN §12.2) ─────────────────────────────────────────

    fn resources_list(&self, _params: Option<&Value>) -> Value {
        json!({
            "resources": [{
                "uri": "space://personal",
                "name": "Space: personal",
                "description": "Space overview: entity count, episode count, contradictions, recent entities.",
                "mimeType": "application/json"
            }],
            "resourceTemplates": [{
                "uriTemplate": "space://{name}",
                "name": "Space by name",
                "description": "Overview of any space by name.",
                "mimeType": "application/json"
            }, {
                "uriTemplate": "entity://{id}",
                "name": "Entity by ID",
                "description": "An entity's current beliefs. Optional ?space=name.",
                "mimeType": "application/json"
            }, {
                "uriTemplate": "episode://{id}",
                "name": "Episode by ID",
                "description": "Full episode record. Optional ?space=name.",
                "mimeType": "application/json"
            }, {
                "uriTemplate": "graph://{entity}?depth=n",
                "name": "Graph around entity",
                "description": "Bounded subgraph. Optional &space=name &direction=out|in|both.",
                "mimeType": "application/json"
            }]
        })
    }

    async fn resources_read(&self, params: Option<&Value>) -> Result<Value, (i64, String)> {
        let p = params.ok_or((INVALID_PARAMS, "missing 'params'".into()))?;
        let uri = p
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or((INVALID_PARAMS, "missing 'uri'".into()))?;
        let (scheme, rest) = uri
            .split_once("://")
            .ok_or((INVALID_PARAMS, format!("invalid URI: {uri}")))?;
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let qp: std::collections::HashMap<&str, &str> = query
            .split('&')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.split_once('='))
            .collect();
        // For space:// URIs the path IS the space name. For entity/episode/graph
        // URIs, the space comes from the ?space= query param (default: personal).
        let space_name = if scheme == "space" {
            path
        } else {
            qp.get("space").copied().unwrap_or("personal")
        };
        let space_id = self
            .brain
            .ensure_space(space_name)
            .await
            .map_err(|e| (INTERNAL_ERROR, format!("ensure_space: {e}")))?;

        let text = match scheme {
            "space" => {
                let entities = self
                    .brain
                    .list_entities(&space_id, 100)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("list_entities: {e}")))?;
                let episode_count = self
                    .brain
                    .episode_count()
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("episode_count: {e}")))?;
                let contradictions = self
                    .brain
                    .contradictions(&space_id)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("contradictions: {e}")))?;
                let summary = json!({
                    "space": path,
                    "space_id": space_id,
                    "entity_count": entities.len(),
                    "episode_count": episode_count,
                    "contradiction_count": contradictions.len(),
                    "recent_entities": entities.iter().take(20).map(|e| json!({
                        "id": e.id, "type": e.ty, "canonical_key": e.canonical_key
                    })).collect::<Vec<_>>()
                });
                serde_json::to_string_pretty(&summary).unwrap_or_default()
            }
            "entity" => {
                let entity_id = path.trim_start_matches('/');
                let beliefs = self
                    .brain
                    .beliefs(&space_id, entity_id)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("beliefs: {e}")))?;
                serde_json::to_string_pretty(&beliefs).unwrap_or_default()
            }
            "episode" => {
                let episode_id = path.trim_start_matches('/');
                let episode = self
                    .brain
                    .get_episode(episode_id)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("get_episode: {e}")))?;
                match episode {
                    Some(ep) => serde_json::to_string_pretty(&ep).unwrap_or_default(),
                    None => {
                        return Err((INVALID_PARAMS, format!("episode not found: {episode_id}")));
                    }
                }
            }
            "graph" => {
                let entity_id = path.trim_start_matches('/');
                let depth = qp
                    .get("depth")
                    .and_then(|s| s.parse::<u8>().ok())
                    .unwrap_or(2);
                let direction = match qp.get("direction").copied() {
                    Some("out") => Direction::Out,
                    Some("in") => Direction::In,
                    _ => Direction::Both,
                };
                let spec = TraversalSpec {
                    start: vec![entity_id.to_string()],
                    max_depth: depth,
                    max_nodes: 256,
                    predicates: PredicateFilter::AllowAll,
                    direction,
                    valid_at: None,
                    min_confidence: 0.0,
                    strategy: Strategy::Bfs,
                };
                let result = self
                    .brain
                    .traverse(&space_id, spec)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("traverse: {e}")))?;
                serde_json::to_string_pretty(&result).unwrap_or_default()
            }
            other => {
                return Err((
                    METHOD_NOT_FOUND,
                    format!("unknown resource scheme: {other}"),
                ));
            }
        };
        Ok(json!({
            "contents": [{ "uri": uri, "mimeType": "application/json", "text": text }]
        }))
    }
}

// ── Argument helpers ───────────────────────────────────────────────────────

/// Tool-level error: `Params` (caller error → JSON-RPC -32602) or `Run`
/// (execution failure → MCP `isError` text result).
enum ToolErr {
    Params(String),
    Run(String),
}

impl ToolErr {
    fn run(e: impl std::fmt::Display) -> Self {
        Self::Run(e.to_string())
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolErr> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolErr::Params(format!("missing required argument '{key}'")))
}

fn str_arg_or(args: &Value, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn space_arg(args: &Value) -> String {
    str_arg_or(args, "space", "personal")
}

fn u_arg_or(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(default)
}

fn parse_mode(s: &str) -> QueryMode {
    match s {
        "lexical" => QueryMode::Lexical,
        "lexical-vector" => QueryMode::LexicalVector,
        "dense" => QueryMode::Dense,
        "graph" => QueryMode::Graph,
        "community" => QueryMode::Community,
        _ => QueryMode::Hybrid,
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, ToolErr> {
    serde_json::to_string_pretty(value).map_err(|e| ToolErr::Run(format!("serialize: {e}")))
}

// ── Tool catalogue ─────────────────────────────────────────────────────────

/// The advertised tool list with hand-written JSON Schemas (P4: no schemars).
fn tool_list() -> Value {
    json!({
        "tools": [
            tool("search",
                "Search the brain via hybrid/lexical/lexical-vector/graph/community retrieval. Returns ranked results with scores, targets, and provenance.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query text." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "mode": { "type": "string", "enum": ["hybrid","lexical","lexical-vector","graph","community"], "description": "Retrieval mode (default: hybrid)." },
                        "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default: 20)." }
                    },
                    "required": ["query"]
                })),
            tool("recall",
                "Assemble context for a query within a token budget — the per-turn call for agents. Returns layered context (pinned facts, high-salience beliefs, query neighborhood, recent episodes).",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What information to assemble." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "token_budget": { "type": "integer", "minimum": 1, "description": "Maximum tokens for the assembled context (default: 3000)." }
                    },
                    "required": ["query"]
                })),
            tool("get_entity",
                "Get an entity's current beliefs — all statements about it with status, confidence, and validity intervals.",
                json!({
                    "type": "object",
                    "properties": {
                        "entity_id": { "type": "string", "description": "The entity's content-derived ID." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["entity_id"]
                })),
            tool("ingest",
                "Ingest text content as a new Primary episode. Set extract:true to trigger realtime extraction via client sampling (§12.3) — the server asks the client's model to extract claims. Requires the Sample capability on authenticated sessions.",
                json!({
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The text to ingest." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "source_path": { "type": "string", "description": "Optional source label, e.g. a file path (default: mcp)." },
                        "extract": { "type": "boolean", "description": "If true, extract claims via client sampling immediately (default: false)." }
                    },
                    "required": ["content"]
                })),
            tool("declare",
                "Declare a statement deterministically (no LLM). Takes a declaration JSON: {op, subject, predicate, object, polarity, valid_from, valid_to}. Writes a Declaration episode.",
                json!({
                    "type": "object",
                    "properties": {
                        "declaration_json": { "type": "string", "description": "Canonical declaration JSON (op = add_statement | merge | retract)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["declaration_json"]
                })),
            tool("why",
                "Get provenance for a statement — supporting/denying assertions with confidence breakdown, extractors, and source episodes.",
                json!({
                    "type": "object",
                    "properties": {
                        "statement_id": { "type": "string", "description": "The statement ID." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["statement_id"]
                })),
            tool("contradictions",
                "List all contradicted statements in a space — statements with both affirming and denying support.",
                json!({
                    "type": "object",
                    "properties": {
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    }
                })),
            tool("stats",
                "Aggregate counts for a space: episodes, entities, statements, and contradicted statements.",
                json!({
                    "type": "object",
                    "properties": {
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    }
                })),
            tool("traverse",
                "Bounded subgraph traversal from a set of start entities. Returns nodes and edges within the depth/node budget. Useful for multi-hop recall (ToG driver).",
                json!({
                    "type": "object",
                    "properties": {
                        "start": { "type": "array", "items": { "type": "string" }, "description": "Entity IDs to start from (at least one required)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "depth": { "type": "integer", "minimum": 1, "description": "Max traversal depth (default: 3)." },
                        "max_nodes": { "type": "integer", "minimum": 1, "description": "Max nodes to return (default: 256)." },
                        "direction": { "type": "string", "enum": ["out","in","both"], "description": "Edge direction (default: both)." }
                    },
                    "required": ["start"]
                })),
            tool("timeline",
                "Belief intervals for an entity over a time range. Returns statements with validity windows, status, and recording timestamps.",
                json!({
                    "type": "object",
                    "properties": {
                        "entity_id": { "type": "string", "description": "The entity's content-derived ID." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "from": { "type": "integer", "description": "Start of range in Unix milliseconds (optional)." },
                        "to": { "type": "integer", "description": "End of range in Unix milliseconds (optional)." }
                    },
                    "required": ["entity_id"]
                })),
            tool("review_merges",
                "List entity merge records in a space — which entities were merged, by whom (rule/user/import), and when.",
                json!({
                    "type": "object",
                    "properties": {
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    }
                })),
            tool("remember",
                "One-shot ingest + sync extraction for short user facts. Ingests text as a Primary episode and immediately extracts claims via client sampling. Requires the Sample capability on authenticated sessions.",
                json!({
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The fact or note to remember." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "source_path": { "type": "string", "description": "Optional source label (default: remember)." }
                    },
                    "required": ["content"]
                })),
            tool("retract",
                "Write a denying assertion — retract a statement extracted from a specific episode. Creates a Declaration episode.",
                json!({
                    "type": "object",
                    "properties": {
                        "subject": { "type": "object", "description": "Entity ref: {\"surface\":\"...\",\"type\":\"...\"}", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} }, "required": ["surface","type"] },
                        "predicate": { "type": "string", "description": "Predicate name." },
                        "object": { "type": "object", "description": "Entity or literal object.", "properties": { "kind": {"type":"string","enum":["entity","literal"]} }, "required": ["kind"] },
                        "episode": { "type": "string", "description": "Episode ID the retraction applies to." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["subject", "predicate", "object", "episode"]
                })),
            tool("merge_entities",
                "Merge two entities: the loser is redirected to the winner. Creates a Declaration episode. Both refs use surface form + type.",
                json!({
                    "type": "object",
                    "properties": {
                        "loser": { "type": "object", "description": "Entity to merge away: {\"surface\":\"...\",\"type\":\"...\"}", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} }, "required": ["surface","type"] },
                        "winner": { "type": "object", "description": "Entity to keep: {\"surface\":\"...\",\"type\":\"...\"}", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} }, "required": ["surface","type"] },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["loser", "winner"]
                })),
            tool("redact",
                "Destructive: remove episodes, entities, or predicates from the brain. Writes audit first, then tombstones. Use dry_run to preview the closure first.",
                json!({
                    "type": "object",
                    "properties": {
                        "target_kind": { "type": "string", "enum": ["episode","entity","predicate"], "description": "What to redact." },
                        "target_id": { "type": "string", "description": "Episode ID, entity ID, or 'entity_id/predicate' for predicate kind." },
                        "reason": { "type": "string", "description": "Audit reason (default: 'mcp redact')." },
                        "dry_run": { "type": "boolean", "description": "Preview the closure without modifying anything (default: false)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["target_kind", "target_id"]
                })),
        ]
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

// ── transports ─────────────────────────────────────────────────────────────

/// Run one MCP session over a read/write pair: newline-delimited JSON-RPC.
///
/// Shared by the stdio, socket, and HTTP transports. Reads until EOF, writing
/// one response per line. Returns on EOF or a fatal IO error.
pub async fn run_session<R, W>(server: Arc<BrainServer>, reader: R, writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    session_loop(server, BufReader::new(reader), BufWriter::new(writer)).await
}

/// The bidirectional framing loop. Shared by `run_session` and `auth_session`.
///
/// Unlike a simple read→respond loop, this can send **server-initiated**
/// requests (e.g. `sampling/createMessage`, §12.3) and await the client's
/// response on the same stream. Architecture:
///
/// - A **write task** owns the writer and drains an outbound channel
///   (`UnboundedReceiver<Value>`). Every message the server sends — responses
///   to client requests AND server-initiated requests — goes through it.
/// - The **read loop** owns the reader. For each line:
///   - If it's a JSON-RPC *response* (no `method`, has matching `id`): resolve
///     the pending `oneshot` so the waiting handler completes.
///   - Otherwise it's a client request: dispatch in a spawned task (so the read
///     loop stays free to read sampling responses), sending the response (if
///     any) through the outbound channel.
async fn session_loop<R, W>(
    server: Arc<BrainServer>,
    mut reader: BufReader<R>,
    out: BufWriter<W>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    use tokio::sync::mpsc;

    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();
    let session = Arc::new(crate::sampling::SessionHandle::new(outbound_tx.clone()));

    // Write task: drain outbound → write to stream.
    let write_task = tokio::spawn(async move {
        let mut out = out;
        while let Some(msg) = outbound_rx.recv().await {
            let serialized =
                serde_json::to_string(&msg).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
            out.write_all(serialized.as_bytes()).await?;
            out.write_all(b"\n").await?;
            out.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    });

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| anyhow::anyhow!("read: {e}"))?;
        if n == 0 {
            break; // EOF — peer closed the stream.
        }
        if line.trim().is_empty() {
            continue;
        }

        // Is this a response to a server-initiated request?
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            if value.get("method").is_none() && value.get("id").is_some() {
                let id = value["id"].as_i64().unwrap_or(-1);
                if let Ok(mut guard) = session.pending.lock() {
                    if let Some(sender) = guard.remove(&id) {
                        let _ = sender.send(value);
                        continue;
                    }
                }
                // Unsolicited response — ignore, fall through to request parsing.
                continue;
            }
        }

        // Client request or notification: dispatch in a task.
        match Message::parse(&line) {
            Ok(msg) => {
                let server = server.clone();
                let session = session.clone();
                let outbound = outbound_tx.clone();
                tokio::spawn(async move {
                    if let Some(resp) = server.handle_with(msg, Some(&session)).await {
                        let _ = outbound.send(resp);
                    }
                });
            }
            Err((id, code, msg)) => {
                let _ = outbound_tx.send(error(id.unwrap_or(Value::Null), code, msg));
            }
        }
    }

    // Drop our senders so the write task's recv() can return None — but
    // spawned dispatch tasks may still hold clones. `session` (which owns an
    // outbound clone via SessionHandle) MUST be dropped here or the channel
    // never closes and the write task hangs forever on disconnect.
    drop(outbound_tx);
    drop(session);
    // Give in-flight dispatch tasks a brief grace period to finish and drop
    // their own outbound clones. If one is stuck on a sampling round-trip
    // (max 120s timeout), don't block the session from closing.
    match tokio::time::timeout(std::time::Duration::from_secs(5), write_task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => tracing::debug!("write task ended: {e}"),
        Ok(Err(e)) => tracing::debug!("write task join error: {e}"),
        Err(_) => tracing::debug!("write task did not exit within 5s, detaching"),
    }
    Ok(())
}

/// Run the MCP server on stdio (Claude Desktop and other MCP clients).
///
/// All diagnostics go to stderr — stdout is the protocol channel.
pub async fn serve_stdio(brain: Brain) -> anyhow::Result<()> {
    let server = Arc::new(BrainServer::from_brain(brain));
    run_session(server, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Open a Brain at `dir` and serve it over stdio.
pub async fn serve_stdio_at(dir: &std::path::Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    serve_stdio(brain).await
}

/// Run the MCP server on a Unix-domain socket (the daemon transport, §4.3).
///
/// Connections are served concurrently; each runs its own JSON-RPC session over
/// the shared `Brain`. The store actor serializes writes (P8 single writer), so
/// many readers + one writer is safe. A stale socket file is cleared before bind.
#[cfg(unix)]
pub async fn serve_socket(brain: Brain, path: &std::path::Path) -> anyhow::Result<()> {
    use tokio::net::UnixListener;
    let _ = std::fs::remove_file(path); // clear a stale socket file.
    let listener =
        UnixListener::bind(path).map_err(|e| anyhow::anyhow!("bind {}: {e}", path.display()))?;
    let server = Arc::new(BrainServer::from_brain(brain));
    tracing::info!("oxibrain MCP listening on {}", path.display());
    let shutdown = crate::daemon::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let server = server.clone();
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    if let Err(e) = run_session(server, read, write).await {
                        tracing::warn!("session ended: {e}");
                    }
                });
            }
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping socket listener");
                break;
            }
        }
    }
    Ok(())
}

/// Run the MCP server on a Unix-domain socket with token authentication.
///
/// Each connection must send a JSON-RPC `auth` request as its first message:
/// `{"jsonrpc":"2.0","id":1,"method":"auth","params":{"token":"<secret>"}}`.
/// The server verifies the token against the store and resolves a `Scope`.
/// On success, a scoped `BrainServer` serves the rest of the session. On
/// failure (invalid/expired/revoked token), the connection is refused.
///
/// This is the authenticated daemon transport (DESIGN §11.2): the scope gate
/// in `BrainServer::enforce_scope` now has a real scope to enforce.
#[cfg(unix)]
pub async fn serve_socket_auth(brain: Brain, path: &std::path::Path) -> anyhow::Result<()> {
    use tokio::net::UnixListener;
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).map_err(|e| anyhow::anyhow!("bind {}: {e}", path.display()))?;
    let brain = Arc::new(brain);
    tracing::info!("oxibrain MCP (auth) listening on {}", path.display());
    let shutdown = crate::daemon::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let brain = brain.clone();
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    if let Err(e) = auth_session(brain, read, write).await {
                        tracing::warn!("auth session ended: {e}");
                    }
                });
            }
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping auth socket listener");
                break;
            }
        }
    }
    Ok(())
}

/// Authenticate one connection, then run a scoped session.
///
/// Reads the first line as an `auth` request. On success, proceeds to the
/// normal session loop with the resolved scope. On failure, responds with an
/// `UNAUTHORIZED` error and closes.
#[cfg(unix)]
async fn auth_session<R, W>(brain: Arc<Brain>, reader: R, writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut out = BufWriter::new(writer);

    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .await
        .map_err(|e| anyhow::anyhow!("auth read: {e}"))?;
    if n == 0 {
        return Ok(()); // client disconnected before sending auth.
    }

    let id_for_response = Message::parse(&line)
        .ok()
        .and_then(|m| m.id)
        .unwrap_or(Value::Null);

    // Extract the token from `auth` params.
    let token = serde_json::from_str::<Value>(&line).ok().and_then(|v| {
        v.get("method")
            .and_then(|m| m.as_str())
            .filter(|m| m == &"auth")
            .and_then(|_| v.get("params")?.get("token")?.as_str().map(String::from))
    });

    let server = match token {
        Some(token) => match brain.verify_token(&token).await {
            Ok(Some(scope)) => {
                write_line(
                    &mut out,
                    &success(id_for_response, json!({"authenticated": true})),
                )
                .await?;
                Arc::new(BrainServer::from_arc_scoped(brain, scope))
            }
            Ok(None) => {
                write_line(
                    &mut out,
                    &error(id_for_response, UNAUTHORIZED, "invalid or revoked token"),
                )
                .await?;
                return Ok(());
            }
            Err(e) => {
                write_line(
                    &mut out,
                    &error(id_for_response, INTERNAL_ERROR, format!("verify: {e}")),
                )
                .await?;
                return Ok(());
            }
        },
        None => {
            write_line(
                &mut out,
                &error(
                    id_for_response,
                    INVALID_PARAMS,
                    "first message must be an auth request with a token",
                ),
            )
            .await?;
            return Ok(());
        }
    };

    session_loop(server, reader, out).await
}

/// Run the MCP server over loopback HTTP (DESIGN §11.6).
///
/// Each HTTP POST body is a JSON-RPC message; the response body is the
/// JSON-RPC response. This is the simplest HTTP mapping of newline-delimited
/// JSON-RPC — one request per connection, no streaming.
///
/// Loopback-only by default. A non-loopback bind is refused (§11.6: requires
/// TLS, which is out of scope for the in-house server; use a reverse proxy).
pub async fn serve_http(
    brain: Brain,
    addr: std::net::SocketAddr,
    ui_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if !addr.ip().is_loopback() {
        anyhow::bail!(
            "HTTP transport is loopback-only (DESIGN §11.6). \
             Use a TLS-terminating reverse proxy for remote access."
        );
    }
    use tokio::net::TcpListener;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("bind {addr}: {e}"))?;
    let server = Arc::new(BrainServer::from_brain(brain));
    let ui_dir = ui_dir.map(std::sync::Arc::new);
    if ui_dir.is_some() {
        tracing::info!("oxibrain HTTP (UI + API) listening on http://{addr}");
    } else {
        tracing::info!("oxibrain HTTP (API only) listening on http://{addr}");
    }
    let shutdown = crate::daemon::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, _) = accept?;
                let server = server.clone();
                let ui = ui_dir.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_http(server, stream, ui).await {
                        tracing::debug!("HTTP session: {e}");
                    }
                });
            }
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping HTTP listener");
                break;
            }
        }
    }
    Ok(())
}

/// Handle one HTTP connection.
///
/// GET requests serve static files from `ui_dir` (if set).
/// POST requests are JSON-RPC messages dispatched to the brain.
async fn handle_http(
    server: Arc<BrainServer>,
    stream: tokio::net::TcpStream,
    ui_dir: Option<std::sync::Arc<std::path::PathBuf>>,
) -> anyhow::Result<()> {
    let mut reader = BufReader::new(stream);

    // Read the request line + headers.
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("");
    let path = parts.get(1).copied().unwrap_or("/").to_string();

    // Consume headers.
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        let n = reader.read_line(&mut header).await?;
        if n == 0 || header.trim().is_empty() {
            break;
        }
        if let Some(rest) = header.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = header.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }

    match method {
        "POST" => handle_http_post(&server, &mut reader, content_length).await,
        "GET" => handle_http_get(&mut reader, &path, ui_dir).await,
        _ => write_http_response(&mut reader, 405, "Method Not Allowed", b"{}").await,
    }
}

/// Handle a POST (JSON-RPC) request.
async fn handle_http_post(
    server: &BrainServer,
    reader: &mut tokio::io::BufReader<tokio::net::TcpStream>,
    content_length: usize,
) -> anyhow::Result<()> {
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    let body_str = String::from_utf8_lossy(&body);

    let response = match Message::parse(&body_str) {
        Ok(msg) => server.handle(msg).await,
        Err((id, code, msg)) => Some(error(id.unwrap_or(Value::Null), code, msg)),
    };

    let response_json = match response {
        Some(v) => serde_json::to_string(&v)?,
        None => serde_json::to_string(&json!({"jsonrpc":"2.0","result":{}}))?,
    };

    write_http_response(reader, 200, "OK", response_json.as_bytes()).await
}

/// Handle a GET (static file) request.
async fn handle_http_get(
    reader: &mut tokio::io::BufReader<tokio::net::TcpStream>,
    path: &str,
    ui_dir: Option<std::sync::Arc<std::path::PathBuf>>,
) -> anyhow::Result<()> {
    let Some(ui_dir) = ui_dir else {
        return write_http_response(
            reader,
            404,
            "Not Found",
            br#"{"error":"no UI directory configured"}"#,
        )
        .await;
    };

    // Strip query string, map to file path.
    let rel = path.split('?').next().unwrap_or("/");
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    let file_path = if rel.is_empty() || rel == "/" {
        ui_dir.join("index.html")
    } else {
        // Prevent directory traversal.
        let safe: std::path::PathBuf = rel
            .split('/')
            .filter(|s| !s.contains("..") && !s.is_empty())
            .collect();
        ui_dir.join(safe)
    };

    match tokio::fs::read(&file_path).await {
        Ok(data) => {
            let ct = content_type(&file_path);
            write_http_response_with_ct(reader, 200, "OK", ct, &data).await
        }
        Err(_) => {
            // SPA fallback: serve index.html for client-side routing.
            match tokio::fs::read(ui_dir.join("index.html")).await {
                Ok(data) => {
                    write_http_response_with_ct(
                        reader,
                        200,
                        "OK",
                        "text/html; charset=utf-8",
                        &data,
                    )
                    .await
                }
                Err(_) => write_http_response(reader, 404, "Not Found", b"not found").await,
            }
        }
    }
}

/// Determine content type from file extension.
fn content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff") | Some("woff2") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Write a minimal HTTP response. `reader` is the TcpStream (we write back
/// through the underlying buffer).
async fn write_http_response(
    stream: &mut tokio::io::BufReader<tokio::net::TcpStream>,
    status: u16,
    reason: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.get_mut().write_all(header.as_bytes()).await?;
    stream.get_mut().write_all(body).await?;
    stream.get_mut().flush().await?;
    Ok(())
}

/// Write an HTTP response with a custom Content-Type.
async fn write_http_response_with_ct(
    stream: &mut tokio::io::BufReader<tokio::net::TcpStream>,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.get_mut().write_all(header.as_bytes()).await?;
    stream.get_mut().write_all(body).await?;
    stream.get_mut().flush().await?;
    Ok(())
}

/// Serialize a JSON value + newline to a BufWriter.
async fn write_line<W: AsyncWrite + Unpin>(
    out: &mut BufWriter<W>,
    value: &Value,
) -> anyhow::Result<()> {
    let s = serde_json::to_string(value)?;
    out.write_all(s.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain_ports::TIME_MAX;
    use serde_json::json;

    fn msg(id: i64, method: &str, params: Option<Value>) -> Message {
        Message {
            id: Some(json!(id)),
            method: method.into(),
            params,
        }
    }

    async fn fresh_server() -> (tempfile::TempDir, BrainServer) {
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        (dir, BrainServer::from_brain(brain))
    }

    #[tokio::test]
    async fn initialize_advertises_tools_and_negotiates_version() {
        let (_dir, server) = fresh_server().await;

        // Default version when the client requests an unknown one.
        let resp = server
            .handle(msg(
                1,
                "initialize",
                Some(json!({"protocolVersion":"1999"})),
            ))
            .await
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], DEFAULT_PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "oxibrain");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert!(resp["result"]["capabilities"]["resources"].is_object());

        // 2026-07-28 is echoed when requested.
        let resp = server
            .handle(msg(
                2,
                "initialize",
                Some(json!({"protocolVersion":"2026-07-28"})),
            ))
            .await
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2026-07-28");
    }

    #[tokio::test]
    async fn tools_list_advertises_full_surface() {
        let (_dir, server) = fresh_server().await;
        let resp = server.handle(msg(1, "tools/list", None)).await.unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "search",
            "recall",
            "get_entity",
            "traverse",
            "timeline",
            "why",
            "contradictions",
            "stats",
            "review_merges",
            "ingest",
            "remember",
            "declare",
            "retract",
            "merge_entities",
            "redact",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
            let tool = resp["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == expected)
                .unwrap();
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn ping_responds_and_unknown_method_errors() {
        let (_dir, server) = fresh_server().await;

        let resp = server.handle(msg(1, "ping", None)).await.unwrap();
        assert!(resp["result"].is_object());

        let resp = server.handle(msg(2, "no/such/method", None)).await.unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn notification_yields_no_response() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(Message {
                id: None,
                method: "notifications/initialized".into(),
                params: None,
            })
            .await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn unknown_tool_is_method_not_found() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(1, "tools/call", Some(json!({"name":"frobnicate"}))))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_required_arg_is_invalid_params() {
        let (_dir, server) = fresh_server().await;
        // search without 'query'
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({"name":"search","arguments":{}})),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn declare_then_get_entity_round_trip() {
        let (_dir, server) = fresh_server().await;

        // Declare "Alice employed_by Acme Corp" — deterministic, no LLM.
        let decl = json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": TIME_MAX.0
        });
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "declare",
                    "arguments": { "space": "t", "declaration_json": decl.to_string() }
                })),
            ))
            .await
            .unwrap();
        assert!(resp.get("result").is_some());
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Declared as episode"), "got: {text}");

        // Resolve Alice's entity id, then read back her beliefs.
        let space_id = server.brain.ensure_space("t").await.unwrap();
        let alice = server
            .brain
            .resolve_entity_id(&space_id, "Person", "Alice")
            .await
            .unwrap()
            .expect("Alice should exist after declare");

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "get_entity",
                    "arguments": { "space": "t", "entity_id": alice }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // Belief carries the statement ID + status + confidence (predicate lives
        // on Statement, not Belief). One active belief at full confidence.
        let beliefs: Vec<Value> = serde_json::from_str(text).expect("beliefs parse");
        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0]["status"], "active");
        assert_eq!(beliefs[0]["confidence"], 1.0);
    }

    #[tokio::test]
    async fn ingest_creates_episode() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "ingest",
                    "arguments": { "space": "t", "content": "A short note about nothing." }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Ingested as episode"), "got: {text}");
    }

    fn scope_for(caps: &[Capability], spaces: &[&str]) -> Scope {
        Scope {
            spaces: spaces.iter().map(|s| s.to_string()).collect(),
            caps: caps.iter().copied().collect(),
            predicate_filter: None,
            entity_type_filter: None,
            expires_at: None,
        }
    }

    async fn fresh_scoped(
        caps: &[Capability],
        spaces: &[&str],
    ) -> (tempfile::TempDir, BrainServer) {
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        // Resolve space ids so the scope names content-derived ids, not labels.
        let mut ids = Vec::new();
        for s in spaces {
            ids.push(brain.ensure_space(s).await.unwrap());
        }
        let scope = Scope {
            spaces: ids,
            ..scope_for(caps, spaces)
        };
        (dir, BrainServer::from_brain_scoped(brain, scope))
    }

    #[tokio::test]
    async fn stats_tool_reports_counts() {
        let (_dir, server) = fresh_server().await;
        // Ingest one note into the space.
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "ingest",
                    "arguments": { "space": "t", "content": "The sky is blue.", "source_path": "test" }
                })),
            ))
            .await
            .unwrap();
        assert!(resp.get("result").is_some(), "ingest should succeed");

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "stats", "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let stats: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(stats["episodes"], 1, "one ingested episode: {text}");
        assert_eq!(
            stats["statements"], 0,
            "no extraction ran (ingest only): {text}"
        );
        assert_eq!(stats["contradictions"], 0, "no contradictions: {text}");
        assert!(
            stats["entities"].as_i64().unwrap() >= 0,
            "entities count present: {text}"
        );
    }

    #[tokio::test]
    async fn scoped_read_only_denies_ingest_and_declare() {
        let (_dir, server) = fresh_scoped(&[Capability::Read], &["t"]).await;

        // Read is allowed (returns a result, possibly empty).
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "contradictions", "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        assert!(resp.get("result").is_some(), "read should be permitted");

        // Ingest requires Ingest cap → denied.
        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "ingest", "arguments": { "space": "t", "content": "x" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);

        // Declare requires Write cap → denied.
        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(json!({
                    "name": "declare", "arguments": { "space": "t", "declaration_json": "{}" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);
    }

    #[tokio::test]
    async fn scoped_write_allows_declare() {
        let (dir, server) = fresh_scoped(&[Capability::Read, Capability::Write], &["t"]).await;
        let space_id = server.brain.ensure_space("t").await.unwrap();
        let decl = json!({
            "op": "add_statement",
            "subject": { "surface": "Bob", "type": "Person" },
            "predicate": "full_name",
            "object": { "kind": "literal", "literal_type": "text", "value": "Bob Smith" },
            "polarity": "affirm",
            "valid_from": 0,
            "valid_to": TIME_MAX.0
        });
        let resp = server
            .handle(msg(1, "tools/call", Some(json!({
                "name": "declare", "arguments": { "space": "t", "declaration_json": decl.to_string() }
            }))))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Declared as episode"), "got: {text}");
        // Reference dir to avoid unused warnings; the TempDir guards cleanup.
        drop(dir);
        drop(space_id);
    }

    #[tokio::test]
    async fn scoped_to_one_space_denies_another() {
        // Token scoped to space "alpha" only.
        let (_dir, server) =
            fresh_scoped(&[Capability::Read, Capability::Ingest], &["alpha"]).await;
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "ingest", "arguments": { "space": "beta", "content": "x" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not scoped")
        );
    }

    #[tokio::test]
    async fn expired_scope_denies_all() {
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let sid = brain.ensure_space("t").await.unwrap();
        // Expiry in the past.
        let scope = Scope {
            spaces: vec![sid],
            caps: [Capability::Read].into_iter().collect(),
            expires_at: Some(oxibrain_ports::Timestamp::from_millis(1)),
            ..Default::default()
        };
        let server = BrainServer::from_brain_scoped(brain, scope);
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "contradictions", "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);
    }

    #[tokio::test]
    async fn run_session_round_trips_over_a_byte_stream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
        // client=a, server=b: a writes → b reads, b writes → a reads.
        let (mut client, server_side) = duplex(8 * 1024);
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let server = Arc::new(BrainServer::from_brain(brain));
        let (read_half, write_half) = tokio::io::split(server_side);
        let _task = tokio::spawn(run_session(server, read_half, write_half));

        // initialize
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = client.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "oxibrain");

        // a second round-trip — tools/list — proves the loop keeps serving.
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert!(resp["result"]["tools"].as_array().unwrap().len() >= 7);
    }

    #[tokio::test]
    async fn sampling_round_trip_through_bidirectional_session() {
        // End-to-end test of the bidirectional session loop: client sends
        // ingest+extract, server sends sampling/createMessage back, client
        // responds with a model completion, extraction runs.
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

        let (client, server_side) = duplex(64 * 1024);
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let server = Arc::new(BrainServer::from_brain(brain));
        let (read_half, write_half) = tokio::io::split(server_side);
        let _task = tokio::spawn(run_session(server, read_half, write_half));

        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        // 1. Client sends ingest with extract:true.
        let ingest_req = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "ingest", "arguments": {
                "content": "Alice works at Acme Corp.",
                "extract": true, "space": "t"
            }}
        }))
        .unwrap();
        client_write.write_all(ingest_req.as_bytes()).await.unwrap();
        client_write.write_all(b"\n").await.unwrap();
        client_write.flush().await.unwrap();

        // 2. Server sends sampling/createMessage (a server-initiated request).
        let mut line = String::new();
        client_read.read_line(&mut line).await.unwrap();
        assert!(!line.is_empty(), "should receive sampling request");
        let sampling_req: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(sampling_req["method"], "sampling/createMessage");
        let sampling_id = sampling_req["id"].as_i64().unwrap();
        assert!(
            sampling_req["params"]["messages"][0]["content"]["text"]
                .as_str()
                .unwrap()
                .contains("Alice works at Acme Corp."),
            "episode content should reach the client's model"
        );

        // 3. Client responds with a model completion (empty claims).
        let claims_text = serde_json::to_string(&json!({"claims":[]})).unwrap();
        let sampling_resp = serde_json::to_string(&json!({
            "jsonrpc": "2.0", "id": sampling_id,
            "result": {
                "role": "assistant",
                "content": {"type": "text", "text": claims_text},
                "model": "test-model"
            }
        }))
        .unwrap();
        client_write
            .write_all(sampling_resp.as_bytes())
            .await
            .unwrap();
        client_write.write_all(b"\n").await.unwrap();
        client_write.flush().await.unwrap();

        // 4. Read the final ingest tool response.
        line.clear();
        client_read.read_line(&mut line).await.unwrap();
        let ingest_resp: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(ingest_resp["id"], 1, "matches original tools/call id");
        let text = ingest_resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected result, got: {ingest_resp}"));
        assert!(
            text.contains("Ingested + extracted"),
            "sampling round-trip should complete extraction, got: {text}"
        );
    }

    #[tokio::test]
    async fn session_loop_returns_on_client_disconnect() {
        // Regression: the bidirectional session loop must return Ok(()) when the
        // client disconnects — not hang forever. Before the `drop(session)` fix,
        // the SessionHandle held an outbound_tx clone that kept the write task's
        // recv() alive permanently. This test awaits the JoinHandle (not _task)
        // so a hang fails the test instead of being masked by runtime teardown.
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

        let (mut client, server_side) = duplex(8 * 1024);
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let server = Arc::new(BrainServer::from_brain(brain));
        let (read_half, write_half) = tokio::io::split(server_side);
        let task = tokio::spawn(run_session(server, read_half, write_half));

        // Exchange one request so the dispatch path runs.
        client
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        client.flush().await.unwrap();
        let mut buf = vec![0u8; 256];
        let _ = client.read(&mut buf).await.unwrap();

        // Close the client → server reads EOF → session_loop must return.
        drop(client);
        let result = tokio::time::timeout(Duration::from_secs(3), task).await;
        assert!(
            result.is_ok(),
            "session_loop should exit on client disconnect, not hang"
        );
        assert!(result.unwrap().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_transport_serves_a_real_connection() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let sock = dir.path().join("mcp.sock");
        let sock_for_task = sock.clone();
        let _task = tokio::spawn(async move {
            let _ = serve_socket(brain, &sock_for_task).await;
        });

        // Retry-connect until the listener is bound (up to ~1s).
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(&sock).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut stream = stream.expect("could not connect to MCP socket");

        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(resp["id"], 1);
        assert!(resp.get("result").is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_auth_valid_token_then_tool_call() {
        use oxibrain::{Capability, Scope};
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let space_id = brain.ensure_space("t").await.unwrap();

        // Issue a token with Read + Ingest.
        let scope = Scope {
            spaces: vec![space_id],
            caps: [Capability::Read, Capability::Ingest].into_iter().collect(),
            ..Default::default()
        };
        let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();

        let sock = dir.path().join("auth.sock");
        let sock_task = dir.path().join("auth.sock");
        let _task = tokio::spawn(async move {
            let _ = serve_socket_auth(brain, &sock_task).await;
        });

        // Wait for listener.
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(&sock).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut stream = stream.expect("connect to auth socket");

        // Send auth.
        let auth = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"auth\",\"params\":{{\"token\":\"{secret}\"}}}}\n"
        );
        stream.write_all(auth.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(resp["result"]["authenticated"], true);

        // Now call contradictions (Read cap).
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"contradictions\",\"arguments\":{\"space\":\"t\"}}}\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let n = stream.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert!(resp.get("result").is_some(), "read tool should succeed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_auth_invalid_token_refused() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let sock = dir.path().join("bad.sock");
        let sock_task = dir.path().join("bad.sock");
        let _task = tokio::spawn(async move {
            let _ = serve_socket_auth(brain, &sock_task).await;
        });

        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = UnixStream::connect(&sock).await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut stream = stream.expect("connect to auth socket");

        // Send bad token.
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"auth\",\"params\":{\"token\":\"bogus\"}}\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_transport_serves_a_jsonrpc_request() {
        use std::time::Duration;
        use tokio::net::TcpStream;

        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let addr: std::net::SocketAddr = "127.0.0.1:18099".parse().unwrap();
        let _task = tokio::spawn(async move {
            let _ = serve_http(brain, addr, None).await;
        });

        // Wait for listener.
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(s) = TcpStream::connect("127.0.0.1:18099").await {
                stream = Some(s);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut stream = stream.expect("connect to HTTP server");

        // Send an HTTP POST with a JSON-RPC ping.
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = format!(
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response_str = String::from_utf8_lossy(&response);
        assert!(response_str.contains("200 OK"), "got: {response_str}");
        assert!(response_str.contains("\"id\":1"), "got: {response_str}");
        assert!(response_str.contains("\"result\""), "got: {response_str}");
    }

    // ── Tests for new §12.2 tools ────────────────────────────────────────

    /// Helper: declare a statement and return the episode id + subject entity id.
    async fn declare_alice(server: &BrainServer, space: &str) -> (String, String) {
        let decl = json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": TIME_MAX.0
        });
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "declare",
                    "arguments": { "space": space, "declaration_json": decl.to_string() }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let ep_id = text.replace("Declared as episode: ", "");
        let space_id = server.brain.ensure_space(space).await.unwrap();
        let alice = server
            .brain
            .resolve_entity_id(&space_id, "Person", "Alice")
            .await
            .unwrap()
            .expect("Alice should exist");
        (ep_id, alice)
    }

    #[tokio::test]
    async fn traverse_returns_subgraph() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "traverse",
                    "arguments": { "space": "t", "start": [alice], "depth": 2 }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).expect("traverse parse");
        assert!(
            !result["nodes"].as_array().unwrap().is_empty(),
            "got: {text}"
        );
        assert!(
            !result["edges"].as_array().unwrap().is_empty(),
            "got: {text}"
        );
    }

    #[tokio::test]
    async fn traverse_missing_start_is_invalid_params() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "traverse",
                    "arguments": { "space": "t", "start": [] }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn timeline_returns_entries() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "timeline",
                    "arguments": { "space": "t", "entity_id": alice }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let entries: Vec<Value> = serde_json::from_str(text).expect("timeline parse");
        assert!(!entries.is_empty(), "should have timeline entries");
        assert_eq!(entries[0]["predicate"], "employed_by");
    }

    #[tokio::test]
    async fn review_merges_lists_records() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        // Declare a second entity, then merge it into Alice.
        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "merge_entities",
                    "arguments": {
                        "space": "t",
                        "loser": { "surface": "Aliss", "type": "Person" },
                        "winner": { "surface": "Alice", "type": "Person" }
                    }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Merged as episode"), "got: {text}");

        // review_merges should show the merge record.
        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(json!({
                    "name": "review_merges",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let merges: Vec<Value> = serde_json::from_str(text).expect("merges parse");
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0]["winner"], alice);
    }

    #[tokio::test]
    async fn retract_denies_a_statement() {
        let (_dir, server) = fresh_server().await;
        let (ep_id, _alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(2, "tools/call", Some(json!({
                "name": "retract",
                "arguments": {
                    "space": "t",
                    "subject": { "surface": "Alice", "type": "Person" },
                    "predicate": "employed_by",
                    "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
                    "episode": ep_id
                }
            }))))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Retracted as episode"), "got: {text}");
    }

    #[tokio::test]
    async fn redact_dry_run_previews_closure() {
        let (_dir, server) = fresh_server().await;
        let (ep_id, _alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "redact",
                    "arguments": {
                        "space": "t",
                        "target_kind": "episode",
                        "target_id": ep_id,
                        "dry_run": true
                    }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let closure: Value = serde_json::from_str(text).expect("closure parse");
        assert!(
            closure["episodes"]
                .as_array()
                .unwrap()
                .contains(&json!(ep_id))
        );
    }

    #[tokio::test]
    async fn remember_ingests_without_session() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "remember",
                    "arguments": { "space": "t", "content": "Quick fact: the sky is blue." }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // Without a sampling session, extraction is skipped but episode ingested.
        assert!(text.contains("Ingested as episode"), "got: {text}");
        assert!(text.contains("extraction skipped"), "got: {text}");
    }

    #[tokio::test]
    async fn scoped_read_denies_redact() {
        let (_dir, server) = fresh_scoped(&[Capability::Read], &["t"]).await;
        let resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({
                    "name": "redact",
                    "arguments": { "space": "t", "target_kind": "episode", "target_id": "x" }
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], UNAUTHORIZED);
    }

    // ── Resources tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn resources_list_returns_templates() {
        let (_dir, server) = fresh_server().await;
        let resp = server.handle(msg(1, "resources/list", None)).await.unwrap();
        let templates = resp["result"]["resourceTemplates"].as_array().unwrap();
        let uris: Vec<&str> = templates
            .iter()
            .map(|t| t["uriTemplate"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"entity://{id}"));
        assert!(uris.contains(&"episode://{id}"));
        assert!(uris.contains(&"graph://{entity}?depth=n"));
    }

    #[tokio::test]
    async fn resources_read_space_returns_overview() {
        let (_dir, server) = fresh_server().await;
        let (_ep, _alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                1,
                "resources/read",
                Some(json!({
                    "uri": "space://t"
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let overview: Value = serde_json::from_str(text).expect("space parse");
        assert!(overview["entity_count"].as_u64().unwrap() >= 1);
        assert!(overview["episode_count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn resources_read_entity_returns_beliefs() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let uri = format!("entity://{alice}?space=t");
        let resp = server
            .handle(msg(1, "resources/read", Some(json!({ "uri": uri }))))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let beliefs: Vec<Value> = serde_json::from_str(text).expect("beliefs parse");
        assert_eq!(beliefs.len(), 1);
    }

    #[tokio::test]
    async fn resources_read_episode_returns_record() {
        let (_dir, server) = fresh_server().await;
        let (ep_id, _alice) = declare_alice(&server, "t").await;

        let uri = format!("episode://{ep_id}?space=t");
        let resp = server
            .handle(msg(1, "resources/read", Some(json!({ "uri": uri }))))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let episode: Value = serde_json::from_str(text).expect("episode parse");
        assert_eq!(episode["id"], ep_id);
    }

    #[tokio::test]
    async fn resources_read_graph_returns_traversal() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let uri = format!("graph://{alice}?depth=2&space=t");
        let resp = server
            .handle(msg(1, "resources/read", Some(json!({ "uri": uri }))))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let result: Value = serde_json::from_str(text).expect("graph parse");
        assert!(!result["nodes"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn resources_read_unknown_scheme_is_error() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                "resources/read",
                Some(json!({
                    "uri": "ftp://nope"
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn http_transport_rejects_non_loopback() {
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let addr: std::net::SocketAddr = "0.0.0.0:8080".parse().unwrap();
        let result = serve_http(brain, addr, None).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("loopback"), "got: {msg}");
    }
}
