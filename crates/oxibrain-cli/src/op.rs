//! The CLI op transport (spec `agent-first-cli-v1` §2–§3; ADR-012/013).
//!
//! `oxibrain <op> --json PAYLOAD` dispatches through the same `BrainServer`
//! implementation MCP sessions use — two transports, one op set, no handler
//! duplication. stdout carries exactly one JSON envelope per call; logs go
//! to stderr (`main` wires tracing there).
//!
//! Phase 2 keeps the v2.12 default-space fallback for omitted `space`; the
//! P3 cutover makes `space` required and deletes the chain.

use oxibrain::{Brain, BrainConfig};
use oxibrain_mcp::BrainServer;
use oxibrain_mcp::protocol::{INVALID_PARAMS, Message, UNAUTHORIZED};
use oxibrain_ports::BrainError;
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Envelope protocol version (spec §3).
pub const API_VERSION: i64 = 1;

// Exit codes (spec §3): 0 ok · 2 invalid_input · 3 not_found ·
// 4 unauthorized · 5 locked_or_busy · 6 conflict · 7 budget · 8 model ·
// 9 internal. P2 maps the classes the in-process hop can see; the
// BrainError 1:1 mapping completes at the P3 cutover.
pub const EXIT_OK: i32 = 0;
pub const EXIT_INVALID_INPUT: i32 = 2;
pub const EXIT_NOT_FOUND: i32 = 3;
pub const EXIT_UNAUTHORIZED: i32 = 4;
pub const EXIT_LOCKED: i32 = 5;
pub const EXIT_INTERNAL: i32 = 9;

/// Parsed `oxibrain <op>` invocation: the op name plus transport flags.
/// Only two flags exist at the op level and both are parsed here — clap's
/// external-subcommand capture is verbatim.
#[derive(Debug)]
pub struct OpArgs {
    pub name: String,
    pub json: Option<String>,
}

/// Parse the external-subcommand capture: `[op, --json, X, ...]`.
///
/// `--json` accepts a literal payload, `@file`, or `-` (stdin). `--format`
/// accepts only `json` in P2 (NDJSON arrives with cursors in P5). `--dir`
/// must precede the op name — clap already consumed it there.
pub fn parse_op_args(args: &[String]) -> Result<OpArgs, String> {
    let mut it = args.iter();
    let name = it
        .next()
        .ok_or_else(|| "no op name: try `oxibrain schema` for the catalogue".to_string())?
        .clone();
    let mut json: Option<String> = None;
    let mut format_seen = false;
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--json" | "-j" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--json requires a value (payload | @file | -)".to_string())?;
                json = Some(v.clone());
            }
            "--format" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--format requires a value (json)".to_string())?;
                if v != "json" {
                    return Err(format!("unsupported --format '{v}' (P2: json only)"));
                }
                format_seen = true;
            }
            _ => {
                return Err(format!(
                    "unexpected argument '{flag}': op arguments belong in the --json payload; \
                     machine verbs live under `oxibrain admin`"
                ));
            }
        }
    }
    let _ = format_seen;
    Ok(OpArgs { name, json })
}

/// Load the op payload: `--json` literal / `@file` / `-` (stdin), falling
/// back to stdin when piped (stdin is the first-class path — spec §2).
fn load_payload(json_spec: Option<&str>) -> Result<Value, String> {
    let raw: String = match json_spec {
        None if !std::io::stdin().is_terminal() => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("stdin read: {e}"))?;
            buf
        }
        None => return Ok(json!({})),
        Some("-") => {
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("stdin read: {e}"))?;
            buf
        }
        Some(spec) => match spec.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?,
            None => spec.to_string(),
        },
    };
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|e| format!("payload is not valid JSON: {e}"))
}

fn print_envelope(env: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(env).unwrap_or_else(|_| env.to_string())
    );
}

fn ok_envelope(op: &str, space: Option<&str>, data: Value, elapsed_ms: u128) -> Value {
    json!({
        "api": API_VERSION,
        "ok": true,
        "op": op,
        "space": space,
        "data": data,
        "meta": { "elapsed_ms": elapsed_ms as u64 },
    })
}

