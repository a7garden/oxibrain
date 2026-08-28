//! `oxibrain space remove <name> [--purge]` — spec §4.5.
//!
//! Refusal order: unknown name → default-space guard → non-empty (unless
//! `--purge`). The purge branch rewrites `documents.toml` FIRST so a save
//! failure aborts before any destruction; then runs the audited full-space
//! redaction (`RedactTarget::Space` drops the `spaces` row inside the same
//! transaction); then purges the documents cache. Vault directory files
//! are NEVER deleted; the dir is rmdir'd only when empty.

use oxibrain::config::UserConfig;
use oxibrain::{Brain, BrainConfig, RedactTarget};
use oxibrain_connectors::documents_config::{DocumentsConfig, RootEntry};
use std::path::Path;

pub async fn run(dir: &Path, home: Option<&Path>, name: &str, purge: bool) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    // Resolve the space id. A lookup miss is fatal for non-purge removal
    // and for a first purge run — except the crash-resume case (spec §4.5):
    // the brain-side redaction dropped the `spaces` row before the process
    // died, so a re-run finds nothing at lookup time. Space ids are
    // deterministic, and a `redactions` tombstone for Space{id} proves the
    // purge already reached the brain — resume steps 3-5 (documents.toml
    // rewrite, cache sweep, vault cleanup; the redaction itself is an
    // idempotent no-op now). No tombstone ⇒ the space never existed and
    // the plain not-found error stands.
    let sid = match brain.lookup_space(name).await? {
        Some(id) => id,
        None if purge => {
            let sid = oxibrain_store::ledger::space_id(name);
            let recorded = brain
                .redaction_recorded(&RedactTarget::Space { id: sid.clone() })
                .await
                .map_err(|e| anyhow::anyhow!("check redactions: {e}"))?;
            if !recorded {
                anyhow::bail!(
                    "space '{name}' not found — create it with: oxibrain space add {name}"
                );
            }
            println!("resuming interrupted purge of '{name}' (brain already redacted)");
            sid
        }
        None => {
            anyhow::bail!("space '{name}' not found — create it with: oxibrain space add {name}")
        }
    };

    let default = UserConfig::load(home)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .default_space;
    if name == default {
        anyhow::bail!(
            "space '{name}' is the default — change it first: oxibrain space default <other>"
        );
    }
    let mut cfg = DocumentsConfig::load(dir)?;
    // Scaffold detection (spec §4.5): the provisioning scaffold is
    // path-conditioned — alias == space == name AND path == the provisioned
    // root (<home>/.oxi/vault/<name>). documents.toml stores EXPANDED
    // absolute paths, so a tilde-literal comparison never matches. With a
    // resolved home the full path condition applies, so a user-authored
    // root that merely reuses the alias is NOT scaffold and removing it
    // requires --purge. Without a home, alias+space is the best available
    // signal.
    let is_scaffold = |r: &RootEntry| {
        r.space == name
            && r.alias == name
            && match home {
                Some(h) => r.path == crate::cmd::provision::root_path_for(h, name),
                None => true,
            }
    };
    let roots_referencing: Vec<_> = cfg
        .roots
        .iter()
        .filter(|r| r.space == name && !is_scaffold(r))
        .cloned()
        .collect();

    if purge {
        // 1. documents.toml first — abort before any destruction on failure.
        cfg.roots.retain(|r| r.space != name);
        DocumentsConfig::save(dir, &cfg)?;
        // 2. brain.db: audited full-space redaction (drops the spaces row).
        let target = RedactTarget::Space { id: sid.clone() };
        let result = brain.redact(&target, "space purge", "cli").await?;
        println!(
            "purged: {} episodes, {} assertions, {} statements, {} mentions",
            result.closure.episodes.len(),
            result.closure.assertions.len(),
            result.closure.statements.len(),
            result.closure.mentions.len()
        );
        // 3. documents cache.
        brain.purge_documents_for_space(name).await?;
    } else {
        let episodes = brain.episode_count_for_space(&sid).await?;
        let chunks = brain.chunk_count_for_space(name).await?;
        let mut reasons = Vec::new();
        if episodes > 0 {
            reasons.push(format!("{episodes} episodes (use --purge)"));
        }
        if chunks > 0 {
            reasons.push(format!("{chunks} document chunks (use --purge)"));
        }
        if !roots_referencing.is_empty() {
            reasons.push(format!(
                "{} documents.toml root(s) reference it (use --purge)",
                roots_referencing.len()
            ));
        }
        if !reasons.is_empty() {
            anyhow::bail!("space '{name}' is not empty: {}", reasons.join("; "));
        }
        // Empty removal: scaffold root out first, then the row.
        cfg.roots.retain(|r| r.space != name);
        DocumentsConfig::save(dir, &cfg)?;
        brain.drop_space(&sid).await?;
        brain.purge_documents_for_space(name).await?;
    }

    // 4. Vault dir: never delete files; rmdir only when empty.
    if let Some(h) = home {
        let vault = h.join(".oxi").join("vault").join(name);
        if vault.is_dir() && std::fs::read_dir(&vault)?.next().is_none() {
            std::fs::remove_dir(&vault)?;
        } else if vault.is_dir() {
            println!("vault dir kept (has files): {}", vault.display());
        }
    }
    println!("space '{name}' removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain::{Brain, BrainConfig};
    use oxibrain_ports::Timestamp;

    async fn brain(dir: &std::path::Path) -> Brain {
        Brain::open(BrainConfig::at(dir)).await.unwrap()
    }

    #[tokio::test]
    async fn remove_unknown_space_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let err = run(dir.path(), Some(home.path()), "ghost", false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn remove_default_space_refuses() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join(".oxi")).unwrap();
        std::fs::write(
            home.path().join(".oxi").join("config.toml"),
            "default_space = \"keep\"\n",
        )
        .unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("keep").await.unwrap();
        drop(b);
        let err = run(dir.path(), Some(home.path()), "keep", false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("default"));
    }

    #[tokio::test]
    async fn remove_fresh_space_drops_scaffold() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("dev").await.unwrap();
        drop(b);
        // Provisioned scaffold (root alias==dev, path ~/.oxi/vault/dev).
        crate::cmd::provision::provision_space_vault(dir.path(), home.path(), "dev").unwrap();
        run(dir.path(), Some(home.path()), "dev", false)
            .await
            .unwrap();
        let b = brain(dir.path()).await;
        assert!(
            b.list_spaces()
                .await
                .unwrap()
                .iter()
                .all(|s| s.name != "dev")
        );
    }

    #[tokio::test]
    async fn purge_keeps_vault_files_and_audits() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let sid = b.ensure_space("work").await.unwrap();
        let _ = b
            .ingest_note(
                &sid,
                "test://t",
                "Alice works for Acme".into(),
                Timestamp::from_millis(1000),
            )
            .await
            .unwrap();
        drop(b);
        // User file in the vault dir — must survive purge.
        let vault = home.path().join(".oxi").join("vault").join("work");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("note.md"), "user data").unwrap();

        run(dir.path(), Some(home.path()), "work", true)
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(vault.join("note.md")).unwrap(),
            "user data"
        );
        let b = brain(dir.path()).await;
        assert!(
            b.list_spaces()
                .await
                .unwrap()
                .iter()
                .all(|s| s.name != "work")
        );
        let audit = b.audit_log(Some(10)).await.unwrap();
        assert!(audit.iter().any(|a| a.operation.contains("redact")));
    }
    #[tokio::test]
    async fn purge_rerun_after_completion_is_harmless_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let sid = b.ensure_space("once").await.unwrap();
        // Ingest at least one episode so the first purge actually drops the
        // spaces row and writes a tombstone (the empty-closure early-return
        // in `execute_space_redaction` would otherwise leave the row in
        // place — see Task 4 idempotency).
        let _ = b
            .ingest_note(
                &sid,
                "test://t",
                "hello".into(),
                Timestamp::from_millis(1000),
            )
            .await
            .unwrap();
        drop(b);
        run(dir.path(), Some(home.path()), "once", true)
            .await
            .unwrap();
        // Re-run after a COMPLETED purge: the spaces row is gone but the
        // redactions tombstone remains, so the run takes the resume path
        // and no-ops (empty redaction, empty sweep) instead of failing.
        run(dir.path(), Some(home.path()), "once", true)
            .await
            .unwrap();
        let b = brain(dir.path()).await;
        assert!(
            b.list_spaces()
                .await
                .unwrap()
                .iter()
                .all(|s| s.name != "once")
        );
    }

    #[tokio::test]
    async fn purge_unknown_space_still_errors_not_found() {
        // No tombstone can exist for a space that was never purged — the
        // resume gate must let the plain not-found error through.
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let err = run(dir.path(), Some(home.path()), "ghost", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }

    #[tokio::test]
    async fn purge_resumes_after_crash_between_redaction_and_sweep() {
        // Spec §4.5 crash recovery: the audited brain-side redaction
        // committed, but the process died before the documents cache sweep
        // and the documents.toml rewrite. The `spaces` row is gone, so a
        // naive lookup re-run fails with "not found" and the cache keeps
        // the purged space's chunks forever. The re-run MUST finish steps
        // 3-5 (rewrite, sweep, vault cleanup).
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let sid = b.ensure_space("crash").await.unwrap();
        let _ = b
            .ingest_note(
                &sid,
                "test://t",
                "hello".into(),
                Timestamp::from_millis(1000),
            )
            .await
            .unwrap();
        drop(b);

        // Pre-crash state: provisioned root + one cached chunk.
        crate::cmd::provision::provision_space_vault(dir.path(), home.path(), "crash").unwrap();
        seed_chunk(dir.path(), "crash");

        // Simulated crash: ONLY the brain-side redaction ran.
        let b = brain(dir.path()).await;
        b.redact(&RedactTarget::Space { id: sid }, "space purge", "cli")
            .await
            .unwrap();
        drop(b);
        let b = brain(dir.path()).await;
        assert!(
            b.lookup_space("crash").await.unwrap().is_none(),
            "spaces row must be gone — this is what breaks naive re-runs"
        );
        drop(b);

        run(dir.path(), Some(home.path()), "crash", true)
            .await
            .unwrap();

        let ro = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
        assert_eq!(
            ro.chunk_count_for_space("crash").unwrap(),
            0,
            "resumed purge must sweep the documents cache"
        );
        drop(ro);
        let cfg = DocumentsConfig::load(dir.path()).unwrap();
        assert!(
            cfg.roots.iter().all(|r| r.space != "crash"),
            "resumed purge must drop the documents.toml root"
        );
        assert!(
            !home
                .path()
                .join(".oxi")
                .join("vault")
                .join("crash")
                .exists(),
            "resumed purge must finish the (empty) vault dir cleanup"
        );
    }

    /// Seed one chunk for `space` into the documents cache (no episodes).
    fn seed_chunk(dir: &std::path::Path, space: &str) {
        let cache = oxibrain_store::documents::DocumentCache::open_rw(dir).unwrap();
        let fingerprint = oxibrain_core::documents::RootFingerprint {
            alias: space.to_owned(),
            canonical_path: format!("/tmp/{space}"),
            space: space.to_owned(),
            include: vec!["**/*.md".to_owned()],
            exclude: Vec::new(),
            max_file_bytes: 1024 * 1024,
        };
        let text = "hello world";
        let upsert = oxibrain_store::documents::DocumentUpsert {
            locator: format!("notes/{space}.md"),
            revision: "rev1".to_owned(),
            media_type: "text/markdown".to_owned(),
            bytes: text.len() as u64,
            modified_ns: 1_700_000_000_000_000_000,
            modified_at: 1_700_000_000,
            chunks: vec![oxibrain_store::documents::ChunkUpsert {
                ordinal: 0,
                span_start: 0,
                span_end: text.len(),
                context: String::new(),
                text: text.to_owned(),
            }],
        };
        let plan = oxibrain_store::documents::ApplyPlan {
            root_actions: vec![(
                space.to_owned(),
                oxibrain_core::documents::RootAction::KeepRoot,
            )],
            roots: vec![oxibrain_store::documents::RootApply {
                fingerprint,
                expected_generation: 0,
                actions: vec![oxibrain_core::documents::FileAction::Add(
                    oxibrain_core::documents::FileObservation {
                        locator: format!("notes/{space}.md"),
                        bytes: text.len() as u64,
                        modified_ns: 1_700_000_000_000_000_000,
                        revision_hint: Some("rev1".to_owned()),
                    },
                )],
                upserts: vec![upsert],
            }],
        };
        cache.apply(&plan).unwrap();
    }
    #[tokio::test]
    async fn purge_chunks_only_space_drops_row_and_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("chunks-only").await.unwrap();
        drop(b);

        // Seed the documents cache directly via `DocumentCache::apply` — no
        // episodes. This exercises the Task 8 purge path when the only
        // non-empty signal is in the documents cache.
        let cache = oxibrain_store::documents::DocumentCache::open_rw(dir.path()).unwrap();
        let fingerprint = oxibrain_core::documents::RootFingerprint {
            alias: "chunks-only".to_owned(),
            canonical_path: "/tmp/chunks-only".to_owned(),
            space: "chunks-only".to_owned(),
            include: vec!["**/*.md".to_owned()],
            exclude: Vec::new(),
            max_file_bytes: 1024 * 1024,
        };
        let observation = oxibrain_core::documents::FileObservation {
            locator: "notes/a.md".to_owned(),
            bytes: 11,
            modified_ns: 1_700_000_000_000_000_000,
            revision_hint: Some("rev1".to_owned()),
        };
        let text = "hello world";
        let upsert = oxibrain_store::documents::DocumentUpsert {
            locator: "notes/a.md".to_owned(),
            revision: "rev1".to_owned(),
            media_type: "text/markdown".to_owned(),
            bytes: text.len() as u64,
            modified_ns: 1_700_000_000_000_000_000,
            modified_at: 1_700_000_000,
            chunks: vec![oxibrain_store::documents::ChunkUpsert {
                ordinal: 0,
                span_start: 0,
                span_end: text.len(),
                context: String::new(),
                text: text.to_owned(),
            }],
        };
        let plan = oxibrain_store::documents::ApplyPlan {
            root_actions: vec![(
                "chunks-only".to_owned(),
                oxibrain_core::documents::RootAction::KeepRoot,
            )],
            roots: vec![oxibrain_store::documents::RootApply {
                fingerprint,
                expected_generation: 0,
                actions: vec![oxibrain_core::documents::FileAction::Add(observation)],
                upserts: vec![upsert],
            }],
        };
        cache.apply(&plan).unwrap();
        drop(cache);
        let ro = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
        assert!(
            ro.chunk_count_for_space("chunks-only").unwrap() >= 1,
            "chunks must be seeded before purge"
        );
        drop(ro);

        run(dir.path(), Some(home.path()), "chunks-only", true)
            .await
            .unwrap();

        // Brain re-opens cleanly after purge.
        let b = brain(dir.path()).await;
        assert!(
            b.list_spaces()
                .await
                .unwrap()
                .iter()
                .all(|s| s.name != "chunks-only"),
            "spaces row must be dropped"
        );
        let ro = oxibrain_store::documents::DocumentCache::open_ro(dir.path()).unwrap();
        assert_eq!(
            ro.chunk_count_for_space("chunks-only").unwrap(),
            0,
            "documents cache must be swept"
        );
    }

    #[tokio::test]
    async fn remove_nonempty_refuses() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let sid = b.ensure_space("busy").await.unwrap();
        let _ = b
            .ingest_note(
                &sid,
                "test://t",
                "hello".into(),
                Timestamp::from_millis(1000),
            )
            .await
            .unwrap();
        drop(b);
        let err = run(dir.path(), Some(home.path()), "busy", false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not empty"));
        assert!(err.to_string().contains("episodes"));
    }

    #[tokio::test]
    async fn remove_refuses_user_root_with_matching_alias_wrong_path() {
        // Spec §4.5: the scaffold is path-conditioned. A user-authored
        // root with alias == space == name but a DIFFERENT path is not the
        // scaffold — non-purge remove must refuse instead of silently
        // dropping it from documents.toml.
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("mine").await.unwrap();
        drop(b);
        let mut cfg = DocumentsConfig::load(dir.path()).unwrap();
        cfg.roots.push(RootEntry {
            alias: "mine".into(),
            path: std::path::PathBuf::from("/data/mine"),
            space: "mine".into(),
            include: vec!["**/*.md".into()],
            exclude: Vec::new(),
            max_file_bytes: 1024 * 1024,
        });
        DocumentsConfig::save(dir.path(), &cfg).unwrap();

        let err = run(dir.path(), Some(home.path()), "mine", false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("documents.toml root"),
            "got: {err}"
        );
        // The root must survive the refusal.
        let cfg = DocumentsConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.roots.len(), 1, "root must not be silently removed");
    }

    #[tokio::test]
    async fn remove_refuses_user_root_with_different_alias() {
        // alias != name: a user root even when it sits on the provisioned
        // path — never the scaffold.
        let dir = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let b = brain(dir.path()).await;
        let _ = b.ensure_space("mine").await.unwrap();
        drop(b);
        let mut cfg = DocumentsConfig::load(dir.path()).unwrap();
        cfg.roots.push(RootEntry {
            alias: "my-notes".into(),
            path: crate::cmd::provision::root_path_for(home.path(), "mine"),
            space: "mine".into(),
            include: vec!["**/*.md".into()],
            exclude: Vec::new(),
            max_file_bytes: 1024 * 1024,
        });
        DocumentsConfig::save(dir.path(), &cfg).unwrap();

        let err = run(dir.path(), Some(home.path()), "mine", false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("documents.toml root"),
            "got: {err}"
        );
    }
}
