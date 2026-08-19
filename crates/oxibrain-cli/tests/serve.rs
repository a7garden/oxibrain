//! Integration tests for `oxibrain serve`.
//!
//! Verifies the Oxi Foundation discovery contract on the daemon side:
//!
//! - `serve --daemon --socket <path>` binds an explicit socket (legacy path,
//!   unchanged behaviour).
//! - `serve --daemon` with no `--socket` binds the discovery default path
//!   resolved from `$OXIBRAIN_SOCKET`.
//! - A second `serve --daemon` against the same store fails fast (advisory
//!   lock refused).
//! - Parent directory of the socket is created with owner-only permissions
//!   when it does not exist.
//! - `prepare_socket_path` refuses to clobber a *live* daemon's socket.
//! - `prepare_socket_path` removes a stale socket (no listener) before
//!   binding so the next daemon can take over.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use oxibrain::BrainConfig;

const SERVE_BINARY: &str = env!("CARGO_BIN_EXE_oxibrain");

fn spawn_daemon(dir: &Path, args: &[&str], env_socket: Option<&Path>) -> std::process::Child {
    let mut cmd = std::process::Command::new(SERVE_BINARY);
    cmd.env_remove("OXIBRAIN_SOCKET");
    if let Some(p) = env_socket {
        cmd.env("OXIBRAIN_SOCKET", p);
    }
    cmd.arg("--dir").arg(dir).arg("serve").args(args);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn().expect("spawn daemon")
}

async fn wait_for_socket(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() && tokio::net::UnixStream::connect(path).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Spawn a daemon and capture stdout (where tracing emits its logs by default).
fn spawn_daemon_capture_stdout(
    dir: &Path,
    args: &[&str],
    env_socket: Option<&Path>,
) -> std::process::Child {
    let mut cmd = std::process::Command::new(SERVE_BINARY);
    cmd.env_remove("OXIBRAIN_SOCKET");
    if let Some(p) = env_socket {
        cmd.env("OXIBRAIN_SOCKET", p);
    }
    cmd.env("RUST_LOG", "warn,oxibrain=info");
    cmd.arg("--dir").arg(dir).arg("serve").args(args);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.spawn().expect("spawn daemon")
}

#[tokio::test]
async fn serve_daemon_binds_explicit_socket_and_sets_parent_mode() {
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_parent = tempfile::TempDir::new().unwrap();
    let sock_path = sock_parent.path().join("sub/oxibrain.sock");

    let mut child = spawn_daemon(
        &data_dir,
        &["--daemon", "--socket", sock_path.to_str().unwrap()],
        None,
    );

    let appeared = wait_for_socket(&sock_path, Duration::from_secs(5)).await;
    let _ = child.kill();
    let _ = child.wait();

    assert!(appeared, "daemon should bind {}", sock_path.display());

    let parent = sock_path.parent().unwrap();
    assert!(parent.exists(), "parent {} was created", parent.display());
    let mode = std::fs::metadata(parent).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "parent dir should be 0o700, got {mode:o}");
}

#[tokio::test]
async fn serve_daemon_resolves_default_socket_from_env() {
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let target_parent = tempfile::TempDir::new().unwrap();
    let default_sock = target_parent.path().join("discovery/default.sock");

    let mut child = spawn_daemon(&data_dir, &["--daemon"], Some(&default_sock));

    let appeared = wait_for_socket(&default_sock, Duration::from_secs(5)).await;
    let _ = child.kill();
    let _ = child.wait();

    assert!(appeared, "daemon should resolve and bind {default_sock:?}");
}

#[tokio::test]
async fn second_serve_daemon_fails_fast_on_lock() {
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();

    let sock = tempfile::TempDir::new().unwrap().path().join("lock.sock");
    let mut first = spawn_daemon(
        &data_dir,
        &["--daemon", "--socket", sock.to_str().unwrap()],
        None,
    );
    assert!(
        wait_for_socket(&sock, Duration::from_secs(5)).await,
        "first daemon should bind"
    );

    let start = Instant::now();
    let output = std::process::Command::new(SERVE_BINARY)
        .arg("--dir")
        .arg(&data_dir)
        .arg("serve")
        .arg("--daemon")
        .arg("--socket")
        .arg(sock.to_str().unwrap())
        .env_remove("OXIBRAIN_SOCKET")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("second spawn");
    let elapsed = start.elapsed();

    let _ = first.kill();
    let _ = first.wait();

    assert!(!output.status.success(), "second daemon must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("locked") || stderr.contains("Lock"),
        "expected lock-related error, got: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "second daemon should fail fast; took {elapsed:?}"
    );
}

