//! Chunking + deterministic context prefix (§9.3, M8 §8.11).
//!
//! A chunk is a span of episode content, used for entity-dense retrieval and
//! contextual retrieval. The chunk *text* is not stored — it is recovered
//! from `episodes.content` via the byte offsets in `chunks.span_start/end`.
//!
//! Paragraph boundaries are retained when they fit. Oversized blocks use
//! scalar-aligned byte windows with complete coverage and guaranteed progress.
//! No script classification, stopword list, or language-specific splitter is used.
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
/// position, that cover every byte of the input.
pub fn split_into_chunks(content: &str, policy: &ChunkPolicy) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut offset = 0;
    // Preserve complete paragraph/tool-step blocks when they fit. Oversized
    // blocks use scalar-aligned windows; separators remain inside coverage.
    for block in content.split_inclusive("\n\n") {
        for mut c in source_windows(block, policy.max_chunk_bytes, 0) {
            c.span_start += offset;
            c.span_end += offset;
            c.ordinal = chunks.len() as u32;
            chunks.push(c);
        }
        offset += block.len();
    }
    chunks
}

/// Full UTF-8 coverage, with overlap confined to the same source occurrence.
/// A limit below the size of one scalar still consumes that scalar to progress.
pub fn source_windows(content: &str, maximum: usize, overlap: usize) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut covered_end = 0;
    let maximum = maximum.max(1);
    while start < content.len() {
        let mut end = start.saturating_add(maximum).min(content.len());
        while end > start && !content.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            end = start
                + content[start..]
                    .chars()
                    .next()
                    .expect("nonempty tail")
                    .len_utf8();
        }
        if end <= covered_end {
            start = covered_end;
            continue;
        }
        out.push(Chunk {
            ordinal: out.len() as u32,
            span_start: start,
            span_end: end,
        });
        covered_end = end;
        if end == content.len() {
            break;
        }
        let mut next = end.saturating_sub(overlap.min(maximum / 2));
        while next < end && !content.is_char_boundary(next) {
            next += 1;
        }
        start = if next > start { next } else { end };
    }
    out
}

/// Split source text on complete paragraph blocks first, grouping adjacent
/// blocks up to `maximum`. Oversized paragraphs prefer complete newline-ended
/// lines; only a line larger than the limit falls back to overlapping UTF-8
/// windows. Spans remain exact byte offsets into the original source.
pub fn source_paragraph_windows(content: &str, maximum: usize, overlap: usize) -> Vec<Chunk> {
    let maximum = maximum.max(1);
    let mut out = Vec::new();
    let mut grouped_start = None;
    let mut grouped_end = 0;
    let mut offset = 0;

    for paragraph in content.split_inclusive("\n\n") {
        let start = offset;
        let end = start + paragraph.len();
        offset = end;

        if paragraph.len() <= maximum {
            if let Some(group_start) = grouped_start {
                if end - group_start <= maximum {
                    grouped_end = end;
                    continue;
                }
                push_chunk(&mut out, group_start, grouped_end);
            }
            grouped_start = Some(start);
            grouped_end = end;
            continue;
        }

        if let Some(group_start) = grouped_start.take() {
            push_chunk(&mut out, group_start, grouped_end);
        }
        append_oversized_paragraph(paragraph, start, maximum, overlap, &mut out);
    }

    if let Some(group_start) = grouped_start {
        push_chunk(&mut out, group_start, grouped_end);
    }
    out
}

fn append_oversized_paragraph(
    paragraph: &str,
    base: usize,
    maximum: usize,
    overlap: usize,
    out: &mut Vec<Chunk>,
) {
    let mut group_start = None;
    let mut group_end = 0;
    let mut line_offset = 0;

    for line in paragraph.split_inclusive('\n') {
        let start = line_offset;
        let end = start + line.len();
        line_offset = end;

        if line.len() > maximum {
            if let Some(group_start) = group_start.take() {
                push_chunk(out, base + group_start, base + group_end);
            }
            for window in source_windows(line, maximum, overlap) {
                push_chunk(
                    out,
                    base + start + window.span_start,
                    base + start + window.span_end,
                );
            }
            continue;
        }

        if let Some(current_start) = group_start {
            if end - current_start > maximum {
                push_chunk(out, base + current_start, base + group_end);
                group_start = Some(start);
            }
        } else {
            group_start = Some(start);
        }
        group_end = end;
    }

    if let Some(group_start) = group_start {
        push_chunk(out, base + group_start, base + group_end);
    }
}

