//! `oxibrain spaces` — list every space with live counts.
//!
//! Read-only: opens the store with `Brain::open_ro`, so it takes no
//! advisory lock and coexists with a running daemon (§16.1).

use anyhow::Result;
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> Result<()> {
    let brain = Brain::open_ro(BrainConfig::at(dir)).await?;
    let spaces = brain.list_spaces().await?;
    println!(
        "{:<24} {:<16} {:<20} {:>9} {:>9}",
        "NAME", "ID", "CREATED", "EPISODES", "ENTITIES"
    );
    for s in &spaces {
        let id = s.id.chars().take(16).collect::<String>();
        println!(
            "{:<24} {:<16} {:<20} {:>9} {:>9}",
            s.name,
            id,
            millis_to_iso(s.created_at.millis()),
            s.episode_count,
            s.entity_count
        );
    }
    Ok(())
}

/// Minimal UTC formatting without a chrono dependency (store-independent).
fn millis_to_iso(ms: i64) -> String {
    // days-since-epoch civil conversion (Howard Hinnant's algorithm)
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spaces_prints_table() {
        // Seed an initialized store so `Brain::open_ro` succeeds
        // (`read_only_open_fails_on_missing_store` in facade.rs requires
        // an initialized directory).
        let dir = tempfile::TempDir::new().unwrap();
        let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
        let _ = brain.ensure_space("work").await.unwrap();
        drop(brain);

        // Run the read-only listing; asserts no error.
        run(dir.path()).await.unwrap();
    }

    #[test]
    fn millis_to_iso_epoch_zero() {
        assert_eq!(millis_to_iso(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn millis_to_iso_known_timestamp() {
        // 2024-01-15T12:34:56Z == 1_705_322_096_000 ms
        assert_eq!(millis_to_iso(1_705_322_096_000), "2024-01-15T12:34:56Z");
    }
}
