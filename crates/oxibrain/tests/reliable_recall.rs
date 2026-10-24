//! M12 public facade regressions; source reachability and provenance, not QA scores.
use oxibrain::{Brain, BrainConfig, ReadScope, RecallRequest, SearchRequest, Timestamp};
use oxibrain_core::context::render_context;
use oxibrain_core::retrieval::{QueryMode, SearchPlane};
use oxibrain_ports::{FakeClock, TokenizerPort};
use std::sync::Arc;

struct Bytes;
impl TokenizerPort for Bytes {
    fn count(&self, s: &str) -> usize {
        s.len()
    }
    fn truncate_to(&self, s: &str, n: usize) -> String {
        s.chars().take(n).collect()
    }
    fn id(&self) -> &str {
        "test-byte-tokenizer"
    }
    fn quality(&self) -> oxibrain_ports::tokenizer::TokenizerQuality {
        oxibrain_ports::tokenizer::TokenizerQuality::Exact
    }
}

#[tokio::test]
async fn old_session_tail_survives_and_current_session_is_excluded() {
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Bytes));
    let space = b.ensure_space("old-sessions").await.unwrap();
    let text = format!(
        "{} UmbraRollback uses restore-release-17",
        "irrelevant ".repeat(250)
    );
    b.record_session(&space, "old", text.clone(), Timestamp(1))
        .await
        .unwrap();
    for i in 2..12 {
        b.record_session(
            &space,
            &format!("new-{i}"),
            format!("Other work {i}"),
            Timestamp(i),
        )
        .await
        .unwrap();
    }
    let mut req = RecallRequest::new(&space, "UmbraRollback", 1600);
    req.session_id = Some("new-11".into());
    req.require_exact_tokens = true;
    req.explain = true;
    let r = b.recall_with_request(req).await.unwrap();
    assert!(render_context(&r.data.context).contains("restore-release-17"));
    assert_eq!(
        r.data.context.total_tokens,
        render_context(&r.data.context).len()
    );
    assert!(r.data.context.total_tokens <= 1600);
    assert_eq!(r.meta.layers_kept, r.data.context.layers.len());
    let citation = r
        .data
        .evidence
        .iter()
        .find(|e| e.source.reference == "session://old")
        .unwrap();
    assert!(text[citation.source.start..citation.source.end].contains("restore-release-17"));
    assert!(!render_context(&r.data.context).contains("new-11"));
    assert_eq!(r.meta.index_coverage["complete"], true);
}

#[tokio::test]
async fn historical_claims_refold_competing_functional_values() {
    use oxibrain::{DeclObject, Declaration, EntityRef};
    let d = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(1000)));
    let b = Brain::with_clock(BrainConfig::at(d.path()), clock.clone())
        .await
        .unwrap();
    let space = b.ensure_space("history").await.unwrap();
    for (t, company) in [(1000, "FirstCompany"), (2000, "LaterCompany")] {
        clock.advance(t - 1000);
        b.declare(
            &space,
            Declaration::AddStatement {
                subject: EntityRef {
                    surface: "ChronoPerson".into(),
                    ty: "Person".into(),
                },
                predicate: "employed_by".into(),
                object: DeclObject::Entity {
                    surface: company.into(),
                    ty: "Organization".into(),
                },
                polarity: "affirm".into(),
                valid_from: 0,
                valid_to: i64::MAX,
            },
        )
        .await
        .unwrap();
    }
    let scope = ReadScope {
        space: space.clone(),
        as_of: Some(Timestamp(500)),
        known_at: Some(Timestamp(1500)),
        min_confidence: 0.0,
    };
    let response = b
        .search_with_scope(SearchRequest {
            text: "ChronoPerson".into(),
            scope: scope.clone(),
            planes: [SearchPlane::Memory].into_iter().collect(),
            mode: QueryMode::Hybrid,
            limit: 20,
        })
        .await
        .unwrap();
    let text = serde_json::to_string(&response.data).unwrap();
    assert!(text.contains("FirstCompany"), "{text}");
    assert!(!text.contains("LaterCompany"), "{text}");
    let mut req = RecallRequest::new(&space, "ChronoPerson", 3000);
    req.scope = scope;
    req.planes = [SearchPlane::Memory].into_iter().collect();
    let text = render_context(&b.recall_with_request(req).await.unwrap().data.context);
    assert!(text.contains("FirstCompany"));
    assert!(!text.contains("LaterCompany"));
}

