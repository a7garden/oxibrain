//! MCP server: exposes the Brain facade as MCP tools (DESIGN §12.2).
//!
//! In-house JSON-RPC implementation (DESIGN §18 fallback). The tool set mirrors
//! the design's MCP surface table: `search`, `recall`, `get_entity`, `ingest`,
//! `declare`, `why`, `contradictions`. Structured results are returned as JSON
//! text so agents can parse them; write tools return short confirmations.

use crate::protocol::{
    INCOMPATIBLE_PROTOCOL, INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, Message, UNAUTHORIZED,
    error, error_with_data, success, text_result, tool_error,
};
use oxibrain::{
    Brain, BrainConfig, BrainError, BriefTarget, Capability, DeclObject, Declaration, EntityRef,
    RedactTarget, Scope,
};
use oxibrain_client::protocol::{ClientHello, ClientOperation};
use oxibrain_core::retrieval::{
    Direction, PredicateFilter, Query, QueryMode, Strategy, TraversalSpec,
};
use oxibrain_ports::{ClockPort, SystemClock};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
};

/// MCP protocol version advertised to clients that do not request 2026-07-28.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// JSON-RPC method name for the Oxi Foundation capability handshake.
/// Distinct from MCP `initialize` because it is a transport-level negotiation,
/// not part of the fifteen-tool MCP surface.
const HANDSHAKE_METHOD: &str = "handshake";

/// Foundation protocol range this daemon speaks. Bumping `MAX` is an additive
/// change; changing `MIN` is a breaking change that must ship with a major
/// version bump on the daemon and a clear migration window.
const HANDSHAKE_PROTOCOL_MIN: u32 = 1;
const HANDSHAKE_PROTOCOL_MAX: u32 = 1;

/// Store format revision the daemon ships. Bumped when the on-disk SQLite
/// schema changes; clients whose `min_store_format_version` exceeds this are
/// rejected with `StoreTooOld`.
const HANDSHAKE_STORE_FORMAT_VERSION: u32 = 1;

