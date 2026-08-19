//! End-to-end curation round-trip tests (Plan C Task 3).
//!
//! These tests drive the Brain facade directly (no `oxibrain` binary) to prove
//! that every curation operation — merge, split, alias, retract, declare,
//! source policy, predicate add — produces the expected store side-effects,
//! and that a full reproject preserves curation state.
//!

use oxibrain::{Brain, BrainConfig};
use oxibrain_core::{BeliefStatus, EntityMerge};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};

async fn fresh_brain() -> (tempfile::TempDir, Brain) {
    let dir = tempfile::tempdir().expect("tempdir");
    let brain = Brain::open(BrainConfig::at(dir.path()))
        .await
        .expect("open");
    (dir, brain)
}

fn add_statement(surface: &str, predicate: &str, object_surface: &str) -> Declaration {
    Declaration::AddStatement {
        subject: EntityRef {
            surface: surface.to_string(),
            ty: "Person".to_string(),
        },
        predicate: predicate.to_string(),
        object: DeclObject::Entity {
            surface: object_surface.to_string(),
            ty: "Organization".to_string(),
        },
        polarity: "affirm".to_string(),
        valid_from: 0,
        valid_to: i64::MAX,
    }
}

#[tokio::test]
async fn merge_then_split_round_trip() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // Seed two independent entities (so the test doesn't depend on merge
    // initialising an empty loser). Without statements, Merge would still
    // create the entities, but seeding a belief first makes the merge's
    // post-state observable through `beliefs`.
    brain
        .declare(&space, add_statement("Bob", "employed_by", "Initech"))
        .await
        .expect("declare bob");
    brain
        .declare(&space, add_statement("Alice", "employed_by", "Initech"))
        .await
        .expect("declare alice");

    // Merge Bob -> Alice.
    brain
        .declare(
            &space,
            Declaration::Merge {
                loser: EntityRef {
                    surface: "Bob".to_string(),
                    ty: "Person".to_string(),
                },
                winner: EntityRef {
                    surface: "Alice".to_string(),
                    ty: "Person".to_string(),
                },
            },
        )
        .await
        .expect("merge");

    // Post-merge: a single merge record exists and is active (undone_at is None).
    let merges_post = brain.list_merges(&space).await.expect("list merges post");
    assert_eq!(merges_post.len(), 1, "exactly one merge record");
    let merge: &EntityMerge = &merges_post[0];
    assert!(
        merge.undone_at.is_none(),
        "merge is active (undone_at None)"
    );

    // Now split Bob. According to the design this finds the most recent
    // active merge where Bob is the loser and marks it undone.
    brain
        .declare(
            &space,
            Declaration::Split {
                entity: EntityRef {
                    surface: "Bob".to_string(),
                    ty: "Person".to_string(),
                },
            },
        )
        .await
        .expect("split");

    // Post-split: merge record's undone_at is now set.
    let merges_post_split = brain
        .list_merges(&space)
        .await
        .expect("list merges post-split");
    assert_eq!(merges_post_split.len(), 1, "merge record still present");
    let merge_after: &EntityMerge = &merges_post_split[0];
    assert!(
        merge_after.undone_at.is_some(),
        "split must set undone_at on the merge record"
    );

    // Sanity: splitting again with no active merge must fail (the merge is
    // already undone). This double-safety net proves we didn't accidentally
    // leave the merge active.
    let repeat = brain
        .declare(
            &space,
            Declaration::Split {
                entity: EntityRef {
                    surface: "Bob".to_string(),
                    ty: "Person".to_string(),
                },
            },
        )
        .await;
    assert!(
        repeat.is_err(),
        "second split on already-undone merge must fail"
    );
}

