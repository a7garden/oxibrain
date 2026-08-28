//! Server-backed handshake tests for the daemonless stdio transport: a real
//! `oxibrain-mcp` session (driven in-process over a duplex pipe — the exact
//! framing `serve --stdio` uses) exercised by `oxibrain-client`.
//!
//! These live in the mcp crate because `cargo publish` resolves
//! dev-dependencies against the crates.io index — a dev-dependency from
//! oxibrain-client on this crate would form a publish cycle (mcp depends on
//! client in production). They cover:
//!
//! 1. The client handshake returns the negotiated `BrainCapabilities`.
//! 2. An incompatible `protocol_version` is rejected with a typed
//!    `HandshakeError` that names the supported range.
//! 3. A `min_store_format_version` above the server's is rejected.
//! 4. A `Read`-only scope cannot escalate through `handshake` — the method
//!    returns the daemon's full capabilities regardless of scope, because the
//!    handshake is a transport-level negotiation that runs **before** MCP tool
//!    routing. The scope is enforced on `tools/call`, not on `handshake`.
//!    This is the documented Foundation §8 contract.
//! 5. The server `ServerInfo` lists the supported protocol range.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use oxibrain::{Brain, BrainConfig, Capability, Scope};
use oxibrain_client::BrainClient;
use oxibrain_client::protocol::{
    ClientOperation, HandshakeError, PROTOCOL_VERSION_MAX, PROTOCOL_VERSION_MIN,
    default_client_hello, parse_handshake_error,
};
use oxibrain_mcp::{BrainServer, run_session, run_session_gated};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Spawn an in-process session server over a duplex pipe and attach a
/// `BrainClient` to the other end. This is the same newline-delimited
/// JSON-RPC framing `serve --stdio` speaks with a spawned child.
async fn spawn_server() -> (tempfile::TempDir, BrainClient) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let server = Arc::new(BrainServer::from_brain(brain));
    let (client_side, server_side) = tokio::io::duplex(8192);
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session(server, sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = BrainClient::from_io(cr, cw);
    (dir, client)
}

