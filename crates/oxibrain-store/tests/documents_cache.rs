//! Integration tests for `DocumentCache` (Daemonless Two-Plane Task 3).
//!
//! These tests use a tempdir + `DocumentCache::open_rw` so they exercise
//! the same lock + migration + apply path real callers do. They assert the
//! spec's exact-row invariants: document/chunk/FTS/vector membership and
//! CAS-driven Busy errors.

use oxibrain_core::documents::{
    CachedFile, FileAction, FileObservation, RootAction, RootFingerprint,
};
use oxibrain_ports::BrainError;
use oxibrain_store::documents::{
    ChunkUpsert, DocumentCache, DocumentUpsert, EMBEDDING_DIM, FtsTable,
};
use std::path::Path;
use tempfile::TempDir;

fn open(dir: &Path) -> DocumentCache {
    DocumentCache::open_rw(dir).expect("open_rw")
}

fn fp(alias: &str, space: &str) -> RootFingerprint {
    RootFingerprint {
        alias: alias.to_owned(),
        canonical_path: format!("/tmp/{alias}"),
        space: space.to_owned(),
        include: vec!["**/*.md".to_owned()],
        exclude: Vec::new(),
        max_file_bytes: 1024 * 1024,
    }
}

fn obs(locator: &str, bytes: u64, rev: &str) -> FileObservation {
    FileObservation {
        locator: locator.to_owned(),
        bytes,
        modified_ns: 1_700_000_000_000_000_000,
        revision_hint: Some(rev.to_owned()),
    }
}

fn ups(locator: &str, revision: &str, text: &str, chunks: usize) -> DocumentUpsert {
    let per = text.chars().count() / chunks.max(1);
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let mut cu = Vec::with_capacity(chunks);
    for i in 0..chunks {
        let start_idx = if i == 0 { 0 } else { char_indices[i * per].0 };
        let end_idx = if i + 1 == chunks {
            text.len()
        } else {
            char_indices[(i + 1) * per].0
        };
        cu.push(ChunkUpsert {
            ordinal: i as u32,
            span_start: start_idx,
            span_end: end_idx,
            context: String::new(),
            text: text[start_idx..end_idx].to_owned(),
        });
    }
    DocumentUpsert {
        locator: locator.to_owned(),
        revision: revision.to_owned(),
        media_type: "text/markdown".to_owned(),
        bytes: text.len() as u64,
        modified_ns: 1_700_000_000_000_000_000,
        modified_at: 1_700_000_000,
        chunks: cu,
    }
}

#[test]
fn open_creates_schema_v1() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    assert_eq!(
        cache.user_version().unwrap(),
        oxibrain_store::documents::DOCUMENTS_SCHEMA_VERSION
    );
    assert_eq!(
        oxibrain_store::documents::DOCUMENTS_SCHEMA_VERSION,
        1
    );
    assert!(dir.path().join("documents.db").exists());
}

#[test]
fn open_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let _first = open(dir.path());
    drop(_first);
    let second = open(dir.path());
    assert_eq!(second.user_version().unwrap(), 1);
}

#[test]
fn second_open_in_same_process_returns_busy() {
    let dir = TempDir::new().unwrap();
    let _first = open(dir.path());
    let second = DocumentCache::open_rw(dir.path());
    assert!(matches!(second, Err(BrainError::Busy(_))));
}

#[test]
fn open_ro_fails_when_db_missing() {
    let dir = TempDir::new().unwrap();
    let err = DocumentCache::open_ro(dir.path()).unwrap_err();
    assert!(matches!(err, BrainError::NotFound(_)));
}

#[test]
fn open_ro_succeeds_when_db_exists() {
    let dir = TempDir::new().unwrap();
    {
        let _rw = open(dir.path());
        // Drop releases the lock; the db file persists.
    }
    let ro = DocumentCache::open_ro(dir.path()).unwrap();
    assert_eq!(ro.user_version().unwrap(), 1);
}

