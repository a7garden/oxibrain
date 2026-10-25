use oxibrain_core::{ContentHash, Episode, EpisodeKind, SourceRef, TrustTier, publication::*};
use oxibrain_ports::Timestamp;
use oxibrain_store::{
    ledger, migration,
    project::{Declaration, ResolutionCache, project_declaration},
    publication,
};
use rusqlite::Connection;
fn setup() -> (Connection, String, String) {
    migration::ensure_vec_extension();
    let conn = Connection::open_in_memory().unwrap();
    migration::run(&conn).unwrap();
    let private = ledger::create_space(&conn, "private", Timestamp(0)).unwrap();
    let shared = ledger::create_space(&conn, "shared", Timestamp(0)).unwrap();
    (conn, private, shared)
}
fn source(conn: &Connection, space: &str, occurrence: &str) -> Episode {
    let sid = oxibrain_core::source_id(space, "sources");
    conn.execute("INSERT OR IGNORE INTO sources(id,space_id,name,kind,mode,claims_json,created_at) VALUES (?1,?2,'sources','conversation','push','{}',0)",rusqlite::params![sid,space]).unwrap();
    let mut ep = Episode {
        id: String::new(),
        space: space.into(),
        seq: 0,
        content_hash: ContentHash([0; 32]),
        content: "same selected bytes".into(),
        source: SourceRef::Conversation,
        trust: TrustTier::Untrusted,
        kind: EpisodeKind::Primary,
        occurred_at: Timestamp(1),
        ingested_at: Timestamp(1),
        redacted_at: None,
    };
    ledger::insert_event(
        conn,
        &mut ep,
        Some(&ledger::IngestAttachment {
            source_id: sid,
            occurrence_id: occurrence.into(),
            accepted_at: Timestamp(1),
            principal: "source".into(),
            claims_json: "{}".into(),
        }),
    )
    .unwrap();
    ep
}
fn request(source: &Episode, dest: &str, occurrence: &str) -> PublishViewV1 {
    PublishViewV1 {
        occurrence_id: occurrence.into(),
        origin: ViewOrigin::Episode {
            space: source.space.clone(),
            episode_id: source.id.clone(),
            revision: exact_digest(&source.content),
        },
        range: ViewRange {
            start: 0,
            end: source.content.len(),
        },
        bytes: source.content.clone(),
        digest: exact_digest(&source.content),
        destination: dest.into(),
        policy_revision: "policy-v1".into(),
        occurred_at: Timestamp(2),
    }
}
fn event(conn: &Connection, space: &str, event: PublicationEventV1, at: i64) -> String {
    project_declaration(
        conn,
        space,
        &Declaration::PublicationV1 { event },
        Timestamp(at),
        &mut ResolutionCache::new(),
    )
    .unwrap()
}
#[test]
fn prior_schema_upgrade_and_projection_rebuild_preserve_episode_and_truth() {
    let (conn, private, shared) = setup();
    conn.execute_batch("DROP TABLE publication_views; DROP TABLE publication_source_observations; PRAGMA user_version=18;").unwrap();
    let one = source(&conn, &private, "one");
    let two = source(&conn, &private, "two");
    migration::run(&conn).unwrap();
    assert_eq!(
        ledger::get_episode(&conn, &one.id)
            .unwrap()
            .unwrap()
            .content,
        one.content
    );
    for ep in [&one, &two] {
        event(
            &conn,
            &private,
            PublicationEventV1::Published {
                request: request(ep, &shared, &ep.id),
                principal: "authenticated-principal".into(),
            },
            3,
        );
    }
    let before = serde_json::to_string(&publication::records(&conn).unwrap()).unwrap();
    assert_eq!(publication::records(&conn).unwrap().len(), 2);
    assert_ne!(
        publication::records(&conn).unwrap()[0].root_family,
        publication::records(&conn).unwrap()[1].root_family
    );
    let assertions: i64 = conn
        .query_row("SELECT COUNT(*) FROM assertions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(assertions, 0);
    let report = oxibrain_store::reproject(&conn).unwrap();
    assert_eq!(report.declarations_skipped, 0);
    assert_eq!(
        before,
        serde_json::to_string(&publication::records(&conn).unwrap()).unwrap()
    );
    let private_dest_episodes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE space_id=?1",
            [&shared],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(private_dest_episodes, 0);
}
#[test]
fn repeated_publication_recitation_withdrawal_and_correction_are_deterministic() {
    let (conn, private, shared) = setup();
    let third = ledger::create_space(&conn, "third", Timestamp(0)).unwrap();
    let ep = source(&conn, &private, "origin");
    let req = request(&ep, &shared, "publish");
    event(
        &conn,
        &private,
        PublicationEventV1::Published {
            request: req.clone(),
            principal: "principal".into(),
        },
        3,
    );
    let id = publication_id(&shared, "principal", "publish");
    let parent = publication::record(&conn, &id).unwrap().unwrap();
    let recite = PublishViewV1 {
        origin: ViewOrigin::Publication {
            space: shared.clone(),
            publication_id: id.clone(),
            revision: parent.view_revision.clone(),
        },
        destination: third.clone(),
        occurrence_id: "recite".into(),
        ..req.clone()
    };
    event(
        &conn,
        &shared,
        PublicationEventV1::Published {
            request: recite,
            principal: "principal".into(),
        },
        4,
    );
    let rows = publication::records(&conn).unwrap();
    assert_eq!(rows[0].root_family, rows[1].root_family);
    event(
        &conn,
        &private,
        PublicationEventV1::Published {
            request: req.clone(),
            principal: "principal".into(),
        },
        5,
    );
    assert_eq!(publication::records(&conn).unwrap().len(), 2);
    let mutated = PublishViewV1 {
        bytes: "changed".into(),
        ..req.clone()
    };
    assert!(
        plan_projection(
            &PublicationEventV1::Published {
                request: mutated,
                principal: "principal".into()
            },
            "declaration",
            &private,
            &rows,
            &[]
        )
        .is_err()
    );
    event(
        &conn,
        &private,
        PublicationEventV1::SourceObserved {
            occurrence_id: "verified-correction".into(),
            origin_identity: req.origin.identity(),
            previous_revision: req.origin.revision().into(),
            observed_revision: Some("actual-new-revision".into()),
            principal: "verified-source-observer".into(),
            occurred_at: Timestamp(6),
        },
        6,
    );
    assert!(
        publication::records(&conn)
            .unwrap()
            .iter()
            .all(|r| r.state == ViewState::Closed)
    );
    let before = serde_json::to_string(&publication::records(&conn).unwrap()).unwrap();
    oxibrain_store::reproject(&conn).unwrap();
    assert_eq!(
        before,
        serde_json::to_string(&publication::records(&conn).unwrap()).unwrap()
    );
}

#[test]
fn unicode_compatible_but_distinct_occurrences_survive_cold_reprojection() {
    let (conn, private, shared) = setup();
    let source = source(&conn, &private, "source");
    for occurrence in ["A", "Ａ"] {
        event(
            &conn,
            &private,
            PublicationEventV1::Published {
                request: request(&source, &shared, occurrence),
                principal: "principal".into(),
            },
            3,
        );
    }
    let rows = publication::records(&conn).unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].declaration_id, rows[1].declaration_id);
    let before = serde_json::to_string(&rows).unwrap();
    oxibrain_store::reproject(&conn).unwrap();
    assert_eq!(
        before,
        serde_json::to_string(&publication::records(&conn).unwrap()).unwrap()
    );
}