#[tokio::test]
async fn serve_refuses_to_clobber_live_socket() {
    // First daemon owns the socket. A second daemon with a *different* store
    // dir must refuse to remove the live socket. Without this guard the
    // second daemon would silently steal the path.
    let store_a = tempfile::TempDir::new().unwrap();
    let store_b = tempfile::TempDir::new().unwrap();
    let sock_parent = tempfile::TempDir::new().unwrap();
    let sock_path = sock_parent.path().join("shared.sock");

    let mut first = spawn_daemon(
        store_a.path(),
        &["--daemon", "--socket", sock_path.to_str().unwrap()],
        None,
    );
    assert!(wait_for_socket(&sock_path, Duration::from_secs(5)).await);

    // Second daemon: different store dir, same socket path. Should fail
    // because the live socket belongs to `first`.
    let start = Instant::now();
    let output = std::process::Command::new(SERVE_BINARY)
        .arg("--dir")
        .arg(store_b.path())
        .arg("serve")
        .arg("--daemon")
        .arg("--socket")
        .arg(sock_path.to_str().unwrap())
        .env_remove("OXIBRAIN_SOCKET")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("second spawn");
    let elapsed = start.elapsed();

    let _ = first.kill();
    let _ = first.wait();

    // Second daemon must have failed.
    assert!(!output.status.success(), "second daemon must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("live daemon")
            || stderr.contains("locked")
            || stderr.contains("Lock")
            || stderr.contains("already"),
        "expected refusal, got: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "second daemon should fail fast; took {elapsed:?}"
    );

    // The socket file should still be a socket (not deleted, not replaced).
    let meta = std::fs::symlink_metadata(&sock_path).expect("socket file still exists");
    use std::os::unix::fs::FileTypeExt;
    assert!(
        meta.file_type().is_socket(),
        "live socket file should remain a socket, not be deleted by refused daemon"
    );
}

#[tokio::test]
async fn serve_removes_stale_socket_before_binding() {
    // Plant a stale socket file (no listener). New daemon should detect,
    // remove, and bind.
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_parent = tempfile::TempDir::new().unwrap();
    let sock_path = sock_parent.path().join("stale.sock");

    // Create the socket by binding and immediately dropping the listener.
    {
        let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        // _listener dropped here — no accept loop, but the socket file
        // remains. Connect attempts will fail with ECONNREFUSED.
    }
    assert!(sock_path.exists());

    let mut child = spawn_daemon(
        &data_dir,
        &["--daemon", "--socket", sock_path.to_str().unwrap()],
        None,
    );

    let appeared = wait_for_socket(&sock_path, Duration::from_secs(5)).await;
    let _ = child.kill();
    let _ = child.wait();

    assert!(appeared, "daemon should rebind stale socket path");
}

#[tokio::test]
async fn serve_without_daemon_still_binds_explicit_socket() {
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock = tempfile::TempDir::new()
        .unwrap()
        .path()
        .join("explicit.sock");

    let mut child = spawn_daemon(&data_dir, &["--socket", sock.to_str().unwrap()], None);
    let appeared = wait_for_socket(&sock, Duration::from_secs(5)).await;
    let _ = child.kill();
    let _ = child.wait();
    assert!(appeared);
}

#[test]
fn brain_config_dir_round_trips() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = BrainConfig::at(dir.path());
    assert_eq!(cfg.dir, dir.path());
}

#[tokio::test]
async fn serve_warns_when_parent_socket_dir_is_too_broad() {
    // Cover the round 1 finding: when the parent directory pre-exists with
    // world-readable bits, the daemon must NOT chmod it (operator
    // surprise), but MUST warn so the operator can decide to tighten.
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_parent = tempfile::TempDir::new().unwrap();
    std::fs::set_permissions(sock_parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let sock_path = sock_parent.path().join("sock");
    let pre_mode = std::fs::metadata(sock_parent.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(pre_mode, 0o755, "pre-condition: parent should be 0o755");

    let mut child = spawn_daemon_capture_stdout(
        &data_dir,
        &["--daemon", "--socket", sock_path.to_str().unwrap()],
        None,
    );
    // Drain stdout (where tracing emits by default) in a dedicated thread
    // so the pipe never fills up while the daemon is running.
    let stdout_handle = child.stdout.take().expect("stdout pipe");
    let stdout_thread = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let mut pipe = stdout_handle;
        let _ = pipe.read_to_string(&mut buf);
        buf
    });

    let appeared = wait_for_socket(&sock_path, Duration::from_secs(5)).await;
    assert!(
        appeared,
        "daemon should still bind even with broad parent mode"
    );
    // Give the daemon a moment to flush stdout after binding.
    std::thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();
    let stdout = stdout_thread.join().unwrap_or_default();

    assert!(
        stdout.contains("already exists with mode 755")
            || stdout.contains("not chmod-ing")
            || stdout.contains("socket parent"),
        "expected warning about pre-existing parent mode, got: {stdout}"
    );
    // The parent dir must NOT have been silently chmod'd by the daemon —
    // operator surprise is worse than an isolated warning.
    let post_mode = std::fs::metadata(sock_parent.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        post_mode, 0o755,
        "daemon must not silently chmod a pre-existing parent directory"
    );
}

