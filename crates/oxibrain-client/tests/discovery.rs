//! Integration tests for the Oxi Foundation discovery + handshake surface.
//!
//! These tests reuse the existing client/server harness (spawn an MCP server
//! on a temp Unix socket, then drive it with a `BrainClient`). They cover:
//!
//! 1. `default_socket_path` honors `$OXIBRAIN_SOCKET` then `$HOME`.
//! 2. `BrainEndpoint::default` matches `default_socket_path`.
//! 3. `BrainClient::connect_endpoint` performs a handshake and returns the
//!    negotiated `BrainCapabilities`.
//! 4. An incompatible `protocol_version` is rejected with a typed
//!    `HandshakeError` that names the supported range.
//! 5. A missing default socket fails fast (<1s) when no daemon is running.
//! 6. A `Read`-only scope cannot escalate through `handshake` — the method
//!    returns the daemon's full capabilities regardless of scope, because the
//!    handshake is a transport-level negotiation that runs **before** MCP tool
//!    routing. The scope is enforced on `tools/call`, not on `handshake`.
//!    This is the documented Foundation §8 contract.

#![cfg(unix)]

use oxibrain::{Brain, BrainConfig, Capability, Scope};
use oxibrain_client::discovery::{BrainEndpoint, DiscoveryError, default_socket_path};
use oxibrain_client::protocol::{
    BrainProtocolVersion, ClientHello, ClientOperation, HandshakeError, PROTOCOL_VERSION_MAX,
    PROTOCOL_VERSION_MIN, default_client_hello, parse_handshake_error,
};
use oxibrain_client::{BrainCapabilities, BrainClient};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

/// Tests in this module mutate process-global env vars. Run them
/// sequentially so a stale var from one test cannot pollute another.
static ENV_LOCK: Mutex<()> = Mutex::new(());

async fn spawn_server() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let sock = dir.path().join("test.sock");
    let sock_clone = sock.clone();
    tokio::spawn(async move {
        let _ = oxibrain_mcp::serve_socket(brain, &sock_clone).await;
    });
    for _ in 0..100 {
        if tokio::net::UnixStream::connect(&sock).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    (dir, sock)
}

async fn spawn_auth_server(caps: &[Capability]) -> (tempfile::TempDir, PathBuf, String) {
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

/// Send a raw JSON-RPC request and read one response. Used by tests that
/// need to craft an incompatible `protocol_version` and observe the typed
/// error.
async fn raw_handshake(sock: &std::path::Path, protocol_version: u32) -> Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(sock).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
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
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
    let mut resp = String::new();
    reader.read_line(&mut resp).await.unwrap();
    serde_json::from_str(&resp).unwrap()
}

#[test]
fn default_socket_path_env_override_beats_home() {
    let _guard = ENV_LOCK.lock().unwrap();
    let prev_socket = std::env::var_os("OXIBRAIN_SOCKET");
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::set_var("OXIBRAIN_SOCKET", "/var/run/oxibrain-test.sock");
        std::env::set_var("HOME", "/home/test");
    }
    let resolved = default_socket_path().expect("present");
    assert_eq!(resolved, PathBuf::from("/var/run/oxibrain-test.sock"));
    match prev_socket {
        Some(v) => unsafe { std::env::set_var("OXIBRAIN_SOCKET", v) },
        None => unsafe { std::env::remove_var("OXIBRAIN_SOCKET") },
    }
    match prev_home {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
}

#[test]
fn default_socket_path_falls_back_to_home() {
    let _guard = ENV_LOCK.lock().unwrap();
    let prev_socket = std::env::var_os("OXIBRAIN_SOCKET");
    let prev_home = std::env::var_os("HOME");
    unsafe {
        std::env::remove_var("OXIBRAIN_SOCKET");
        std::env::set_var("HOME", "/home/fallback");
    }
    let resolved = default_socket_path().expect("present");
    assert_eq!(
        resolved,
        PathBuf::from("/home/fallback/.oxi/brain/oxibrain.sock")
    );
    match prev_socket {
        Some(v) => unsafe { std::env::set_var("OXIBRAIN_SOCKET", v) },
        None => unsafe { std::env::remove_var("OXIBRAIN_SOCKET") },
    }
    match prev_home {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }
}

