//! Integration tests for `oxibrain serve` — the daemonless stdio transport.
//!
//! The server is the REAL binary, spawned as a caller-owned child
//! (`serve --stdio --dir <tempdir>`), driven over its piped stdio. This is
//! exactly the shape `BrainClient::spawn_local` produces in production.
//!
//! Covered:
//!
//! - Raw newline-delimited JSON-RPC round trips (initialize, ping, tools).
//! - `BrainClient::spawn_local` + handshake + planes-aware search +
//!   `document_history` + `pending_stats` over the spawned child.
//! - Two-plane invariant 15: with `$HOME` pointed at an empty temp dir, a
//!   child serving an explicit `--dir` creates/opens NOTHING under that
//!   fake home (asserted by filesystem snapshot before/after).

#![cfg_attr(test, allow(clippy::unwrap_used))]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const SERVE_BINARY: &str = env!("CARGO_BIN_EXE_oxibrain");

struct StdioChild {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl StdioChild {
    fn spawn(dir: &Path, env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(SERVE_BINARY);
        cmd.arg("--dir")
            .arg(dir)
            .arg("admin")
            .arg("serve")
            .arg("--stdio");
        cmd.env_remove("OXIBRAIN_SOCKET");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn serve child");
        let stdin = child.stdin.take().expect("stdin pipe");
        let stdout = BufReader::new(child.stdout.take().expect("stdout pipe"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, value: &Value) {
        let mut line = serde_json::to_string(value).unwrap();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read one response line for the given request id (with a timeout via
    /// the child watchdog; the harness kills the child on drop).
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout unexpectedly");
        serde_json::from_str(&line).unwrap()
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let resp = self.recv();
        assert_eq!(resp["id"], id, "response id mismatch: {resp}");
        resp
    }
}

impl Drop for StdioChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn serve_stdio_raw_jsonrpc_round_trip() {
    let dir = tempfile::TempDir::new().unwrap();
    // Tools never implicitly create spaces (§4.4) — seed "personal" before
    // spawning the child.
    let brain = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
        .await
        .unwrap();
    let _ = brain.ensure_space("personal").await.unwrap();
    // Isolated HOME/OXI_HOME: the child must resolve the default brain dir
    // from a scratch home, never the developer's real ~/.oxi.
    let fake_home = tempfile::TempDir::new().unwrap();
    let home_str = fake_home.path().to_str().unwrap().to_string();
    let mut child = StdioChild::spawn(dir.path(), &[("HOME", &home_str)]);

    // MCP initialize handshake.
    let resp = child.request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {} }),
    );
    assert_eq!(resp["result"]["protocolVersion"], "2025-11-25");

    // Ping.
    let resp = child.request(2, "ping", json!({}));
    assert!(resp.get("result").is_some(), "ping failed: {resp}");

    // A tool call through the full session stack.
    let resp = child.request(
        3,
        "tools/call",
        json!({
            "name": "contradictions",
            "arguments": { "space": "personal" }
        }),
    );
    assert!(resp.get("error").is_none(), "tool call failed: {resp}");
    assert!(resp["result"]["content"][0]["text"].is_string());

