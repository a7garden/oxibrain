//! Security store: token CRUD and audit log (DESIGN §11.2, §11.4).
//!
//! Tokens are operational state (random, not content-derived) and are NOT part
//! of the reprojection contract. The `tokens` table is created in migration v4.
//! The `audit_log` table exists from v1.

use crate::sql_err;
use oxibrain_core::security::{Scope, TokenInfo};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

// ── Token generation ────────────────────────────────────────────────────

/// Generate a cryptographically secure 32-byte token secret, hex-encoded
/// with `obt_` prefix. Uses `getrandom` (OS CSPRNG) — never a non-cryptographic
/// hash.
fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS entropy source unavailable");
    format!("obt_{}", hex::encode(bytes))
}

/// SHA-256 hash of the token secret, hex-encoded.
fn hash_secret(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
}

// ── Token CRUD ──────────────────────────────────────────────────────────

/// Issue a token. Returns (TokenInfo, secret). The secret is shown once.
pub fn issue_token(
    conn: &Connection,
    scope: &Scope,
    issued_by: &str,
    label: Option<&str>,
    now: Timestamp,
) -> Result<(TokenInfo, String), BrainError> {
    let secret = generate_secret();
    let hash = hash_secret(&secret);
    let id = oxibrain_core::id::token_id(&hash, now);
    let scope_json =
        serde_json::to_string(scope).map_err(|e| BrainError::Storage(e.to_string()))?;

    conn.execute(
        "INSERT INTO tokens (id, token_hash, scope_json, issued_at, issued_by, revoked_at, label)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
        params![id, hash, scope_json, now.millis(), issued_by, label],
    )
    .map_err(sql_err)?;

    let info = TokenInfo {
        id,
        scope: scope.clone(),
        issued_at: now,
        issued_by: issued_by.to_string(),
        revoked_at: None,
        label: label.map(String::from),
    };
    Ok((info, secret))
}

/// Verify a token by its secret. Returns the scope if valid and not
/// expired/revoked.
pub fn verify_token(
    conn: &Connection,
    secret: &str,
    now: Timestamp,
) -> Result<Option<Scope>, BrainError> {
    let hash = hash_secret(secret);
    let row = conn
        .query_row(
            "SELECT scope_json, revoked_at FROM tokens WHERE token_hash = ?1",
            params![hash],
            |r| {
                let scope_json: String = r.get(0)?;
                let revoked_at: Option<i64> = r.get(1)?;
                Ok((scope_json, revoked_at))
            },
        )
        .optional()
        .map_err(sql_err)?;

    match row {
        None => Ok(None),
        Some((scope_json, revoked_at)) => {
            if revoked_at.is_some() {
                return Ok(None);
            }
            let scope: Scope = serde_json::from_str(&scope_json)
                .map_err(|e| BrainError::Storage(format!("scope parse: {e}")))?;
            if let Some(exp) = scope.expires_at {
                if now >= exp {
                    return Ok(None);
                }
            }
            Ok(Some(scope))
        }
    }
}

/// Revoke a token by id.
pub fn revoke_token(conn: &Connection, id: &str, now: Timestamp) -> Result<(), BrainError> {
    let n = conn
        .execute(
            "UPDATE tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
            params![now.millis(), id],
        )
        .map_err(sql_err)?;
    if n == 0 {
        return Err(BrainError::NotFound(format!(
            "token {id} (or already revoked)"
        )));
    }
    Ok(())
}

