//! `oxibrain sync <DIR> [--space s]` — vault sync with occurrence identity.
//!
//! Scans DIR recursively for `.md`/`.html` files (oxibrain-connectors),
//! classifies each against the ledger's event-path state for the vault source
//! (`oxibrain_core::classify_event`), and ingests new/modified files via the
//! event path with derived occurrence IDs (§4.2). The orchestration lives in
//! `oxibrain::vault` — this command is arg parsing, printing, and transport
//! selection.
//!
//! Two transports: with the store free, one embedded pass runs in-process.
//! When the daemon holds the P8 advisory lock, the command attaches to it
//! over the default socket and runs the same pass via the `sync/run` RPC —
//! the daemon is the sole writer, so recurring ingestion is always
//! daemon-hosted (ADR-010).
//!
//! Occurrence chain: `occurrence_id = H(source_id, locator, predecessor, content_hash)`.
//! A → B → A creates three events because the predecessor differs.
//! Unchanged files are skipped — re-syncing an unchanged tree is a no-op.
//! Legacy episodes (pre-event-identity) participate in Unchanged classification
//! but are never re-ingested.

use anyhow::Context;
use oxibrain::{vault, vault::SyncReport};
#[cfg(unix)]
use oxibrain_client::default_socket_path;
use std::path::Path;

pub async fn run(dir: &Path, root: &Path, space: &str) -> anyhow::Result<()> {
    let report = match oxibrain::Brain::open(oxibrain::BrainConfig::at(dir)).await {
        Ok(brain) => vault::sync_vault(&brain, root, space).await?,
        Err(oxibrain::BrainError::Locked { holder }) => {
            eprintln!("note: store locked ({holder}); attaching to the daemon socket");
            run_via_daemon(root, space).await?
        }
        Err(e) => return Err(e.into()),
    };
    print_report(&report);
    Ok(())
}

/// Run one sync pass through the daemon's `sync/run` RPC (trusted local
/// socket). Registers the vault as a pull source; the daemon adopts it into
/// a debounced watcher.
async fn run_via_daemon(root: &Path, space: &str) -> anyhow::Result<SyncReport> {
    let socket = socket_path()?;
    let mut client = oxibrain_client::BrainClient::connect(&socket)
        .await
        .with_context(|| format!("attach to daemon at {}", socket.display()))?;
    let out = client
        .sync_run(&root.to_string_lossy(), space)
        .await
        .context("sync/run on daemon")?;
    Ok(SyncReport {
        new: out.new,
        modified: out.modified,
        unchanged: out.unchanged,
    })
}

/// Resolve the daemon socket by the Oxi Foundation convention: explicit
/// `$OXIBRAIN_SOCKET`, else `~/.oxi/brain/oxibrain.sock`.
#[cfg(unix)]
fn socket_path() -> anyhow::Result<std::path::PathBuf> {
    default_socket_path().ok_or_else(|| {
        anyhow::anyhow!("no daemon socket: $OXIBRAIN_SOCKET unset and $HOME unavailable")
    })
}

#[cfg(not(unix))]
fn socket_path() -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("daemon attach is only supported on Unix")
}

fn print_report(report: &SyncReport) {
    if !report.new.is_empty() {
        for p in &report.new {
            println!("  new: {p}");
        }
    }
    if !report.modified.is_empty() {
        for p in &report.modified {
            println!("  modified: {p}");
        }
    }
    println!(
        "sync complete: {} new, {} unchanged, {} modified",
        report.new.len(),
        report.unchanged.len(),
        report.modified.len()
    );
}
