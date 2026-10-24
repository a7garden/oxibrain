//! The actual CLI binary, isolated engine, stdin and ENV-name credentials.
use oxibrain::{Brain, BrainConfig, Scope, SourceRef, TrustTier};
use oxibrain_core::{
    Capability,
    publication::{PUBLICATION_VERSION, PublishViewV1, ViewOrigin, ViewRange, exact_digest},
};
use oxibrain_ports::{ClockPort, SystemClock};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn command(
    dir: &Path,
    operation: &str,
    input: &[u8],
    origin: Option<&str>,
    destination: Option<&str>,
) -> (i32, Value, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oxibrain"));
    command
        .arg("--dir")
        .arg(dir)
        .args(["admin", "source", "view", operation]);
    if let Some(secret) = origin {
        command
            .args(["--origin-token-env", "CLI_SOURCE_ORIGIN_TOKEN"])
            .env("CLI_SOURCE_ORIGIN_TOKEN", secret);
    }
    if let Some(secret) = destination {
        command
            .args(["--destination-token-env", "CLI_SOURCE_DESTINATION_TOKEN"])
            .env("CLI_SOURCE_DESTINATION_TOKEN", secret);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fixture CLI launch");
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(input);
    }
    let output = child.wait_with_output().expect("fixture CLI completion");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let value = serde_json::from_str(&stdout).expect("one machine JSON envelope");
    (output.status.code().unwrap_or(-1), value, stderr)
}
async fn token(brain: &Brain, space: &str, capability: Capability) -> String {
    let scope = Scope {
        spaces: vec![space.into()],
        caps: [capability].into_iter().collect(),
        predicate_filter: None,
        entity_type_filter: None,
        expires_at: None,
        label: "isolated CLI source-view test".into(),
    };
    brain
        .issue_token(&scope, "fixture", Some("isolated CLI source-view test"))
        .await
        .unwrap()
        .1
}
#[tokio::test]
async fn actual_cli_publish_read_withdraw_retract_and_reopen_keep_exact_scopes_and_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let origin = brain.ensure_space("private-origin").await.unwrap();
    let destination = brain.ensure_space("shared-destination").await.unwrap();
    let content = "Published sentence. PRIVATE_SOURCE_SUFFIX";
    let episode = brain
        .ingest(
            &origin,
            content.into(),
            SourceRef::Note {
                path: "fixture-private-source".into(),
            },
            TrustTier::Untrusted,
            "fixture-only",
        )
        .await
        .unwrap();
    let source_read = token(&brain, &origin, Capability::Read).await;
    let source_write = token(&brain, &origin, Capability::Write).await;
    let dest_write = token(&brain, &destination, Capability::Write).await;
    let dest_read = token(&brain, &destination, Capability::Read).await;
    let selected = "Published sentence.";
    let request = PublishViewV1 {
        occurrence_id: "cli-source-view-occurrence".into(),
        origin: ViewOrigin::Episode {
            space: origin.clone(),
            episode_id: episode.clone(),
            revision: exact_digest(content),
        },
        range: ViewRange {
            start: 0,
            end: selected.len(),
        },
        bytes: selected.into(),
        digest: exact_digest(selected),
        destination: destination.clone(),
        policy_revision: "owner-explicit-test/1".into(),
        occurred_at: SystemClock.now(),
    };
    let payload =
        serde_json::to_vec(&json!({"version":PUBLICATION_VERSION,"request":request})).unwrap();
    let (code, published, stderr) = command(
        dir.path(),
        "publish",
        &payload,
        Some(&source_read),
        Some(&dest_write),
    );
    assert_eq!(code, 0, "{published}");
    assert_eq!(published["status"], "success");
    let receipt = published["data"].clone();
    let id = receipt["publication_id"].as_str().unwrap();
    for secret in [&source_read, &source_write, &dest_read, &dest_write] {
        assert!(!published.to_string().contains(secret));
        assert!(!stderr.contains(secret));
    }
    assert!(!published.to_string().contains("PRIVATE_SOURCE_SUFFIX"));
    assert!(!published.to_string().contains(&episode));
    let (_, retry, _) = command(
        dir.path(),
        "publish",
        &payload,
        Some(&source_read),
        Some(&dest_write),
    );
    assert_eq!(retry["data"], receipt);
    let read = json!({"version":PUBLICATION_VERSION,"destination":destination,"publication_id":id});
    let (code, view, _) = command(
        dir.path(),
        "read",
        &serde_json::to_vec(&read).unwrap(),
        None,
        Some(&dest_read),
    );
    assert_eq!(code, 0, "{view}");
    assert_eq!(view["data"]["bytes"], selected);
    assert_eq!(view["data"]["extractable"], false);
    assert_eq!(view["data"]["independent_support"], false);
    assert!(!view.to_string().contains("PRIVATE_SOURCE_SUFFIX"));
    assert!(!view.to_string().contains(&episode));
    assert!(!view.to_string().contains(&origin));
    let (code, denied, stderr) = command(
        dir.path(),
        "read",
        &serde_json::to_vec(&read).unwrap(),
        None,
        Some(&source_read),
    );
    assert_eq!(code, 4);
    assert_eq!(denied["error"]["code"], "publication_unavailable");
    assert!(!denied.to_string().contains(&source_read));
    assert!(!stderr.contains(&source_read));
    let withdraw = json!({"version":PUBLICATION_VERSION,"destination":destination,"publication_id":id,"occurrence_id":"cli-withdraw-original"});
    let (code, withdrawn, _) = command(
        dir.path(),
        "withdraw",
        &serde_json::to_vec(&withdraw).unwrap(),
        None,
        Some(&dest_write),
    );
    assert_eq!(code, 0, "{withdrawn}");
    let (_, closed, _) = command(
        dir.path(),
        "read",
        &serde_json::to_vec(&read).unwrap(),
        None,
        Some(&dest_read),
    );
    assert_eq!(closed["data"]["state"], "closed");
    assert_eq!(closed["data"]["bytes"], "");
    let (_, retry, _) = command(
        dir.path(),
        "publish",
        &payload,
        Some(&source_read),
        Some(&dest_write),
    );
    assert_eq!(
        retry["data"], receipt,
        "replaying original publish must not revive withdrawn bytes"
    );
    let retract = json!({"version":PUBLICATION_VERSION,"origin":request.origin,"occurrence_id":"cli-source-retract-original"});
    let (code, retracted, _) = command(
        dir.path(),
        "retract",
        &serde_json::to_vec(&retract).unwrap(),
        Some(&source_write),
        None,
    );
    assert_eq!(code, 0, "{retracted}");
    let reopened = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    assert!(
        reopened.get_episode(&episode).await.unwrap().is_some(),
        "source retraction is not raw source deletion"
    );
}
#[test]
fn actual_cli_rejects_oversized_duplicate_and_raw_credential_payload_without_echo_or_store_creation()
 {
    for bytes in [
        vec![b' '; 1_048_577],
        b"{\"token\":\"PRIVATE_CREDENTIAL\",\"token\":\"OTHER\"}".to_vec(),
    ] {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("not-created");
        let (code, response, stderr) = command(
            &dir,
            "publish",
            &bytes,
            Some("FIXTURE_ORIGIN_SECRET"),
            Some("FIXTURE_DEST_SECRET"),
        );
        assert_eq!(code, 2);
        assert_eq!(response["error"]["code"], "invalid_input");
        assert!(!response.to_string().contains("PRIVATE_CREDENTIAL"));
        assert!(!stderr.contains("PRIVATE_CREDENTIAL"));
        assert!(!dir.exists());
    }
}
