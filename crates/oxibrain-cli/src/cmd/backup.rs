use std::path::{Path, PathBuf};

pub async fn run_backup(
    _dir: &Path,
    _no_projection: bool,
    _no_cache: bool,
    _out: Option<PathBuf>,
) -> anyhow::Result<()> {
    // The CLI has not been wired through the facade's store-level
    // `oxibrain_store::backup::online_backup` yet (facade work is post-M0).
    // The previous raw-file copy skipped the WAL advisory lock and could
    // produce a torn snapshot, so we fail visibly instead of silently writing
    // a "backup" that may not be safe to restore.
    anyhow::bail!(
        "backup is not wired into the CLI in M0; the WAL-safe store API \
         oxibrain_store::backup::online_backup is available but CLI \
         integration is post-M0"
    );
}

pub async fn run_restore(_dir: &Path, _backup: PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("restore is not implemented in M0");
}