#[tokio::test]
async fn exact_tokenizer_and_temporal_document_capabilities_are_enforced() {
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path())).await.unwrap();
    let space = b.ensure_space("scope").await.unwrap();
    let mut req = RecallRequest::new(&space, "anything", 10);
    req.require_exact_tokens = true;
    assert!(
        b.recall_with_request(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("tokenizer_unavailable")
    );
    let mut req = RecallRequest::new(&space, "anything", 10);
    req.scope.known_at = Some(Timestamp(0));
    assert!(
        b.recall_with_request(req)
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported_temporal_scope")
    );
}

#[tokio::test]
async fn raw_index_backfill_is_idempotent_and_does_not_extract() {
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path())).await.unwrap();
    let space = b.ensure_space("backfill").await.unwrap();
    b.record_session(
        &space,
        "old",
        "BackfillNeedle restore-19".into(),
        Timestamp(1),
    )
    .await
    .unwrap();
    let db = rusqlite::Connection::open(d.path().join("brain.db")).unwrap();
    db.execute("DELETE FROM memory_chunk_fts_word", []).unwrap();
    db.execute("DELETE FROM memory_chunk_fts_ngram", [])
        .unwrap();
    assert_eq!(b.index_memory().await.unwrap(), 1);
    assert_eq!(b.index_memory().await.unwrap(), 0);
    let r = b
        .recall_with_request(RecallRequest::new(&space, "BackfillNeedle", 500))
        .await
        .unwrap();
    assert!(render_context(&r.data.context).contains("restore-19"));
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM extractions", [], |r| r.get(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn historical_retraction_alias_and_merge_do_not_enrich_earlier_reads() {
    use oxibrain::{DeclObject, Declaration, EntityRef};
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(1000)));
    let b = Brain::with_clock(BrainConfig::at(dir.path()), clock.clone())
        .await
        .unwrap();
    let space = b.ensure_space("identity-history").await.unwrap();
    let person = EntityRef {
        surface: "BeforePerson".into(),
        ty: "Person".into(),
    };
    let object = DeclObject::Entity {
        surface: "BeforeProject".into(),
        ty: "Project".into(),
    };
    b.declare(
        &space,
        Declaration::AddStatement {
            subject: person.clone(),
            predicate: "works_on".into(),
            object: object.clone(),
            polarity: "affirm".into(),
            valid_from: 0,
            valid_to: i64::MAX,
        },
    )
    .await
    .unwrap();
    clock.advance(1000);
    b.declare(
        &space,
        Declaration::Alias {
            entity: person.clone(),
            surface: "FutureAlias".into(),
        },
    )
    .await
    .unwrap();
    b.declare(
        &space,
        Declaration::Merge {
            loser: person.clone(),
            winner: EntityRef {
                surface: "FutureWinner".into(),
                ty: "Person".into(),
            },
        },
    )
    .await
    .unwrap();
    b.declare(
        &space,
        Declaration::Retract {
            subject: person,
            predicate: "works_on".into(),
            object,
            episode: String::new(),
        },
    )
    .await
    .unwrap();
    let mut req = RecallRequest::new(&space, "BeforePerson", 2000);
    req.planes = [SearchPlane::Memory].into_iter().collect();
    req.scope.known_at = Some(Timestamp(1500));
    let r = b.recall_with_request(req).await.unwrap();
    let text = render_context(&r.data.context);
    assert!(text.contains("BeforeProject"), "{text}");
    assert!(
        !text.contains("FutureAlias") && !text.contains("FutureWinner"),
        "{text}"
    );
}