#[tokio::test]
async fn alias_makes_entity_findable_by_alias() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // Declare a statement whose subject surface is "Alice".
    brain
        .declare(&space, add_statement("Alice", "employed_by", "Acme"))
        .await
        .expect("declare alice");

    // Confirm a baseline lookup by surface.
    let baseline = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("alice exists");

    // Add an alias "Al" on Alice.
    brain
        .declare(
            &space,
            Declaration::Alias {
                entity: EntityRef {
                    surface: "Alice".to_string(),
                    ty: "Person".to_string(),
                },
                surface: "Al".to_string(),
            },
        )
        .await
        .expect("alias");

    // The alias resolves to the same entity. This is the proof that the
    // user's surface form participates in the same lookup machinery as the
    // canonical surface (design: Alias adds a UserDeclared EntityKey).
    let via_alias = brain
        .resolve_entity_id(&space, "Person", "Al")
        .await
        .expect("resolve alias")
        .expect("alias resolves");
    assert_eq!(
        via_alias, baseline,
        "alias surface must resolve to the canonical entity"
    );

    // Beliefs asked through the alias-attached entity id still return Alice's
    // beliefs.
    let beliefs = brain.beliefs(&space, &via_alias).await.expect("beliefs");
    assert!(
        !beliefs.is_empty(),
        "Alice's beliefs still reachable via alias"
    );
}

#[tokio::test]
async fn retract_by_statement_id() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // Declare the statement.
    let decl = add_statement("Alice", "employed_by", "Acme");
    brain.declare(&space, decl.clone()).await.expect("declare");

    // Find Alice's entity id, then the statement id from her beliefs.
    let alice_id = brain
        .resolve_entity_id(&space, "Person", "Alice")
        .await
        .expect("resolve")
        .expect("alice");
    let beliefs = brain.beliefs(&space, &alice_id).await.expect("beliefs");
    assert_eq!(beliefs.len(), 1, "exactly one belief before retract");
    let statement_id = beliefs[0].statement.clone();
    let original_status = beliefs[0].status;
    assert_eq!(original_status, BeliefStatus::Active, "starts Active");

    // Snapshot pre-retract, isolating the assertions section. The snapshot
    // helper renders NULL columns as empty strings between `|` separators.
    let pre_snap = brain.snapshot_truth(&space).await.expect("pre snap");
    let pre_assertions = section(&pre_snap, "assertions");
    assert!(
        pre_assertions.contains('|'),
        "pre-retract assertions section must contain at least one row; got: \
         {pre_assertions}"
    );
    // The trailing field (retracted_at) is NULL before retract — render as
    // empty between pipes. So a row ends with `|<empty>|` followed by \n.
    // Find rows that end with a double-pipe or trailing-pipe+newline.
    let pre_row_count = pre_assertions.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(pre_row_count, 1, "exactly one assertion row pre-retract");

    // Retract via retract_parts + declare(Retract), mirroring the CLI path.
    let (subject, predicate, object) = brain
        .retract_parts(&space, &statement_id)
        .await
        .expect("retract_parts");
    brain
        .declare(
            &space,
            Declaration::Retract {
                subject,
                predicate,
                object,
                episode: String::new(),
            },
        )
        .await
        .expect("declare retract");

    // Post-retract: the assertion row's retracted_at column is now set to a
    // numeric ms timestamp. The retract produced an auditable episode, so
    // the truth snapshot must change.
    let post_snap = brain.snapshot_truth(&space).await.expect("post snap");
    let post_assertions = section(&post_snap, "assertions");
    assert_ne!(
        pre_assertions, post_assertions,
        "assertions section must change after retract"
    );

    // Take the first/only assertion row and inspect its trailing field.
    // Before retract, `retracted_at` rendered as empty (NULL). After, it is
    // a positive integer (the millisecond timestamp). Concretely, the post
    // row's last `|` segment contains a digit-only string; the pre row's
    // last segment is empty.
    let pre_row = pre_assertions
        .lines()
        .find(|l| !l.is_empty())
        .expect("pre row");
    let post_row = post_assertions
        .lines()
        .find(|l| !l.is_empty())
        .expect("post row");
    let pre_last = pre_row.rsplit('|').next().expect("pre tail");
    let post_last = post_row.rsplit('|').next().expect("post tail");
    assert!(
        pre_last.is_empty(),
        "pre-retract trailing field (retracted_at) must be empty/NULL; got: \
         `{pre_last}`"
    );
    assert!(
        !post_last.is_empty() && post_last.chars().all(|c| c.is_ascii_digit()),
        "post-retract trailing field must be a numeric ms timestamp; got: \
         `{post_last}`"
    );

    // Belt-and-braces: belief() lookups after retract return no rows because
    // the engine re-folds over visible (non-retracted) assertions, which now
    // contain zero. This is the documented engine behaviour — retracted
    // beliefs don't appear in `belief()` lists. The retracted state lives on
    // the assertion's retracted_at column instead, verifiable through the
    // truth snapshot above.
    let post_beliefs = brain
        .beliefs(&space, &alice_id)
        .await
        .expect("post beliefs");
    assert!(
        post_beliefs.is_empty(),
        "post-retract beliefs() returns empty (retracted dropped from fold)"
    );
}