#[test]
fn apply_add_leaves_expected_rows() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());

    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 0,
            actions: vec![FileAction::Add(obs("notes/a.md", 11, "rev1"))],
            upserts: vec![ups("notes/a.md", "rev1", "hello world", 1)],
        }],
    };
    cache.apply(&plan).unwrap();

    assert_row_counts(&cache, "documents", 1);
    assert_row_counts(&cache, "doc_chunks", 1);
    assert_row_counts(&cache, "doc_fts_word", 1);
    assert_row_counts(&cache, "doc_fts_ngram", 1);
    assert_row_counts(&cache, "doc_manifest", 1);
    assert_row_counts(&cache, "doc_vectors", 0);
    assert_eq!(cache.generation("vault").unwrap(), 1);
    assert_eq!(cache.list_roots().unwrap().len(), 1);
}

#[test]
fn apply_replace_clears_old_chunks_fts_and_writes_new() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());

    // Initial add with one chunk.
    let add_plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 0,
            actions: vec![FileAction::Add(obs("notes/a.md", 11, "rev1"))],
            upserts: vec![ups("notes/a.md", "rev1", "hello world", 1)],
        }],
    };
    cache.apply(&add_plan).unwrap();

    // Replace with two chunks.
    let replace_plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 1,
            actions: vec![FileAction::Replace(obs("notes/a.md", 100, "rev2"))],
            upserts: vec![ups("notes/a.md", "rev2", "the quick brown fox", 2)],
        }],
    };
    cache.apply(&replace_plan).unwrap();

    assert_row_counts(&cache, "documents", 1);
    assert_row_counts(&cache, "doc_chunks", 2);
    assert_row_counts(&cache, "doc_fts_word", 2);
    assert_row_counts(&cache, "doc_fts_ngram", 2);
    assert_row_counts(&cache, "doc_manifest", 1);
    assert_eq!(cache.generation("vault").unwrap(), 2);
}

#[test]
fn apply_delete_removes_everything() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();

    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 1,
            actions: vec![FileAction::Delete {
                locator: "notes/a.md".to_owned(),
            }],
            upserts: Vec::new(),
        }],
    };
    cache.apply(&plan).unwrap();

    for t in [
        "documents",
        "doc_chunks",
        "doc_fts_word",
        "doc_fts_ngram",
        "doc_manifest",
        "doc_vectors",
    ] {
        assert_row_counts(&cache, t, 0);
    }
}

#[test]
fn remove_root_cascade_clears_vectors_and_fts_first() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();

    // Add a fake vector so we can verify the cascade touches it.
    let chunk_id: String = cache
        .conn
        .query_row(
            "SELECT id FROM doc_chunks ORDER BY ordinal LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    cache
        .upsert_vectors(&[(chunk_id.clone(), vec![0.0f32; EMBEDDING_DIM])])
        .unwrap();
    assert_row_counts(&cache, "doc_vectors", 1);

    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::RemoveRoot)],
        roots: Vec::new(),
    };
    cache.apply(&plan).unwrap();

    for t in [
        "documents",
        "doc_chunks",
        "doc_fts_word",
        "doc_fts_ngram",
        "doc_manifest",
        "doc_vectors",
    ] {
        assert_row_counts(&cache, t, 0);
    }
    assert!(cache.list_roots().unwrap().is_empty());
}

#[test]
fn reset_root_rebuilds_at_generation_one() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();
    assert_eq!(cache.generation("vault").unwrap(), 1);

    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::ResetRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 0,
            actions: vec![FileAction::Add(obs("notes/a.md", 8, "rev9"))],
            upserts: vec![ups("notes/a.md", "rev9", "rewritten", 1)],
        }],
    };
    cache.apply(&plan).unwrap();

    assert_row_counts(&cache, "documents", 1);
    assert_eq!(cache.generation("vault").unwrap(), 1);
}

