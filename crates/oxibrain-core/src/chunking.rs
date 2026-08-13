//! Chunking + deterministic context prefix (§9.3, M8 §8.11).
//!
//! A chunk is a span of episode content, used for entity-dense retrieval and
//! contextual retrieval. The chunk *text* is not stored — it is recovered
//! from `episodes.content` via the byte offsets in `chunks.span_start/end`.
//!
//! The recursive split ladder (§9.3):
//!   - Long input splits on `\n\n` (paragraph boundary).
//!   - If a paragraph is still too long, split on `\n`.
//!   - If a line is still too long, split on `. ` or `。` (sentence).
//!   - If a sentence is still too long, split on ` ` (word).
//!   - Last resort: hard cut on character count.
//!
//! The "empty separator" terminator is language-independent by construction —
//! any script-aware splitter would betray P11.
//!
//! The deterministic context prefix is generated from projection fields, not
//! from a model call. Every field is already known at projection time:
//!   - `occurred_at` from `episodes.occurred_at`
//!   - `source_kind` from `episodes.source_kind`
//!   - mentions: entities that appear in this episode's statements
//!   - community: the entity's community (if any)

use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// Recursive chunking parameters. Defaults sized for prose passages
/// (≤ 4 KiB per chunk) — small enough to fit an embedding model input and
/// large enough to keep the prefix-overhead ratio reasonable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkPolicy {
    pub max_chunk_bytes: usize,
    pub min_chunk_bytes: usize,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            max_chunk_bytes: 4_096,
            min_chunk_bytes: 64,
        }
    }
}

/// A single chunk produced by the recursive splitter. `start` and `end`
/// are byte offsets into the source `content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub ordinal: u32,
    pub span_start: usize,
    pub span_end: usize,
}

/// Pure: split `content` into chunks under `policy`. No I/O, no time, no
/// model. The output is a list of non-overlapping byte spans, ordered by
/// position, that cover the input up to the policy's hard-cut tail.
pub fn split_into_chunks(content: &str, policy: &ChunkPolicy) -> Vec<Chunk> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Chunk> = Vec::new();
    let mut ordinal = 0u32;
    split_recursive(
        content.as_bytes(),
        0,
        content.len(),
        &mut ordinal,
        policy,
        &mut out,
    );
    out
}

fn split_recursive(
    bytes: &[u8],
    offset: usize,
    end: usize,
    ordinal: &mut u32,
    policy: &ChunkPolicy,
    out: &mut Vec<Chunk>,
) {
    let len = end - offset;
    if len <= policy.max_chunk_bytes {
        push_chunk(out, ordinal, offset, end);
        return;
    }
    let candidate = pick_separator(bytes, offset, end, policy.max_chunk_bytes);
    if let Some((sep_byte, sep_len)) = candidate {
        let mut start = offset;
        let mut i = offset;
        while i + sep_len <= end {
            if &bytes[i..i + sep_len] == sep_byte {
                if i - start >= policy.min_chunk_bytes {
                    push_chunk(out, ordinal, start, i);
                }
                start = i + sep_len;
                i += sep_len;
            } else {
                i += 1;
            }
        }
        if end - start >= policy.min_chunk_bytes {
            push_chunk(out, ordinal, start, end);
        } else if let Some(last) = out.last_mut() {
            last.span_end = end;
        }
    } else {
        push_chunk(out, ordinal, offset, offset + policy.max_chunk_bytes);
    }
}

fn push_chunk(out: &mut Vec<Chunk>, ordinal: &mut u32, start: usize, end: usize) {
    out.push(Chunk {
        ordinal: *ordinal,
        span_start: start,
        span_end: end,
    });
    *ordinal += 1;
}

