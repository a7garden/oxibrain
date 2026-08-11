//! The reprojection determinism test (DESIGN §14.3): the single most valuable
//! test in the suite. For a sequence of declarations, incremental projection
//! must produce byte-identical results to full reprojection.

use oxibrain::Brain;
use oxibrain_ports::{TIME_MAX, TIME_MIN};
use oxibrain_store::project::{DeclObject, Declaration, EntityRef};
use rusqlite::Connection;
use tempfile::TempDir;

fn dump_table(conn: &Connection, table: &str, columns: &str, order: &str) -> String {
    let sql = format!("SELECT {columns} FROM {table} ORDER BY {order}");
    let n_cols = columns.split(',').count();
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| {
            let mut parts = Vec::new();
            for i in 0..n_cols {
                let val: String = match r.get_ref(i) {
                    Ok(rusqlite::types::ValueRef::Null) => "null".into(),
                    Ok(rusqlite::types::ValueRef::Integer(i)) => i.to_string(),
                    Ok(rusqlite::types::ValueRef::Real(f)) => f.to_string(),
                    Ok(rusqlite::types::ValueRef::Text(t)) => {
                        format!("\"{}\"", String::from_utf8_lossy(t))
                    }
                    Ok(rusqlite::types::ValueRef::Blob(b)) => format!("blob({})", b.len()),
                    Err(_) => "?".into(),
                };
                parts.push(val);
            }
            Ok(parts.join(","))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    rows.join(";")
}

fn dump_all(conn: &Connection) -> String {
    let mut out = String::new();
    for (table, cols, order) in [
        (
            "entities",
            "id, space_id, type_name, canonical_key, created_at, merged_into",
            "id",
        ),
        (
            "entity_keys",
            "id, space_id, entity_id, type_name, normalized, surface, origin",
            "id",
        ),
        (
            "statements",
            "id, space_id, subject_id, predicate, object_entity, object_literal",
            "id",
        ),
        (
            "assertions",
            "id, statement_id, episode_id, extractor_id, polarity, claimed_from, claimed_to, confidence, recorded_at, retracted_at",
            "id",
        ),
        (
            "mentions",
            "id, assertion_id, role, surface, span_start, span_end, resolved_to, method",
            "id",
        ),
        (
            "beliefs",
            "statement_id, valid_from, valid_to, status, confidence, support_json",
            "statement_id, valid_from",
        ),
    ] {
        out.push_str(table);
        out.push(':');
        out.push_str(&dump_table(conn, table, cols, order));
        out.push('\n');
    }
    out
}

fn make_declarations() -> Vec<Declaration> {
    vec![
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "Person".into(),
            },
            predicate: "employed_by".into(),
            object: DeclObject::Entity {
                surface: "Acme".into(),
                ty: "Organization".into(),
            },
            polarity: "affirm".into(),
            valid_from: 100,
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "Person".into(),
            },
            predicate: "employed_by".into(),
            object: DeclObject::Entity {
                surface: "Globex".into(),
                ty: "Organization".into(),
            },
            polarity: "affirm".into(),
            valid_from: 200,
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "Person".into(),
            },
            predicate: "works_on".into(),
            object: DeclObject::Entity {
                surface: "ProjectX".into(),
                ty: "Project".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Bob".into(),
                ty: "Person".into(),
            },
            predicate: "born_in".into(),
            object: DeclObject::Entity {
                surface: "Seoul".into(),
                ty: "Place".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
        Declaration::AddStatement {
            subject: EntityRef {
                surface: "Bob".into(),
                ty: "Person".into(),
            },
            predicate: "born_in".into(),
            object: DeclObject::Entity {
                surface: "Busan".into(),
                ty: "Place".into(),
            },
            polarity: "affirm".into(),
            valid_from: TIME_MIN.millis(),
            valid_to: TIME_MAX.millis(),
        },
    ]
}

#[tokio::test]
async fn reproject_is_byte_identical() {
    let dir = TempDir::new().unwrap();
    let config = oxibrain::BrainConfig::at(dir.path().to_str().unwrap());
    let brain = Brain::open(config).await.unwrap();
    let space = brain.ensure_space("test").await.unwrap();

    let decls = make_declarations();
    for decl in &decls {
        brain.declare(&space, decl.clone()).await.unwrap();
    }

    // Dump the projection after incremental application.
    let db_path = dir.path().join("brain.db");
    let conn_before = Connection::open(&db_path).unwrap();
    let before = dump_all(&conn_before);
    drop(conn_before);

    // Reproject.
    brain.reproject().await.unwrap();

    // Dump after reproject.
    let conn_after = Connection::open(&db_path).unwrap();
    let after = dump_all(&conn_after);

    assert_eq!(
        before, after,
        "projection must be byte-identical after reproject"
    );
}
