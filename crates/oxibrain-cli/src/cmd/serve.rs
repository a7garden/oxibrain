//! `oxibrain serve` — start the MCP server (DESIGN §12.4).
//!
//! Default transport is stdio (what Claude Desktop expects). `--socket <path>`
//! listens on a Unix-domain socket for the daemon topology (§4.3): several apps
//! share one brain through the single-writer store actor (P8).
//! `--socket <path> --require-token` gates each connection behind a token
//! handshake (§11.2). `--http <addr>` serves loopback HTTP.
//!
//! `--daemon` writes a PID file to `<dir>/.oxibrain.pid` so external supervisors
//! (launchd) can manage the process. When `--socket` is omitted **and**
//! `--daemon` is set, the daemon binds the Oxi Foundation default socket
//! (`$OXIBRAIN_SOCKET` if set, otherwise `$HOME/.oxi/brain/oxibrain.sock`).
//! Without `--daemon`, an omitted `--socket` keeps stdio as the transport.
//! The binary never forks — backgrounding is the supervisor's job (§15). All
//! socket/HTTP listeners shut down gracefully on SIGINT/SIGTERM.

use anyhow::Context;
use oxibrain::{Brain, BrainConfig};
#[cfg(unix)]
use oxibrain_client::discovery::default_socket_path;
use oxibrain_ports::BrainError;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

pub async fn run(
    dir: &Path,
    socket: Option<std::path::PathBuf>,
    http: Option<String>,
    require_token: bool,
    daemon: bool,
    ui_dir: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let brain = match Brain::open(BrainConfig::at(dir)).await {
        Ok(b) => b,
        Err(BrainError::Locked { holder }) => {
            // §4.3: "fails fast with a clear error if a daemon holds the lock,
            // and prints the command to attach instead."
            anyhow::bail!(
                "store is locked — another oxibrain process owns it ({holder}).\n\
                 If a daemon is already running, connect to it (e.g. via its socket) \
                 instead of starting a second one.\n\
                 To start a new daemon, ensure no other oxibrain process is running."
            );
        }
        Err(e) => return Err(e.into()),
    };

    // Write the PID file in daemon mode. RAII: removed on drop when `run`
    // returns (after graceful shutdown or error). The advisory lock inside the
    // Brain is the real single-writer guard (P8); the PID file is informational.
    let _pid = if daemon {
        let pid = oxibrain_mcp::PidFile::acquire(dir)
            .map_err(|e| anyhow::anyhow!("write PID file: {e}"))?;
        tracing::info!(
            "daemon PID {} → {}",
            std::process::id(),
            pid.path().display()
        );
        Some(pid)
    } else {
        None
    };

    if let Some(addr_str) = http {
        let addr: std::net::SocketAddr = addr_str
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --http address '{addr_str}': {e}"))?;
        return oxibrain_mcp::serve_http(brain, addr, ui_dir).await;
    }

    // Resolve the socket path: explicit --socket wins; otherwise, in daemon
    // mode, fall back to the Oxi Foundation default ($OXIBRAIN_SOCKET or
    // $HOME/.oxi/brain/oxibrain.sock). Stdio stays the fallback for
    // non-daemon invocations with no --socket.
    let socket_path = match socket {
        Some(p) => Some(p),
        #[cfg(unix)]
        None if daemon => Some(resolve_default_socket()?),
        None => None,
    };

    match socket_path {
        #[cfg(unix)]
        Some(path) => {
            prepare_socket_path(&path)?;
            if require_token {
                oxibrain_mcp::serve_socket_auth(brain, &path).await
            } else {
                tracing::warn!(
                    "serving on socket without --require-token: relying on filesystem \
                     permissions alone (DESIGN §11.2). Pass --require-token for token auth."
                );
                oxibrain_mcp::serve_socket(brain, &path).await
            }
        }
        #[cfg(not(unix))]
        Some(_) => anyhow::bail!("--socket is only supported on Unix"),
        None => {
            if require_token {
                anyhow::bail!("--require-token requires --socket");
            }
            oxibrain_mcp::serve_stdio(brain).await
        }
    }
}

/// Resolve the canonical Oxi Foundation default socket path.
///
/// Prefers `$OXIBRAIN_SOCKET` when set (the explicit override described in
/// `doc/spec/oxi-foundation-v1.md` §1), otherwise falls back to
/// `$HOME/.oxi/brain/oxibrain.sock`. Surfaces a clear error when neither is
/// available so the operator can fix their environment instead of guessing.
#[cfg(unix)]
fn resolve_default_socket() -> anyhow::Result<std::path::PathBuf> {
    if let Some(p) = default_socket_path() {
        return Ok(p);
    }
    anyhow::bail!(
        "no default oxibrain socket: neither $OXIBRAIN_SOCKET nor $HOME is set. \
         Specify --socket explicitly or export one of these environment variables."
    );
}

