//! JSONL export/import (DESIGN §12.5). Full-fidelity round-trip of durable tables.
//!
//! Format: one JSON object per line, `{"table":"<name>","row":{...}}`.
//! Tables are exported in dependency order. Indexes, beliefs, and other
//! derived state are NOT exported — they are rebuilt by `reproject()`.

use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;
use rusqlite::types::{Value as SqlValue, ValueRef};

/// Tables to export, in dependency order (parents before children).
const EXPORT_TABLES: &[&str] = &[
    "spaces",
    "episodes",
    "extractions",
    "summaries",
    "episode_links",
    "entities",
    "entity_keys",
    "entity_merges",
    "statements",
    "assertions",
    "mentions",
    "ingest_jobs",
    "extraction_failures",
    "audit_log",
    "redactions",
    "tokens",
    "predicates",
    "meta",
];

/// Export all durable tables to a JSONL string.
pub fn export_jsonl(conn: &Connection) -> Result<String, BrainError> {
    let mut output = String::new();
    for table in EXPORT_TABLES {
        let rows = export_table(conn, table)?;
        for row in rows {
            let entry = serde_json::json!({"table": table, "row": row});
            let line = serde_json::to_string(&entry)
                .map_err(|e| BrainError::Storage(format!("jsonl serialize: {e}")))?;
            output.push_str(&line);
            output.push('\n');
        }
    }
    Ok(output)
}

/// Read all rows from a table as JSON values.
fn export_table(conn: &Connection, table: &str) -> Result<Vec<serde_json::Value>, BrainError> {
    let col_names = get_column_names(conn, table)?;
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let mut result = Vec::new();
    let mut rows = stmt.query([]).map_err(sql_err)?;
    while let Some(row) = rows.next().map_err(sql_err)? {
        let mut obj = serde_json::Map::new();
        for (i, col_name) in col_names.iter().enumerate() {
            let val = row.get_ref(i).map_err(sql_err)?;
            obj.insert(col_name.clone(), value_ref_to_json(val));
        }
        result.push(serde_json::Value::Object(obj));
    }
    Ok(result)
}

fn value_ref_to_json(val: ValueRef) -> serde_json::Value {
    match val {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(n) => serde_json::json!(n),
        ValueRef::Real(f) => serde_json::json!(f),
        ValueRef::Text(bytes) => {
            let s = std::str::from_utf8(bytes).unwrap_or("");
            serde_json::Value::String(s.to_string())
        }
        ValueRef::Blob(bytes) => serde_json::Value::String(format!("hex:{}", hex::encode(bytes))),
    }
}

/// Get column names for a table.
fn get_column_names(conn: &Connection, table: &str) -> Result<Vec<String>, BrainError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

/// Import JSONL into the store. Assumes the store is fresh (tables empty).
pub fn import_jsonl(conn: &Connection, jsonl: &str) -> Result<(), BrainError> {
    conn.execute("PRAGMA foreign_keys=OFF", [])
        .map_err(sql_err)?;

    for line in jsonl.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| BrainError::Storage(format!("jsonl parse: {e}")))?;
        let table = entry
            .get("table")
            .and_then(|v| v.as_str())
            .ok_or_else(|| BrainError::Storage("missing 'table' field".into()))?;
        let row = entry
            .get("row")
            .ok_or_else(|| BrainError::Storage("missing 'row' field".into()))?;
        insert_row(conn, table, row)?;
    }

    conn.execute("PRAGMA foreign_keys=ON", [])
        .map_err(sql_err)?;
    Ok(())
}

fn insert_row(conn: &Connection, table: &str, row: &serde_json::Value) -> Result<(), BrainError> {
    let obj = row
        .as_object()
        .ok_or_else(|| BrainError::Storage(format!("expected JSON object for table {table}")))?;

    let col_names: Vec<&String> = obj.keys().collect();
    let placeholders = std::iter::repeat("?")
        .take(col_names.len())
        .collect::<Vec<_>>()
        .join(",");
    let cols = col_names
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join(",");

    let sql = format!("INSERT OR IGNORE INTO {table} ({cols}) VALUES ({placeholders})");
    let params: Vec<SqlValue> = col_names.iter().map(|c| json_to_sql(&obj[*c])).collect();
    let param_refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();

    conn.execute(&sql, param_refs.as_slice())
        .map_err(|e| BrainError::Storage(format!("insert into {table}: {e}")))?;
    Ok(())
}

fn json_to_sql(val: &serde_json::Value) -> SqlValue {
    match val {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(*b as i64),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                SqlValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => {
            if let Some(hex_part) = s.strip_prefix("hex:") {
                if let Ok(bytes) = hex::decode(hex_part) {
                    return SqlValue::Blob(bytes);
                }
            }
            SqlValue::Text(s.clone())
        }
        other => SqlValue::Text(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration;
    use crate::project::{DeclObject, Declaration, EntityRef};
    use oxibrain_ports::Timestamp;
    use rusqlite::Connection;
    use std::collections::HashMap;

    fn fresh_db_with_data() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        migration::run(&conn).expect("migrate");
        let sid = crate::ledger::create_space(&conn, "personal", Timestamp::from_millis(0))
            .expect("space");
        let decl = Declaration::AddStatement {
            subject: EntityRef {
                surface: "Alice".into(),
                ty: "person".into(),
            },
            predicate: "employed_by".into(),
            object: DeclObject::Entity {
                surface: "Acme".into(),
                ty: "organization".into(),
            },
            polarity: "affirm".into(),
            valid_from: 0,
            valid_to: oxibrain_ports::TIME_MAX.millis(),
        };
        crate::project::project_declaration(&conn, &sid, &decl, Timestamp::from_millis(1000))
            .expect("declare");
        conn
    }

    fn count_all(conn: &Connection) -> HashMap<String, i64> {
        let mut map = HashMap::new();
        for table in EXPORT_TABLES {
            if let Ok(n) =
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            {
                map.insert(table.to_string(), n);
            }
        }
        map
    }

    #[test]
    fn export_import_round_trip() {
        let conn = fresh_db_with_data();
        let counts_before = count_all(&conn);
        let jsonl = export_jsonl(&conn).unwrap();
        assert!(!jsonl.is_empty());

        let conn2 = Connection::open_in_memory().expect("open");
        migration::run(&conn2).expect("migrate");
        import_jsonl(&conn2, &jsonl).unwrap();

        let counts_after = count_all(&conn2);
        for (table, before) in &counts_before {
            let after = counts_after.get(table).copied().unwrap_or(0);
            assert_eq!(*before, after, "table {table}: {before} != {after}");
        }
    }

    #[test]
    fn export_includes_all_tables() {
        let conn = fresh_db_with_data();
        let jsonl = export_jsonl(&conn).unwrap();
        for table in &[
            "spaces",
            "episodes",
            "entities",
            "statements",
            "assertions",
            "predicates",
        ] {
            assert!(
                jsonl.contains(&format!("\"table\":\"{table}\"")),
                "missing {table}"
            );
        }
    }

    #[test]
    fn round_trip_preserves_beliefs_after_reproject() {
        let conn = fresh_db_with_data();
        let jsonl = export_jsonl(&conn).unwrap();

        let conn2 = Connection::open_in_memory().expect("open");
        migration::run(&conn2).expect("migrate");
        import_jsonl(&conn2, &jsonl).unwrap();
        crate::reproject::reproject(&conn2).unwrap();

        let beliefs_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
            .unwrap();
        let beliefs_after: i64 = conn2
            .query_row("SELECT COUNT(*) FROM beliefs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(beliefs_before, beliefs_after);
    }
}
