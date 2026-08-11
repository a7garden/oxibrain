//! Content-derived ids. Every projection id is derived from content, not random,
//! so reprojection is byte-identical (DESIGN.md §5.6, P1).

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
}
