//! `pdc-djot/1` body parsing (§6.1): plain-text extraction plus the Reader
//! index artifacts — stable block targets, canonical links, managed-asset
//! references, task items, raw-HTML detection, container depth, and the
//! level-one heading title fallback.
//!
//! This function never fails structurally: violations the contract classifies
//! as `unsafe_content` (e.g. a raw `=html` block) land in
//! `PdcBody::unsafe_constructs` and the document still parses. Hard failures
//! (`duplicate_block_id`, `document_too_complex`) are enforced by the caller
//! in [`super::parse_djot_document`].
//!
//! The scanner is a deliberate, deterministic subset of the pinned Djot
//! dialect (§6.1): a line-based pass plus a hand-rolled inline pass. It gives
//! semantics to the constructs the contract defines — block/span targets
//! (§7.2), canonical links and managed assets (§8), task items (§9.1), query
//! blocks (§9.2), raw `=html` blocks — and merely strips the remaining
//! markers (emphasis, strong, verbatim, highlight, link/image/heading
//! syntax) from the index text.

use super::{PdcBody, PdcLink, PdcTask};

/// Upper bound for the list-nesting share of [`PdcBody::container_depth`]
/// (§4.2: at most 256 simultaneously open block containers; indentation
/// deeper than that does not represent more open containers).
const MAX_LIST_NESTING: usize = super::PDC_MAX_CONTAINER_DEPTH;

/// Text recorded in [`PdcBody::unsafe_constructs`] for each raw `=html`
/// block (§6.1: raw blocks are nonconforming; the parse continues).
const RAW_HTML_BLOCK: &str = "raw html block";

/// Parse a `pdc-djot/1` body slice (everything after the closing `---`).
pub fn parse_djot_body(src: &str) -> PdcBody {
    let mut body = PdcBody {
        text: String::new(),
        wiki_links: Vec::new(),
        query_blocks: 0,
        query_errors: Vec::new(),
        block_ids: Vec::new(),
        document_links: Vec::new(),
        asset_refs: Vec::new(),
        tasks: Vec::new(),
        unsafe_constructs: Vec::new(),
        container_depth: 0,
        fallback_title: None,
    };
    // Open fenced block: (opening backtick run length, is a raw `=html` block).
    let mut fence: Option<(usize, bool)> = None;

    for raw_line in src.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some((marker, is_raw)) = fence {
            if is_closing_fence(line.trim(), marker) {
                fence = None;
            } else if !is_raw {
                // Fenced code and query-block code stay visible in the
                // corpus text (§9.2: Readers that cannot execute a query
                // block still show it). Raw `=html` content never does.
                body.text.push_str(line);
                body.text.push('\n');
            }
            continue;
        }

        // Container depth (§4.2): quote markers plus list nesting per line.
        let (quote_depth, rest) = split_quote_markers(line);
        let depth = quote_depth + list_nesting(rest);
        if depth > body.container_depth {
            body.container_depth = depth;
        }

        let trimmed = rest.trim();
        if trimmed.is_empty() {
            body.text.push('\n');
            continue;
        }
        if let Some((marker, info)) = fence_open(trimmed) {
            let is_raw = info.split_whitespace().next() == Some("=html");
            if is_raw {
                body.unsafe_constructs.push(RAW_HTML_BLOCK.to_string());
            }
            fence = Some((marker, is_raw));
            continue;
        }
        if trimmed.starts_with('{') && trimmed.ends_with('}') {
            // Block attribute line (§7.2): carries stable ids, no body text.
            body.block_ids.extend(scan_attrs(trimmed).ids);
            continue;
        }
        if let Some((level, content)) = heading(trimmed) {
            let mut rendered = String::new();
            render_inline(&mut body, &mut rendered, content, &mut None);
            let title = rendered.trim();
            if level == 1 && body.fallback_title.is_none() && !title.is_empty() {
                body.fallback_title = Some(title.to_string());
            }
            body.text.push_str(&rendered);
            body.text.push('\n');
            continue;
        }
        if let Some((completed, label)) = task_item(rest) {
            let mut rendered = String::new();
            let mut task_id = None;
            render_inline(&mut body, &mut rendered, label, &mut task_id);
            body.text.push_str(&rendered);
            body.text.push('\n');
            body.tasks.push(PdcTask {
                completed,
                text: rendered.trim().to_string(),
                id: task_id,
            });
            continue;
        }
        if is_table_row(trimmed) {
            render_table_row(&mut body, trimmed);
            continue;
        }
        let mut rendered = String::new();
        render_inline(&mut body, &mut rendered, trimmed, &mut None);
        body.text.push_str(&rendered);
        body.text.push('\n');
    }
    body
}