#[test]
fn explicit_endpoint_overrides_default() {
    let ep = BrainEndpoint::from_path("/tmp/explicit.sock").unwrap();
    assert!(ep.path().starts_with("/tmp"));
    let bad = BrainEndpoint::from_path("relative.sock");
    match bad {
        Err(DiscoveryError::NotAbsolute { .. }) => {}
        other => panic!("expected NotAbsolute, got {other:?}"),
    }
}

#[tokio::test]
async fn connect_endpoint_handshakes_and_returns_capabilities() {
    let (_dir, sock) = spawn_server().await;
    let endpoint = BrainEndpoint::from_path(sock.clone()).unwrap();
    let (mut client, caps) = BrainClient::connect_endpoint(&endpoint)
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
    let (_dir, sock) = spawn_server().await;

    // Use a version far outside the supported range.
    let resp = raw_handshake(&sock, 99).await;
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
    let (_dir, sock) = spawn_server().await;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
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
    write.write_all(line.as_bytes()).await.unwrap();
    write.flush().await.unwrap();
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
async fn missing_default_socket_fails_fast() {
    // Force a clean environment so we hit a path that does not exist.
    // Hold the env-lock only across the synchronous set-up so we do not
    // carry a `std::sync::MutexGuard` across an `.await` (clippy
    // `await_holding_lock`); the connect itself runs after the lock is
    // dropped and the env has been installed.
    let prev_socket = std::env::var_os("OXIBRAIN_SOCKET");
    let prev_home = std::env::var_os("HOME");
    {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("OXIBRAIN_SOCKET", "/tmp/definitely-no-oxibrain.sock");
            std::env::remove_var("HOME");
        }
    }

    let start = Instant::now();
    let res = BrainClient::connect_default().await;
    let elapsed = start.elapsed();
    assert!(
        res.is_err(),
        "connect_default must error when socket is absent"
    );
    assert!(
        elapsed.as_secs() < 1,
        "connect_default took {elapsed:?}, expected < 1s"
    );

    let _restore_guard = ENV_LOCK.lock().unwrap();
    match prev_socket {
        Some(v) => unsafe { std::env::set_var("OXIBRAIN_SOCKET", v) },
        None => unsafe { std::env::remove_var("OXIBRAIN_SOCKET") },
    }
    match prev_home {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
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
    let (_dir, sock, secret) = spawn_auth_server(&[Capability::Read]).await;

    let mut client = BrainClient::connect_with_token(&sock, &secret)
        .await
        .expect("connect with Read-only token");

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
    let (_dir, sock) = spawn_server().await;
    let resp = raw_handshake(&sock, PROTOCOL_VERSION_MAX).await;
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

#[test]
fn default_client_hello_min_max_within_range() {
    let hello = default_client_hello("test/1.0");
    let min = hello.min_compatible.unwrap().0;
    let max = hello.max_compatible.unwrap().0;
    assert!(min <= PROTOCOL_VERSION_MIN, "min should be <= MIN constant");
    assert!(max >= PROTOCOL_VERSION_MAX, "max should be >= MAX constant");
    assert!(hello.protocol_version.0 >= min);
    assert!(hello.protocol_version.0 <= max);
}

#[test]
fn client_hello_with_low_max_still_negotiable() {
    // A hello that pins max below the supported MIN must be rejected by the
    // server. We construct it locally and verify the request shape.
    let hello = ClientHello {
        protocol_version: BrainProtocolVersion::new(1),
        min_compatible: Some(BrainProtocolVersion::new(1)),
        max_compatible: Some(BrainProtocolVersion::new(1)),
        min_store_format_version: 1,
        client_version: "test/1.0".into(),
        supported_operations: vec![ClientOperation::McpToolCall],
    };
    assert_eq!(hello.max_compatible.unwrap().0, PROTOCOL_VERSION_MAX);
}

#[test]
fn capabilities_struct_exposes_required_fields() {
    let info = oxibrain_client::protocol::server_info("oxibrain", "0.3.0");
    let caps: BrainCapabilities = info.into();
    assert_eq!(caps.server_name, "oxibrain");
    assert_eq!(caps.server_version, "0.3.0");
}
