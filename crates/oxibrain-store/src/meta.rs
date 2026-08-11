use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, params};

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>, BrainError> {
    let v = match conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
        r.get(0)
    }) {
        Ok(s) => Some(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(sql_err(e)),
    };
    Ok(v)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<(), BrainError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn ensure_schema_versions(conn: &Connection) -> Result<(), BrainError> {
    use crate::schema::{LEDGER_SCHEMA_VERSION, PROJECTION_VERSION};
    if get(conn, "ledger_schema_version")?.is_none() {
        set(
            conn,
            "ledger_schema_version",
            &LEDGER_SCHEMA_VERSION.to_string(),
        )?;
    }
    if get(conn, "projection_version")?.is_none() {
        set(conn, "projection_version", &PROJECTION_VERSION.to_string())?;
    }
    Ok(())
}