#[tokio::test]
async fn completed_backfill_repairs_omitted_projection_and_compacted_sources() {
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path())).await.unwrap();
    let space = b.ensure_space("repair-index").await.unwrap();
    let id = b
        .record_session(
            &space,
            "repair",
            "CompactNeedle recovery-53".into(),
            Timestamp(1),
        )
        .await
        .unwrap();
    b.index_memory().await.unwrap();
    let conn = rusqlite::Connection::open(d.path().join("brain.db")).unwrap();
    conn.execute(
        "UPDATE episodes SET content_compacted=CAST(content AS BLOB),content='' WHERE id=?1",
        [id],
    )
    .unwrap();
    conn.execute("DELETE FROM memory_chunk_fts_word", [])
        .unwrap();
    conn.execute("DELETE FROM memory_chunk_fts_ngram", [])
        .unwrap();
    assert_eq!(b.index_memory().await.unwrap(), 1);
    let r = b
        .recall_with_request(RecallRequest::new(&space, "CompactNeedle", 1000))
        .await
        .unwrap();
    assert!(render_context(&r.data.context).contains("recovery-53"));
    b.reproject().await.unwrap();
    let r = b
        .recall_with_request(RecallRequest::new(&space, "CompactNeedle", 1000))
        .await
        .unwrap();
    assert!(render_context(&r.data.context).contains("recovery-53"));
}

#[tokio::test]
async fn raw_index_batch_rolls_back_rows_and_cursor_together() {
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path())).await.unwrap();
    let space = b.ensure_space("index-crash").await.unwrap();
    let id = b
        .record_session(
            &space,
            "crash",
            "CrashNeedle checkpoint-66".into(),
            Timestamp(1),
        )
        .await
        .unwrap();
    let conn = rusqlite::Connection::open(d.path().join("brain.db")).unwrap();
    conn.execute("DELETE FROM memory_source_chunks", [])
        .unwrap();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    oxibrain_store::read_evidence::index_source(&conn, &space, &id, "CrashNeedle checkpoint-66")
        .unwrap();
    oxibrain_store::meta::set(
        &conn,
        &format!("raw-index/{space}/3"),
        "{\"target\":0,\"after\":0,\"complete\":true}",
    )
    .unwrap();
    conn.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        oxibrain_store::read_evidence::missing_source_count(&conn, &space).unwrap(),
        1
    );
    assert!(
        oxibrain_store::meta::get(&conn, &format!("raw-index/{space}/3"))
            .unwrap()
            .is_none()
    );
    assert_eq!(b.index_memory().await.unwrap(), 1);
    let r = b
        .recall_with_request(RecallRequest::new(&space, "CrashNeedle", 1000))
        .await
        .unwrap();
    assert!(render_context(&r.data.context).contains("checkpoint-66"));
}

