//! `oxibrain admin migrate` — journaled, resumable legacy-layout migration
//! into the canonical unified Oxi home.
//!
//! Currently moves `<oxi-home>/models` into `<oxi-home>/brain/models` per
//! the unified-home contract: preflight/dry-run first, journal before the
//! first mutation, copy + verify (never rename), and the legacy source is
//! kept as a recoverable backup for the whole compatibility window.

use oxibrain::migrate::{self, MigrationPaths, PlanState};

/// Resolve the migration layout for the active Oxi home.
fn layout() -> (std::path::PathBuf, MigrationPaths) {
    let home = oxibrain::paths::oxi_home();
    (home.clone(), migrate::models_paths(&home))
}

pub fn run(dry_run: bool) -> anyhow::Result<()> {
    let (home, paths) = layout();
    let override_note = if std::env::var_os("OXI_HOME").is_some() {
        " (OXI_HOME)"
    } else {
        ""
    };
    if dry_run {
        let plan = migrate::preflight(&paths);
        println!("oxi home: {}{override_note}", home.display());
        println!("source:      {}", plan.source.display());
        println!("destination: {}", plan.destination.display());
        match plan.state {
            PlanState::NothingToDo => {
                println!("state: nothing_to_do — no legacy models to migrate");
            }
            PlanState::Ready => {
                println!(
                    "state: ready — {} file(s), {} byte(s) would be copied",
                    plan.files_to_copy, plan.bytes_to_copy
                );
                println!("required action: re-run without --dry-run to migrate");
            }
            PlanState::AlreadyMigrated => {
                println!(
                    "state: already_migrated — destination mirrors the source; the backup stays at {}",
                    plan.source.display()
                );
            }
            PlanState::Conflict => {
                println!(
                    "state: conflict — {} and {} both exist with differing content;\n\
                     resolve by hand (the migration refuses to merge or overwrite)",
                    plan.source.display(),
                    plan.destination.display()
                );
            }
        }
        return Ok(());
    }

    let report = migrate::migrate(&paths)?;
    println!("oxi home: {}{override_note}", home.display());
    match report.state {
        PlanState::Ready => println!(
            "migrated {} file(s) ({} bytes) into {} — backup retained at {}",
            report.copied_files,
            report.copied_bytes,
            paths.destination.display(),
            paths.source.display()
        ),
        PlanState::AlreadyMigrated => {
            println!("already migrated — nothing to do");
        }
        PlanState::NothingToDo => println!("nothing to migrate"),
        PlanState::Conflict => unreachable!("migrate() errors on conflict"),
    }
    Ok(())
}