/// Spawn a token-gated session (the `serve --stdio` auth shape) and attach a
/// client that authenticates first. Returns the tempdir, the authenticated
/// client, and the issued secret.
async fn spawn_auth_server(caps: &[Capability]) -> (tempfile::TempDir, BrainClient, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("personal").await.unwrap();
    let scope = Scope {
        spaces: vec![space_id],
        caps: caps.iter().copied().collect(),
        ..Default::default()
    };
    let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();

    let (client_side, server_side) = tokio::io::duplex(8192);
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session_gated(Arc::new(brain), sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let mut client = BrainClient::from_io(cr, cw);
    client.auth(&secret).await.expect("auth");
    (dir, client, secret)
}

/// A raw duplex end for hand-crafted requests (incompatible versions etc.).
async fn raw_session() -> (
    tempfile::TempDir,
    BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    tokio::io::WriteHalf<tokio::io::DuplexStream>,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let (client_side, server_side) = tokio::io::duplex(8192);
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session(Arc::new(BrainServer::from_brain(brain)), sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    (dir, BufReader::new(cr), cw)
}

/// Send a raw JSON-RPC request and read one response. Used by tests that
/// need to craft an incompatible `protocol_version` and observe the typed
/// error.
async fn raw_handshake(
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
    protocol_version: u32,
) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "handshake",
        "params": {
            "protocol_version": protocol_version,
            "min_store_format_version": 1,
            "client_version": "test/1.0",
            "supported_operations": ["mcp_tool_call"]
        }
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

#[tokio::test]
async fn client_handshake_returns_capabilities_over_stdio_session() {
    let (_dir, mut client) = spawn_server().await;
    let caps = client
        .handshake(default_client_hello("stdio-client/0.1"))
        .await
        .expect("handshake");

    assert_eq!(caps.protocol_version.0, PROTOCOL_VERSION_MAX);
    assert_eq!(caps.server_name, "oxibrain");
    assert!(
        caps.supported_operations
            .contains(&ClientOperation::McpToolCall)
    );
    assert!(caps.store_format_version >= 1);

    // Subsequent tool calls work as usual.
    client.ping().await.expect("ping");
}

#[tokio::test]
async fn handshake_with_incompatible_version_is_rejected() {
    let (_dir, mut reader, mut writer) = raw_session().await;

    // Use a version far outside the supported range.
    let resp = raw_handshake(&mut reader, &mut writer, 99).await;
    let err = resp.get("error").expect("error response");
    let typed = parse_handshake_error(err).expect("typed handshake error");
    match typed {
        HandshakeError::IncompatibleProtocol {
            requested,
            min_compatible,
            max_compatible,
        } => {
            assert_eq!(requested, 99);
            assert_eq!(min_compatible, PROTOCOL_VERSION_MIN);
            assert_eq!(max_compatible, PROTOCOL_VERSION_MAX);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[tokio::test]
async fn client_hello_with_too_high_min_store_format_is_rejected() {
    let (_dir, mut reader, mut writer) = raw_session().await;
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "handshake",
        "params": {
            "protocol_version": 1,
            "min_store_format_version": 99,
            "client_version": "test/1.0"
        }
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    let v: Value = serde_json::from_str(&resp).unwrap();
    let err = v.get("error").expect("error");
    let typed = parse_handshake_error(err).expect("typed");
    match typed {
        HandshakeError::StoreTooOld {
            server_format,
            client_min,
        } => {
            assert!(client_min >= server_format);
            assert_eq!(client_min, 99);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[tokio::test]
async fn read_only_scope_does_not_escalate_through_handshake() {
    // The handshake is a transport-level negotiation that runs before any
    // tool routing. It returns the daemon's full ServerInfo regardless of
    // the caller's scope — the scope is enforced on `tools/call`, never
    // on the handshake. The escalation test below confirms this is the
    // contract: a Read-only token can perform the handshake AND still be
    // denied Ingest later.
    let (_dir, mut client, _secret) = spawn_auth_server(&[Capability::Read]).await;

    let caps = client
        .handshake(default_client_hello("read-only-client/0.1"))
        .await
        .expect("handshake must succeed even with Read-only scope");

    // The negotiated capabilities reflect what the *daemon* supports, not
    // what the *caller* is allowed to invoke. Foundation §8 forbids
    // discovery from broadening scope.
    assert!(
        caps.supported_operations
            .contains(&ClientOperation::McpToolCall),
        "handshake must not narrow daemon capabilities to Read-only"
    );
    assert_eq!(caps.server_name, "oxibrain");

    // Read works (Read cap).
    let _ = client.contradictions("personal").await.expect("read");

    // Ingest is denied — proving scope is still enforced after the handshake.
    let denied = client
        .ingest("escalation attempt", "personal", "evil.md")
        .await;
    assert!(denied.is_err(), "ingest must still be denied");
    let msg = denied.unwrap_err().to_string();
    assert!(
        msg.contains("Ingest") || msg.contains("token lacks"),
        "expected scope denial, got: {msg}"
    );
}
#[tokio::test]
async fn handshake_server_info_lists_supported_range() {
    let (_dir, mut reader, mut writer) = raw_session().await;
    let resp = raw_handshake(&mut reader, &mut writer, PROTOCOL_VERSION_MAX).await;
    let result = resp.get("result").expect("result");
    assert_eq!(
        result["min_compatible"].as_u64().unwrap() as u32,
        PROTOCOL_VERSION_MIN
    );
    assert_eq!(
        result["max_compatible"].as_u64().unwrap() as u32,
        PROTOCOL_VERSION_MAX
    );
    assert!(result["store_format_version"].as_u64().unwrap() >= 1);
    assert_eq!(result["server_name"], "oxibrain");
    assert!(result["server_version"].is_string());
    assert!(
        result["supported_operations"]
            .as_array()
            .unwrap()
            .contains(&json!("mcp_tool_call")),
        "supported_operations missing mcp_tool_call"
    );
}
