use oxibrain::{
    Brain, BrainConfig, Capability, DocumentRootSpec, IndexOptions, PublicationCredentials,
    PublishViewV1, RedactTarget, Scope, Timestamp, ViewOrigin, ViewRange, ViewState,
};
use oxibrain_core::publication::{PublicationEventV1, exact_digest};
use tempfile::TempDir;
async fn setup() -> (TempDir, Brain, String, String) {
    let dir = TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let private = brain.ensure_space("private").await.unwrap();
    let shared = brain.ensure_space("shared").await.unwrap();
    (dir, brain, private, shared)
}
async fn token(brain: &Brain, space: &str, caps: &str) -> (String, String) {
    let (info, secret) = brain
        .issue_token(
            &Scope {
                spaces: vec![space.into()],
                caps: Capability::parse_set(caps),
                ..Default::default()
            },
            "fixture",
            None,
        )
        .await
        .unwrap();
    (info.id, secret)
}
async fn origin(brain: &Brain, space: &str) -> (String, String) {
    let text = "PRIVATE PREFIX. Allowed selected bytes. PRIVATE SUFFIX.";
    let id = brain
        .ingest_note(space, "private-locator.md", text.into(), Timestamp(1))
        .await
        .unwrap();
    (id, text.into())
}
fn req(
    origin: ViewOrigin,
    dest: &str,
    text: &str,
    selected: &str,
    occurrence: &str,
) -> PublishViewV1 {
    let start = text.find(selected).unwrap();
    PublishViewV1 {
        occurrence_id: occurrence.into(),
        origin,
        range: ViewRange {
            start,
            end: start + selected.len(),
        },
        bytes: selected.into(),
        digest: exact_digest(selected),
        destination: dest.into(),
        policy_revision: "authorized-policy-v1".into(),
        occurred_at: Timestamp(2),
    }
}
fn episode(space: &str, id: &str, text: &str) -> ViewOrigin {
    ViewOrigin::Episode {
        space: space.into(),
        episode_id: id.into(),
        revision: exact_digest(text),
    }
}
#[tokio::test]
async fn separate_grants_retry_receipt_and_nonextractable_scoped_view() {
    let (dir, brain, private, shared) = setup().await;
    let (id, text) = origin(&brain, &private).await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, audience) = token(&brain, &shared, "read").await;
    let request = req(
        episode(&private, &id, &text),
        &shared,
        &text,
        "Allowed selected bytes.",
        "one",
    );
    let one = brain
        .publish_source_view(
            request.clone(),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    let two = brain
        .publish_source_view(
            request.clone(),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    assert_eq!(one, two);
    let view = brain
        .read_published_view(&shared, &one.publication_id, audience.clone())
        .await
        .unwrap();
    assert_eq!(view.bytes, "Allowed selected bytes.");
    assert!(!view.extractable);
    assert!(!view.independent_support);
    let output = serde_json::to_string(&view).unwrap();
    for hidden in [
        private.as_str(),
        id.as_str(),
        "private-locator",
        "PRIVATE PREFIX",
        "origin_identity",
        "root_family",
    ] {
        assert!(!output.contains(hidden));
    }
    assert!(
        brain
            .read_published_view(&private, &one.publication_id, audience)
            .await
            .is_err()
    );
    let forged = oxibrain::Declaration::PublicationV1 {
        event: PublicationEventV1::Published {
            request: request.clone(),
            principal: "claimed-authority".into(),
        },
    };
    assert!(brain.declare(&private, forged).await.is_err());
    let changed = PublishViewV1 {
        policy_revision: "changed".into(),
        ..request
    };
    assert!(
        brain
            .publish_source_view(changed, PublicationCredentials::new(read, write))
            .await
            .is_err()
    );
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE space_id=?1",
            [shared],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn recitation_retains_root_and_withdrawal_closes_descendants_without_resurrecting_bytes_on_retry()
 {
    let (dir, brain, private, shared) = setup().await;
    let third = brain.ensure_space("third").await.unwrap();
    let (id, text) = origin(&brain, &private).await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, shared_read) = token(&brain, &shared, "read").await;
    let (_, third_write) = token(&brain, &third, "write").await;
    let (_, third_read) = token(&brain, &third, "read").await;
    let request = req(
        episode(&private, &id, &text),
        &shared,
        &text,
        "Allowed selected bytes.",
        "first",
    );
    let first = brain
        .publish_source_view(
            request.clone(),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    let recitation = req(
        ViewOrigin::Publication {
            space: shared.clone(),
            publication_id: first.publication_id.clone(),
            revision: first.view_revision.clone(),
        },
        &third,
        "Allowed selected bytes.",
        "Allowed selected bytes.",
        "second",
    );
    let second = brain
        .publish_source_view(
            recitation,
            PublicationCredentials::new(shared_read, third_write),
        )
        .await
        .unwrap();
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    let rows = oxibrain_store::publication::records(&conn).unwrap();
    assert_eq!(rows[0].root_family, rows[1].root_family);
    let decl = oxibrain_store::ledger::get_episode(
        &conn,
        &rows
            .iter()
            .find(|row| row.id == second.publication_id)
            .unwrap()
            .declaration_id,
    )
    .unwrap()
    .unwrap();
    assert!(!decl.content.contains(&private));
    assert!(!decl.content.contains(&id));
    drop(conn);
    brain
        .withdraw_published_view(
            shared.clone(),
            first.publication_id.clone(),
            "withdraw".into(),
            write.clone(),
        )
        .await
        .unwrap();
    let closed = brain
        .read_published_view(&third, &second.publication_id, third_read.clone())
        .await
        .unwrap();
    assert_eq!(closed.state, ViewState::Closed);
    assert!(closed.bytes.is_empty());
    let retry = brain
        .publish_source_view(request, PublicationCredentials::new(read, write))
        .await
        .unwrap();
    assert_eq!(first, retry);
    brain.reproject().await.unwrap();
    let before = {
        let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
        oxibrain_store::publication::records(&conn).unwrap()
    };
    let exported = brain.export_jsonl().await.unwrap();
    assert!(!exported.contains("\"table\":\"publication_views\""));
    let imported_dir = TempDir::new().unwrap();
    let imported = Brain::open(BrainConfig::at(imported_dir.path()))
        .await
        .unwrap();
    imported.import_jsonl(exported).await.unwrap();
    imported.reproject().await.unwrap();
    let conn = oxibrain_store::Store::open_read_only(imported_dir.path()).unwrap();
    let after = oxibrain_store::publication::records(&conn).unwrap();
    assert_eq!(
        before, after,
        "lineage must rebuild from exported Declarations alone"
    );
    drop(conn);
    let restored_view = imported
        .read_published_view(&third, &second.publication_id, third_read)
        .await
        .unwrap();
    assert_eq!(restored_view.state, ViewState::Closed);
    assert!(restored_view.bytes.is_empty());
    let safe = serde_json::to_string(&restored_view).unwrap();
    assert!(!safe.contains(&private));
    assert!(!safe.contains(&id));
}
#[tokio::test]
async fn root_redaction_erases_every_publication_copy_and_reprojection_cannot_restore_it() {
    let (dir, brain, private, shared) = setup().await;
    let third = brain.ensure_space("third").await.unwrap();
    let (id, text) = origin(&brain, &private).await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, audience) = token(&brain, &shared, "read").await;
    let (_, third_write) = token(&brain, &third, "write").await;
    let (_, third_audience) = token(&brain, &third, "read").await;
    let request = req(
        episode(&private, &id, &text),
        &shared,
        &text,
        "Allowed selected bytes.",
        "copy",
    );
    let receipt = brain
        .publish_source_view(request, PublicationCredentials::new(read, write))
        .await
        .unwrap();
    let recitation = brain
        .publish_source_view(
            req(
                ViewOrigin::Publication {
                    space: shared.clone(),
                    publication_id: receipt.publication_id.clone(),
                    revision: receipt.view_revision.clone(),
                },
                &third,
                "Allowed selected bytes.",
                "Allowed selected bytes.",
                "recitation-copy",
            ),
            PublicationCredentials::new(audience.clone(), third_write),
        )
        .await
        .unwrap();
    let declarations = {
        let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
        oxibrain_store::publication::records(&conn).unwrap()
    };
    for row in &declarations {
        assert!(
            brain
                .get_episode(&row.declaration_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            brain
                .get_episode_in_space(&row.destination, &row.declaration_id)
                .await
                .unwrap()
                .is_none()
        );
    }
    let result = brain
        .redact(
            &RedactTarget::Episode { id: id.clone() },
            "remove source",
            "fixture-owner",
        )
        .await
        .unwrap();
    assert_eq!(result.closure.episodes.len(), 3);
    brain.reproject().await.unwrap();
    assert!(
        brain
            .read_published_view(&shared, &receipt.publication_id, audience)
            .await
            .is_err()
    );
    assert!(
        brain
            .read_published_view(&third, &recitation.publication_id, third_audience)
            .await
            .is_err()
    );
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    assert!(
        oxibrain_store::publication::records(&conn)
            .unwrap()
            .is_empty()
    );
    let raw: String = conn
        .query_row("SELECT group_concat(content,' ') FROM episodes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(!raw.contains("Allowed selected bytes."));
    assert!(!raw.contains("PRIVATE PREFIX"));
}
#[tokio::test]
async fn document_publication_uses_exact_reader_revision_and_actual_file_correction_closes_current_view()
 {
    let (dir, brain, private, shared) = setup().await;
    let vault = TempDir::new().unwrap();
    let path = vault.path().join("note.md");
    let raw = "---\nformat: pdc-document/2\nbody: pdc-markdown/1\nid: 018f47c6-4a77-7c52-9db8-0e5f9bcb1705\ncreated: 2026-09-14T12:34:56.789Z\nupdated: 2026-09-14T12:34:56.789Z\ntitle: PRIVATE TITLE\n---\nPRIVATE PREFIX.\n\nAllowed selected view.\n";
    std::fs::write(&path, raw).unwrap();
    brain
        .register_document_root(DocumentRootSpec {
            space: private.clone(),
            alias: "private-root".into(),
            path: vault.path().into(),
            include: None,
            exclude: None,
            max_file_bytes: None,
        })
        .await
        .unwrap();
    brain
        .index_documents(IndexOptions::default())
        .await
        .unwrap();
    let decoded = oxibrain_connectors::parse_markdown_document("", raw.as_bytes())
        .unwrap()
        .body
        .text;
    let origin = ViewOrigin::Document {
        space: private.clone(),
        root: "private-root".into(),
        document_id: oxibrain_core::documents::document_id("private-root", "note.md"),
        document_uuid: Some("018f47c6-4a77-7c52-9db8-0e5f9bcb1705".into()),
        revision: format!("blake3:{}", blake3::hash(raw.as_bytes()).to_hex()),
        decoder: oxibrain_connectors::DECODER_VERSION.into(),
    };
    let request = req(
        origin,
        &shared,
        &decoded,
        "Allowed selected view.",
        "document",
    );
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, audience) = token(&brain, &shared, "read").await;
    let receipt = brain
        .publish_source_view(
            request.clone(),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    assert_eq!(
        brain
            .read_published_view(&shared, &receipt.publication_id, audience.clone())
            .await
            .unwrap()
            .bytes,
        "Allowed selected view."
    );
    let malformed_git = vault.path().join(".git");
    std::fs::write(
        &malformed_git,
        "gitdir: PRIVATE-GIT-LOCATOR-does-not-exist\n",
    )
    .unwrap();
    let denied = brain
        .read_published_view(&shared, &receipt.publication_id, audience.clone())
        .await
        .unwrap_err()
        .to_string();
    for private_detail in [
        "PRIVATE-GIT-LOCATOR",
        "PRIVATE TITLE",
        "note.md",
        vault.path().to_str().unwrap(),
        private.as_str(),
    ] {
        assert!(
            !denied.contains(private_detail),
            "private root failures must remain opaque"
        );
    }
    std::fs::remove_file(malformed_git).unwrap();
    let replacement_root = TempDir::new().unwrap();
    std::fs::write(replacement_root.path().join("note.md"), raw).unwrap();
    let config_path = dir.path().join("documents.toml");
    let original_config = std::fs::read_to_string(&config_path).unwrap();
    let replacement_config = original_config.replace(
        vault.path().to_str().unwrap(),
        replacement_root.path().to_str().unwrap(),
    );
    assert_ne!(replacement_config, original_config);
    let clock = std::sync::Arc::new(DeliveryClock {
        now: Timestamp(i64::MAX / 2),
        pending: std::sync::Mutex::new(Some((
            2,
            dir.path().into(),
            DeliveryEffect::EditFile(config_path.clone(), replacement_config),
        ))),
    });
    let checked = Brain::with_clock(BrainConfig::at(dir.path()), clock)
        .await
        .unwrap();
    assert!(
        checked
            .read_published_view(&shared, &receipt.publication_id, audience.clone())
            .await
            .is_err(),
        "replacing a registered root at delivery cannot reuse a prior root proof even for equal bytes"
    );
    std::fs::write(&config_path, original_config).unwrap();
    let clock = std::sync::Arc::new(DeliveryClock {
        now: Timestamp(i64::MAX / 2),
        pending: std::sync::Mutex::new(Some((
            2,
            dir.path().into(),
            DeliveryEffect::EditFile(
                path.clone(),
                raw.replace("Allowed selected view.", "Corrected source view."),
            ),
        ))),
    });
    let checked = Brain::with_clock(BrainConfig::at(dir.path()), clock)
        .await
        .unwrap();
    assert!(
        checked
            .read_published_view(&shared, &receipt.publication_id, audience.clone())
            .await
            .is_err(),
        "an actual source correction at delivery cannot release stale bytes"
    );
    std::fs::write(
        &path,
        raw.replace("Allowed selected view.", "Corrected source view."),
    )
    .unwrap();
    let closed = brain
        .read_published_view(&shared, &receipt.publication_id, audience)
        .await
        .unwrap();
    assert_eq!(closed.state, ViewState::Closed);
    assert!(closed.bytes.is_empty());
    let stale = PublishViewV1 {
        occurrence_id: "stale-republish".into(),
        ..request
    };
    assert!(
        brain
            .publish_source_view(stale, PublicationCredentials::new(read, write))
            .await
            .is_err()
    );
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    let before = oxibrain_store::index_ops::snapshot_truth(&conn, &shared).unwrap();
    drop(conn);
    brain.reproject().await.unwrap();
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    assert_eq!(
        before,
        oxibrain_store::index_ops::snapshot_truth(&conn, &shared).unwrap()
    );
}

#[tokio::test]
async fn cold_backup_reopen_preserves_receipt_identity_and_withdrawal_retry_has_no_second_event() {
    let (dir, brain, private, shared) = setup().await;
    let (id, text) = origin(&brain, &private).await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, audience) = token(&brain, &shared, "read").await;
    let request = req(
        episode(&private, &id, &text),
        &shared,
        &text,
        "Allowed selected bytes.",
        "backup-occurrence",
    );
    let original = brain
        .publish_source_view(
            request.clone(),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    let backup = TempDir::new().unwrap();
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    oxibrain_store::online_backup(&conn, &backup.path().join("brain.db")).unwrap();
    drop(conn);
    let restored = Brain::open(BrainConfig::at(backup.path())).await.unwrap();
    restored.reproject().await.unwrap();
    assert_eq!(
        original,
        restored
            .publish_source_view(request, PublicationCredentials::new(read, write.clone()))
            .await
            .unwrap()
    );
    assert_eq!(
        restored
            .read_published_view(&shared, &original.publication_id, audience)
            .await
            .unwrap()
            .bytes,
        "Allowed selected bytes."
    );
    for _ in 0..2 {
        restored
            .withdraw_published_view(
                shared.clone(),
                original.publication_id.clone(),
                "withdraw-once".into(),
                write.clone(),
            )
            .await
            .unwrap();
    }
    let conn = oxibrain_store::Store::open_read_only(backup.path()).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE space_id=?1",
            [&shared],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 2,
        "one ledger-backed source registration plus one withdrawal"
    );
    let withdrawals: i64 = conn.query_row("SELECT COUNT(*) FROM episodes WHERE space_id=?1 AND json_extract(content,'$.op')='publication_v1'", [&shared], |row| row.get(0)).unwrap();
    assert_eq!(
        withdrawals, 1,
        "exact withdrawal retry cannot append another event"
    );
}

struct DeliveryClock {
    now: Timestamp,
    pending: std::sync::Mutex<Option<(usize, std::path::PathBuf, DeliveryEffect)>>,
}
enum DeliveryEffect {
    Revoke(String),
    Redact(String),
    EditFile(std::path::PathBuf, String),
}

#[tokio::test]
async fn oversized_raw_and_compacted_episode_sources_are_rejected_before_materialization() {
    let (dir, brain, private, shared) = setup().await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let huge = format!(
        "Allowed.{}",
        "x".repeat(oxibrain_core::publication::MAX_SOURCE_BYTES)
    );
    for compacted in [false, true] {
        let (id, _) = origin(&brain, &private).await;
        let conn = rusqlite::Connection::open(dir.path().join("brain.db")).unwrap();
        if compacted {
            conn.execute(
                "UPDATE episodes SET content='',content_compacted=?1 WHERE id=?2",
                rusqlite::params![huge.as_bytes(), id],
            )
            .unwrap();
        } else {
            conn.execute(
                "UPDATE episodes SET content=?1,content_compacted=NULL WHERE id=?2",
                rusqlite::params![huge, id],
            )
            .unwrap();
        }
        drop(conn);
        assert!(
            brain
                .publish_source_view(
                    req(
                        episode(&private, &id, &huge),
                        &shared,
                        &huge,
                        "Allowed.",
                        if compacted { "compacted" } else { "raw" }
                    ),
                    PublicationCredentials::new(read.clone(), write.clone())
                )
                .await
                .is_err()
        );
    }
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    assert!(
        oxibrain_store::publication::records(&conn)
            .unwrap()
            .is_empty()
    );
}
impl oxibrain::ClockPort for DeliveryClock {
    fn now(&self) -> Timestamp {
        let mut pending = self.pending.lock().unwrap();
        if let Some((calls, path, effect)) = pending.as_mut() {
            *calls -= 1;
            if *calls == 0 {
                let conn = rusqlite::Connection::open(path.join("brain.db")).unwrap();
                match effect {
                    DeliveryEffect::Revoke(id) => {
                        oxibrain_store::security::revoke_token(&conn, id, self.now).unwrap();
                    }
                    DeliveryEffect::Redact(id) => {
                        oxibrain_store::redaction::execute_redaction(
                            &conn,
                            &RedactTarget::Episode { id: id.clone() },
                            "in-flight source redaction",
                            "fixture-owner",
                            self.now,
                        )
                        .unwrap();
                    }
                    DeliveryEffect::EditFile(path, bytes) => {
                        std::fs::write(path, bytes).unwrap();
                    }
                }
                *pending = None;
            }
        }
        self.now
    }
}
#[tokio::test]
async fn revoked_token_or_redaction_between_materialization_and_delivery_cannot_return_source_bytes()
 {
    for redact in [false, true] {
        let (dir, brain, private, shared) = setup().await;
        let (id, text) = origin(&brain, &private).await;
        let (_, read) = token(&brain, &private, "read").await;
        let (_, write) = token(&brain, &shared, "write").await;
        let (audience_id, audience) = token(&brain, &shared, "read").await;
        let receipt = brain
            .publish_source_view(
                req(
                    episode(&private, &id, &text),
                    &shared,
                    &text,
                    "Allowed selected bytes.",
                    "in-flight",
                ),
                PublicationCredentials::new(read, write),
            )
            .await
            .unwrap();
        let clock = std::sync::Arc::new(DeliveryClock {
            now: Timestamp(i64::MAX / 2),
            pending: std::sync::Mutex::new(Some((
                2,
                dir.path().into(),
                if redact {
                    DeliveryEffect::Redact(id)
                } else {
                    DeliveryEffect::Revoke(audience_id)
                },
            ))),
        });
        let checked = Brain::with_clock(BrainConfig::at(dir.path()), clock)
            .await
            .unwrap();
        let result = checked
            .read_published_view(&shared, &receipt.publication_id, audience)
            .await;
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(!error.contains("Allowed selected"));
        assert!(!error.contains("PRIVATE"));
    }
}

#[tokio::test]
async fn source_retraction_requires_real_origin_write_authority_and_does_not_delete_source() {
    let (dir, brain, private, shared) = setup().await;
    let (id, text) = origin(&brain, &private).await;
    let (_, read) = token(&brain, &private, "read").await;
    let (_, write) = token(&brain, &shared, "write").await;
    let (_, origin_write) = token(&brain, &private, "write").await;
    let (_, audience) = token(&brain, &shared, "read").await;
    let source = episode(&private, &id, &text);
    let receipt = brain
        .publish_source_view(
            req(
                source.clone(),
                &shared,
                &text,
                "Allowed selected bytes.",
                "retract",
            ),
            PublicationCredentials::new(read.clone(), write.clone()),
        )
        .await
        .unwrap();
    for denied in [read, write] {
        assert!(
            brain
                .retract_source_views(source.clone(), "retraction".into(), denied)
                .await
                .is_err()
        );
    }
    for entity_filter in [false, true] {
        let mut filtered = Scope {
            spaces: vec![private.clone(), shared.clone()],
            caps: Capability::parse_set("write"),
            ..Default::default()
        };
        if entity_filter {
            filtered.entity_type_filter = Some(vec!["Person".into()]);
        } else {
            filtered.predicate_filter = Some(vec!["works_on".into()]);
        }
        let (_, restricted) = brain
            .issue_token(&filtered, "filtered", None)
            .await
            .unwrap();
        assert!(
            brain
                .retract_source_views(
                    source.clone(),
                    "filtered-retract".into(),
                    restricted.clone()
                )
                .await
                .is_err()
        );
        assert!(
            brain
                .withdraw_published_view(
                    shared.clone(),
                    receipt.publication_id.clone(),
                    "filtered-withdraw".into(),
                    restricted
                )
                .await
                .is_err()
        );
    }
    for _ in 0..2 {
        brain
            .retract_source_views(source.clone(), "retraction".into(), origin_write.clone())
            .await
            .unwrap();
    }
    let (_, fresh_read) = token(&brain, &private, "read").await;
    let (_, fresh_write) = token(&brain, &shared, "write").await;
    assert!(
        brain
            .publish_source_view(
                req(
                    source.clone(),
                    &shared,
                    &text,
                    "Allowed selected bytes.",
                    "after-retraction"
                ),
                PublicationCredentials::new(fresh_read, fresh_write)
            )
            .await
            .is_err()
    );
    let closed = brain
        .read_published_view(&shared, &receipt.publication_id, audience)
        .await
        .unwrap();
    assert_eq!(closed.state, ViewState::Closed);
    assert!(closed.bytes.is_empty());
    let conn = oxibrain_store::Store::open_read_only(dir.path()).unwrap();
    let original = oxibrain_store::ledger::get_episode(&conn, &id)
        .unwrap()
        .unwrap();
    assert_eq!(original.content, text);
    assert!(original.redacted_at.is_none());
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM publication_source_observations",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    drop(conn);
    brain.reproject().await.unwrap();
}
