//! Predicate registry persistence (DESIGN §5.5). Seeds core/v1 from Rust const
//! array, loads PredicateDefs from the predicates table.

use crate::sql_err;
use oxibrain_core::registry::{CORE_V1_MAJOR, CORE_V1_MINOR, PredicateDef, core_v1};
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};
use std::collections::HashMap;

/// Seed the core/v1 ontology into the predicates table. Idempotent (INSERT OR IGNORE).
pub fn seed_core_v1(conn: &Connection) -> Result<(), BrainError> {
    for def in core_v1() {
        let json = serde_json::to_string(def).expect("predicate def serializable");
        let canon =
            oxibrain_core::canonical_json_value(&serde_json::from_str(&json).expect("valid json"));
        conn.execute(
            "INSERT OR IGNORE INTO predicates (name, major_version, minor_version, def_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![def.name, CORE_V1_MAJOR, CORE_V1_MINOR, canon],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Load a single predicate by name.
pub fn load_predicate(conn: &Connection, name: &str) -> Result<Option<PredicateDef>, BrainError> {
    let row = conn.query_row(
        "SELECT def_json FROM predicates WHERE name = ?1",
        params![name],
        |r| r.get::<_, String>(0),
    );
    match row {
        Ok(json) => {
            let def: PredicateDef = serde_json::from_str(&json)
                .map_err(|e| BrainError::Storage(format!("predicate parse: {e}")))?;
            Ok(Some(def))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(sql_err(e)),
    }
}

/// Load all predicates into a map keyed by name.
pub fn load_all_predicates(conn: &Connection) -> Result<HashMap<String, PredicateDef>, BrainError> {
    let mut stmt = conn
        .prepare("SELECT name, def_json FROM predicates")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            let name: String = r.get(0)?;
            let json: String = r.get(1)?;
            Ok((name, json))
        })
        .map_err(sql_err)?;
    let mut result = HashMap::new();
    for row in rows {
        let (name, json) = row.map_err(sql_err)?;
        let def: PredicateDef = serde_json::from_str(&json)
            .map_err(|e| BrainError::Storage(format!("predicate parse: {e}")))?;
        result.insert(name, def);
    }
    Ok(result)
}