#[test]
fn cas_mismatch_returns_busy_and_no_partial_state() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();

    // Caller snapshot thinks the cache is at gen 0 — a stale view.
    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 0,
            actions: vec![FileAction::Replace(obs("notes/a.md", 8, "rev2"))],
            upserts: vec![ups("notes/a.md", "rev2", "rewritten", 1)],
        }],
    };
    let err = cache.apply(&plan).unwrap_err();
    assert!(matches!(err, BrainError::Busy(_)));

    // No partial state — everything still reflects the rev1 add.
    // `add_plan` indexes two chunks per file, so we expect 2 chunks.
    assert_row_counts(&cache, "documents", 1);
    assert_row_counts(&cache, "doc_chunks", 2);
    assert_row_counts(&cache, "doc_fts_word", 2);
    assert_row_counts(&cache, "doc_fts_ngram", 2);
    assert_row_counts(&cache, "doc_manifest", 1);
    assert_eq!(cache.generation("vault").unwrap(), 1);
}

#[test]
fn rebuild_equivalence_after_drop_and_rebuild() {
    let dir = TempDir::new().unwrap();
    let plan = add_plan("vault", "personal", &[("notes/a.md", "rev1")]);

    let snap1 = {
        let cache = open(dir.path());
        cache.apply(&plan).unwrap();
        snapshot_counts(&cache)
    };
    // Drop the db files; reopen; re-apply the identical plan.
    let _ = std::fs::remove_file(dir.path().join("documents.db"));
    let _ = std::fs::remove_file(dir.path().join("documents.db-wal"));
    let _ = std::fs::remove_file(dir.path().join("documents.db-shm"));
    let snap2 = {
        let cache = open(dir.path());
        cache.apply(&plan).unwrap();
        snapshot_counts(&cache)
    };
    assert_eq!(snap1, snap2);
}

#[test]
fn embedded_count_pending_clear_vectors() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());

    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 0,
            actions: vec![
                FileAction::Add(obs("notes/a.md", 12, "rev1")),
                FileAction::Add(obs("notes/b.md", 12, "rev1")),
            ],
            upserts: vec![
                ups("notes/a.md", "rev1", "alpha bravo", 2),
                ups("notes/b.md", "rev1", "charlie delta", 2),
            ],
        }],
    };
    // The chunk texts the `ups` helper produced — the exact bytes that
    // must be recoverable from FTS for embedding.
    let expected_texts: std::collections::HashSet<String> = plan.roots[0]
        .upserts
        .iter()
        .flat_map(|u| u.chunks.iter().map(|c| c.text.clone()))
        .collect();
    cache.apply(&plan).unwrap();

    let (embedded, total) = cache.embedded_count("personal").unwrap();
    assert_eq!((embedded, total), (0, 4));
    let pending = cache.pending_vector_chunks("personal", 100).unwrap();
    assert_eq!(pending.len(), 4);
    // Text recovered from FTS equals the chunk text we wrote.
    let recovered_text: std::collections::HashSet<String> =
        pending.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(recovered_text, expected_texts);

    // Embed one chunk.
    let (chunk_id, _) = pending[0].clone();
    cache
        .upsert_vectors(&[(chunk_id, vec![0.0f32; EMBEDDING_DIM])])
        .unwrap();
    let (embedded, total) = cache.embedded_count("personal").unwrap();
    assert_eq!((embedded, total), (1, 4));
    assert_eq!(cache.pending_vector_chunks("personal", 100).unwrap().len(), 3);

    cache.clear_vectors().unwrap();
    let (embedded, total) = cache.embedded_count("personal").unwrap();
    assert_eq!((embedded, total), (0, 4));
}