/// Extract a single `---label---...---next-label---` slice from a snapshot.
fn section(snapshot: &str, label: &str) -> String {
    let start_header = format!("---{label}---");
    let Some(start) = snapshot.find(&start_header) else {
        return String::new();
    };
    let after = &snapshot[start + start_header.len()..];
    // Next `\n` after the header line.
    let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &after[body_start..];
    // Up to the next `---` marker (start of next section) or end of string.
    let end = body.find("\n---").unwrap_or(body.len());
    body[..end].to_string()
}

#[tokio::test]
async fn declare_raw_json() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // Build a Declaration::AddStatement via serde_json round-trip.
    let decl = add_statement("Alice", "employed_by", "Acme");
    let json = serde_json::to_string(&decl).expect("serialize");
    let parsed: Declaration = serde_json::from_str(&json).expect("parse");

    let before = brain.episode_count().await.expect("count before");
    let ep_id = brain.declare(&space, parsed).await.expect("declare");
    let after = brain.episode_count().await.expect("count after");

    assert!(
        after > before,
        "declaring an episode must increase the count (before={before}, after={after})"
    );
    assert!(!ep_id.is_empty(), "declare returns non-empty episode id");
}

#[tokio::test]
async fn source_policy_changes_trust() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");
    // First, register the source.
    brain
        .declare(
            &space,
            Declaration::RegisterSource {
                name: "daily_news".to_string(),
                kind: "rss".to_string(),
                mode: "push".to_string(),
                claims_json: "{}".to_string(),
            },
        )
        .await
        .expect("register source");

    let before = brain.episode_count().await.expect("count before policy");
    brain
        .declare(
            &space,
            Declaration::SetSourcePolicy {
                source_name: "daily_news".to_string(),
                trust: "trusted".to_string(),
                effective_from: 0,
                effective_to: None,
            },
        )
        .await
        .expect("set policy");
    let after = brain.episode_count().await.expect("count after policy");

    assert!(
        after > before,
        "SetSourcePolicy must produce an episode (before={before}, after={after})"
    );

    // Re-declaring the same policy is idempotent at the engine level (content
    // hash collision). The episode count should not grow.
    let pre_dup = after;
    let second = brain
        .declare(
            &space,
            Declaration::SetSourcePolicy {
                source_name: "daily_news".to_string(),
                trust: "trusted".to_string(),
                effective_from: 0,
                effective_to: None,
            },
        )
        .await
        .expect("duplicate policy");
    let post_dup = brain.episode_count().await.expect("count after dup");
    assert_eq!(
        post_dup, pre_dup,
        "duplicate SetSourcePolicy is content-deterministic (ep={second})"
    );
}

#[tokio::test]
async fn predicate_add_and_list() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // RegisterPredicate accepts raw def_json; the projection extracts
    // major/minor version fields and stores the row.
    let def_json = serde_json::json!({
        "name": "custom_likes",
        "major_version": 1,
        "minor_version": 0,
        "args": [{"name": "thing", "type": "Entity"}],
        "profile_relevant": true,
    })
    .to_string();

    let before = brain.snapshot_truth(&space).await.expect("snap before");
    brain
        .declare(
            &space,
            Declaration::RegisterPredicate {
                name: "custom_likes".to_string(),
                def_json: def_json.clone(),
            },
        )
        .await
        .expect("register predicate");
    let after = brain.snapshot_truth(&space).await.expect("snap after");

    assert_ne!(
        before, after,
        "RegisterPredicate must change the truth snapshot"
    );
    assert!(
        after.contains("custom_likes"),
        "snapshot truth must contain the new predicate name; got: {after}"
    );

    // Reprojection is still valid after introducing a new predicate: same
    // snapshot text across two reprojects.
    brain.reproject().await.expect("reproject 1");
    let snap1 = brain.snapshot_truth(&space).await.expect("snap1");
    brain.reproject().await.expect("reproject 2");
    let snap2 = brain.snapshot_truth(&space).await.expect("snap2");
    assert_eq!(snap1, snap2, "reprojection is deterministic");
}