fn push_chunk(out: &mut Vec<Chunk>, start: usize, end: usize) {
    out.push(Chunk {
        ordinal: out.len() as u32,
        span_start: start,
        span_end: end,
    });
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
    parts.push(format!("· {source_kind}"));
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
    fn paragraph_windows_group_complete_blocks_and_prefer_line_boundaries() {
        let first = "a".repeat(8);
        let second = "b".repeat(8);
        let third = "c".repeat(8);
        let content = format!("{first}\n\n{second}\n\n{third}");
        let chunks = source_paragraph_windows(&content, 20, 4);
        assert_eq!(
            chunks
                .iter()
                .map(|c| (c.span_start, c.span_end))
                .collect::<Vec<_>>(),
            vec![(0, 20), (20, content.len())]
        );

        let content = format!("{}\n{}", "x".repeat(1_200), "y".repeat(1_200));
        let chunks = source_paragraph_windows(&content, 2_048, 256);
        assert_eq!(
            chunks
                .iter()
                .map(|c| (c.span_start, c.span_end))
                .collect::<Vec<_>>(),
            vec![(0, 1_201), (1_201, content.len())]
        );
    }

    #[test]
    fn paragraph_windows_fall_back_to_utf8_windows_for_long_lines() {
        let content = "界".repeat(900);
        let chunks = source_paragraph_windows(&content, 1_024, 128);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.first().unwrap().span_start, 0);
        assert_eq!(chunks.last().unwrap().span_end, content.len());
        for chunk in &chunks {
            assert!(content.is_char_boundary(chunk.span_start));
            assert!(content.is_char_boundary(chunk.span_end));
        }
        for pair in chunks.windows(2) {
            assert!(pair[1].span_start <= pair[0].span_end);
            assert!(pair[1].span_end > pair[0].span_end);
        }
    }

    #[test]
    fn source_windows_skip_redundant_utf8_boundary_windows() {
        let content = "A0aਵ";
        let chunks = source_windows(content, 2, 1);
        assert_eq!(
            chunks
                .iter()
                .map(|c| (c.span_start, c.span_end))
                .collect::<Vec<_>>(),
            vec![(0, 2), (1, 3), (3, 6)]
        );
        assert!(chunks.windows(2).all(|pair| {
            pair[1].span_start <= pair[0].span_end && pair[1].span_end > pair[0].span_end
        }));
        assert_eq!(&content[chunks[0].span_start..chunks[0].span_end], "A0");
        assert_eq!(&content[chunks[1].span_start..chunks[1].span_end], "0a");
        assert_eq!(&content[chunks[2].span_start..chunks[2].span_end], "ਵ");
    }

    proptest::proptest! {
        #[test]
        fn paragraph_window_plan_covers_every_utf8_byte_and_keeps_fitting_paragraphs(
            content in proptest::prelude::any::<String>(),
            maximum in 1usize..128,
            overlap in 0usize..128,
        ) {
            let chunks = source_paragraph_windows(&content, maximum, overlap);
            if content.is_empty() {
                proptest::prop_assert!(chunks.is_empty());
            } else {
                proptest::prop_assert_eq!(chunks.first().map(|c| c.span_start), Some(0));
                proptest::prop_assert_eq!(chunks.last().map(|c| c.span_end), Some(content.len()));
                let mut covered_end = 0;
                for chunk in &chunks {
                    proptest::prop_assert!(chunk.span_start <= covered_end);
                    proptest::prop_assert!(chunk.span_end > covered_end);
                    proptest::prop_assert!(content.is_char_boundary(chunk.span_start));
                    proptest::prop_assert!(content.is_char_boundary(chunk.span_end));
                    covered_end = chunk.span_end;
                }
                proptest::prop_assert_eq!(covered_end, content.len());
            }

            let mut paragraph_start = 0;
            for paragraph in content.split_inclusive("\n\n") {
                let paragraph_end = paragraph_start + paragraph.len();
                if paragraph.len() <= maximum {
                    proptest::prop_assert!(chunks.iter().any(|chunk| {
                        chunk.span_start <= paragraph_start && chunk.span_end >= paragraph_end
                    }), "a fitting paragraph must remain inside a complete chunk");
                }
                paragraph_start = paragraph_end;
            }
        }
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