fn err_envelope(op: &str, code: &str, message: &str, retryable: bool) -> Value {
    json!({
        "api": API_VERSION,
        "ok": false,
        "op": op,
        "error": { "code": code, "message": message, "retryable": retryable },
    })
}

/// Map a `BrainError` from the open/handle path to an exit code (P2 subset;
/// the 1:1 mapping completes at P3).
fn brain_error_parts(e: &BrainError) -> (i32, &'static str, bool) {
    match e {
        BrainError::Locked { .. } | BrainError::Busy(_) => (EXIT_LOCKED, "locked", true),
        BrainError::Unauthorized(_) | BrainError::Scope { .. } => {
            (EXIT_UNAUTHORIZED, "unauthorized", false)
        }
        BrainError::NotFound(_) | BrainError::SpaceNotFound { .. } => {
            (EXIT_NOT_FOUND, "not_found", false)
        }
        BrainError::Invalid(_) | BrainError::SpaceNameInvalid { .. } => {
            (EXIT_INVALID_INPUT, "invalid_input", false)
        }
        _ => (EXIT_INTERNAL, "internal", false),
    }
}

/// Run one op end to end; prints the envelope and returns the exit code.
pub async fn run(dir: &Path, args: &[String]) -> i32 {
    let started = Instant::now();
    let parsed = match parse_op_args(args) {
        Ok(p) => p,
        Err(m) => {
            print_envelope(&err_envelope("", "invalid_input", &m, false));
            return EXIT_INVALID_INPUT;
        }
    };
    let op_name = parsed.name.clone();

    if oxibrain_ops::find(&op_name).is_none() {
        let names: Vec<&str> = oxibrain_ops::ops().iter().map(|o| o.name).collect();
        let m = format!(
            "unknown op '{op_name}'; ops: {}; machine verbs live under `oxibrain admin`",
            names.join(", ")
        );
        print_envelope(&err_envelope(&op_name, "invalid_input", &m, false));
        return EXIT_INVALID_INPUT;
    }

    let payload = match load_payload(parsed.json.as_deref()) {
        Ok(p) => p,
        Err(m) => {
            print_envelope(&err_envelope(&op_name, "invalid_input", &m, false));
            return EXIT_INVALID_INPUT;
        }
    };

    let brain = match Brain::open(BrainConfig::at(dir)).await {
        Ok(b) => b,
        Err(e) => {
            let (code, name, retry) = brain_error_parts(&e);
            print_envelope(&err_envelope(&op_name, name, &e.to_string(), retry));
            return code;
        }
    };

    // v2.13 (ADR-013): no default-space fallback — the server enforces the
    // required `space` argument and enumerates available spaces in the
    // error.
    let server = Arc::new(BrainServer::from_arc(Arc::new(brain)));
    let msg = Message {
        id: Some(json!(1)),
        method: "tools/call".into(),
        params: Some(json!({ "name": op_name, "arguments": payload })),
    };
    let resp = match server.handle(msg).await {
        Some(v) => v,
        None => {
            print_envelope(&err_envelope(&op_name, "internal", "no response", false));
            return EXIT_INTERNAL;
        }
    };

    let elapsed = started.elapsed().as_millis();
    let space = payload
        .get("space")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if let Some(rpc_err) = resp.get("error") {
        let code_num = rpc_err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = rpc_err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("protocol error");
        let (exit, code, retry) = if message.starts_with("plan_stale:") {
            // Rail outcome: the dry-run's ledger state moved. `ToolErr::Run`
            // maps to the JSON-RPC internal code on the wire, so this class
            // must be recovered from the message. Exit shares the conflict
            // slot (exits stay contiguous 1–9, F12); recovery is a fresh
            // dry-run, so retryable.
            (6, "plan_stale", true)
        } else {
            match code_num {
                c if c == INVALID_PARAMS => (EXIT_INVALID_INPUT, "invalid_input", false),
                c if c == UNAUTHORIZED => (EXIT_UNAUTHORIZED, "unauthorized", false),
                _ => (EXIT_INTERNAL, "internal", false),
            }
        };
        print_envelope(&err_envelope(&op_name, code, message, retry));
        return exit;
    }

    let result = &resp["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        // The tool ran and failed (MCP semantics: runtime failure inside a
        // successful JSON-RPC response). The BrainError Display prefixes in
        // `oxibrain-ports/src/error.rs` are a stable contract — classify on
        // them until ToolErr carries the typed error (P6).
        let (code, exit, retry) =
            if text.starts_with("invalid input:") || text.starts_with("invalid space name") {
                ("invalid_input", EXIT_INVALID_INPUT, false)
            } else if text.starts_with("not found:") || text.starts_with("space '") {
                ("not_found", EXIT_NOT_FOUND, false)
            } else if text.starts_with("unauthorized:") || text.starts_with("insufficient scope") {
                ("unauthorized", EXIT_UNAUTHORIZED, false)
            } else if text.starts_with("store locked") || text.starts_with("busy:") {
                ("locked", EXIT_LOCKED, true)
            } else if text.starts_with("budget exceeded:") {
                ("budget", 7, false)
            } else if text.starts_with("model error:") || text.starts_with("provider error") {
                ("model", 8, false)
            } else if text.starts_with("conflict:") {
                ("conflict", 6, false)
            } else if text.starts_with("plan_stale:") {
                // Rail outcome: the dry-run's ledger state moved. Exit code
                // shares the conflict slot (spec keeps exits contiguous
                // 1–9, F12); the recovery is a fresh dry-run, so retryable.
                ("plan_stale", 6, true)
            } else {
                ("internal", EXIT_INTERNAL, false)
            };
        print_envelope(&err_envelope(&op_name, code, &text, retry));
        return exit;
    }

    // Success payloads are pretty-JSON strings; plain text falls back to a
    // JSON string value.
    let data = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    print_envelope(&ok_envelope(&op_name, space.as_deref(), data, elapsed));
    EXIT_OK
}

