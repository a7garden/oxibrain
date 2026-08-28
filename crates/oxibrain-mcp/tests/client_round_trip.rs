//! Round-trip integration tests: a real `oxibrain-mcp` session (driven
//! in-process over a duplex pipe — the exact newline-delimited JSON-RPC
//! framing `serve --stdio` speaks with a spawned child) exercised by
//! `oxibrain-client`. These live in the mcp crate (not client) because
//! `cargo publish` resolves dev-dependencies against the crates.io index — a
//! client dev-dependency on mcp would form a publish cycle.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use oxibrain::{Brain, BrainConfig, Capability, Scope};
use oxibrain_client::{BrainClient, DocumentHitDto};
use oxibrain_mcp::{BrainServer, run_session, run_session_gated};
use oxibrain_ports::TIME_MAX;
use serde_json::json;
use std::sync::Arc;

async fn spawn_server() -> (tempfile::TempDir, BrainClient) {
    let dir = tempfile::TempDir::new().unwrap();
    let (client_side, server_side) = tokio::io::duplex(8192);
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    // Tools never implicitly create spaces (§4.4) — seed the ones the
    // round-trip tests use.
    let _ = brain.ensure_space("personal").await.unwrap();
    let _ = brain.ensure_space("work").await.unwrap();
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session(Arc::new(BrainServer::from_brain(brain)), sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    (dir, BrainClient::from_io(cr, cw))
}

/// Spawn a token-gated session (the `serve --stdio` auth shape) with a
/// pre-authenticated client.
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

#[tokio::test]
async fn client_round_trips_over_trusted_session() {
    let (_dir, mut client) = spawn_server().await;

    // Ping
    client.ping().await.expect("ping");

    // Ingest
    let result = client
        .ingest("Alice works at Acme Corp", "personal", "test.md")
        .await
        .expect("ingest");
    assert!(result.contains("Ingested as episode"));

    // Contradictions (read, should return empty array JSON)
    let result = client
        .contradictions("personal")
        .await
        .expect("contradictions");
    assert!(result.is_array());
}

#[tokio::test]
async fn client_declare_and_get_entity_round_trip() {
    let (_dir, mut client) = spawn_server().await;

    let decl = json!({
        "op": "add_statement",
        "subject": { "surface": "Alice", "type": "Person" },
        "predicate": "employed_by",
        "object": { "kind": "entity", "surface": "Acme Corp", "type": "Organization" },
        "polarity": "affirm",
        "valid_from": 1000,
        "valid_to": TIME_MAX.0
    })
    .to_string();

    let result = client.declare("personal", &decl).await.expect("declare");
    assert!(result.contains("Declared as episode"));
}

#[tokio::test]
async fn client_auth_valid_token_allows_scoped_ops() {
    let (_dir, mut client, _secret) =
        spawn_auth_server(&[Capability::Read, Capability::Ingest]).await;

    // Ingest (Ingest cap) — should succeed.
    let result = client
        .ingest("A test note", "personal", "eval.md")
        .await
        .expect("ingest");
    assert!(result.contains("Ingested as episode"));

    // Contradictions (Read cap) — should succeed.
    let _ = client
        .contradictions("personal")
        .await
        .expect("contradictions");
}

#[tokio::test]
async fn client_auth_read_only_denies_ingest() {
    let (_dir, mut client, _secret) = spawn_auth_server(&[Capability::Read]).await;

    // Read is fine.
    let _ = client
        .contradictions("personal")
        .await
        .expect("read should work");

    // Ingest should be denied.
    let result = client
        .ingest("denied content", "personal", "blocked.md")
        .await;
    assert!(result.is_err(), "ingest should be denied by scope");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Ingest") || err.contains("token lacks"),
        "got: {err}"
    );
}

#[tokio::test]
async fn client_auth_read_only_denies_extract_uncached() {
    // The native extract method is write-gated: a Read-only scope cannot
    // drain the backlog through it.
    let (_dir, mut client, _secret) = spawn_auth_server(&[Capability::Read]).await;
    let result = client.extract_uncached(5).await;
    assert!(
        result.is_err(),
        "extract_uncached should be denied by scope"
    );
}

#[tokio::test]
async fn client_auth_read_only_can_read_pending_stats() {
    let (_dir, mut client, _secret) = spawn_auth_server(&[Capability::Read]).await;
    let pending = client.pending_stats().await.expect("pending_stats");
    assert_eq!(pending.count, 0);
}

