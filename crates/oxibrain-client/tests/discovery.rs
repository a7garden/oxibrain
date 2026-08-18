//! Unit tests for the client-side discovery + handshake surface that need no
//! running server: socket-path resolution (`$OXIBRAIN_SOCKET` then `$HOME`),
//! endpoint construction, `ClientHello` construction, and fast-fail when no
//! daemon is listening.
//!
//! The server-backed handshake tests and the full client round-trips live in
//! the `oxibrain-mcp` crate (`tests/handshake.rs`, `tests/client_round_trip.rs`):
//! `cargo publish` resolves dev-dependencies against the crates.io index, so a
//! dev-dependency from this crate on `oxibrain-mcp` would form a publish
//! cycle (mcp depends on client in production).

#![cfg(unix)]

use oxibrain_client::discovery::{BrainEndpoint, DiscoveryError, default_socket_path};
use oxibrain_client::protocol::{
    BrainProtocolVersion, ClientHello, ClientOperation, PROTOCOL_VERSION_MAX, PROTOCOL_VERSION_MIN,
    default_client_hello,
};
use oxibrain_client::{BrainCapabilities, BrainClient};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

/// Tests in this module mutate process-global env vars. Run them
/// sequentially so a stale var from one test cannot pollute another.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