// ---------------------------------------------------------------------------
// Line-level structure
// ---------------------------------------------------------------------------

/// Strips leading block-quote markers (`>`, whitespace allowed between
/// markers). Returns the marker depth and the remaining content.
fn split_quote_markers(line: &str) -> (usize, &str) {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let mut j = i;
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'>' {
            depth += 1;
            i = j + 1;
        } else {
            break;
        }
    }
    let rest = &line[i..];
    (depth, rest.strip_prefix(' ').unwrap_or(rest))
}

/// List-nesting share of the container depth for one line: half the leading
/// whitespace of a list-item line, capped (see [`MAX_LIST_NESTING`]). Plain
/// indented lines are not containers in Djot.
fn list_nesting(rest: &str) -> usize {
    let item = rest.trim_start();
    if !has_list_marker(item) {
        return 0;
    }
    ((rest.len() - item.len()) / 2).min(MAX_LIST_NESTING)
}

/// Djot list marker: `- `, `* `, `+ `, or `12. `/`12) `.
fn has_list_marker(item: &str) -> bool {
    let bytes = item.as_bytes();
    match bytes.first() {
        Some(b'-' | b'*' | b'+') => bytes.get(1).is_some_and(|c| c.is_ascii_whitespace()),
        Some(d) if d.is_ascii_digit() => {
            let mut k = 1;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            k + 1 < bytes.len()
                && (bytes[k] == b'.' || bytes[k] == b')')
                && bytes[k + 1].is_ascii_whitespace()
        }
        _ => false,
    }
}

