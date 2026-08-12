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
use oxibrain::{Brain, BrainConfig, Capability, Declaration, Scope};
use oxibrain_core::retrieval::{Query, QueryMode};
use oxibrain_ports::{ClockPort, SystemClock};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

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

    /// Resolve a space name to its content-derived ID, creating it if absent.
    async fn ensure_space(&self, name: &str) -> Result<String, ToolErr> {
        self.brain.ensure_space(name).await.map_err(ToolErr::run)
    }

    /// Capability required to invoke a tool (DESIGN §12.2 surface table).
    /// `None` for unknown tools — dispatch then returns method-not-found.
    fn required_capability(tool: &str) -> Option<Capability> {
        match tool {
            "search" | "recall" | "get_entity" | "why" | "contradictions" => Some(Capability::Read),
            "ingest" => Some(Capability::Ingest),
            "declare" => Some(Capability::Write),
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

    /// Handle one JSON-RPC message. Returns a response value for requests,
    /// `None` for notifications (which expect no response).
    pub async fn handle(&self, msg: Message) -> Option<Value> {
        match msg.method.as_str() {
            "initialize" => msg
                .id
                .map(|id| success(id, self.initialize(msg.params.as_ref()))),
            "notifications/initialized" | "initialized" => None,
            "ping" => msg.id.map(|id| success(id, json!({}))),
            "tools/list" => msg.id.map(|id| success(id, tool_list())),
            "tools/call" => match msg.id {
                Some(id) => match self.call_tool(msg.params.as_ref()).await {
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
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "oxibrain", "version": env!("CARGO_PKG_VERSION") }
        })
    }

    /// Dispatch a `tools/call`. Argument/lookup errors map to JSON-RPC errors;
    /// tool execution failures map to an MCP `isError` text result.
    async fn call_tool(&self, params: Option<&Value>) -> Result<Value, (i64, String)> {
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
            "ingest" => self.tool_ingest(&args).await,
            "declare" => self.tool_declare(&args).await,
            "why" => self.tool_why(&args).await,
            "contradictions" => self.tool_contradictions(&args).await,
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

    async fn tool_ingest(&self, args: &Value) -> Result<String, ToolErr> {
        let content = str_arg(args, "content")?;
        let space_id = self.ensure_space(&space_arg(args)).await?;
        let path = str_arg_or(args, "source_path", "mcp");
        let now = SystemClock.now();
        let id = self
            .brain
            .ingest_note(&space_id, &path, content.to_string(), now)
            .await
            .map_err(ToolErr::run)?;
        Ok(format!("Ingested as episode: {id}"))
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
        "semantic" => QueryMode::Semantic,
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
                "Search the brain via hybrid/lexical/semantic/graph/community retrieval. Returns ranked results with scores, targets, and provenance.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "The search query text." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "mode": { "type": "string", "enum": ["hybrid","lexical","semantic","graph","community"], "description": "Retrieval mode (default: hybrid)." },
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
                "Ingest text content as a new Primary episode. Returns the episode ID. Extraction is not triggered by this call.",
                json!({
                    "type": "object",
                    "properties": {
                        "content": { "type": "string", "description": "The text to ingest." },
                        "space": { "type": "string", "description": "Space name (default: personal)." },
                        "source_path": { "type": "string", "description": "Optional source label, e.g. a file path (default: mcp)." }
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
                }))
        ]
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

// ── stdio transport ────────────────────────────────────────────────────────

/// Run the MCP server on stdio, for Claude Desktop and other MCP clients.
///
/// Reads newline-delimited JSON-RPC from stdin; writes one response per line to
/// stdout. All diagnostics go to stderr — stdout is the protocol channel.
pub async fn serve_stdio(brain: Brain) -> anyhow::Result<()> {
    let server = Arc::new(BrainServer::from_brain(brain));
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut out = BufWriter::new(tokio::io::stdout());
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| anyhow::anyhow!("stdin read: {e}"))?;
        if n == 0 {
            break; // EOF — client closed stdin.
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match Message::parse(&line) {
            Ok(msg) => server.handle(msg).await,
            Err((id, code, msg)) => {
                // Unparseable/invalid request: best-effort error response.
                Some(error(id.unwrap_or(Value::Null), code, msg))
            }
        };
        if let Some(resp) = response {
            let serialized = serde_json::to_string(&resp)
                .map_err(|e| anyhow::anyhow!("serialize response: {e}"))?;
            out.write_all(serialized.as_bytes()).await?;
            out.write_all(b"\n").await?;
            out.flush().await?;
        }
    }
    Ok(())
}

/// Open a Brain at `dir` and serve it over stdio.
pub async fn serve_stdio_at(dir: &std::path::Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    serve_stdio(brain).await
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
    async fn tools_list_advertises_all_seven_tools() {
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
            "ingest",
            "declare",
            "why",
            "contradictions",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
            // Every tool must carry a JSON-Schema inputSchema.
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
}
