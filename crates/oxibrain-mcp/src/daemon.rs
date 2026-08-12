//! Daemon lifecycle: PID-file management and graceful-shutdown signal
//! handling (DESIGN §4.3, §15).
//!
//! The advisory lock in `oxibrain-store` is the real single-writer enforcement
//! (P8). The PID file is informational — it lets external supervisors (launchd)
//! and monitoring tools find the daemon process, send it signals, and detect a
//! crash. Backgrounding itself is delegated to external supervision (§15: "the
//! same artifact … is the one launchd supervises"); the binary never forks.

use oxibrain::Brain;
use std::path::{Path, PathBuf};

/// RAII guard for `<dir>/.oxibrain.pid`.
///
/// Writes the current process id on creation and removes the file on drop, but
/// only if the file still contains *our* pid — another daemon that started
/// after us may have overwritten it, and we must not delete its file.
///
/// The advisory lock acquired by `Brain::open` prevents two daemons from ever
/// holding the same store simultaneously, so a stale PID file (left behind by a
/// crash) is naturally overwritten on the next start. No separate liveness
/// check is needed.
pub struct PidFile {
    path: PathBuf,
    pid: u32,
}

impl PidFile {
    /// Write our pid to `<dir>/.oxibrain.pid`, creating or overwriting the file.
    pub fn acquire(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join(".oxibrain.pid");
        let pid = std::process::id();
        std::fs::write(&path, pid.to_string())?;
        Ok(Self { path, pid })
    }

    /// The path to the PID file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        // Only remove if the file still names us. If another daemon overwrote
        // it, leave the file alone.
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.trim().parse::<u32>() == Ok(self.pid) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Wait for a shutdown signal: SIGINT (Ctrl+C) or, on Unix, SIGTERM (what
/// launchd and systemd send on stop). Completes once, then the caller should
/// stop accepting new work.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Background extraction worker (DESIGN §7.6, sub-project L1).
///
/// Periodically calls `brain.extract_pending()` to drain the `ingest_jobs`
/// queue. Stops when `stop` is signaled (e.g., by the daemon shutdown path).
/// Single-writer actor (P8) serializes all writes; the worker claims jobs
/// (atomic SQL), processes them (LLM call outside any transaction per §7.2),
/// then writes results (short transaction). Multiple LLM calls can be in-flight
/// concurrently — only the write-back is serialized.
pub async fn run_extraction_worker(
    brain: std::sync::Arc<Brain>,
    space: String,
    config: oxibrain_core::extraction::ExtractorConfig,
    budget: oxibrain_core::extraction::ExtractionBudget,
    interval: std::time::Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = brain.extract_pending(&space, &config, &budget).await {
                    tracing::warn!(
                        space = %space,
                        error = %e,
                        "background extraction tick failed"
                    );
                }
            }
            _ = stop.changed() => {
                if *stop.borrow() {
                    tracing::info!(space = %space, "background extraction worker stopping");
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_writes_and_removes() {
        let dir = tempfile::TempDir::new().unwrap();
        let pid_path = dir.path().join(".oxibrain.pid");

        {
            let pf = PidFile::acquire(dir.path()).unwrap();
            assert!(pid_path.exists(), "PID file should exist while held");
            let content = std::fs::read_to_string(&pid_path).unwrap();
            assert_eq!(content, std::process::id().to_string());
            assert_eq!(pf.path(), &pid_path);
        }
        assert!(!pid_path.exists(), "PID file should be removed after drop");
    }

    #[test]
    fn pid_file_overwrites_stale_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let pid_path = dir.path().join(".oxibrain.pid");

        // Simulate a crash: write a stale PID file.
        std::fs::write(&pid_path, "99999999").unwrap();

        let pf = PidFile::acquire(dir.path()).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, std::process::id().to_string());
        drop(pf);
        assert!(!pid_path.exists());
    }

    #[tokio::test]
    async fn extraction_worker_channel_round_trip() {
        // Sanity test: the watch channel used for stop signaling works as
        // expected. The full worker integration is exercised by manual
        // `oxibrain serve --daemon` runs.
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        rx.changed().await.unwrap();
        assert!(*rx.borrow());
    }
}