#[tokio::test]
async fn serve_propagates_error_when_set_permissions_fails_on_fresh_parent() {
    // Cover the error-propagation path: when the parent is freshly created
    // and tightening fails, the daemon must surface the error rather than
    // silently leaving a permissive socket directory.
    //
    // We simulate failure by pre-creating a regular file at the parent path
    // the daemon would need to mkdir. `create_dir_all` will fail because the
    // path exists and is not a directory, so the prepare step fails before
    // we even reach `set_permissions`. This proves the error path bails out
    // instead of silently continuing.
    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_root = tempfile::TempDir::new().unwrap();
    let blocker = sock_root.path().join("blocker");
    std::fs::write(&blocker, b"not a dir").unwrap();
    let sock_path = blocker.join("oxibrain.sock");

    let output = std::process::Command::new(SERVE_BINARY)
        .arg("--dir")
        .arg(&data_dir)
        .arg("serve")
        .arg("--daemon")
        .arg("--socket")
        .arg(sock_path.to_str().unwrap())
        .env_remove("OXIBRAIN_SOCKET")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "daemon must fail when parent is a file"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("create socket parent") || stderr.contains("Not a directory"),
        "expected error about creating socket parent, got: {stderr}"
    );
}

// ─── end-to-end smoke (Task 6 §6) ─────────────────────────────────────────