/// `oxibrain describe` — orientation data (spec §7): the agent's first call
/// and the `space` enum source. P2 delivers the space/freshness core;
/// registry/model digests join with the P3 schema work.
pub async fn describe(dir: &Path) -> i32 {
    let started = Instant::now();
    let brain = match Brain::open_ro(BrainConfig::read_only_at(dir)).await {
        Ok(b) => b,
        Err(e) => {
            let (code, name, retry) = brain_error_parts(&e);
            print_envelope(&err_envelope("describe", name, &e.to_string(), retry));
            return code;
        }
    };
    let data = match describe_data(&brain).await {
        Ok(d) => d,
        Err(e) => {
            print_envelope(&err_envelope("describe", "internal", &e.to_string(), false));
            return EXIT_INTERNAL;
        }
    };
    let elapsed = started.elapsed().as_millis();
    print_envelope(&ok_envelope("describe", None, data, elapsed));
    EXIT_OK
}

async fn describe_data(brain: &Brain) -> anyhow::Result<Value> {
    let mut spaces = Vec::new();
    for s in brain.list_spaces().await? {
        let documents = brain.document_count_for_space(&s.name).await.unwrap_or(0);
        spaces.push(json!({
            "name": s.name,
            "id": s.id,
            "episodes": s.episode_count,
            "entities": s.entity_count,
            "documents": documents,
        }));
    }
    let (roots, files) = brain.document_counts().await?;
    let pending = brain.pending_extraction_stats().await?;
    Ok(json!({
        "spaces": spaces,
        "documents": { "roots": roots, "cached_files": files },
        "pending_extraction": { "count": pending.count, "oldest_seq": pending.oldest_seq },
        "versions": { "api": API_VERSION, "oxibrain": env!("CARGO_PKG_VERSION") },
    }))
}

