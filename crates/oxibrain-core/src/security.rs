//! Security domain types (DESIGN §11). Capabilities, scopes, tokens, redaction.
//!
//! These are pure types — the store enforces them, the facade checks them.
//! Token *secrets* are operational state (random, not content-derived) and are
//! therefore exempt from the P1 reprojection contract.

use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Capability + Scope
// ---------------------------------------------------------------------------

/// What a token holder may do. DESIGN §11.2.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// search, recall, get_entity, traverse, timeline, why
    Read,
    /// declare, retract, merge_entities, review_merges
    Write,
    /// ingest, remember
    Ingest,
    /// sampling LlmPort — off by default (§12.3)
    Sample,
    /// token issue/revoke, predicate add, config change
    Admin,
    /// redact — separate capability on purpose (§12.2)
    Redact,
}

impl Capability {
    /// Parse a comma-separated capability string: "read,write,ingest".
    pub fn parse_set(s: &str) -> CapabilitySet {
        let mut set = CapabilitySet::new();
        for part in s.split(',') {
            let trimmed = part.trim().to_ascii_lowercase();
            let cap = match trimmed.as_str() {
                "read" | "query" => Some(Capability::Read),
                "write" | "declare" => Some(Capability::Write),
                "ingest" => Some(Capability::Ingest),
                "sample" => Some(Capability::Sample),
                "admin" => Some(Capability::Admin),
                "redact" => Some(Capability::Redact),
                _ => None,
            };
            if let Some(c) = cap {
                set.insert(c);
            }
        }
        set
    }

    /// Render as lowercase string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::Read => "read",
            Capability::Write => "write",
            Capability::Ingest => "ingest",
            Capability::Sample => "sample",
            Capability::Admin => "admin",
            Capability::Redact => "redact",
        }
    }
}

/// A bit set of capabilities.
pub type CapabilitySet = BTreeSet<Capability>;

/// Authorization scope carried by a token. DESIGN §11.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    /// Space ids this scope grants access to.
    pub spaces: Vec<String>,
    /// Capabilities granted.
    pub caps: CapabilitySet,
    /// Optional predicate allow-list (e.g. hide `health_*`).
    pub predicate_filter: Option<Vec<String>>,
    /// Optional entity-type allow-list.
    pub entity_type_filter: Option<Vec<String>>,
    /// When the token expires.
    pub expires_at: Option<Timestamp>,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            spaces: Vec::new(),
            caps: CapabilitySet::new(),
            predicate_filter: None,
            entity_type_filter: None,
            expires_at: None,
        }
    }
}

impl Scope {
    /// Check if a capability is granted for a space and not expired.
    pub fn permits(&self, cap: Capability, space: &str, now: Timestamp) -> bool {
        self.caps.contains(&cap)
            && self.spaces.iter().any(|s| s == space)
            && self.expires_at.map_or(true, |exp| now < exp)
    }

    /// Check if a predicate passes the filter (or there is no filter).
    pub fn permits_predicate(&self, predicate: &str) -> bool {
        self.predicate_filter
            .as_ref()
            .map_or(true, |filter| filter.iter().any(|p| p == predicate))
    }
}

// ---------------------------------------------------------------------------
// Token info (public metadata; the secret is never stored)
// ---------------------------------------------------------------------------

/// Public metadata for a token. The secret itself is shown once at issuance
/// and stored only as a SHA-256 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    /// Content-derived from (token_hash, issued_at).
    pub id: String,
    /// The scope this token grants.
    pub scope: Scope,
    /// When the token was issued.
    pub issued_at: Timestamp,
    /// Who issued the token (admin token id or "cli").
    pub issued_by: String,
    /// When the token was revoked, if applicable.
    pub revoked_at: Option<Timestamp>,
    /// Human-readable hint.
    pub label: Option<String>,
}

// ---------------------------------------------------------------------------
// Redaction types
// ---------------------------------------------------------------------------

/// What to redact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RedactTarget {
    /// Redact a single episode and everything extracted from it.
    Episode {
        id: String,
    },
    /// Redact all assertions about an entity across all episodes.
    Entity {
        space: String,
        entity_id: String,
    },
    /// Redact assertions for a specific predicate on an entity.
    PredicateScoped {
        space: String,
        entity_id: String,
        predicate: String,
    },
}

/// The set of objects that will be affected by a redaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RedactionClosure {
    pub episodes: Vec<String>,
    pub assertions: Vec<String>,
    pub statements: Vec<String>,
    pub mentions: Vec<String>,
    pub extractions: Vec<String>,
    pub summaries: Vec<String>,
}

/// What a redaction actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionResult {
    pub closure: RedactionClosure,
    pub beliefs_refolded: usize,
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// A single audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: Timestamp,
    pub actor: String,
    pub scope: Option<String>,
    pub operation: String,
    pub target: Option<String>,
    pub detail_json: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxibrain_ports::Timestamp;

    fn now() -> Timestamp {
        Timestamp::from_millis(1_000_000)
    }

    #[test]
    fn capability_parse_set() {
        let set = Capability::parse_set("read, write, ingest");
        assert!(set.contains(&Capability::Read));
        assert!(set.contains(&Capability::Write));
        assert!(set.contains(&Capability::Ingest));
        assert!(!set.contains(&Capability::Admin));
    }

    #[test]
    fn capability_parse_unknown_ignored() {
        let set = Capability::parse_set("read, bogus, write");
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn scope_permits_grants() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read,write"),
            ..Default::default()
        };
        assert!(scope.permits(Capability::Read, "work", now()));
        assert!(scope.permits(Capability::Write, "work", now()));
    }

    #[test]
    fn scope_permits_denies_wrong_space() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read"),
            ..Default::default()
        };
        assert!(!scope.permits(Capability::Read, "personal", now()));
    }

    #[test]
    fn scope_permits_denies_missing_cap() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read"),
            ..Default::default()
        };
        assert!(!scope.permits(Capability::Write, "work", now()));
    }

    #[test]
    fn scope_permits_denies_expired() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read"),
            expires_at: Some(Timestamp::from_millis(500)),
            ..Default::default()
        };
        assert!(!scope.permits(Capability::Read, "work", now()));
        // Before expiry is fine.
        assert!(scope.permits(Capability::Read, "work", Timestamp::from_millis(400)));
    }

    #[test]
    fn scope_default_permits_nothing() {
        let scope = Scope::default();
        assert!(!scope.permits(Capability::Read, "work", now()));
    }

    #[test]
    fn scope_predicate_filter() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read"),
            predicate_filter: Some(vec!["works_on".into()]),
            ..Default::default()
        };
        assert!(scope.permits_predicate("works_on"));
        assert!(!scope.permits_predicate("salary"));
    }

    #[test]
    fn scope_no_predicate_filter_allows_all() {
        let scope = Scope {
            spaces: vec!["work".into()],
            caps: Capability::parse_set("read"),
            ..Default::default()
        };
        assert!(scope.permits_predicate("anything"));
    }
}
