//! Degradation test (DESIGN §14.3).
//!
//! The C1 contract: "the brain is additive, never load-bearing." When the
//! daemon is unreachable, every consumer-facing API must fail fast with a
//! typed error rather than hanging. This test verifies that.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use oxibrain_client::BrainClient;
use std::time::Instant;

#[tokio::test]
async fn connect_nonexistent_socket_fails_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("no-daemon-here.sock");

    let start = Instant::now();
    let result = BrainClient::connect(&path).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must error on non-existent socket");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("connect")
            || msg.contains("no such file")
            || msg.contains("Connection refused"),
        "expected connection error, got: {msg}"
    );
    // Must fail fast — under 1 second.
    assert!(elapsed.as_secs() < 1, "took {elapsed:?}, expected < 1s");
}

#[tokio::test]
async fn connect_with_token_nonexistent_socket_fails_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("no-daemon-here.sock");

    let start = Instant::now();
    let result = BrainClient::connect_with_token(&path, "fake-token").await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must error on non-existent socket");
    assert!(elapsed.as_secs() < 1, "took {elapsed:?}, expected < 1s");
}

#[tokio::test]
async fn connect_endpoint_nonexistent_socket_fails_fast() {
    use oxibrain_client::discovery::BrainEndpoint;
    use std::time::Instant;

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("no-daemon-here.sock");
    let endpoint = BrainEndpoint::from_path(path.clone()).unwrap();

    let start = Instant::now();
    let result = BrainClient::connect_endpoint(&endpoint).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must error on non-existent socket");
    assert!(elapsed.as_secs() < 1, "took {elapsed:?}, expected < 1s");
}