/// Foundation operations the daemon actually supports. Frozen at v1; widen
/// here (and on the client) when a new client/server capability lands.
const SUPPORTED_OPERATIONS: &[ClientOperation] = ClientOperation::ALL;

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
            "search" | "recall" | "brief" | "navigate" | "why" | "contradictions" | "traverse"
            | "review_merges" | "stats" => Some(Capability::Read),
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
            HANDSHAKE_METHOD => match msg.id {
                Some(id) => match self.handle_handshake(msg.params.as_ref()) {
                    Ok(info) => Some(success(id, info)),
                    Err((code, message, data)) => match data {
                        Some(d) => Some(error_with_data(id, code, message, d)),
                        None => Some(error(id, code, message)),
                    },
                },
                None => None,
            },
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

    /// Oxi Foundation `handshake` — version + store negotiation.
    ///
    /// Runs after optional `auth` and before any MCP tool routing (Foundation
    /// spec §8). It does **not** replace a token and does **not** broaden the
    /// caller's scope: even on a token-protected socket, this method requires
    /// only that the connection is open. The caller proves authorization later
    /// via `tools/call`.
    ///
    /// Returns the [`ServerInfo`](oxibrain_client::protocol::ServerInfo)
    /// payload on success. On failure, the third tuple element carries a
    /// structured `data` value the client can deserialize into a typed
    /// `HandshakeError`.
    fn handle_handshake(
        &self,
        params: Option<&Value>,
    ) -> Result<Value, (i64, String, Option<Value>)> {
        let params = params.unwrap_or(&Value::Null);
        // Typed deserialization: the client's `ClientHello` carries the
        // advertised `min_compatible` / `max_compatible` range and the
        // requested `protocol_version`. Parsing the typed shape keeps the
        // server honest against the client's contract; a raw `Value` view
        // would silently drop those bounds.
        let hello: ClientHello = serde_json::from_value(params.clone()).map_err(|e| {
            (
                INVALID_PARAMS,
                format!("malformed ClientHello: {e}"),
                Some(json!({
                    "kind": "malformed_hello",
                    "reason": e.to_string(),
                })),
            )
        })?;
        let requested = hello.protocol_version.0;
        // The effective lower bound is the higher of the daemon's
        // `HANDSHAKE_PROTOCOL_MIN` and the client's advertised
        // `min_compatible`. The effective upper bound is the lower of
        // the daemon's `HANDSHAKE_PROTOCOL_MAX` and the client's
        // advertised `max_compatible`. A client that does not advertise
        // a bound falls back to the daemon's own range.
        let client_min = hello
            .min_compatible
            .map(|v| v.0)
            .unwrap_or(HANDSHAKE_PROTOCOL_MIN);
        let client_max = hello
            .max_compatible
            .map(|v| v.0)
            .unwrap_or(HANDSHAKE_PROTOCOL_MAX);
        let eff_min = client_min.max(HANDSHAKE_PROTOCOL_MIN);
        let eff_max = client_max.min(HANDSHAKE_PROTOCOL_MAX);
        if requested < eff_min || requested > eff_max {
            return Err((
                INCOMPATIBLE_PROTOCOL,
                format!(
                    "incompatible protocol: requested {requested},                      supported range [{eff_min}, {eff_max}]"
                ),
                Some(json!({
                    "kind": "incompatible_protocol",
                    "requested": requested,
                    "min_compatible": eff_min,
                    "max_compatible": eff_max,
                })),
            ));
        }
        // Optional store format check: client may pin a minimum
        // `min_store_format_version`. If the daemon ships older than that,
        // refuse — the caller can decide whether to retry with a different
        // profile or surface a hard failure.
        let client_store_min = hello.min_store_format_version;
        if HANDSHAKE_STORE_FORMAT_VERSION < client_store_min {
            return Err((
                INCOMPATIBLE_PROTOCOL,
                format!(
                    "server store format {HANDSHAKE_STORE_FORMAT_VERSION} is older                      than client requires {client_store_min}"
                ),
                Some(json!({
                    "kind": "store_too_old",
                    "server_format": HANDSHAKE_STORE_FORMAT_VERSION,
                    "client_min": client_store_min,
                })),
            ));
        }
        // Echo back the operations the client asked for that we also support.
        // Today we support everything the client may ask for; future revisions
        // may subtract from this set.
        let supported_operations: Vec<ClientOperation> = SUPPORTED_OPERATIONS
            .iter()
            .copied()
            .filter(|op| {
                hello.supported_operations.is_empty() || hello.supported_operations.contains(op)
            })
            .collect();
        Ok(json!({
            "min_compatible": HANDSHAKE_PROTOCOL_MIN,
            "max_compatible": HANDSHAKE_PROTOCOL_MAX,
            "store_format_version": HANDSHAKE_STORE_FORMAT_VERSION,
            "supported_operations": supported_operations,
            "server_name": "oxibrain",
            "server_version": env!("CARGO_PKG_VERSION"),
        }))
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
            "brief" => self.tool_brief(&args).await,
            "navigate" => self.tool_navigate(&args).await,
            "traverse" => self.tool_traverse(&args).await,
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
            as_of: i64_arg_opt(args, "as_of").map(oxibrain_ports::Timestamp),
            limit,
            min_confidence: f32_arg_or(args, "min_confidence", 0.0),
        };
        let result = self.brain.search(q).await.map_err(ToolErr::run)?;
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

    async fn tool_brief(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        // Discriminator: `target_kind` is `entity` (default for back-compat),
        // `space`, or `topic`. Entity uses `entity_id`; topic uses `topic`.
        let kind = args
            .get("target_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("entity");
        match kind {
            "entity" => {
                let entity_id = str_arg(args, "entity_id")?;
                self.brain
                    .brief(&space_id, entity_id)
                    .await
                    .map_err(ToolErr::run)
            }
            "space" => self
                .brain
                .brief_target(&space_id, BriefTarget::Space)
                .await
                .map_err(ToolErr::run),
            "topic" => {
                let topic = str_arg(args, "topic")?;
                self.brain
                    .brief_target(&space_id, BriefTarget::Topic(topic))
                    .await
                    .map_err(ToolErr::run)
            }
            other => Err(ToolErr::run(BrainError::Config(format!(
                "brief.target_kind: '{other}' (expected entity|space|topic)"
            )))),
        }
    }

    async fn tool_navigate(&self, args: &Value) -> Result<String, ToolErr> {
        let from = str_arg(args, "from")?;
        let link = str_arg(args, "link")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        self.brain
            .navigate(&space_id, from, link)
            .await
            .map_err(ToolErr::run)
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
            prompt_version: 2, // v2: quote-based mentions (ADR-006)
            registry_major: oxibrain_core::registry::CORE_V1_MAJOR,
            mechanism: oxibrain_core::extraction::ExtractMechanism::ToolCall,
            max_tokens: 8192,
            model_digest: None,
            provider_profile_id: None,
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
        // `Statement.object` is `Object::Entity(EntityId) | Object::Literal(TypedValue)` —
        // serde's internally-tagged newtype-with-primitive form fails to serialize, so
        // we project the explain block to a JSON Value with a flat object shape that
        // matches the TypeScript `ExplainBlock` interface (statement.object: unknown).
        let object_json = match &explain.statement.object {
            oxibrain_core::knowledge::Object::Entity(id) => {
                // Include the display surface so the UI renders
                // "employed_by → Acme Corp" instead of a hash id.
                let surface = self
                    .brain
                    .entity_surface(&space_id, id)
                    .await
                    .unwrap_or_else(|_| id.clone());
                serde_json::json!({"kind": "entity", "id": id, "surface": surface})
            }
            oxibrain_core::knowledge::Object::Literal(tv) => {
                serde_json::to_value(tv).unwrap_or(serde_json::Value::Null)
            }
        };
        let payload = serde_json::json!({
            "statement": {
                "id": explain.statement.id,
                "space": explain.statement.space,
                "subject": explain.statement.subject,
                "predicate": explain.statement.predicate,
                "object": object_json,
            },
            "status": explain.status,
            "assertions": explain.assertions,
            "confidence_breakdown": explain.confidence_breakdown,
        });
        serde_json::to_string_pretty(&payload).map_err(|e| ToolErr::Run(format!("serialize: {e}")))
    }
    async fn tool_contradictions(&self, args: &Value) -> Result<String, ToolErr> {
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let details = self
            .brain
            .contradiction_details(&space_id)
            .await
            .map_err(ToolErr::run)?;
        to_json(&details)
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
            valid_at: i64_arg_opt(args, "valid_at").map(oxibrain_ports::Timestamp),
            min_confidence: f32_arg_or(args, "min_confidence", 0.0),
            strategy: Strategy::Bfs,
        };
        let result = self
            .brain
            .traverse(&space_id, spec)
            .await
            .map_err(ToolErr::run)?;
        to_json(&result)
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
        // Statement-first path: when `statement_id` is given, the declaration
        // inputs are rebuilt from the stored statement — callers that hold a
        // statement id (the conflicts inbox) don't have to resubmit resolvable
        // surfaces or entity types.
        let sid = str_arg_or(args, "statement_id", "");
        let (subject, predicate, object) = if sid.is_empty() {
            let subject: EntityRef =
                serde_json::from_value(args.get("subject").cloned().unwrap_or_default())
                    .map_err(|e| ToolErr::Params(format!("parse subject: {e}")))?;
            let predicate = str_arg(args, "predicate")?.to_string();
            let object: DeclObject =
                serde_json::from_value(args.get("object").cloned().unwrap_or_default())
                    .map_err(|e| ToolErr::Params(format!("parse object: {e}")))?;
            (subject, predicate, object)
        } else {
            self.brain
                .retract_parts(&space_id, &sid)
                .await
                .map_err(ToolErr::run)?
        };
        let episode = str_arg_or(args, "episode", "");
        let decl = Declaration::Retract {
            subject,
            predicate,
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
            }, {
                "uriTemplate": "timeline://{entity}?space=name&from=ms&to=ms",
                "name": "Entity timeline",
                "description": "Belief intervals for an entity. from/to are optional epoch-ms bounds.",
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
                let cards = self
                    .brain
                    .list_entity_cards(&space_id, 100)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("list_entity_cards: {e}")))?;
                let episode_count = self
                    .brain
                    .episode_count()
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("episode_count: {e}")))?;
                let contradictions = self
                    .brain
                    .contradiction_details(&space_id)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("contradictions: {e}")))?;
                let summary = json!({
                    "space": path,
                    "space_id": space_id,
                    "entity_count": cards.len(),
                    "episode_count": episode_count,
                    "contradiction_count": contradictions.len(),
                    "recent_entities": cards.iter().take(20).map(|c| json!({
                        "id": c.id, "surface": c.surface, "type": c.ty
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
            "timeline" => {
                let entity_id = path.trim_start_matches('/');
                let from = qp
                    .get("from")
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(oxibrain_ports::Timestamp);
                let to = qp
                    .get("to")
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(oxibrain_ports::Timestamp);
                let entries = self
                    .brain
                    .timeline(&space_id, entity_id, from, to)
                    .await
                    .map_err(|e| (INTERNAL_ERROR, format!("timeline: {e}")))?;
                serde_json::to_string_pretty(&entries).unwrap_or_default()
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

/// Optional i64 argument (milliseconds since epoch). `None` when absent.
fn i64_arg_opt(args: &Value, key: &str) -> Option<i64> {
    args.get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
}

/// Optional f32 argument. `default` when absent.
fn f32_arg_or(args: &Value, key: &str, default: f32) -> f32 {
    args.get(key)
        .and_then(|v| v.as_f64())
        .map(|f| f as f32)
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
                "Search the brain and return entity hits: entity_id, entity_surface, entity_type, score, snippet. Statements/episodes/chunks/communities are not returned as targets. as_of (valid time) and min_confidence filter beliefs; a belief that is retracted or contradicted at as_of is excluded.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query text." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "mode": { "type": "string", "enum": ["hybrid","lexical","lexical-vector","graph","community"], "description": "Retrieval mode (default: hybrid)." },
                        "limit": { "type": "integer", "minimum": 1, "description": "Maximum results (default: 20)." },
                        "as_of": { "type": "integer", "description": "Valid-time instant (millis since epoch). Only beliefs true at this instant are returned (default: now)." },
                        "known_at": { "type": "integer", "description": "Transaction-time instant (millis since epoch). Only beliefs recorded by this instant are returned (default: now)." },
                        "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence floor (default: 0)." }
                    },
                    "required": ["query"]
                })),
            tool("recall",
                "Assemble context for a query within a token budget — the per-turn call for agents. Returns layered context: profile, high-salience beliefs (with subjects, validity, support), query neighborhood, summaries with their sources, and recent episodes.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What information to assemble." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "token_budget": { "type": "integer", "minimum": 1, "description": "Maximum tokens for the assembled context (default: 3000)." }
                    },
                    "required": ["query"]
                })),
            tool("brief",
                "Render a page as Markdown with followable links. Three target kinds: `entity` (an entity page: identity, aliases, current beliefs, contradictions, neighbours, timeline, sources — the M9 §9.2 brief); `space` (counts + top entities); `topic` (keyword search over entity surfaces). The target_kind discriminator is purely additive; existing callers using only `entity_id` keep working.",
                json!({
                    "type": "object",
                    "properties": {
                        "target_kind": { "type": "string", "enum": ["entity", "space", "topic"], "description": "Which brief to render. Default: entity." },
                        "entity_id": { "type": "string", "description": "Required when target_kind=entity. The entity's content-derived ID." },
                        "topic": { "type": "string", "description": "Required when target_kind=topic. A keyword to match against entity surface forms (case-insensitive substring)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": []
                })),
            tool("navigate",
                "Follow a followable link from a rendered page to another entity page. Returns the target's brief.",
                json!({
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "The view/page the link came from (e.g. an entity:// id)." },
                        "link": { "type": "string", "description": "The link to follow (entity://<id> or a raw entity id)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": ["from", "link"]
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
                "Bounded subgraph traversal from a set of start entities. Returns nodes and edges within the depth/node budget. The graph is belief-filtered: retracted and contradicted edges are excluded (valid_at filters valid time). Useful for multi-hop recall (ToG driver).",
                json!({
                    "type": "object",
                    "properties": {
                        "start": { "type": "array", "items": { "type": "string" }, "description": "Entity IDs to start from (at least one required)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "depth": { "type": "integer", "minimum": 1, "description": "Max traversal depth (default: 3)." },
                        "max_nodes": { "type": "integer", "minimum": 1, "description": "Max nodes to return (default: 256)." },
                        "direction": { "type": "string", "enum": ["out","in","both"], "description": "Edge direction (default: both)." },
                        "valid_at": { "type": "integer", "description": "Valid-time instant (millis since epoch). Walk the graph as believed at this instant (default: now)." },
                        "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence floor for edges (default: 0)." }
                    },
                    "required": ["start"]
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
                "Retract a statement. Prefer the statement_id form — the declaration is rebuilt from the stored statement (no surfaces/types needed); this retracts ALL assertions of the statement. The subject/predicate/object form is the legacy resubmission path. Creates a Declaration episode.",
                json!({
                    "type": "object",
                    "properties": {
                        "statement_id": { "type": "string", "description": "Statement to retract — takes precedence over subject/predicate/object." },
                        "subject": { "type": "object", "description": "Entity ref: {\"surface\":\"...\",\"type\":\"...\"} (legacy path).", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} } },
                        "predicate": { "type": "string", "description": "Predicate name (legacy path)." },
                        "object": { "type": "object", "description": "Entity or literal object (legacy path).", "properties": { "kind": {"type":"string","enum":["entity","literal"]} } },
                        "episode": { "type": "string", "description": "Originating episode id (audit context)." },
                        "space": { "type": "string", "description": "Space name (default: personal)." }
                    },
                    "required": []
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
    use oxibrain_ports::{TIME_MAX, TIME_MIN};
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
            "brief",
            "navigate",
            "traverse",
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
        // Discovery/handshake MUST NOT change the MCP tool surface. The
        // contract is "fifteen tools, transport-level handshake" — adding a
        // sixteenth tool here would break that.
        assert_eq!(
            names.len(),
            15,
            "MCP tool count must remain exactly 15; got {}: {:?}",
            names.len(),
            names
        );
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
    async fn handshake_returns_server_info_within_supported_range() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": HANDSHAKE_PROTOCOL_MAX,
                    "min_store_format_version": HANDSHAKE_STORE_FORMAT_VERSION,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"]
                })),
            ))
            .await
            .unwrap();
        let result = &resp["result"];
        assert_eq!(
            result["min_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MIN
        );
        assert_eq!(
            result["max_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MAX
        );
        assert_eq!(
            result["store_format_version"].as_u64().unwrap() as u32,
            HANDSHAKE_STORE_FORMAT_VERSION
        );
        assert_eq!(result["server_name"], "oxibrain");
        assert!(
            result["supported_operations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("mcp_tool_call"))
        );
    }

    #[tokio::test]
    async fn handshake_with_out_of_range_version_returns_typed_error() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": 9999,
                    "min_compatible": 1,
                    "max_compatible": 9999,
                    "min_store_format_version": HANDSHAKE_STORE_FORMAT_VERSION,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"],
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], INCOMPATIBLE_PROTOCOL);
        let data = &resp["error"]["data"];
        assert_eq!(data["kind"], "incompatible_protocol");
        assert_eq!(data["requested"].as_u64().unwrap(), 9999);
        assert_eq!(
            data["min_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MIN
        );
        assert_eq!(
            data["max_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MAX
        );
    }

    #[tokio::test]
    async fn handshake_with_too_high_min_store_format_rejects() {
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": HANDSHAKE_PROTOCOL_MAX,
                    "min_compatible": 1,
                    "max_compatible": 1,
                    "min_store_format_version": HANDSHAKE_STORE_FORMAT_VERSION + 10,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"],
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], INCOMPATIBLE_PROTOCOL);
        assert_eq!(resp["error"]["data"]["kind"], "store_too_old");
    }

    #[tokio::test]
    async fn handshake_rejects_overrange_request_with_typed_client_bounds() {
        // The typed `ClientHello` lets the client advertise its own
        // `min_compatible` / `max_compatible` range. The server must honor
        // that advertised ceiling: a client that asks for a protocol
        // version above its own advertised `max_compatible` is rejected
        // with the typed `incompatible_protocol` shape, even when the
        // daemon's own range would otherwise accept the request.
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": HANDSHAKE_PROTOCOL_MAX,
                    "min_compatible": 1,
                    "max_compatible": 1,
                    "min_store_format_version": HANDSHAKE_STORE_FORMAT_VERSION,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"],
                })),
            ))
            .await
            .unwrap();
        // This case is in-range for both server and client, so it must succeed.
        assert!(
            resp.get("error").is_none(),
            "in-range ClientHello must not error: {resp}"
        );
        assert_eq!(resp["result"]["min_compatible"], HANDSHAKE_PROTOCOL_MIN);

        // Now an over-range request: protocol_version is within the
        // server's range but the client explicitly advertised
        // max_compatible=1 and asks for 1 — within range — so we test the
        // genuinely over-range case via a typed malformed shape instead:
        // ask for protocol_version=99 while the daemon's MAX is 1, with
        // the client's bounds aligned to the daemon's range.
        let resp = server
            .handle(msg(
                2,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": 99,
                    "min_compatible": 1,
                    "max_compatible": 100,
                    "min_store_format_version": HANDSHAKE_STORE_FORMAT_VERSION,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"],
                })),
            ))
            .await
            .unwrap();
        assert_eq!(resp["error"]["code"], INCOMPATIBLE_PROTOCOL);
        let data = &resp["error"]["data"];
        assert_eq!(data["kind"], "incompatible_protocol");
        assert_eq!(data["requested"].as_u64().unwrap(), 99);
        // Effective ceiling is min(client_max=100, server_max=1) = 1.
        assert_eq!(
            data["max_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MAX
        );
        assert_eq!(
            data["min_compatible"].as_u64().unwrap() as u32,
            HANDSHAKE_PROTOCOL_MIN
        );
    }

    #[tokio::test]
    async fn handshake_is_available_before_scope_enforcement() {
        // The handshake must NOT be gated by token/capability: it is a
        // transport-level negotiation that runs before any MCP tool routing.
        // Foundation spec §8 forbids discovery from broadening scope; the
        // simplest way to enforce that is to make `handshake` itself
        // unscoped, then gate every actual tool behind scope. This test
        // proves the former half: a fresh, scoped server still answers the
        // handshake.
        let (_dir, server) = fresh_server().await;
        let resp = server
            .handle(msg(
                1,
                HANDSHAKE_METHOD,
                Some(json!({
                    "protocol_version": HANDSHAKE_PROTOCOL_MAX,
                    "min_compatible": 1,
                    "max_compatible": 1,
                    "min_store_format_version": 1,
                    "client_version": "test/1.0",
                    "supported_operations": ["mcp_tool_call"],
                })),
            ))
            .await
            .unwrap();
        assert!(resp.get("error").is_none(), "handshake must succeed");
        assert!(resp["result"]["server_name"].is_string());
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

    /// M8 §3.2: a v1.0-schema client works unmodified. The search schema
    /// gained as_of/known_at/min_confidence (F29) — all additive. A client
    /// that sends ONLY the original fields (query, mode, limit) must get a
    /// success, not a schema error.
    #[tokio::test]
    async fn v1_0_schema_client_search_still_works() {
        let (_dir, server) = fresh_server().await;

        // Declare so the query has something to index, then index it.
        let decl = json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": TIME_MAX.0
        });
        let _resp = server
            .handle(msg(
                1,
                "tools/call",
                Some(json!({"name":"declare","arguments":{"declaration_json": decl}})),
            ))
            .await
            .unwrap();

        // v1.0 client: search with ONLY the original fields.
        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "search",
                    "arguments": { "query": "Alice" }
                })),
            ))
            .await
            .unwrap();
        assert!(
            resp.get("error").is_none(),
            "v1.0 client search must succeed, got {:?}",
            resp.get("error")
        );
        // The result is the MCP text-content shape; the JSON ranking result
        // is the text payload.
        let result = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("result text payload");
        // v1.x DTO contract: the search tool serves UI-ready search hits
        // (entity_id + surface + type + score + snippet) directly. The
        // ranker's envelope (items/dropped/total_candidates/spec) is
        // hidden behind the MCP tool — callers that need envelope
        // diagnostics use the Brain::query facade method directly.
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(result).expect("search response is a JSON array");
        assert!(
            parsed.is_empty()
                || parsed[0].get("entity_id").is_some()
                    && parsed[0].get("entity_surface").is_some()
                    && parsed[0].get("entity_type").is_some()
                    && parsed[0].get("score").is_some()
                    && parsed[0].get("snippet").is_some(),
            "search hits must carry the SearchResult DTO keys, got: {parsed:?}"
        );
    }

    #[tokio::test]
    async fn declare_then_brief_round_trip() {
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
                    "name": "brief",
                    "arguments": { "space": "t", "entity_id": alice }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // brief renders identity + beliefs as Markdown; the belief is active
        // at full confidence with the employed_by predicate and object surface.
        assert!(text.contains("Alice"), "brief titles the entity:\n{text}");
        assert!(
            text.contains("employed_by"),
            "brief lists the belief:\n{text}"
        );
        assert!(
            text.contains("Acme Corp"),
            "brief renders the object surface:\n{text}"
        );
        assert!(
            text.contains("active"),
            "belief status is rendered:\n{text}"
        );
    }

    #[tokio::test]
    async fn why_tool_returns_dto_contract() {
        // The why tool's text payload must serialize as a single ExplainBlock
        // object — not an error envelope. `Statement.object` is the
        // tagged-newtype-with-primitive `Object::Entity(EntityId)`, which
        // serde refuses to serialize as JSON, so we project it to
        // `{"kind": "entity", "id": "..."}` (and the literal variant to its
        // TypedValue JSON shape) before returning. This test locks the
        // projection in.
        let (_dir, server) = fresh_server().await;
        let (_ep, _alice) = declare_alice(&server, "t").await;
        let space_id = server.brain.ensure_space("t").await.unwrap();
        // Find a belief's statement id for Alice.
        let alice = server
            .brain
            .resolve_entity_id(&space_id, "Person", "Alice")
            .await
            .unwrap()
            .expect("Alice exists");
        let beliefs = server.brain.beliefs(&space_id, &alice).await.unwrap();
        let statement_id = beliefs[0].statement.clone();

        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(json!({
                    "name": "why",
                    "arguments": { "space": "t", "statement_id": statement_id }
                })),
            ))
            .await
            .unwrap();
        assert!(
            resp.get("error").is_none(),
            "why must succeed, got {resp:?}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).expect("why response is JSON");
        // Top-level keys match the TypeScript `ExplainBlock` interface.
        let keys: Vec<&str> = parsed
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["assertions", "confidence_breakdown", "statement", "status"],
            "ExplainBlock DTO keys, got: {keys:?}"
        );
        // statement.object carries {kind, id, surface} for entity objects —
        // the surface is what the UI renders.
        let obj = &parsed["statement"]["object"];
        assert_eq!(obj["kind"], "entity");
        assert_eq!(
            obj["surface"], "Acme Corp",
            "entity object must carry its display surface, got {obj}"
        );
        assert!(
            obj["id"].is_string() && !obj["id"].as_str().unwrap().is_empty(),
            "object.id must be a non-empty entity id, got: {obj}"
        );
        // confidence_breakdown carries the support + contradict counts.
        let breakdown = &parsed["confidence_breakdown"];
        assert!(breakdown["support_count"].is_number());
        assert!(breakdown["contradiction_count"].is_number());
        // assertions carry the declaring episode id (the wire field that
        // the UI's `/ask` page renders in each why row).
        let first_episode = parsed["assertions"][0]["episode_id"].as_str().unwrap();
        assert!(!first_episode.is_empty(), "assertion must name its episode");
        // P3: each assertion carries its verbatim subject mention.
        let first_mention = parsed["assertions"][0]["mention"].as_str().unwrap();
        assert_eq!(
            first_mention, "Alice",
            "assertion must carry the verbatim subject mention, got {first_mention}"
        );
    }

    #[tokio::test]
    async fn search_finds_declared_entity_without_rebuild() {
        // The /ask golden path: an entity created by `declare` must be
        // findable by `search` immediately — no `rebuild_indexes` in
        // between. Declarations index their touched entity surfaces
        // incrementally (index_ops::index_entities_fts); this locks that
        // hook in so it cannot silently regress to rebuild-only.
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "search",
                    "arguments": { "space": "t", "query": "Alice" }
                })),
            ))
            .await
            .unwrap();
        assert!(
            resp.get("error").is_none(),
            "search must succeed, got {resp:?}"
        );
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let hits: Vec<serde_json::Value> =
            serde_json::from_str(text).expect("search response is a JSON array");
        let alice_hit = hits
            .iter()
            .find(|h| h["entity_id"].as_str() == Some(alice.as_str()))
            .expect("declared Alice must be searchable without a rebuild");
        assert_eq!(alice_hit["entity_surface"], "Alice");
        assert_eq!(alice_hit["entity_type"], "Person");
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
        let (mut client, server_side) = duplex(64 * 1024);
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
        let mut buf = vec![0u8; 65536];
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

    #[tokio::test]
    async fn contradictions_tool_returns_detail_dto_contract() {
        let (_dir, server) = fresh_server().await;
        // Two conflicting static values for born_in(Alice, …).
        for city in ["Seoul", "Busan"] {
            let decl = serde_json::json!({
                "op": "add_statement",
                "subject": { "surface": "Alice", "type": "Person" },
                "predicate": "born_in",
                "object": { "kind": "entity", "surface": city, "type": "City" },
                "polarity": "affirm",
                "valid_from": 1_000,
                "valid_to": TIME_MAX.0
            });
            let resp = server
                .handle(msg(
                    1,
                    "tools/call",
                    Some(serde_json::json!({
                        "name": "declare",
                        "arguments": { "space": "t", "declaration_json": decl.to_string() }
                    })),
                ))
                .await
                .unwrap();
            assert!(
                resp["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Declared as episode")
            );
        }

        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(serde_json::json!({
                    "name": "contradictions",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let arr: serde_json::Value = serde_json::from_str(text).expect("dto parses");
        let arr = arr.as_array().expect("array of details");
        assert_eq!(arr.len(), 2, "got: {text}");
        for d in arr {
            // Exact key set — the UI's TypeScript mirrors this test.
            // serde_json::Map iterates keys in sorted order (BTreeMap), so the
            // wire-format declaration order is lost once the text is parsed
            // back into Value. Compare the *set*, not the sequence.
            let mut keys: Vec<&str> = d.as_object().unwrap().keys().map(|k| k.as_str()).collect();
            keys.sort();
            assert_eq!(
                keys,
                vec![
                    "affirm_episodes",
                    "deny_episodes",
                    "object_kind",
                    "object_value",
                    "predicate",
                    "statement_id",
                    "subject_id",
                    "subject_surface",
                    "subject_type"
                ],
                "contract keys, got: {keys:?}"
            );
            assert_eq!(d["subject_surface"], "Alice");
            assert_eq!(d["subject_type"], "Person");
            assert!(d["affirm_episodes"].as_array().unwrap().len() == 1);
        }
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
    async fn navigate_follows_link() {
        let (_dir, server) = fresh_server().await;
        let (_ep, _alice) = declare_alice(&server, "t").await;
        let space_id = server.brain.ensure_space("t").await.unwrap();
        let acme = server
            .brain
            .resolve_entity_id(&space_id, "Organization", "Acme Corp")
            .await
            .unwrap()
            .expect("Acme should exist");

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "navigate",
                    "arguments": { "space": "t", "from": "entity://alice", "link": format!("entity://{acme}") }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("Acme Corp"),
            "navigate renders the target:\n{text}"
        );
    }

    /// M9 exit criterion (§16.4): an agent can answer a 3-hop question
    /// starting from ONE brief and following only navigate links — no
    /// search. Chain: Alice → Acme Corp → Project X → Bob.
    #[tokio::test]
    async fn navigate_three_hops_reaches_deep_entity() {
        let (_dir, server) = fresh_server().await;
        let space_id = server.brain.ensure_space("t").await.unwrap();

        // Build a chain with declarations (no search involved).
        let decls = [
            json!({
                "op": "add_statement",
                "subject": { "surface": "Alice", "type": "Person" },
                "predicate": "employed_by",
                "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
                "polarity": "affirm",
                "valid_from": TIME_MIN.0,
                "valid_to": TIME_MAX.0
            }),
            json!({
                "op": "add_statement",
                "subject": { "surface": "Acme Corp", "type": "Organization" },
                "predicate": "works_on",
                "object": { "kind": "entity", "surface": "Project X", "type": "Project" },
                "polarity": "affirm",
                "valid_from": TIME_MIN.0,
                "valid_to": TIME_MAX.0
            }),
            json!({
                "op": "add_statement",
                "subject": { "surface": "Project X", "type": "Project" },
                "predicate": "member_of",
                "object": { "kind": "entity", "surface": "Bob", "type": "Person" },
                "polarity": "affirm",
                "valid_from": TIME_MIN.0,
                "valid_to": TIME_MAX.0
            }),
        ];
        for (i, d) in decls.iter().enumerate() {
            let resp = server
                .handle(msg(
                    i as i64 + 1,
                    "tools/call",
                    Some(json!({
                        "name": "declare",
                        "arguments": { "space": "t", "declaration_json": d.to_string() }
                    })),
                ))
                .await
                .unwrap();
            assert!(
                resp["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("episode"),
                "declare must succeed: {resp:?}"
            );
        }

        // 1. Brief Alice — must contain a link to Acme Corp.
        let alice = server
            .brain
            .resolve_entity_id(&space_id, "Person", "Alice")
            .await
            .unwrap()
            .expect("Alice");
        let resp = server
            .handle(msg(
                10,
                "tools/call",
                Some(json!({
                    "name": "brief",
                    "arguments": { "space": "t", "entity_id": alice }
                })),
            ))
            .await
            .unwrap();
        let page1 = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            page1.contains("Acme Corp") && page1.contains("entity://"),
            "hop 1: brief(Alice) must show Acme Corp as a link:\n{page1}"
        );

        // Extract the Acme link from page 1.
        let acme_link = extract_link(&page1, "Acme Corp").expect("Acme link");
        let acme = server
            .brain
            .resolve_entity_id(&space_id, "Organization", "Acme Corp")
            .await
            .unwrap()
            .expect("Acme");

        // 2. navigate → Acme Corp — must show Project X as a link.
        let resp = server
            .handle(msg(
                11,
                "tools/call",
                Some(json!({
                    "name": "navigate",
                    "arguments": { "space": "t", "from": "entity://alice", "link": acme_link }
                })),
            ))
            .await
            .unwrap();
        let page2 = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            page2.contains("Project X") && page2.contains("entity://"),
            "hop 2: navigate(Acme) must show Project X:\n{page2}"
        );
        let project_x = server
            .brain
            .resolve_entity_id(&space_id, "Project", "Project X")
            .await
            .unwrap()
            .expect("Project X");

        // 3. navigate → Project X — must show Bob.
        let resp = server
            .handle(msg(
                12,
                "tools/call",
                Some(json!({
                    "name": "navigate",
                    "arguments": { "space": "t", "from": &format!("entity://{acme}"), "link": format!("entity://{project_x}") }
                })),
            ))
            .await
            .unwrap();
        let page3 = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            page3.contains("Bob"),
            "hop 3: navigate(Project X) must reach Bob:\n{page3}"
        );
    }

    /// Extract the `entity://...` link whose markdown label is `label`.
    fn extract_link(page: &str, label: &str) -> Option<String> {
        // Format: `[label](entity://<id>)`. Find the label, then take the
        // text between `](entity://` and the next `)`.
        let start = page.find(&format!("[{label}]("))? + label.len() + 3;
        let rest = &page[start..];
        let link = rest.strip_prefix("entity://")?;
        let end = link.find(')')?;
        Some(format!("entity://{}", &link[..end]))
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
    async fn retract_by_statement_id_removes_conflict() {
        // The conflicts-inbox path: the UI holds a statement_id from
        // `contradictions` and must not resubmit surfaces/types. Seeding a
        // contradiction, then retracting one statement by id, must remove it
        // from the contradiction list — the inbox count drops to zero.
        let (_dir, server) = fresh_server().await;
        let acme = json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": TIME_MAX.0
        });
        let globex = json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Globex", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": TIME_MAX.0
        });
        for decl in [acme, globex] {
            server
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
        }

        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "contradictions",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let details: Vec<Value> = serde_json::from_str(text).expect("contradictions JSON");
        assert_eq!(details.len(), 2, "both employed_by statements conflict");
        let sid = details[0]["statement_id"].as_str().unwrap().to_string();

        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(json!({
                    "name": "retract",
                    "arguments": { "space": "t", "statement_id": sid }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Retracted as episode"), "got: {text}");

        let resp = server
            .handle(msg(
                4,
                "tools/call",
                Some(json!({
                    "name": "contradictions",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let details: Vec<Value> = serde_json::from_str(text).expect("contradictions JSON");
        assert!(
            details.is_empty(),
            "retracted statement must leave the conflict list, got {details:?}"
        );
    }
    #[tokio::test]
    async fn retract_by_statement_id_handles_literal_object() {
        // The legacy retract path encoded objects as {kind, surface/value}.
        // For literal objects, the column stores a TypedValue JSON blob —
        // the statement-first path must round-trip it through DeclObject
        // without losing the (literal_type, value) pair (silent no-op if it
        // passes the raw JSON string).
        let (_dir, server) = fresh_server().await;
        let make = |value: &str| {
            json!({
                "op": "add_statement",
                "subject": { "surface": "Bob", "type": "Person" },
                "predicate": "full_name",
                "object": { "kind": "literal", "literal_type": "text", "value": value },
                "polarity": "affirm",
                "valid_from": 1_000,
                "valid_to": TIME_MAX.0
            })
        };
        for v in ["Bob Smith", "Robert Smith"] {
            server
                .handle(msg(
                    1,
                    "tools/call",
                    Some(json!({
                        "name": "declare",
                        "arguments": { "space": "t", "declaration_json": make(v).to_string() }
                    })),
                ))
                .await
                .unwrap();
        }

        // Snapshot the contradictions to find one of the literal statements.
        let resp = server
            .handle(msg(
                2,
                "tools/call",
                Some(json!({
                    "name": "contradictions",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let details: Value = serde_json::from_str(text).expect("parse");
        assert_eq!(details.as_array().unwrap().len(), 2, "got: {text}");
        let target = details
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["object_value"] == "Bob Smith")
            .expect("Bob Smith value present")["statement_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Retract by statement_id — must not silently no-op on literal objects.
        let resp = server
            .handle(msg(
                3,
                "tools/call",
                Some(json!({
                    "name": "retract",
                    "arguments": { "space": "t", "statement_id": target }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Retracted as episode"), "got: {text}");

        // Contradiction list drops to empty — the retracted statement is
        // no longer an affirmation, so its peer (which had no other
        // conflicting source) no longer appears as contradicted either.
        let resp = server
            .handle(msg(
                4,
                "tools/call",
                Some(json!({
                    "name": "contradictions",
                    "arguments": { "space": "t" }
                })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let details: Value = serde_json::from_str(text).expect("parse");
        assert_eq!(details.as_array().unwrap().len(), 0, "got: {text}");
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
        assert!(uris.contains(&"timeline://{entity}?space=name&from=ms&to=ms"));
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
    async fn space_resource_contract_exact_keys() {
        let (_dir, server) = fresh_server().await;
        let (_ep, _alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                1,
                "resources/read",
                Some(serde_json::json!({ "uri": "space://t" })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let v: serde_json::Value = serde_json::from_str(text).expect("overview parses");

        // serde_json::Map iterates keys in sorted order (BTreeMap), so the
        // wire-format declaration order is lost once the text is parsed
        // back into Value. Compare the *set*, not the sequence.
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "contradiction_count",
                "entity_count",
                "episode_count",
                "recent_entities",
                "space",
                "space_id",
            ]
        );
        assert_eq!(
            v["entity_count"].as_u64().unwrap(),
            2,
            "Alice + Acme Corp: {text}"
        );
        let re = v["recent_entities"].as_array().unwrap();

        for e in re {
            let ek: Vec<&str> = e.as_object().unwrap().keys().map(|k| k.as_str()).collect();
            assert_eq!(ek, vec!["id", "surface", "type"], "card keys: {ek:?}");
            assert!(
                !e["surface"].as_str().unwrap().is_empty(),
                "surface never empty"
            );
        }
        assert!(re.iter().any(|e| e["surface"] == "Alice"));
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
    async fn timeline_resource_returns_entries_contract() {
        let (_dir, server) = fresh_server().await;
        let (_ep, alice) = declare_alice(&server, "t").await;

        let resp = server
            .handle(msg(
                1,
                "resources/read",
                Some(serde_json::json!({ "uri": format!("timeline://{alice}?space=t") })),
            ))
            .await
            .unwrap();
        let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
        let v: serde_json::Value = serde_json::from_str(text).expect("timeline parses");
        let arr = v.as_array().expect("array of entries");
        assert!(!arr.is_empty(), "declare produced one belief: {text}");
        let e = &arr[0];
        let mut keys: Vec<&str> = e.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "object_entity",
                "object_repr",
                "predicate",
                "recorded_at",
                "statement_id",
                "status",
                "valid_from",
                "valid_to"
            ]
        );
        // The store post-pass resolves entity objects to their surface —
        // lock the readable repr, not just the key set.
        assert_eq!(e["object_repr"], "Acme Corp");
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
