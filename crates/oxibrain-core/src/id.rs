//! Content-derived ids. Every projection id is derived from content, not random,
//! so reprojection is byte-identical (ARCHITECTURE.md §5.6, P1).

use crate::canonical;
use crate::types::{ContentHash, SourceRef};
use blake3::Hasher;
use oxibrain_ports::Timestamp;

/// Hex string id (TEXT PRIMARY KEY in SQLite).
pub type Id = String;

fn hex(bytes: [u8; 32]) -> String {
    hex::encode(bytes)
}

/// BLAKE3 over canonical JSON of the fields.
fn derive(fields: &[(&str, &str)]) -> [u8; 32] {
    let mut h = Hasher::new();
    for (k, v) in fields {
        h.update(k.as_bytes());
        h.update(&[0u8]); // key/value separator
        h.update(v.as_bytes());
        h.update(&[0u8]);
    }
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    out
}

/// `EpisodeId = blake3(space, content_hash, source_ref, occurred_at)`
pub fn episode_id(
    space: &str,
    content_hash: &ContentHash,
    source: &SourceRef,
    occurred_at: Timestamp,
) -> Id {
    // source serialized canonically so the id is stable
    let source_json = serde_json::to_value(source)
        .ok()
        .map(|v| canonical::canonical_json_value(&v))
        .expect("source serializable");
    hex(derive(&[
        ("space", space),
        ("content_hash", &content_hash.hex()),
        ("source_ref", &source_json),
        ("occurred_at", &occurred_at.millis().to_string()),
    ]))
}

/// Content hash over normalized content. M0 normalization: NFKC + CRLF→LF + trim trailing ws.
pub fn content_hash(content: &str) -> ContentHash {
    let normalized = normalize_content(content);
    let mut h = Hasher::new();
    h.update(normalized.as_bytes());
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    ContentHash(out)
}

/// NFKC unicode normalization + CR/CRLF→LF + trailing-whitespace trim.
pub fn normalize_content(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfkc()
        .collect::<String>()
        .trim_end()
        .to_string()
}
use crate::knowledge::{Polarity, claim_repr, object_repr};

/// `EntityId = blake3(space, entity_type, first_episode_id, first_span_start)`
pub fn entity_id(
    space: &str,
    entity_type: &str,
    first_episode_id: &str,
    first_span_start: u32,
) -> Id {
    hex(derive(&[
        ("space", space),
        ("entity_type", entity_type),
        ("first_episode_id", first_episode_id),
        ("first_span_start", &first_span_start.to_string()),
    ]))
}

/// `EntityKeyId = blake3(entity_id, normalized, ty)`
pub fn entity_key_id(entity_id: &str, normalized: &str, ty: &str) -> Id {
    hex(derive(&[
        ("entity_id", entity_id),
        ("normalized", normalized),
        ("ty", ty),
    ]))
}

/// `StatementId = blake3(space, subject, predicate, object_repr)`
pub fn statement_id(
    space: &str,
    subject: &str,
    predicate: &str,
    object: &crate::knowledge::Object,
) -> Id {
    let repr = object_repr(object);
    hex(derive(&[
        ("space", space),
        ("subject", subject),
        ("predicate", predicate),
        ("object_repr", &repr),
    ]))
}

/// `AssertionId = blake3(statement_id, episode_id, extractor_id, claim_repr)`
pub fn assertion_id(
    statement_id: &str,
    episode_id: &str,
    extractor_id: &str,
    polarity: Polarity,
    claimed_from: Timestamp,
    claimed_to: Timestamp,
    confidence: f32,
) -> Id {
    let repr = claim_repr(polarity, claimed_from, claimed_to, confidence);
    hex(derive(&[
        ("statement_id", statement_id),
        ("episode_id", episode_id),
        ("extractor_id", extractor_id),
        ("claim_repr", &repr),
    ]))
}

/// `MentionId = blake3(assertion_id, role, span_start)`
pub fn mention_id(assertion_id: &str, role: &str, span_start: u32) -> Id {
    hex(derive(&[
        ("assertion_id", assertion_id),
        ("role", role),
        ("span_start", &span_start.to_string()),
    ]))
}
/// `ChunkId = blake3(episode_id, ordinal)` (§5.7, M8 §8.11).
pub fn chunk_id(episode_id: &str, ordinal: u32) -> Id {
    hex(derive(&[
        ("episode_id", episode_id),
        ("ordinal", &ordinal.to_string()),
    ]))
}