#[tokio::test]
async fn optional_embedding_failure_falls_back_and_redaction_during_embedding_retries() {
    use oxibrain::{DeclObject, Declaration, EntityRef};
    struct PausingEmbedder {
        entered: tokio::sync::Notify,
        resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl oxibrain_ports::EmbeddingPort for PausingEmbedder {
        fn dim(&self) -> usize {
            1
        }
        fn embed(&self, _: &[&str]) -> Result<Vec<Vec<f32>>, oxibrain::BrainError> {
            self.entered.notify_one();
            self.resume.lock().unwrap().recv().unwrap();
            Err(oxibrain::BrainError::Config(
                "simulated adapter failure".into(),
            ))
        }
    }
    let d = tempfile::tempdir().unwrap();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let adapter = Arc::new(PausingEmbedder {
        entered: tokio::sync::Notify::new(),
        resume: std::sync::Mutex::new(resume_rx),
    });
    let b = Brain::open(BrainConfig::at(d.path()))
        .await
        .unwrap()
        .with_embedder(adapter.clone());
    let space = b.ensure_space("read-race").await.unwrap();
    let episode = b
        .declare(
            &space,
            Declaration::AddStatement {
                subject: EntityRef {
                    surface: "ProtectedPerson".into(),
                    ty: "Person".into(),
                },
                predicate: "employed_by".into(),
                object: DeclObject::Entity {
                    surface: "ProtectedCompany".into(),
                    ty: "Organization".into(),
                },
                polarity: "affirm".into(),
                valid_from: 0,
                valid_to: i64::MAX,
            },
        )
        .await
        .unwrap();
    let mut req = RecallRequest::new(&space, "ProtectedPerson", 2000);
    req.planes = [SearchPlane::Memory].into_iter().collect();
    let read = b.recall_with_request(req);
    let redact = async {
        adapter.entered.notified().await;
        b.redact(
            &oxibrain::RedactTarget::Episode { id: episode },
            "race test",
            "test",
        )
        .await
        .unwrap();
        resume_tx.send(()).unwrap();
    };
    let (read, ()) = tokio::join!(read, redact);
    let response = read.unwrap();
    assert!(!render_context(&response.data.context).contains("ProtectedCompany"));
    // A fresh observation has no remaining claim, so no second adapter call occurs.
    assert!(response.data.evidence.is_empty());
}

#[tokio::test]
async fn document_revision_changed_during_packing_is_retried_with_fresh_coordinates() {
    struct EditingTokenizer {
        path: std::path::PathBuf,
        fired: std::sync::atomic::AtomicBool,
    }
    impl TokenizerPort for EditingTokenizer {
        fn count(&self, text: &str) -> usize {
            if !text.is_empty() && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                std::fs::write(&self.path, "RevisionNeedle fresh-value\n").unwrap();
            }
            text.len()
        }
        fn truncate_to(&self, text: &str, n: usize) -> String {
            text.chars().take(n).collect()
        }
        fn id(&self) -> &str {
            "revision-race-byte-tokenizer"
        }
    }
    let d = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    let path = vault.path().join("race.md");
    std::fs::write(&path, "RevisionNeedle obsolete-value\n").unwrap();
    std::fs::write(
        d.path().join("documents.toml"),
        format!(
            "[[root]]\nalias=\"vault\"\npath={}\nspace=\"race\"\n",
            serde_json::to_string(&vault.path().to_string_lossy()).unwrap()
        ),
    )
    .unwrap();
    let b = Brain::open(BrainConfig::at(d.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(EditingTokenizer {
            path: path.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
        }));
    let space = b.ensure_space("race").await.unwrap();
    let mut req = RecallRequest::new(space, "RevisionNeedle", 2000);
    req.planes = [SearchPlane::Documents].into_iter().collect();
    let result = b.recall_with_request(req).await.unwrap();
    let text = render_context(&result.data.context);
    assert!(
        text.contains("fresh-value") && !text.contains("obsolete-value"),
        "{text}"
    );
    let digest = format!(
        "blake3:{}",
        blake3::hash(&std::fs::read(path).unwrap()).to_hex()
    );
    assert_eq!(result.data.evidence[0].source.revision, digest);
}

#[tokio::test]
async fn both_time_axes_reach_denials_and_profile_rendering() {
    use oxibrain::{DeclObject, Declaration, EntityRef};
    let d = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(1000)));
    let b = Brain::with_clock(BrainConfig::at(d.path()), clock.clone())
        .await
        .unwrap();
    let space = b.ensure_space("denial-history").await.unwrap();
    let declaration = |polarity: &str, from| Declaration::AddStatement {
        subject: EntityRef {
            surface: "TemporalPerson".into(),
            ty: "Person".into(),
        },
        predicate: "employed_by".into(),
        object: DeclObject::Entity {
            surface: "TemporalCompany".into(),
            ty: "Organization".into(),
        },
        polarity: polarity.into(),
        valid_from: from,
        valid_to: i64::MAX,
    };
    b.declare(&space, declaration("affirm", 0)).await.unwrap();
    clock.advance(1000);
    b.declare(&space, declaration("deny", 1000)).await.unwrap();
    clock.advance(1000);
    let mut results = Vec::new();
    for (known, valid) in [(1500, 1500), (2500, 500), (2500, 1500)] {
        let mut request = RecallRequest::new(&space, "TemporalPerson", 2000);
        request.planes = [SearchPlane::Memory].into_iter().collect();
        request.scope.known_at = Some(Timestamp(known));
        request.scope.as_of = Some(Timestamp(valid));
        results.push(render_context(
            &b.recall_with_request(request).await.unwrap().data.context,
        ));
    }
    assert!(results[0].contains("deny=0"), "{:?}", results);
    assert!(
        results[1].contains("status=Active") && results[1].contains("valid=0..999"),
        "{:?}",
        results
    );
    assert!(!results[2].contains("TemporalCompany"), "{:?}", results);
}