/// `oxibrain schema [op]` — registry introspection (spec §7).
pub fn schema(op: Option<&str>) -> i32 {
    match op {
        None => {
            let data = json!({
                "ops": oxibrain_ops::ops().iter().map(|o| json!({
                    "name": o.name,
                    "summary": o.summary,
                    "caps": o.caps,
                    "mutating": o.mutating,
                    "schema": (o.schema)(),
                })).collect::<Vec<_>>(),
            });
            print_envelope(&ok_envelope("schema", None, data, 0));
            EXIT_OK
        }
        Some(name) => match oxibrain_ops::find(name) {
            Some(o) => {
                let data = json!({
                    "name": o.name,
                    "summary": o.summary,
                    "caps": o.caps,
                    "mutating": o.mutating,
                    "schema": (o.schema)(),
                });
                print_envelope(&ok_envelope("schema", None, data, 0));
                EXIT_OK
            }
            None => {
                let m = format!("unknown op '{name}'");
                print_envelope(&err_envelope("schema", "invalid_input", &m, false));
                EXIT_INVALID_INPUT
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_op_args_accepts_json_and_rejects_stray_flags() {
        let args = vec!["search".to_string(), "--json".into(), "{\"q\":1}".into()];
        let p = parse_op_args(&args).unwrap();
        assert_eq!(p.name, "search");
        assert_eq!(p.json.as_deref(), Some("{\"q\":1}"));

        assert!(parse_op_args(&[]).is_err());
        assert!(parse_op_args(&["search".into(), "--space".into(), "dev".into()]).is_err());
        assert!(parse_op_args(&["search".into(), "--json".into()]).is_err());
        assert!(parse_op_args(&["search".into(), "--format".into(), "md".into()]).is_err());
        let err = parse_op_args(&["predicate".into(), "list".into()]).unwrap_err();
        assert!(
            err.contains("`oxibrain admin`"),
            "misdirected admin verb must be hinted: {err}"
        );
    }

    #[test]
    fn load_payload_literal_and_at_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "{\"space\":\"t\"}").unwrap();
        let spec = format!("@{}", tmp.path().display());
        let v = load_payload(Some(&spec)).unwrap();
        assert_eq!(v["space"], "t");
        let v = load_payload(Some("{\"a\":1}")).unwrap();
        assert_eq!(v["a"], 1);
        assert!(load_payload(Some("not json")).is_err());
    }

    #[tokio::test]
    async fn op_dispatch_round_trip_declare_and_error() {
        // An empty-but-valid store with one space: prove the transport
        let decl = serde_json::json!({
            "op": "add_statement",
            "subject": { "surface": "Alice", "type": "Person" },
            "predicate": "employed_by",
            "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
            "polarity": "affirm",
            "valid_from": 1_000,
            "valid_to": 4_102_444_800_000i64,
        });
        // A store with one space: prove the transport reaches the real
        // handler and returns an envelope for both ok and error classes.
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        brain.ensure_space("t").await.unwrap();
        drop(brain);
        let args = vec![
            "declare".to_string(),
            "--json".into(),
            json!({ "space": "t", "declaration_json": decl.to_string() }).to_string(),
        ];
        // `run` prints to stdout; assert on the handler outcome instead by
        // driving the same message shape the transport builds.
        let parsed = parse_op_args(&args).unwrap();
        assert_eq!(parsed.name, "declare");
        let payload = load_payload(parsed.json.as_deref()).unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let server = Arc::new(BrainServer::from_arc(Arc::new(brain)));
        let msg = Message {
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "declare", "arguments": payload })),
        };
        let resp = server.handle(msg).await.unwrap();
        assert!(resp["error"].is_null(), "declare failed: {resp}");
        assert_ne!(resp["result"]["isError"], json!(true));

        // unknown space → typed params error through the same hop (stats
        // left the tool list in v2.13; contradictions carries the contract).
        let msg = Message {
            id: Some(json!(2)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "contradictions", "arguments": { "space": "ghost" } })),
        };
        let resp = server.handle(msg).await.unwrap();
        // ToolErr::Params maps to a JSON-RPC error (INVALID_PARAMS), which
        // the CLI transport translates into the invalid_input envelope.
        let message = resp["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("space add"),
            "unexpected error message: {message}"
        );
    }
}