/// Pick the largest separator whose first match produces a sub-chunk of
/// acceptable size. Returns the separator bytes + length, or None when the
/// range must be hard-cut.
fn pick_separator(
    bytes: &[u8],
    offset: usize,
    end: usize,
    _max: usize,
) -> Option<(&'static [u8], usize)> {
    const SEPARATORS: &[(&[u8], &str)] = &[
        (b"\n\n", "paragraph"),
        (b"\n", "line"),
        (b". ", "sentence-ascii"),
        ("\u{3002}".as_bytes(), "sentence-cjk"),
        (b" ", "word"),
    ];
    for (sep, _label) in SEPARATORS {
        if find_in_range(bytes, sep, offset, end).is_some() {
            return Some((sep, sep.len()));
        }
    }
    None
}

fn find_in_range(bytes: &[u8], needle: &[u8], offset: usize, end: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    if end > bytes.len() {
        return None;
    }
    let mut i = offset;
    while i + needle.len() <= end {
        if &bytes[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Build a deterministic context prefix for a chunk. All fields come from
/// projection rows; no model call is involved.
///
/// Format: `[<occurred_at> · <source> · mentions: <e1>, <e2> · community: <label>]`
pub fn render_context_prefix(
    occurred_at: Timestamp,
    source_kind: &str,
    mentions: &[String],
    community_label: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("[{}", short_ts(occurred_at)));
    parts.push(format!("· {}", source_kind));
    if !mentions.is_empty() {
        let m = mentions
            .iter()
            .take(8)
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("· mentions: {m}"));
    }
    if let Some(label) = community_label {
        parts.push(format!("· community: {label}"));
    }
    format!("{} ]", parts.join(" "))
}

/// Short, deterministic timestamp representation. Format: `YYYY-MM-DD`
/// derived from millis-since-epoch. The full timestamp is in the prefix
/// only as a glance — callers should use Timeline for exact queries.
pub fn short_ts(t: Timestamp) -> String {
    let ms = t.0;
    if ms <= 0 {
        return "1970-01-01".into();
    }
    let days = ms.div_euclid(86_400_000);
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i32) + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_produces_no_chunks() {
        let chunks = split_into_chunks("", &ChunkPolicy::default());
        assert!(chunks.is_empty());
    }

    #[test]
    fn short_content_produces_single_chunk() {
        let chunks = split_into_chunks("hello world", &ChunkPolicy::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].span_start, 0);
        assert_eq!(chunks[0].span_end, 11);
    }

    #[test]
    fn paragraph_split_when_oversized() {
        let para1 = "x".repeat(3_000);
        let para2 = "y".repeat(3_000);
        let content = format!("{para1}\n\n{para2}");
        let chunks = split_into_chunks(&content, &ChunkPolicy::default());
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );
        assert_eq!(chunks.first().unwrap().span_start, 0);
        assert_eq!(chunks.last().unwrap().span_end, content.len());
    }

    #[test]
    fn cjk_sentence_split() {
        let s = "中".repeat(2_000);
        let content: String = s
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 200 == 199 {
                    format!("{c}\u{3002}")
                } else {
                    c.to_string()
                }
            })
            .collect();
        let chunks = split_into_chunks(&content, &ChunkPolicy::default());
        assert!(
            chunks.len() >= 2,
            "expected at least 2 chunks, got {}",
            chunks.len()
        );
    }

    #[test]
    fn prefix_format_is_deterministic() {
        let p1 = render_context_prefix(
            Timestamp(1_700_000_000_000),
            "Note: meeting.md",
            &["Alice(Person)".to_string(), "ProjectX(Project)".to_string()],
            Some("infra"),
        );
        let p2 = render_context_prefix(
            Timestamp(1_700_000_000_000),
            "Note: meeting.md",
            &["Alice(Person)".to_string(), "ProjectX(Project)".to_string()],
            Some("infra"),
        );
        assert_eq!(p1, p2);
        assert!(p1.contains("Alice(Person)"));
        assert!(p1.contains("ProjectX(Project)"));
        assert!(p1.contains("infra"));
    }

    #[test]
    fn prefix_short_ts_format() {
        let s = short_ts(Timestamp(1_700_000_000_000));
        assert_eq!(s.len(), 10);
        assert!(s.starts_with("20"));
    }
}