#[test]
fn freshness_counters_after_apply() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[
        ("notes/a.md", "rev1"),
        ("notes/b.md", "rev1"),
        ("notes/c.md", "rev1"),
    ])).unwrap();

    // The list_roots API + generation API together give freshness info:
    // one root, generation 1, three files in the manifest.
    let roots = cache.list_roots().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].alias, "vault");
    assert_eq!(roots[0].space, "personal");
    assert_eq!(roots[0].generation, 1);
    // The full fingerprint round-trips: diff_roots on identical config
    // must report KeepRoot (not ResetRoot), or the cache would rebuild
    // on every query.
    let configured = fp("vault", "personal");
    let diff = oxibrain_core::documents::diff_roots(
        std::slice::from_ref(&configured),
        &roots,
    );
    assert_eq!(
        diff,
        vec![("vault".to_owned(), RootAction::KeepRoot)]
    );
    let manifest = cache.root_manifest("vault").unwrap();
    assert_eq!(manifest.len(), 3);
    let mut locators: Vec<&str> = manifest.iter().map(|m: &CachedFile| m.locator.as_str()).collect();
    locators.sort();
    assert_eq!(
        locators,
        vec!["notes/a.md", "notes/b.md", "notes/c.md"]
    );
}

#[test]
fn search_fts_finds_indexed_text() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan(
        "vault",
        "personal",
        &[("notes/a.md", "rev1"), ("notes/b.md", "rev1")],
    )).unwrap();

    let hits = cache
        .search_fts("personal", FtsTable::Word, "alpha", 10)
        .unwrap();
    assert!(!hits.is_empty(), "FTS word search must hit the indexed chunk");
    for (chunk_id, score) in &hits {
        let n: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE id = ?1",
                rusqlite::params![chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        assert!(*score > 0.0);
    }

    let hits = cache
        .search_fts("personal", FtsTable::Ngram, "bravo", 10)
        .unwrap();
    assert!(!hits.is_empty(), "FTS ngram search must hit the indexed chunk");
}

#[test]
fn search_fts_quotes_punctuation() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan(
        "vault",
        "personal",
        &[("notes/x.md", "rev1")],
    )).unwrap();
    // Replace with text that contains punctuation.
    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 1,
            actions: vec![FileAction::Replace(obs("notes/x.md", 8, "rev2"))],
            upserts: vec![ups("notes/x.md", "rev2", "alpha:bravo?charlie", 1)],
        }],
    };
    cache.apply(&plan).unwrap();

    // These would have broken FTS5 syntax if not quoted.
    let hits = cache
        .search_fts("personal", FtsTable::Word, "alpha:bravo?charlie", 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn meta_get_and_set_roundtrip() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    assert_eq!(cache.meta_get("anything").unwrap(), None);
    cache.meta_set("anything", "v1").unwrap();
    assert_eq!(
        cache.meta_get("anything").unwrap().as_deref(),
        Some("v1")
    );
    cache.meta_set("anything", "v2").unwrap();
    assert_eq!(
        cache.meta_get("anything").unwrap().as_deref(),
        Some("v2")
    );
}

#[test]
fn chunks_returns_metadata_for_known_ids() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();
    let chunk_id: String = cache
        .conn
        .query_row(
            "SELECT id FROM doc_chunks ORDER BY ordinal LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let rows = cache.chunks(&[&chunk_id]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].chunk_id, chunk_id);
    assert_eq!(rows[0].root_alias, "vault");
    assert_eq!(rows[0].space, "personal");
    assert_eq!(rows[0].locator, "notes/a.md");
    assert_eq!(rows[0].revision, "rev1");
    assert_eq!(rows[0].media_type, "text/markdown");
    assert_eq!(rows[0].ordinal, 0);
    assert!(rows[0].span_end >= rows[0].span_start);

    // Unknown ids are skipped silently.
    let rows = cache.chunks(&["nope"]).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn knn_returns_nearest_chunk_ids() {
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();

    // Fetch the two chunk ids and embed them with distinct vectors.
    let mut stmt = cache
        .conn
        .prepare("SELECT id FROM doc_chunks ORDER BY id")
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    drop(stmt);
    assert_eq!(ids.len(), 2);

    let mut near = vec![1.0f32; EMBEDDING_DIM];
    near[0] = 9.0;
    let far = vec![0.0f32; EMBEDDING_DIM];
    cache
        .upsert_vectors(&[(ids[0].clone(), near.clone()), (ids[1].clone(), far)])
        .unwrap();

    // Query with the near vector: ids[0] must rank first (smallest L2).
    let hits = cache.knn(&near, 2).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0, ids[0]);
    assert!(hits[0].1 <= hits[1].1, "hits must be distance-ascending");

    // limit=1 returns just the nearest.
    let hits = cache.knn(&near, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, ids[0]);

    // Dimension mismatch is a typed error, not a panic.
    let err = cache.knn(&[0.0f32; 8], 1).unwrap_err();
    assert!(matches!(err, BrainError::Invalid(_)));
}