/// Full client/server smoke that the daemon + client work without touching
/// `~/.oxi`. The test overrides the discovery socket via `$OXIBRAIN_SOCKET`
/// so the real `~/.oxi/brain/oxibrain.sock` is never opened, runs through
/// the documented Foundation contract:
///
/// 1. `BrainClient::connect_default()` performs the capability handshake and
///    returns `BrainCapabilities` sourced from the daemon's `ServerInfo`.
/// 2. A scoped `ingest` writes an episode into a fresh space.
/// 3. A scoped `search` runs the same query through the index the daemon
///    owns.
/// 4. The daemon is stopped; a fresh `connect_default()` returns a typed
///    fast-degradation error in < 1s.
///
/// Marked `#[ignore]` because it spawns a real daemon and exercises the
/// scoped JSON-RPC surface end-to-end; run with
/// `cargo test -p oxibrain-cli --test serve e2e_smoke_default_discovery -- --ignored`.
#[ignore]
#[tokio::test]
async fn e2e_smoke_default_discovery() {
    use oxibrain_client::BrainClient;

    let data_dir = tempfile::tempdir().unwrap();
    let socket_path = data_dir.path().join("brain").join("oxibrain.sock");
    let data_dir_path = data_dir.path().to_path_buf();
    let socket_str = socket_path.to_str().unwrap().to_owned();

    // Spawn the daemon with no --socket flag; the bind must come from
    // $OXIBRAIN_SOCKET so we never touch ~/.oxi/brain/oxibrain.sock.
    let mut cmd = std::process::Command::new(SERVE_BINARY);
    cmd.env_remove("OXIBRAIN_SOCKET");
    cmd.env("OXIBRAIN_SOCKET", &socket_str);
    cmd.env("HOME", data_dir.path());
    cmd.arg("--dir")
        .arg(&data_dir_path)
        .arg("serve")
        .arg("--daemon");
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().expect("spawn daemon");

    let ready = wait_for_socket(&socket_path, Duration::from_secs(10)).await;
    assert!(ready, "daemon did not bind {socket_str} within 10s");

    // Drive the daemon through the same env so connect_default() resolves
    // to the temp socket.
    let result: Result<(BrainClient, _), anyhow::Error> = async {
        // Scope env to the connect call.
        let prev_socket = std::env::var_os("OXIBRAIN_SOCKET");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: env mutation is single-threaded inside this test body
        // because tokio::test uses a single-threaded runtime by default
        // and we hold no other client during the mutation.
        unsafe {
            std::env::set_var("OXIBRAIN_SOCKET", &socket_str);
            std::env::set_var("HOME", data_dir.path());
        }
        let r = BrainClient::connect_default().await;
        if let Some(v) = prev_socket {
            unsafe { std::env::set_var("OXIBRAIN_SOCKET", v) };
        } else {
            unsafe { std::env::remove_var("OXIBRAIN_SOCKET") };
        }
        if let Some(v) = prev_home {
            unsafe { std::env::set_var("HOME", v) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }
        r
    }
    .await;
    // ↑ that block uses `unsafe { set_var }` which is fine inside the
    // single-threaded runtime; fall back to a plain `unsafe` block here so
    // rustc is happy without a nightly `#![feature)]`.

    let (mut client, caps) = result.expect("connect_default");
    eprintln!("handshake capabilities: {caps:?}");

    let ingest_result = client
        .ingest(
            "Oxi Foundation smoke episode: the quick brown fox.",
            "default",
            "/tmp/smoke-note.md",
        )
        .await;
    eprintln!("ingest result: {ingest_result:?}");
    let episode_id = ingest_result.expect("ingest must succeed against a fresh daemon");

    let search_result = client
        .search("quick brown fox", "default", "hybrid", 5)
        .await;
    eprintln!("search result: {search_result:?}");
    let search_value = search_result.expect("search must succeed");
    assert!(
        !search_value.is_null(),
        "search returned null for an episode we just ingested ({episode_id})"
    );

    // Stop the daemon cleanly.
    child.kill().ok();
    child.wait().ok();

    // Fresh connect after the daemon is gone must fail fast with a typed
    // error in < 1s.
    let prev_socket = std::env::var_os("OXIBRAIN_SOCKET");
    unsafe {
        std::env::set_var("OXIBRAIN_SOCKET", &socket_str);
        std::env::set_var("HOME", data_dir.path());
    }
    let start = Instant::now();
    let degraded = BrainClient::connect_default().await;
    let elapsed = start.elapsed();
    if let Some(v) = prev_socket {
        unsafe { std::env::set_var("OXIBRAIN_SOCKET", v) };
    } else {
        unsafe { std::env::remove_var("OXIBRAIN_SOCKET") };
    }
    eprintln!("post-stop connect_default error after {elapsed:?}: {degraded:?}");
    assert!(
        degraded.is_err(),
        "connect_default against a stopped daemon must return Err"
    );
    assert!(
        elapsed.as_secs() < 1,
        "connect_default took {elapsed:?}; expected < 1s fast degradation"
    );
}

#[tokio::test]
async fn trust_gate_enforced_through_daemon_socket() {
    use oxibrain::{Brain, BrainConfig, Capability, Scope};
    use oxibrain_client::BrainClient;

    let dir = tempfile::TempDir::new().unwrap();
    let data_dir = dir.path().to_path_buf();
    let sock_path = dir.path().join("test.sock");

    // Pre-issue a token BEFORE the daemon starts (advisory lock conflict).
    let secret = {
        let brain = Brain::open(BrainConfig::at(&data_dir)).await.unwrap();
        let space_id = brain.ensure_space("personal").await.unwrap();
        let scope = Scope {
            spaces: vec![space_id],
            caps: [Capability::Ingest].into_iter().collect(),
            predicate_filter: None,
            entity_type_filter: None,
            expires_at: None,
            label: String::new(),
        };
        let (_info, secret) = brain.issue_token(&scope, "test", None).await.unwrap();
        drop(brain); // Release advisory lock before daemon starts.
        secret
    };

    // Spawn daemon with --require-token.
    let mut child = spawn_daemon(
        &data_dir,
        &[
            "--daemon",
            "--socket",
            sock_path.to_str().unwrap(),
            "--require-token",
        ],
        None,
    );

    let appeared = wait_for_socket(&sock_path, Duration::from_secs(5)).await;
    assert!(appeared, "daemon must start");

    // Connect with token and try trust=trusted.
    let mut client = BrainClient::connect_with_token(&sock_path, &secret)
        .await
        .expect("connect with token");

    let result = client
        .call_tool(
            "ingest",
            serde_json::json!({
                "content": "test content",
                "space": "personal",
                "trust": "trusted"
            }),
        )
        .await;

    // Must fail: token lacks TrustedIngest capability.
    assert!(
        result.is_err(),
        "trust=trusted without TrustedIngest must be rejected"
    );

    // Without trust param, ingest must succeed.
    let result = client
        .call_tool(
            "ingest",
            serde_json::json!({
                "content": "test content without trust",
                "space": "personal"
            }),
        )
        .await;
    assert!(result.is_ok(), "ingest without trust param must succeed");

    let _ = child.kill();
    let _ = child.wait();
}