/// `(completed, label)` for a task list item (§9.1): `- [ ]`, `- [x]` or
/// `- [X]` behind any bullet or ordered marker. `label` excludes the markers.
fn task_item(rest: &str) -> Option<(bool, &str)> {
    let item = rest.trim_start();
    let bytes = item.as_bytes();
    let mut k = match bytes.first() {
        Some(b'-' | b'*' | b'+') => 1,
        Some(d) if d.is_ascii_digit() => {
            let mut k = 1;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k < bytes.len() && (bytes[k] == b'.' || bytes[k] == b')') {
                k + 1
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if !bytes.get(k).is_some_and(|c| c.is_ascii_whitespace()) {
        return None;
    }
    k += 1;
    if bytes.get(k) != Some(&b'[') {
        return None;
    }
    let completed = match bytes.get(k + 1) {
        Some(b' ') => false,
        Some(b'x' | b'X') => true,
        _ => return None,
    };
    if bytes.get(k + 2) != Some(&b']') {
        return None;
    }
    k += 3;
    if bytes.get(k) == Some(&b' ') {
        k += 1;
    }
    Some((completed, &item[k..]))
}

/// `(level, content)` for `#`–`######` ATX headings. `#foo` is not a heading:
/// the pinned dialect requires whitespace after the marker.
fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let bytes = trimmed.as_bytes();
    let level = bytes.iter().take_while(|&&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    match bytes.get(level) {
        None => Some((level as u8, "")),
        Some(b' ' | b'\t') => Some((level as u8, trimmed[level + 1..].trim_start())),
        _ => None,
    }
}

/// `(backtick run length, info string)` when `trimmed` opens a fenced block.
fn fence_open(trimmed: &str) -> Option<(usize, &str)> {
    let run = trimmed.bytes().take_while(|&b| b == b'`').count();
    if run < 3 {
        return None;
    }
    Some((run, trimmed[run..].trim()))
}

/// A closing fence: only backticks, at least as many as the opening fence.
fn is_closing_fence(trimmed: &str, open_run: usize) -> bool {
    trimmed.len() >= open_run && trimmed.bytes().all(|b| b == b'`')
}

/// A Djot pipe-table row: leading and trailing `|` with cells between.
fn is_table_row(trimmed: &str) -> bool {
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() >= 2
}

/// Table rows contribute their cell text; the `|---|---|` delimiter row
/// carries none.
fn render_table_row(body: &mut PdcBody, trimmed: &str) {
    let inner = &trimmed[1..trimmed.len() - 1];
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    if cells
        .iter()
        .all(|cell| !cell.is_empty() && cell.bytes().all(|b| b == b'-' || b == b':'))
    {
        return;
    }
    let mut rendered = String::new();
    for (k, cell) in cells.iter().enumerate() {
        if k > 0 {
            rendered.push(' ');
        }
        render_inline(body, &mut rendered, cell, &mut None);
    }
    body.text.push_str(&rendered);
    body.text.push('\n');
}

// ---------------------------------------------------------------------------
// Attribute groups
// ---------------------------------------------------------------------------

/// Scanned content of one `{…}` attribute group.
struct SpanAttrs {
    /// Stable `b-…` ids (§7.2), prefix as written in the source.
    ids: Vec<String>,
    /// The reserved `pdc-task` class (§9.1).
    task: bool,
    /// The reserved `pdc-embed` class (§8.1).
    embed: bool,
}

/// Scans `{…}` attribute content for stable `#b-…` ids and the reserved
/// `pdc-task` / `pdc-embed` classes. Quoted values are skipped whole.
fn scan_attrs(text: &str) -> SpanAttrs {
    let mut attrs = SpanAttrs {
        ids: Vec::new(),
        task: false,
        embed: false,
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            marker @ (b'#' | b'.') => {
                let start = i + 1;
                i = attr_token_end(bytes, start);
                let token = &text[start..i];
                if marker == b'#' {
                    if token.starts_with("b-") {
                        attrs.ids.push(token.to_string());
                    }
                } else {
                    match token {
                        "pdc-task" => attrs.task = true,
                        "pdc-embed" => attrs.embed = true,
                        _ => {}
                    }
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    attrs
}

/// End of one attribute token: whitespace, a closing brace, or a quote.
fn attr_token_end(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'}' && bytes[i] != b'"'
    {
        i += 1;
    }
    i
}

/// Applies an inline attribute group: stable ids go to `block_ids` (§7.2) and
/// the task flag captures the id for the enclosing task item (§9.1). Returns
/// the scanned group so callers can read flags like `pdc-embed` (§8.1).
fn apply_attrs(body: &mut PdcBody, text: &str, task_id: &mut Option<String>) -> SpanAttrs {
    let attrs = scan_attrs(text);
    body.block_ids.extend(attrs.ids.iter().cloned());
    if attrs.task && task_id.is_none() {
        *task_id = attrs.ids.first().cloned();
    }
    attrs
}

// ---------------------------------------------------------------------------
// Inline rendering
// ---------------------------------------------------------------------------

/// Renders one fragment of inline content into `out`, stripping markup and
/// recording contract artifacts (§7.2, §8, §9.1) into `body`. `task_id`
/// receives the `b-…` id of a `pdc-task` span, if one is present.
fn render_inline(body: &mut PdcBody, out: &mut String, src: &str, task_id: &mut Option<String>) {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\\' if i + 1 < n => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '`' => i = render_verbatim(out, &chars, i),
            '$' => i = render_math(out, &chars, i),
            '*' | '_' => match emphasis_marker_len(&chars, i) {
                Some(len) => i += len,
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '<' => i = render_angle_span(out, &chars, i),
            '^' | '~' if chars.get(i + 1) == Some(&'{') => match find_char(&chars, i + 2, '}') {
                Some(j) => {
                    let inner: String = chars[i + 2..j].iter().collect();
                    render_inline(body, out, &inner, task_id);
                    i = j + 1;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '!' if chars.get(i + 1) == Some(&'[') => {
                match render_bracket_construct(body, out, &chars, i + 1, true, task_id) {
                    Some(next) => i = next,
                    None => {
                        out.push('!');
                        i += 1;
                    }
                }
            }
            '[' => match render_bracket_construct(body, out, &chars, i, false, task_id) {
                Some(next) => i = next,
                None => {
                    out.push('[');
                    i += 1;
                }
            },
            '{' => i = render_brace(body, out, &chars, i, task_id),
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
}

/// Renders a `[label](target)` link, `![alt](target)` image, or a
/// `[label]{attrs}` span beginning at `bracket` (index of `[`). Returns the
/// position after the construct, or `None` when the brackets do not form a
/// recognized construct (the caller falls back to literal text).
fn render_bracket_construct(
    body: &mut PdcBody,
    out: &mut String,
    chars: &[char],
    bracket: usize,
    is_image: bool,
    task_id: &mut Option<String>,
) -> Option<usize> {
    let close = find_matching(chars, bracket, '[', ']')?;
    let label: String = chars[bracket + 1..close].iter().collect();
    let after = close + 1;

    if chars.get(after) == Some(&'(') {
        let target_close = find_matching(chars, after, '(', ')')?;
        let target: String = chars[after + 1..target_close].iter().collect();
        let mut end = target_close + 1;
        let mut embed = false;
        let attr_group = if chars.get(end) == Some(&'{') {
            find_attr_group_end(chars, end)
        } else {
            None
        };
        if let Some(j) = attr_group {
            let inner: String = chars[end + 1..j].iter().collect();
            embed = apply_attrs(body, &inner, task_id).embed;
            end = j + 1;
        }
        render_inline(body, out, &label, task_id);
        match classify_target(&target) {
            Some(LinkTarget::Document { uuid, block }) => {
                body.document_links.push(PdcLink { uuid, block, embed });
            }
            Some(LinkTarget::Asset(digest)) => body.asset_refs.push(digest),
            None => {}
        }
        return Some(end);
    }

    if !is_image && chars.get(after) == Some(&'{') {
        // Inline span with attributes: `[label]{#b-… .pdc-task}` (§7.2, §9.1).
        let j = find_attr_group_end(chars, after)?;
        let inner: String = chars[after + 1..j].iter().collect();
        render_inline(body, out, &label, task_id);
        apply_attrs(body, &inner, task_id);
        return Some(j + 1);
    }

    None
}

/// Handles one `{…}` group. Djot brace markup (`{=highlight}`, `{+insert}`,
/// `{-delete}`) renders its content; an attribute group attached to a
/// preceding element (span target §7.2, embed class §8.1, task span §9.1) is
/// stripped; anything else is literal text.
fn render_brace(
    body: &mut PdcBody,
    out: &mut String,
    chars: &[char],
    i: usize,
    task_id: &mut Option<String>,
) -> usize {
    let markup_end = if matches!(chars.get(i + 1), Some('=' | '+' | '-')) {
        find_char(chars, i + 2, '}')
    } else {
        None
    };
    if let Some(j) = markup_end {
        let inner: String = chars[i + 2..j].iter().collect();
        render_inline(body, out, &inner, task_id);
        return j + 1;
    }

    let attached = out.chars().next_back().is_some_and(|c| !c.is_whitespace());
    let group_end = if attached {
        find_attr_group_end(chars, i)
    } else {
        None
    };
    if let Some(j) = group_end {
        let inner: String = chars[i + 1..j].iter().collect();
        apply_attrs(body, &inner, task_id);
        return j + 1;
    }

    out.push('{');
    i + 1
}

/// Verbatim text: a backtick run of length n closes at the next run of
/// exactly n backticks. Content is copied verbatim, without backticks.
fn render_verbatim(out: &mut String, chars: &[char], i: usize) -> usize {
    let n = char_run(chars, i, '`');
    match find_exact_run(chars, i + n, '`', n) {
        Some(j) => {
            out.extend(chars[i + n..j].iter());
            j + n
        }
        None => {
            out.extend(std::iter::repeat_n('`', n));
            i + n
        }
    }
}

/// Inline math: `$…$` keeps its content, drops the delimiters.
fn render_math(out: &mut String, chars: &[char], i: usize) -> usize {
    match find_char(chars, i + 1, '$') {
        Some(j) => {
            out.extend(chars[i + 1..j].iter());
            j + 1
        }
        None => {
            out.push('$');
            i + 1
        }
    }
}

/// `<…>` without whitespace (autolink or inline tag): keeps the inner text,
/// drops the brackets. With whitespace it is literal text.
fn render_angle_span(out: &mut String, chars: &[char], i: usize) -> usize {
    if let Some(j) = find_char(chars, i + 1, '>') {
        let inner = &chars[i + 1..j];
        if !inner.is_empty() && !inner.contains(&'<') && !inner.iter().any(|&c| c.is_whitespace()) {
            out.extend(inner.iter());
            return j + 1;
        }
    }
    out.push('<');
    i + 1
}

/// Length of the emphasis/strong marker at `i`, or `None` when the character
/// is literal text. Markers are stripped from plain text, never their content.
fn emphasis_marker_len(chars: &[char], i: usize) -> Option<usize> {
    let c = chars[i];
    if i + 1 < chars.len() && chars[i + 1] == c && emphasis_boundary(chars, i, 2) {
        return Some(2);
    }
    if emphasis_boundary(chars, i, 1) {
        Some(1)
    } else {
        None
    }
}

/// Deliberately simple emphasis rule (deterministic; no full Djot parser):
/// a marker must open (nothing/whitespace/opening bracket before, non-
/// whitespace after) or close (non-whitespace before, nothing/whitespace/
/// closing punctuation after). `_` never forms intra-word markers, which
/// keeps `snake_case` intact.
fn emphasis_boundary(chars: &[char], i: usize, len: usize) -> bool {
    let before = if i == 0 { None } else { Some(chars[i - 1]) };
    let after = chars.get(i + len).copied();
    let can_open = after.is_some_and(|c| !c.is_whitespace())
        && before
            .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '"' | '\''));
    let can_close = before.is_some_and(|c| !c.is_whitespace())
        && after.is_none_or(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\''
                )
        });
    if chars[i] == '_' {
        (can_open && before.is_none_or(|c| c.is_whitespace()))
            || (can_close && after.is_none_or(|c| c.is_whitespace()))
    } else {
        can_open || can_close
    }
}

/// End of a `{…}` attribute group, honoring quoted values; `None` if the
/// group never closes on this line.
fn find_attr_group_end(chars: &[char], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '}' => return Some(i),
            quote @ ('"' | '\'') => {
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Small scanning helpers
// ---------------------------------------------------------------------------

/// First position of `target` at or after `from`.
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars[from..]
        .iter()
        .position(|&c| c == target)
        .map(|p| from + p)
}

/// Length of the run of `c` starting at `at`.
fn char_run(chars: &[char], at: usize, c: char) -> usize {
    chars[at..].iter().take_while(|&&x| x == c).count()
}

/// First position at or after `from` where a run of exactly `len` `c`
/// characters starts (runs of other lengths are skipped whole).
fn find_exact_run(chars: &[char], from: usize, c: char, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == c {
            let run = char_run(chars, i, c);
            if run == len {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

/// Position of the `close_c` matching the `open_c` at `open` (nesting-aware).
fn find_matching(chars: &[char], open: usize, open_c: char, close_c: char) -> Option<usize> {
    let mut depth = 0usize;
    for (p, &c) in chars.iter().enumerate().skip(open) {
        if c == open_c {
            depth += 1;
        } else if c == close_c {
            depth -= 1;
            if depth == 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Classification of one link/image target (§8).
enum LinkTarget {
    /// `pdc://document/<uuid>` with an optional `#b-<uuid>` fragment.
    Document { uuid: String, block: Option<String> },
    /// `pdc://asset/sha256/<64-hex-digest>`.
    Asset(String),
}

fn classify_target(target: &str) -> Option<LinkTarget> {
    let t = target.trim();
    let t = t
        .strip_prefix('<')
        .and_then(|r| r.strip_suffix('>'))
        .unwrap_or(t);
    if let Some(rest) = t.strip_prefix("pdc://document/") {
        let (uuid, fragment) = match rest.split_once('#') {
            Some((uuid, frag)) => (uuid, Some(frag)),
            None => (rest, None),
        };
        if uuid.is_empty() {
            return None;
        }
        let block = fragment.filter(|f| f.starts_with("b-")).map(str::to_string);
        Some(LinkTarget::Document {
            uuid: uuid.to_string(),
            block,
        })
    } else if let Some(rest) = t.strip_prefix("pdc://asset/sha256/") {
        let digest = match rest.split_once('#') {
            Some((digest, _)) => digest,
            None => rest,
        };
        if digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Some(LinkTarget::Asset(digest.to_string()))
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantics_fixture_artifacts() {
        let src = "{#b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32}\n## Stable section\n\n[Minimal](pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db)\n\n[Embedded fallback](pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db){.pdc-embed}\n\n- [ ] Open task\n- [x] Completed task\n";
        let body = parse_djot_body(src);
        assert!(
            body.block_ids
                .contains(&"b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32".to_string())
        );
        assert_eq!(body.document_links.len(), 2);
        assert!(body.document_links[1].embed);
        assert_eq!(body.tasks.len(), 2);
        assert!(body.tasks[0].text.contains("Open task"));
        assert!(!body.tasks[0].completed);
        assert!(body.tasks[1].completed);
        assert!(body.unsafe_constructs.is_empty());
        assert!(body.text.contains("Stable section"));
        assert!(body.text.contains("Minimal"));
        assert!(!body.text.contains("pdc://"));
    }

    #[test]
    fn raw_html_block_is_unsafe_but_parses() {
        let src = "``` =html\n<script>alert(\"unsafe\")</script>\n```\n";
        let body = parse_djot_body(src);
        assert!(
            !body.unsafe_constructs.is_empty(),
            "raw html must be flagged"
        );
        assert!(
            !body.text.contains("<script>"),
            "raw html must not leak into index text: {:?}",
            body.text
        );
    }

    #[test]
    fn duplicate_block_ids_are_reported_to_caller_via_block_ids() {
        let src = "{#b-018f47c6-9c8a-79af-b060-7a6a3b79016c}\n## First\n\n{#b-018f47c6-9c8a-79af-b060-7a6a3b79016c}\n## Second\n";
        let body = parse_djot_body(src);
        assert_eq!(body.block_ids.len(), 2, "caller dedupes and errors");
    }

    #[test]
    fn nested_quotes_drive_container_depth() {
        let mut src = String::new();
        for _ in 0..257 {
            src.push('>');
        }
        src.push_str("\n# deep\n");
        let body = parse_djot_body(&src);
        assert_eq!(body.container_depth, 257);
    }

    #[test]
    fn first_level_one_heading_is_title_fallback() {
        let body = parse_djot_body("# Minimal document\n\ntext\n");
        assert_eq!(body.fallback_title.as_deref(), Some("Minimal document"));
        let body2 = parse_djot_body("## only h2\n");
        assert_eq!(body2.fallback_title, None);
    }

    #[test]
    fn asset_refs_collected() {
        let src = "![Diagram](pdc://asset/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef)\n";
        let body = parse_djot_body(src);
        assert_eq!(
            body.asset_refs,
            vec!["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()]
        );
    }

    #[test]
    fn task_span_identity_and_query_blocks() {
        let src = "- [ ] [Write spec]{#b-018f47c6-c718-728c-9d91-b2bc700814bb .pdc-task}\n\n``` pdc-query\ntag = \"standard\"\n```\n";
        let body = parse_djot_body(src);
        assert_eq!(body.tasks.len(), 1);
        assert!(!body.tasks[0].completed);
        assert_eq!(body.tasks[0].text, "Write spec");
        assert_eq!(
            body.tasks[0].id.as_deref(),
            Some("b-018f47c6-c718-728c-9d91-b2bc700814bb")
        );
        // §9.2: query blocks remain visible content for Readers that do not
        // implement the query contract.
        assert!(body.text.contains("tag = \"standard\""));
        // §7.2: the inline span id is a stable target of the document.
        assert!(
            body.block_ids
                .iter()
                .any(|id| id == "b-018f47c6-c718-728c-9d91-b2bc700814bb")
        );
    }

    #[test]
    fn document_link_fragment_target() {
        let src = "[Section](pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db#b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32)\n";
        let body = parse_djot_body(src);
        assert_eq!(body.document_links.len(), 1);
        assert_eq!(
            body.document_links[0].block.as_deref(),
            Some("b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32")
        );
        assert!(!body.document_links[0].embed);
        assert!(!body.text.contains("pdc://"));
    }
}