/// List all tokens (active and revoked).
pub fn list_tokens(conn: &Connection) -> Result<Vec<TokenInfo>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, scope_json, issued_at, issued_by, revoked_at, label
             FROM tokens ORDER BY issued_at",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            let id: String = r.get(0)?;
            let scope_json: String = r.get(1)?;
            let issued_at: i64 = r.get(2)?;
            let issued_by: String = r.get(3)?;
            let revoked_at: Option<i64> = r.get(4)?;
            let label: Option<String> = r.get(5)?;
            let scope: Scope = serde_json::from_str(&scope_json).expect("valid scope json in db");
            Ok(TokenInfo {
                id,
                scope,
                issued_at: Timestamp::from_millis(issued_at),
                issued_by,
                revoked_at: revoked_at.map(Timestamp::from_millis),
                label,
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

// ── Audit log ───────────────────────────────────────────────────────────

/// Write an audit entry. Call BEFORE acting (§11.5).
pub fn write_audit(
    conn: &Connection,
    actor: &str,
    scope: Option<&str>,
    operation: &str,
    target: Option<&str>,
    detail_json: Option<&str>,
    now: Timestamp,
) -> Result<(), BrainError> {
    conn.execute(
        "INSERT INTO audit_log (ts, actor, scope, operation, target, detail_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now.millis(), actor, scope, operation, target, detail_json],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// List recent audit entries, most recent first.
pub fn list_audit(conn: &Connection, limit: Option<i64>) -> Result<Vec<AuditRow>, BrainError> {
    let limit = limit.unwrap_or(100);
    let mut stmt = conn
        .prepare(
            "SELECT id, ts, actor, scope, operation, target, detail_json
             FROM audit_log ORDER BY ts DESC LIMIT ?1",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![limit], |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                ts: Timestamp::from_millis(r.get::<_, i64>(1)?),
                actor: r.get(2)?,
                scope: r.get(3)?,
                operation: r.get(4)?,
                target: r.get(5)?,
                detail_json: r.get(6)?,
            })
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

/// A row from the audit log.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub id: i64,
    pub ts: Timestamp,
    pub actor: String,
    pub scope: Option<String>,
    pub operation: String,
    pub target: Option<String>,
    pub detail_json: Option<String>,
}

/// Read all redaction targets from the `redactions` table, ordered by time.
/// Returns (target_json, reason, redacted_at). Used by reproject to replay
/// redactions with the correct fold timestamp.
pub fn list_redactions(conn: &Connection) -> Result<Vec<(String, String, Timestamp)>, BrainError> {
    let mut stmt = conn
        .prepare("SELECT target_json, reason, redacted_at FROM redactions ORDER BY redacted_at ASC")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                Timestamp::from_millis(r.get::<_, i64>(2)?),
            ))
        })
        .map_err(sql_err)?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row.map_err(sql_err)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration;
    use oxibrain_core::security::{Capability, CapabilitySet, Scope};
    use rusqlite::Connection;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        migration::run(&conn).expect("migrate");
        conn
    }

    #[test]
    fn test_generate_secret_is_cryptographically_unique() {
        let s1 = generate_secret();
        let s2 = generate_secret();
        assert!(s1.starts_with("obt_"));
        assert!(s2.starts_with("obt_"));
        assert_ne!(s1, s2, "two consecutive secrets must differ");
        // obt_ prefix (4) + 32 bytes hex (64) = 68 chars
        assert_eq!(s1.len(), 68);
    }

    fn sample_scope() -> Scope {
        let mut caps = CapabilitySet::new();
        caps.insert(Capability::Read);
        caps.insert(Capability::Write);
        Scope {
            spaces: vec!["personal".into()],
            caps,
            ..Default::default()
        }
    }

    #[test]
    fn token_issue_verify_cycle() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let scope = sample_scope();

        let (_info, secret) = issue_token(&conn, &scope, "cli", Some("test"), now).unwrap();
        assert!(!secret.is_empty());
        assert!(secret.starts_with("obt_"));

        // Verify with correct secret.
        let verified = verify_token(&conn, &secret, now).unwrap();
        assert!(verified.is_some());
        let verified_scope = verified.unwrap();
        assert!(verified_scope.caps.contains(&Capability::Read));

        // Verify with wrong secret.
        let wrong = verify_token(&conn, "obt_wrong", now).unwrap();
        assert!(wrong.is_none());
    }

    #[test]
    fn token_revoke_blocks_verify() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let scope = sample_scope();

        let (info, secret) = issue_token(&conn, &scope, "cli", None, now).unwrap();
        assert!(verify_token(&conn, &secret, now).unwrap().is_some());

        revoke_token(&conn, &info.id, now).unwrap();
        assert!(verify_token(&conn, &secret, now).unwrap().is_none());
    }

    #[test]
    fn token_expiry() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let scope = Scope {
            spaces: vec!["personal".into()],
            caps: Capability::parse_set("read"),
            expires_at: Some(Timestamp::from_millis(2000)),
            ..Default::default()
        };

        let (_info, secret) = issue_token(&conn, &scope, "cli", None, now).unwrap();

        // Before expiry: OK.
        assert!(
            verify_token(&conn, &secret, Timestamp::from_millis(1500))
                .unwrap()
                .is_some()
        );
        // At expiry: blocked.
        assert!(
            verify_token(&conn, &secret, Timestamp::from_millis(2000))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn audit_write_and_list() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        write_audit(
            &conn,
            "cli",
            Some("personal"),
            "declare",
            Some("stmt1"),
            None,
            now,
        )
        .unwrap();
        write_audit(&conn, "admin", None, "token_issue", None, Some("{}"), now).unwrap();

        let entries = list_audit(&conn, None).unwrap();
        assert_eq!(entries.len(), 2);
        // Most recent first (same ts, but insertion order preserved by DESC).
    }

    #[test]
    fn token_uniqueness() {
        let conn = fresh_db();
        let now = Timestamp::from_millis(1000);
        let scope = sample_scope();

        let (_, s1) = issue_token(&conn, &scope, "cli", None, now).unwrap();
        let (_, s2) = issue_token(&conn, &scope, "cli", None, now).unwrap();
        // Two tokens issued at the same timestamp should have different secrets
        // (random entropy makes collisions astronomically unlikely).
        assert_ne!(s1, s2);
    }
}
