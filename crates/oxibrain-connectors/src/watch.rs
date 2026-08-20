//! Debounced directory watcher for vault pull sources (ECOSYSTEM C4).
//!
//! "The connector watches the vault": filesystem events are coalesced with a
//! quiet-period debounce so a burst of saves (an editor's write-temp-rename
//! cycle, a sync tool touching many files) settles into exactly one tick.
//! The C4 minimum-diff threshold is not reimplemented here — content-hash
//! classification (`oxibrain-core::sync::classify_event`) already makes a
//! re-scan of unchanged files a no-op, which is the same guarantee.
//!
//! The watcher handle must be kept alive by the caller; dropping it closes
//! the event channel and the debounce thread exits.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// Watch `dir` recursively and invoke `on_settled` once the tree has been
/// quiet for `quiet`. The callback runs on the debounce thread; it should
/// hand off to whatever performs the sync pass (e.g. block on the brain
/// runtime). Events arriving while the callback runs are queued and produce
/// a further tick afterwards — settling is re-entrant by construction.
pub fn spawn_quiet<F>(
    dir: &Path,
    quiet: Duration,
    mut on_settled: F,
) -> notify::Result<notify::RecommendedWatcher>
where
    F: FnMut() + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher.watch(dir, RecursiveMode::Recursive)?;
    let spawn = std::thread::Builder::new()
        .name("vault-watch".into())
        .spawn(move || {
            loop {
                match rx.recv_timeout(quiet) {
                    Ok(_) => {
                        // Drain until the tree is quiet — coalesce the burst.
                        while rx.recv_timeout(quiet).is_ok() {}
                        on_settled();
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    if let Err(e) = spawn {
        return Err(notify::Error::generic(&format!(
            "spawn debounce thread: {e}"
        )));
    }
    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// One write → one tick; a burst of writes → one more tick (coalesced).
    #[test]
    fn fires_after_quiet_and_coalesces_bursts() {
        let dir = tempfile::tempdir().unwrap();
        let (tick_tx, tick_rx) = mpsc::channel::<()>();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        let _watcher = spawn_quiet(dir.path(), Duration::from_millis(120), move || {
            hits2.fetch_add(1, Ordering::SeqCst);
            let _ = tick_tx.send(());
        })
        .unwrap();

        std::fs::write(dir.path().join("a.md"), "# a\n").unwrap();
        tick_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("first write must tick");

        for n in 0..3 {
            std::fs::write(dir.path().join(format!("b{n}.md")), "# b\n").unwrap();
        }
        tick_rx
            .recv_timeout(Duration::from_secs(15))
            .expect("burst must tick");
        // Drain anything further within a quiet-and-a-half window.
        while tick_rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "burst of 3 rapid writes must coalesce into a single tick"
        );
    }
}
