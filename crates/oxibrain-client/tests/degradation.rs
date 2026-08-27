//! Degradation test (DESIGN §14.3).
//!
//! The C1 contract: "the brain is additive, never load-bearing." When the
//! local server process cannot be started, every consumer-facing API must
//! fail fast with a typed error rather than hanging. The daemonless
//! transport has no socket to probe — the failure mode is a spawn error.

#![cfg_attr(test, allow(clippy::unwrap_used))]

use oxibrain_client::{BrainClient, LocalProcessEndpoint};
use std::time::Instant;

#[tokio::test]
async fn spawn_local_missing_executable_fails_fast() {
    let endpoint = LocalProcessEndpoint::new(
        "/nonexistent/oxibrain-for-sure-missing",
        "/tmp/definitely-no-brain",
    );

    let start = Instant::now();
    let result = BrainClient::spawn_local(endpoint).await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must error on non-existent executable");
    assert!(
        elapsed.as_secs() < 5,
        "took {elapsed:?}, expected fast failure"
    );
}

#[tokio::test]
async fn spawn_local_with_token_missing_executable_fails_fast() {
    let endpoint = LocalProcessEndpoint::new(
        "/nonexistent/oxibrain-for-sure-missing",
        "/tmp/definitely-no-brain",
    );

    let start = Instant::now();
    let result = BrainClient::spawn_local_with_token(endpoint, "fake-token").await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must error on non-existent executable");
    assert!(
        elapsed.as_secs() < 5,
        "took {elapsed:?}, expected fast failure"
    );
}