#[tokio::test]
async fn a_failed_attempted_exact_count_is_not_misreported_as_budget_exclusion() {
    struct Broken;
    impl TokenizerPort for Broken {
        fn count(&self, _: &str) -> usize {
            usize::MAX
        }
        fn truncate_to(&self, _: &str, _: usize) -> String {
            String::new()
        }
        fn id(&self) -> &str {
            "broken-exact"
        }
        fn quality(&self) -> oxibrain_ports::tokenizer::TokenizerQuality {
            oxibrain_ports::tokenizer::TokenizerQuality::Exact
        }
        fn count_exact(&self, text: &str) -> Result<Option<usize>, oxibrain::BrainError> {
            if text.is_empty() {
                Ok(Some(0))
            } else {
                Err(oxibrain::BrainError::TokenizerUnavailable(
                    "invalid artifact encoder".into(),
                ))
            }
        }
    }
    let d = tempfile::tempdir().unwrap();
    let b = Brain::open(BrainConfig::at(d.path()))
        .await
        .unwrap()
        .with_tokenizer(Arc::new(Broken));
    let space = b.ensure_space("bad-counter").await.unwrap();
    b.record_session(
        &space,
        "source",
        "ExactNeedle recovery-72".into(),
        Timestamp(1),
    )
    .await
    .unwrap();
    let mut req = RecallRequest::new(space, "ExactNeedle", 2000);
    req.require_exact_tokens = true;
    assert!(matches!(
        b.recall_with_request(req).await,
        Err(oxibrain::BrainError::TokenizerUnavailable(_))
    ));
}

#[tokio::test]
async fn visible_merge_names_work_now_without_leaking_into_the_past() {
    use oxibrain::{DeclObject, Declaration, EntityRef};
    let d = tempfile::tempdir().unwrap();
    let clock = Arc::new(FakeClock::new(Timestamp(1000)));
    let b = Brain::with_clock(BrainConfig::at(d.path()), clock.clone())
        .await
        .unwrap();
    let space = b.ensure_space("scoped-merge").await.unwrap();
    let person = EntityRef {
        surface: "OriginalPerson".into(),
        ty: "Person".into(),
    };
    b.declare(
        &space,
        Declaration::AddStatement {
            subject: person.clone(),
            predicate: "works_on".into(),
            object: DeclObject::Entity {
                surface: "OriginalProject".into(),
                ty: "Project".into(),
            },
            polarity: "affirm".into(),
            valid_from: 0,
            valid_to: i64::MAX,
        },
    )
    .await
    .unwrap();
    clock.advance(1000);
    b.declare(
        &space,
        Declaration::Merge {
            loser: person,
            winner: EntityRef {
                surface: "VisibleWinner".into(),
                ty: "Person".into(),
            },
        },
    )
    .await
    .unwrap();
    clock.advance(1000);
    let mut now = RecallRequest::new(&space, "VisibleWinner", 2000);
    now.planes = [SearchPlane::Memory].into_iter().collect();
    let mut then = now.clone();
    then.scope.known_at = Some(Timestamp(1500));
    then.text = "OriginalPerson".into();
    let current = render_context(&b.recall_with_request(now).await.unwrap().data.context);
    let past = render_context(&b.recall_with_request(then).await.unwrap().data.context);
    assert!(
        current.contains("VisibleWinner") && current.contains("OriginalProject"),
        "{current}"
    );
    assert!(
        past.contains("OriginalPerson") && !past.contains("VisibleWinner"),
        "{past}"
    );
}