#[tokio::test]
async fn reproject_preserves_curation() {
    let (_dir, brain) = fresh_brain().await;
    let space = brain.ensure_space("personal").await.expect("space");

    // Sequence of curation ops: merge, alias, split. Net effect: merge is
    // undone but the alias and the underlying entities remain.
    brain
        .declare(&space, add_statement("Bob", "employed_by", "Initech"))
        .await
        .expect("declare bob");
    brain
        .declare(&space, add_statement("Alice", "employed_by", "Initech"))
        .await
        .expect("declare alice");

    // Merge Bob -> Alice.
    brain
        .declare(
            &space,
            Declaration::Merge {
                loser: EntityRef {
                    surface: "Bob".to_string(),
                    ty: "Person".to_string(),
                },
                winner: EntityRef {
                    surface: "Alice".to_string(),
                    ty: "Person".to_string(),
                },
            },
        )
        .await
        .expect("merge");

    // Alias on the winner.
    brain
        .declare(
            &space,
            Declaration::Alias {
                entity: EntityRef {
                    surface: "Alice".to_string(),
                    ty: "Person".to_string(),
                },
                surface: "Ali".to_string(),
            },
        )
        .await
        .expect("alias");

    // Split Bob back out.
    brain
        .declare(
            &space,
            Declaration::Split {
                entity: EntityRef {
                    surface: "Bob".to_string(),
                    ty: "Person".to_string(),
                },
            },
        )
        .await
        .expect("split");

    // Snapshot before reproject.
    let before = brain.snapshot_truth(&space).await.expect("snap before");
    let merges_before = brain.list_merges(&space).await.expect("merges before");
    let alice_beliefs_before = brain
        .beliefs(
            &space,
            &brain
                .resolve_entity_id(&space, "Person", "Alice")
                .await
                .expect("resolve")
                .expect("alice"),
        )
        .await
        .expect("alice beliefs before");
    let ali_id_before = brain
        .resolve_entity_id(&space, "Person", "Ali")
        .await
        .expect("resolve ali")
        .expect("ali resolves");

    // Reproject everything from scratch.
    brain.reproject().await.expect("reproject");

    // After reproject: byte-identical truth snapshot, same merge records,
    // Alice's beliefs still reachable, alias still resolves.
    let after = brain.snapshot_truth(&space).await.expect("snap after");
    assert_eq!(
        before, after,
        "truth snapshot must be invariant under reproject"
    );

    let merges_after = brain.list_merges(&space).await.expect("merges after");
    assert_eq!(
        merges_before.len(),
        merges_after.len(),
        "merge record count must be invariant"
    );
    assert!(
        merges_after[0].undone_at.is_some(),
        "the single merge must still be undone (split state preserved)"
    );

    let alice_beliefs_after = brain
        .beliefs(
            &space,
            &brain
                .resolve_entity_id(&space, "Person", "Alice")
                .await
                .expect("resolve 2")
                .expect("alice still there"),
        )
        .await
        .expect("alice beliefs after");
    assert_eq!(
        alice_beliefs_before.len(),
        alice_beliefs_after.len(),
        "Alice's belief count must be invariant"
    );

    let ali_id_after = brain
        .resolve_entity_id(&space, "Person", "Ali")
        .await
        .expect("resolve ali after")
        .expect("ali still resolves");
    assert_eq!(
        ali_id_before, ali_id_after,
        "alias surface must still resolve to the same entity"
    );
}