#[test]
fn replace_removes_old_vectors_and_changes_chunk_ids() {
    // Spec invariant 10: replace a document at the same locator and
    // ordinals; no old vector survives, and new chunk ids differ.
    let dir = TempDir::new().unwrap();
    let cache = open(dir.path());
    cache.apply(&add_plan("vault", "personal", &[("notes/a.md", "rev1")])).unwrap();

    let old_ids: Vec<String> = {
        let mut stmt = cache
            .conn
            .prepare("SELECT id FROM doc_chunks ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    cache
        .upsert_vectors(&[
            (old_ids[0].clone(), vec![0.5f32; EMBEDDING_DIM]),
            (old_ids[1].clone(), vec![0.5f32; EMBEDDING_DIM]),
        ])
        .unwrap();
    let (embedded, total) = cache.embedded_count("personal").unwrap();
    assert_eq!((embedded, total), (2, 2));

    // Replace the document (new revision, same chunk count).
    let plan = oxibrain_store::documents::ApplyPlan {
        root_actions: vec![("vault".to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint: fp("vault", "personal"),
            expected_generation: 1,
            actions: vec![FileAction::Replace(obs("notes/a.md", 30, "rev2"))],
            upserts: vec![ups("notes/a.md", "rev2", "echo foxtrot golf hotel", 2)],
        }],
    };
    cache.apply(&plan).unwrap();

    // No old chunk row or vector survived.
    for id in &old_ids {
        let n: i64 = cache
            .conn
            .query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "old chunk row {id} survived replace");
    }
    let (embedded, total) = cache.embedded_count("personal").unwrap();
    assert_eq!((embedded, total), (0, 2), "old vectors survived replace");

    // New chunk ids differ from old (revision is part of the id).
    let new_ids: Vec<String> = {
        let mut stmt = cache
            .conn
            .prepare("SELECT id FROM doc_chunks ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(new_ids.len(), 2);
    assert!(
        new_ids.iter().all(|id| !old_ids.contains(id)),
        "new chunk ids must differ from old"
    );
}

// ── helpers ────────────────────────────────────────────────────────────

fn add_plan(
    alias: &str,
    space: &str,
    files: &[(&str, &str)],
) -> oxibrain_store::documents::ApplyPlan {
    let fingerprint = fp(alias, space);
    let mut actions = Vec::new();
    let mut upserts = Vec::new();
    for (locator, rev) in files {
        actions.push(FileAction::Add(obs(locator, 11, rev)));
        upserts.push(ups(locator, rev, "alpha bravo charlie delta", 2));
    }
    oxibrain_store::documents::ApplyPlan {
        root_actions: vec![(alias.to_owned(), RootAction::KeepRoot)],
        roots: vec![oxibrain_store::documents::RootApply {
            fingerprint,
            expected_generation: 0,
            actions,
            upserts,
        }],
    }
}

fn assert_row_counts(cache: &DocumentCache, table: &str, expected: i64) {
    let n: i64 = cache
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        n, expected,
        "expected {expected} rows in {table}, got {n}"
    );
}

fn snapshot_counts(cache: &DocumentCache) -> [i64; 6] {
    [
        assert_row_counts_val(cache, "documents"),
        assert_row_counts_val(cache, "doc_chunks"),
        assert_row_counts_val(cache, "doc_fts_word"),
        assert_row_counts_val(cache, "doc_fts_ngram"),
        assert_row_counts_val(cache, "doc_manifest"),
        assert_row_counts_val(cache, "doc_vectors"),
    ]
}

fn assert_row_counts_val(cache: &DocumentCache, table: &str) -> i64 {
    cache
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}