//! Integration tests for oxibrain-client: full round-trip through the socket
//! transport, including token authentication and scope enforcement.

#![cfg(unix)]

use oxibrain::{Brain, BrainConfig, Capability, Scope};
use oxibrain_client::BrainClient;
use oxibrain_ports::TIME_MAX;
use serde_json::json;

async fn spawn_server() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let sock = dir.path().join("test.sock");
    let sock_clone = sock.clone();
    tokio::spawn(async move {
        let _ = oxibrain_mcp::serve_socket(brain, &sock_clone).await;
    });

    // Wait for the listener to bind.
    for _ in 0..100 {
        if tokio::net::UnixStream::connect(&sock).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (dir, sock)
}

async fn spawn_auth_server(caps: &[Capability]) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space_id = brain.ensure_space("personal").await.unwrap();

    let scope = Scope {
        spaces: vec![space_id],
        caps: caps.iter().copied().collect(),
        ..Default::default()
    };
    let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();

    let sock = dir.path().join("auth.sock");
    let sock_clone = sock.clone();
    tokio::spawn(async move {
        let _ = oxibrain_mcp::serve_socket_auth(brain, &sock_clone).await;
    });

    for _ in 0..100 {
        if tokio::net::UnixStream::connect(&sock).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (dir, sock, secret)
}

#[tokio::test]
async fn client_round_trips_over_trusted_socket() {
    let (_dir, sock) = spawn_server().await;
    let mut client = BrainClient::connect(&sock).await.expect("connect");

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
    let (_dir, sock) = spawn_server().await;
    let mut client = BrainClient::connect(&sock).await.expect("connect");

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
    let (_dir, sock, secret) = spawn_auth_server(&[Capability::Read, Capability::Ingest]).await;

    let mut client = BrainClient::connect_with_token(&sock, &secret)
        .await
        .expect("connect with token");

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
    let (_dir, sock, secret) = spawn_auth_server(&[Capability::Read]).await;

    let mut client = BrainClient::connect_with_token(&sock, &secret)
        .await
        .expect("connect with token");

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
async fn client_auth_invalid_token_fails_connection() {
    let (_dir, sock, _secret) = spawn_auth_server(&[Capability::Read]).await;

    let result = BrainClient::connect_with_token(&sock, "bogus-token").await;
    assert!(result.is_err(), "invalid token should fail");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("authentication failed"), "got: {err}");
}

#[tokio::test]
async fn connect_endpoint_performs_handshake_then_tool_call() {
    use oxibrain_client::discovery::BrainEndpoint;

    let (_dir, sock) = spawn_server().await;
    let endpoint = BrainEndpoint::from_path(sock.clone()).unwrap();

    let (mut client, caps) = BrainClient::connect_endpoint(&endpoint)
        .await
        .expect("connect_endpoint + handshake");
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
async fn connect_endpoint_handshake_with_token_combines_auth_and_capability() {
    use oxibrain_client::discovery::BrainEndpoint;

    let (_dir, sock, secret) = spawn_auth_server(&[Capability::Read, Capability::Ingest]).await;
    let endpoint = BrainEndpoint::from_path(sock.clone()).unwrap();

    let (mut client, _caps) = BrainClient::connect_endpoint_handshake(&endpoint, Some(&secret))
        .await
        .expect("connect + auth + handshake");

    // Read works.
    let _ = client.contradictions("personal").await.expect("read");
    // Ingest works (Ingest cap).
    let ingested = client
        .ingest("combined bring-up", "personal", "endpoint.md")
        .await
        .expect("ingest");
    assert!(ingested.contains("Ingested as episode"));
}