/// EntityMerge id = blake3(loser, winner, provenance)
pub fn entity_merge_id(loser: &str, winner: &str, provenance: &str) -> Id {
    hex(derive(&[
        ("loser", loser),
        ("winner", winner),
        ("provenance", provenance),
    ]))
}

/// Token id = blake3(token_hash, issued_at). Operational state (random nonce
/// in the hash), not part of the reprojection contract.
pub fn token_id(token_hash: &str, issued_at: Timestamp) -> Id {
    hex(derive(&[
        ("token_hash", token_hash),
        ("issued_at", &issued_at.millis().to_string()),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn episode_id_is_stable() {
        let ch = content_hash("hello world");
        let src = SourceRef::Note {
            path: "a.md".into(),
        };
        let id1 = episode_id("s1", &ch, &src, Timestamp(1000));
        let id2 = episode_id("s1", &ch, &src, Timestamp(1000));
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_space_different_id() {
        let ch = content_hash("hello world");
        let src = SourceRef::Note {
            path: "a.md".into(),
        };
        let id1 = episode_id("s1", &ch, &src, Timestamp(1000));
        let id2 = episode_id("s2", &ch, &src, Timestamp(1000));
        assert_ne!(id1, id2);
    }

    proptest! {
        #[test]
        fn content_hash_deterministic(s in ".{0,200}") {
            let h1 = content_hash(&s).hex();
            let h2 = content_hash(&s).hex();
            prop_assert_eq!(h1, h2);
        }

        #[test]
        fn normalization_is_stable(s in "[a-z \n\r]{0,80}") {
            // CRLF and trailing ws collapse; NFKC stable.
            let h1 = content_hash(&s).hex();
            let n = normalize_content(&s);
            let h2 = content_hash(&n).hex();
            prop_assert_eq!(h1, h2, "normalize must be idempotent-ish for hashing");
        }
    }
    use crate::knowledge::{Object, Polarity, TypedValue};
    use oxibrain_ports::{TIME_MAX, TIME_MIN};

    #[test]
    fn entity_id_stable() {
        let id1 = entity_id("s1", "Person", "ep1", 0);
        let id2 = entity_id("s1", "Person", "ep1", 0);
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_object_different_statement() {
        let s1 = statement_id("s1", "e1", "works_on", &Object::Entity("e2".into()));
        let s2 = statement_id("s1", "e1", "works_on", &Object::Entity("e3".into()));
        assert_ne!(s1, s2);
    }

    #[test]
    fn literal_object_statement_stable() {
        let o = Object::Literal(TypedValue::Text("hello".into()));
        let s1 = statement_id("s1", "e1", "full_name", &o);
        let s2 = statement_id("s1", "e1", "full_name", &o);
        assert_eq!(s1, s2);
    }

    #[test]
    fn assertion_id_stable() {
        let a1 = assertion_id(
            "st1",
            "ep1",
            "ext1",
            Polarity::Affirm,
            TIME_MIN,
            TIME_MAX,
            1.0,
        );
        let a2 = assertion_id(
            "st1",
            "ep1",
            "ext1",
            Polarity::Affirm,
            TIME_MIN,
            TIME_MAX,
            1.0,
        );
        assert_eq!(a1, a2);
    }

    #[test]
    fn different_polarity_different_assertion() {
        let a1 = assertion_id(
            "st1",
            "ep1",
            "ext1",
            Polarity::Affirm,
            TIME_MIN,
            TIME_MAX,
            1.0,
        );
        let a2 = assertion_id(
            "st1",
            "ep1",
            "ext1",
            Polarity::Deny,
            TIME_MIN,
            TIME_MAX,
            1.0,
        );
        assert_ne!(a1, a2);
    }

    #[test]
    fn mention_id_stable() {
        let m1 = mention_id("a1", "subject", 42);
        let m2 = mention_id("a1", "subject", 42);
        assert_eq!(m1, m2);
    }
}