#[tokio::test]
async fn client_auth_invalid_token_fails_connection() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let (client_side, server_side) = tokio::io::duplex(8192);
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session_gated(Arc::new(brain), sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let mut client = BrainClient::from_io(cr, cw);

    let result = client.auth("bogus-token").await;
    assert!(result.is_err(), "invalid token should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("authentication failed"), "got: {err}");
}

#[tokio::test]
async fn handshake_then_tool_call_over_session() {
    let (_dir, mut client) = spawn_server().await;
    let caps = client
        .handshake(oxibrain_client::protocol::default_client_hello(
            "round-trip/0.1",
        ))
        .await
        .expect("handshake");
    assert_eq!(caps.server_name, "oxibrain");

    // The negotiated client behaves like a normal one.
    client.ping().await.expect("ping");
    let ingested = client
        .ingest("endpoint round-trip", "personal", "endpoint.md")
        .await
        .expect("ingest");
    assert!(ingested.contains("Ingested as episode"));
}

#[tokio::test]
async fn auth_and_handshake_combine_over_gated_session() {
    let (_dir, mut client, _secret) =
        spawn_auth_server(&[Capability::Read, Capability::Ingest]).await;
    let caps = client
        .bring_up(
            None,
            oxibrain_client::protocol::default_client_hello("combined/0.1"),
        )
        .await
        .expect("handshake after auth");
    assert_eq!(caps.server_name, "oxibrain");

    // Read works.
    let _ = client.contradictions("personal").await.expect("read");
    // Ingest works (Ingest cap).
    let ingested = client
        .ingest("combined bring-up", "personal", "endpoint.md")
        .await
        .expect("ingest");
    assert!(ingested.contains("Ingested as episode"));
}

#[tokio::test]
async fn client_lists_spaces_over_session() {
    let (_dir, mut client) = spawn_server().await;

    let _ = client
        .ingest("preparing the work space", "work", "seed.md")
        .await
        .expect("ingest");

    let spaces = client.list_spaces().await.expect("list_spaces");
    assert!(spaces.iter().any(|s| s.name == "work"));
    assert!(spaces.iter().all(|s| !s.id.is_empty()));
}

#[tokio::test]
async fn client_search_returns_both_planes_and_native_methods() {
    // Documents plane via the session: seed a root, index through the search
    // tool itself (request-time reconcile), and confirm the envelope shape.
    let dir = tempfile::TempDir::new().unwrap();
    let vault = tempfile::TempDir::new().unwrap();
    std::fs::write(vault.path().join("note.md"), "quantum llama content").unwrap();
    std::fs::write(
        dir.path().join("documents.toml"),
        format!(
            "[[root]]\nalias = \"vault\"\npath = \"{}\"\nspace = \"personal\"\n",
            vault.path().display()
        ),
    )
    .unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("personal").await.unwrap();
    let (client_side, server_side) = tokio::io::duplex(8192);
    let (sr, sw) = tokio::io::split(server_side);
    tokio::spawn(async move {
        let _ = run_session(Arc::new(BrainServer::from_brain(brain)), sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let mut client = BrainClient::from_io(cr, cw);

    let response = client
        .search("quantum", "personal", "lexical", 10)
        .await
        .expect("search");
    assert!(response.memory.is_empty());
    assert!(
        response
            .documents
            .iter()
            .any(|d: &DocumentHitDto| d.text.contains("quantum llama content")),
        "documents plane should hit: {:?}",
        response.documents
    );
    assert!(!response.freshness.reconciled_roots.is_empty());

    // Planes-restricted search: documents only, memory untouched.
    let docs_only = client
        .search_planes("quantum", "personal", "lexical", 10, &["documents"])
        .await
        .expect("search planes");
    assert!(docs_only.memory.is_empty());
    assert!(!docs_only.documents.is_empty());

    // Native methods over the same session.
    let pending = client.pending_stats().await.expect("pending_stats");
    assert_eq!(pending.count, 0);
    assert!(pending.oldest_seq.is_none());

    // document_history on a plain (non-git) root errors clearly.
    let history = client
        .document_history("personal", "vault", "note.md", 5)
        .await;
    assert!(history.is_err(), "plain root has no git history");
}