/// Prepare a socket path for binding: create the parent directory with
/// owner-only permissions and reconcile any pre-existing socket file.
///
/// Three checks run before the listener loop ever starts:
///
/// 1. The parent directory is created with `0o700` permissions so the socket
///    cannot be reached by users other than the daemon owner. This is the
///    "filesystem permissions" mode described in DESIGN §11.2.
/// 2. If a file already exists at the target path and it is *not* a socket
///    (regular file, directory, symlink to anything else), we bail — the
///    operator pointed us at the wrong path.
/// 3. If a *socket* file already exists, we probe it by trying
///    `connect()`: success means a competing daemon owns it (refuse);
///    `ECONNREFUSED`/`ENOENT` means it is stale (remove before binding).
///    A probe-fail with `EACCES`/`EPERM` means we cannot reach the listener
///    even though one is there — we still refuse rather than delete, since
///    removing a file we cannot read is unsafe.
///
/// The canonical `Brain::open` call in `run` also obtained the advisory
/// lock on the store (P8), so two daemons can never hold the same store at
/// once. The PID file is informational; the socket probe here is what stops
/// a *different-store* daemon from silently stealing another daemon's
/// socket path.
#[cfg(unix)]
fn prepare_socket_path(path: &Path) -> anyhow::Result<()> {
    use std::fs;
    use std::io::ErrorKind;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create socket parent {}", parent.display()))?;
            // Tighten the directory we just created to owner-only. We MUST
            // propagate this error: if `create_dir_all` succeeded under a
            // permissive umask and `set_permissions` failed silently, the
            // socket directory would stay accessible to other users, which
            // breaks the "filesystem permissions" mode (DESIGN §11.2).
            //
            // `set_permissions` follows symlinks: if `parent` is a symlink
            // to a directory, the permissions of the *target* directory are
            // modified. This is the intended behavior — operators point the
            // daemon at a path they own.
            fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("tighten socket parent {} to 0o700", parent.display()))?;
        } else if parent.exists() {
            // Pre-existing parent: deliberately do not chmod — that would
            // surprise the operator — but warn if the mode is too broad so
            // they can tighten it themselves if they want owner-only
            // isolation.
            match std::fs::metadata(parent) {
                Ok(meta) => {
                    let mode = meta.permissions().mode() & 0o777;
                    if mode & 0o077 != 0 {
                        tracing::warn!(
                            "socket parent {} already exists with mode {mode:o};                              not chmod-ing (operator-controlled). Other users may                              be able to reach the socket — use --require-token or                              restrict the directory manually.",
                            parent.display(),
                        );
                    }
                }
                Err(e) => tracing::warn!("could not stat socket parent {}: {e}", parent.display(),),
            }
        }
    }

    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let ft = meta.file_type();
        if !(ft.is_socket() || ft.is_fifo()) {
            anyhow::bail!("{} exists and is not a socket; cannot bind", path.display());
        }
        // Probe: try to connect. A successful connect means a live owner
        // exists — refuse rather than clobber.
        match futures_probe(path) {
            Ok(()) => anyhow::bail!(
                "{} is held by a live daemon; refusing to bind.                 If that daemon has crashed, remove the socket manually after                 verifying no process is listening.",
                path.display()
            ),
            Err(e)
                if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::ConnectionRefused =>
            {
                // Stale: the socket file is on disk but no listener is
                // accepting. Safe to remove.
                fs::remove_file(path)
                    .with_context(|| format!("remove stale socket {}", path.display()))?;
            }
            Err(other) => {
                // Permission denied, IO error, etc. Refuse to remove — the
                // file is owned by someone else and we cannot safely touch it.
                anyhow::bail!(
                    "{} could not be probed ({}); refusing to bind to avoid                     clobbering an unreachable owner",
                    path.display(),
                    other
                );
            }
        }
    }
    Ok(())
}

/// Synchronous wrapper around `tokio::net::UnixStream::connect` so
/// `prepare_socket_path` stays callable from non-async sites.
///
/// Spins up a tiny current-thread runtime on a dedicated thread for the
/// one-shot connect. The cost is a single thread creation + tiny rt per
/// daemon start, which is negligible.
#[cfg(unix)]
fn futures_probe(path: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let path = path.to_path_buf();
    let handle = std::thread::Builder::new()
        .name("oxibrain-socket-probe".into())
        .spawn(move || -> std::io::Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| Error::new(ErrorKind::Other, format!("probe rt: {e}")))?;
            rt.block_on(async move {
                let stream = tokio::net::UnixStream::connect(&path).await?;
                drop(stream);
                Ok::<(), std::io::Error>(())
            })
        })
        .map_err(|e| Error::new(ErrorKind::Other, format!("probe thread: {e}")))?;
    handle
        .join()
        .map_err(|_| Error::new(ErrorKind::Other, "probe thread panicked"))?
}

#[cfg(not(unix))]
fn prepare_socket_path(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}