    // The Foundation handshake rides the same channel.
    let resp = child.request(
        4,
        "handshake",
        json!({
            "protocol_version": 1,
            "min_store_format_version": 1,
            "client_version": "cli-test/0.1",
            "supported_operations": []
        }),
    );
    assert_eq!(resp["result"]["server_name"], "oxibrain");
}

#[tokio::test]
async fn spawn_local_client_round_trip_and_native_methods() {
    let dir = tempfile::TempDir::new().unwrap();
    // Tools never implicitly create spaces (§4.4) — seed "personal" before
    // spawning the child.
    let seed = oxibrain::Brain::open(oxibrain::BrainConfig::at(dir.path()))
        .await
        .unwrap();
    let _ = seed.ensure_space("personal").await.unwrap();
    drop(seed);
    let vault = tempfile::TempDir::new().unwrap();
    std::fs::write(vault.path().join("note.md"), "zephyr document body").unwrap();
    std::fs::write(
        dir.path().join("documents.toml"),
        format!(
            "[[root]]\nalias = \"vault\"\npath = \"{}\"\nspace = \"personal\"\n",
            vault.path().display()
        ),
    )
    .unwrap();

    let endpoint = oxibrain_client::LocalProcessEndpoint::new(SERVE_BINARY, dir.path());
    let mut client = oxibrain_client::BrainClient::spawn_local(endpoint)
        .await
        .expect("spawn_local");

    // Foundation handshake over the child's stdio.
    let caps = client
        .handshake(oxibrain_client::protocol::default_client_hello(
            "cli-test/0.1",
        ))
        .await
        .expect("handshake");
    assert_eq!(caps.server_name, "oxibrain");

    client.ping().await.expect("ping");

    // Ingest through the tool surface.
    let ingested = client
        .ingest("spawned-local round trip", "personal", "t.md")
        .await
        .expect("ingest");
    assert!(ingested.contains("Ingested as episode"));

    // Planes-aware search: documents only.
    let docs_only = client
        .search_planes("zephyr", "personal", "lexical", 5, &["documents"])
        .await
        .expect("documents-plane search");
    assert!(docs_only.memory.is_empty());
    assert!(
        docs_only
            .documents
            .iter()
            .any(|d| d.text.text.contains("zephyr document body")),
        "documents plane hit expected: {:?}",
        docs_only.documents
    );

    // Default search runs both planes.
    let both = client
        .search("zephyr", "personal", "lexical", 5)
        .await
        .expect("both-planes search");
    assert!(!both.documents.is_empty());

    // Native methods.
    let pending = client.pending_stats().await.expect("pending_stats");
    assert_eq!(pending.count, 1, "the ingested episode is uncached");

    // document_history on a plain (non-git) root errors clearly.
    let history = client
        .document_history("personal", "vault", "note.md", 5)
        .await;
    assert!(history.is_err(), "plain root has no git history");
}

#[tokio::test]
async fn spawn_local_with_token_round_trip() {
    // Issue a real token in the store, then spawn a child that authenticates
    // with it as its first message.
    use oxibrain::{Brain, BrainConfig, Capability, Scope};
    use oxibrain_ports::SystemClock;
    use std::sync::Arc;

    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::with_clock(BrainConfig::at(dir.path()), Arc::new(SystemClock))
        .await
        .unwrap();
    let space_id = brain.ensure_space("personal").await.unwrap();
    let scope = Scope {
        spaces: vec![space_id],
        caps: [Capability::Read].into_iter().collect(),
        ..Default::default()
    };
    let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();
    drop(brain);

    let endpoint = oxibrain_client::LocalProcessEndpoint::new(SERVE_BINARY, dir.path());
    let mut client = oxibrain_client::BrainClient::spawn_local_with_token(endpoint, &secret)
        .await
        .expect("spawn_local_with_token");

    // Read cap works.
    let _ = client.contradictions("personal").await.expect("read");
    // Ingest is denied by the Read-only scope.
    let denied = client.ingest("should be denied", "personal", "no.md").await;
    assert!(denied.is_err(), "ingest must be denied by scope");
}

#[test]
fn stdio_child_never_touches_fake_home() {
    // Invariant 15: with an explicit --dir the child must not create or
    // open anything under $HOME. Point HOME at an empty temp dir, snapshot
    // it, run a session, snapshot again.
    let dir = tempfile::TempDir::new().unwrap();
    let fake_home = tempfile::TempDir::new().unwrap();

    fn snapshot(root: &Path) -> BTreeSet<PathBuf> {
        let mut out = BTreeSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(cur) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&cur) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    out.insert(path.strip_prefix(root).unwrap().to_path_buf());
                    if path.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
        out
    }

    let before = snapshot(fake_home.path());
    let home_str = fake_home.path().to_str().unwrap().to_string();
    let mut child = StdioChild::spawn(dir.path(), &[("HOME", &home_str)]);

    let resp = child.request(
        1,
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {} }),
    );
    assert!(resp.get("result").is_some());
    let resp = child.request(2, "ping", json!({}));
    assert!(resp.get("result").is_some());

    let after = snapshot(fake_home.path());
    assert_eq!(
        before,
        after,
        "serve --stdio with explicit --dir must not touch $HOME \
         (created: {:?}, missing: {:?})",
        after.difference(&before).collect::<Vec<_>>(),
        before.difference(&after).collect::<Vec<_>>()
    );
    assert!(
        !fake_home.path().join(".oxi").exists(),
        "no ~/.oxi may be created by a child with an explicit --dir"
    );
}

#[test]
fn serve_stays_alive_until_stdin_closes() {
    // The child must not exit before its parent closes stdin — a session
    // child that exits early breaks every client round trip. Poll for a
    // while rather than a single fixed sleep: exec on a cold external
    // volume can be slow, and the invariant is "alive while stdin is open",
    // not "alive exactly 200 ms in".
    let dir = tempfile::TempDir::new().unwrap();
    // Isolated HOME: keep the child off the developer's real ~/.oxi.
    let fake_home = tempfile::TempDir::new().unwrap();
    let home_str = fake_home.path().to_str().unwrap().to_string();
    let mut child = StdioChild::spawn(dir.path(), &[("HOME", &home_str)]);
    child.request(1, "ping", json!({}));
    for _ in 0..20 {
        match child.child.try_wait() {
            Ok(Some(status)) => panic!("child exited early: {status:?}"),
            Ok(None) => {}
            Err(e) => panic!("try_wait must not error: {e}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
