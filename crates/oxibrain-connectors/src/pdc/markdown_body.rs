//! `pdc-markdown/1` body projection (PDC 2 §6.1, §7.2, §8, §9).
//!
//! The body text is the **verbatim post-frontmatter source** — the legacy
//! Markdown scan after separating the envelope — so every construct (wiki
//! syntax, math, footnotes, raw HTML) stays indexed as inert text/source and
//! nothing is lost by projection. On top of that text pass, the scanner
//! records Reader artifacts:
//!
//! - caret block IDs `^<id>` with charset `[A-Za-z0-9-]+`, and
//!   `id="b-<uuid>"` inside raw HTML, as stable block targets (§7.2);
//! - GFM task-list items (`- [ ]` open, `- [x]`/`- [X]` completed; any other
//!   checkbox is a preserved extension, not a task — §9.1);
//! - canonical `pdc://document/…` links and `pdc://asset/sha256/…` digests
//!   (§8); vault-relative links/embeds are recorded as written
//!   ([`WikiLink`](super::WikiLink)) and never resolved;
//! - active/unsafe constructs (`unsafe_content`, §6.1/§11): script-family
//!   elements, refresh metas, event handlers, and
//!   `javascript:`/`vbscript:`/`data:` URLs. Benign raw HTML stays
//!   source-only and is not flagged;
//! - fenced `base` blocks (info string exactly `base`): validated against
//!   the `pdc-query/1` safe-YAML rules, preserved as content, never
//!   executed (§9.2, `pdc-query/1` §3.2).
//!
//! One deterministic pass, line-based like the djot/html scanners; no full
//! CommonMark parse is needed for projection-level fidelity.

use super::{PdcBody, PdcLink, PdcTask, WikiLink};

/// Caret block ID charset (PDC 2 §7.2): Latin letters, digits, hyphens.
fn is_caret_id_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-'
}

/// Parse the Markdown body slice into its index artifacts.
pub fn parse_markdown_body(src: &str) -> PdcBody {
    let mut scan = Scan {
        body: PdcBody {
            text: src.to_string(),
            block_ids: Vec::new(),
            document_links: Vec::new(),
            wiki_links: Vec::new(),
            asset_refs: Vec::new(),
            tasks: Vec::new(),
            unsafe_constructs: Vec::new(),
            container_depth: 0,
            fallback_title: None,
            query_blocks: 0,
            query_errors: Vec::new(),
        },
        title_taken: false,
    };
    scan.run();
    scan.body
}

struct Scan {
    body: PdcBody,
    title_taken: bool,
}

impl Scan {
    fn run(&mut self) {
        let mut in_fence: Option<Fence> = None;
        // Lines of an open ```base fence, buffered for safe-YAML validation.
        let mut base_buffer: Option<Vec<String>> = None;
        // Owned copy: the scanner mutates `body` while walking its text.
        let text = self.body.text.clone();

        for line in text.lines() {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start_matches(' ').len();
            // Container-depth proxy: list indentation (half the leading
            // spaces) and blockquote runs, same 256 cap as the other profiles.
            let quote_run = trimmed.chars().take_while(|&c| c == '>').count();
            self.body.container_depth = self.body.container_depth.max(indent / 2).max(quote_run);

            if let Some(fence) = &in_fence {
                if is_closing_fence(trimmed, fence) {
                    if let Some(buffer) = base_buffer.take() {
                        self.validate_base_query(&buffer);
                    }
                    in_fence = None;
                } else if let Some(buffer) = &mut base_buffer {
                    buffer.push(line.to_string());
                }
                continue;
            }
            if let Some(fence) = opening_fence(line) {
                if fence.info == "base" {
                    base_buffer = Some(Vec::new());
                }
                in_fence = Some(fence);
                continue;
            }

            self.scan_content_line(line, trimmed);
        }
        // An unterminated ```base fence still carried a query attempt.
        if let Some(buffer) = base_buffer {
            self.validate_base_query(&buffer);
        }
    }

    /// Per-line projection outside fences.
    fn scan_content_line(&mut self, line: &str, trimmed: &str) {
        // Fallback title: first level-one heading (PDC 2 §5.1).
        if !self.title_taken && line.starts_with("# ") && !line.starts_with("##") {
            let title = line[2..].trim();
            if !title.is_empty() {
                self.body.fallback_title = Some(title.to_string());
                self.title_taken = true;
            }
        }

        // Stable targets: caret block IDs at line end, and raw-HTML
        // `id="b-<uuid>"` attributes (PDC 2 §7.2).
        if let Some(id) = trailing_caret_id(trimmed) {
            self.body.block_ids.push(id);
        }
        for id in html_b_uuid_ids(trimmed) {
            self.body.block_ids.push(id);
        }

        // GFM task items (§9.1).
        if let Some(task) = task_item(trimmed) {
            self.body.tasks.push(task);
        }

        // Inline projection: wiki links/embeds, Markdown links/images,
        // pdc:// targets, unsafe constructs.
        self.scan_inline(trimmed);
    }

    /// Character scan over one content line (outside code fences).
    fn scan_inline(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'<' => match raw_html_construct(&line[i..]) {
                    RawHtml::Autolink(len) => {
                        let inner = &line[i + 1..i + len - 1];
                        if let Some(target) = autolink_target(inner) {
                            self.classify_target(&target, false, true);
                        }
                        i += len;
                    }
                    RawHtml::Tag(tag_len, name) => {
                        let tag_src = &line[i..i + tag_len];
                        if is_active_tag(&name) || (name == "meta" && is_refresh_meta(tag_src)) {
                            self.body
                                .unsafe_constructs
                                .push(format!("raw html `<{name}…>` element"));
                        } else if let Some(handler) = event_handler(tag_src) {
                            self.body
                                .unsafe_constructs
                                .push(format!("raw html event handler `{handler}`"));
                        }
                        i += tag_len;
                    }
                    RawHtml::Other(len) => i += len,
                },
                b'!' if i + 1 < bytes.len() && bytes[i + 1] == b'[' => {
                    // `bracket` = the first `[` after `!`; `![[…]]` embeds and
                    // `![…](…)` images both carry the embed flag.
                    i += self.wiki_or_markdown(line, i + 1, true);
                }
                b'[' => {
                    i += self.wiki_or_markdown(line, i, false);
                }
                _ => i += 1,
            }
        }
    }

    /// Handle `[[…]]` (wiki) or `[…](…)` (Markdown) starting at `i`.
    /// Returns the number of bytes consumed.
    fn wiki_or_markdown(&mut self, line: &str, bracket: usize, embed: bool) -> usize {
        let bytes = line.as_bytes();
        if bracket + 1 < bytes.len() && bytes[bracket + 1] == b'[' {
            // Wiki link/embed: skip `[[`, find `]]`.
            let inner_start = bracket + 2;
            let mut k = inner_start;
            while k + 1 < bytes.len() && !(bytes[k] == b']' && bytes[k + 1] == b']') {
                k += 1;
            }
            if k + 1 < bytes.len() {
                let inner = &line[inner_start..k];
                self.record_wiki(inner, embed);
                return k + 2 - bracket;
            }
            return 1;
        }
        // Markdown link/image: `]` must be followed by `(`.
        let mut k = bracket;
        while k < bytes.len() && bytes[k] != b']' {
            k += 1;
        }
        if k + 1 < bytes.len() && bytes[k + 1] == b'(' {
            let target_start = k + 2;
            let Some(target_len) = balanced_parens_len(&line[target_start..]) else {
                return 1;
            };
            let raw_target = &line[target_start..target_start + target_len];
            let target = normalize_target(raw_target);
            self.classify_target(&target, embed, false);
            return target_start + target_len - bracket;
        }
        1
    }

    /// Wiki-link inner text: `target`, `target|label`, `target#fragment`.
    fn record_wiki(&mut self, inner: &str, embed: bool) {
        let reference = inner.split('|').next().unwrap_or(inner);
        let (target, fragment) = match reference.split_once('#') {
            Some((t, f)) => (t, Some(f)),
            None => (reference, None),
        };
        let target = target.trim();
        // Non-canonical wiki targets (PDC 2 §8.1): schemes and absolute
        // paths are not vault links. Record nothing; projection stays
        // vault-relative.
        if target.contains("://")
            || target.starts_with('/')
            || target.starts_with("file:")
            || target.starts_with("data:")
        {
            return;
        }
        self.body.wiki_links.push(WikiLink {
            target: target.to_string(),
            block: fragment.map(str::to_string),
            embed,
        });
    }

    /// Route one link/image/autolink target.
    fn classify_target(&mut self, target: &str, embed: bool, autolink: bool) {
        if let Some(rest) = target.strip_prefix("pdc://document/") {
            let (uuid, block) = match rest.split_once('#') {
                Some((u, b)) => (u, Some(b.to_string())),
                None => (rest, None),
            };
            self.body.document_links.push(PdcLink {
                uuid: uuid.to_string(),
                block,
                embed,
            });
            return;
        }
        if let Some(digest) = target.strip_prefix("pdc://asset/sha256/") {
            if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
                self.body.asset_refs.push(digest.to_ascii_lowercase());
            }
            return;
        }
        if unsafe_scheme(target) {
            self.body
                .unsafe_constructs
                .push(format!("unsafe URL scheme in `{target}`"));
            return;
        }
        if autolink {
            // `<https://…>` and other external autolinks are not canonical
            // links (PDC 2 §8.1); `<pdc://…>` was handled above.
            return;
        }
        if target.contains("://")
            || target.starts_with('#')
            || target.starts_with('/')
            || target.starts_with("mailto:")
            || target.starts_with("file:")
            || target.starts_with("data:")
        {
            return;
        }
        // Vault-relative Markdown link/image: projection-only, like wiki
        // links. Fragment after `#` is recorded as written.
        let (path, fragment) = match target.split_once('#') {
            Some((p, f)) => (p, Some(f.to_string())),
            None => (target, None),
        };
        if path.is_empty() && fragment.is_some() {
            return; // pure heading self-anchor
        }
        self.body.wiki_links.push(WikiLink {
            target: path.to_string(),
            block: fragment,
            embed,
        });
    }

    /// Safe-YAML validation for one fenced `base` block (pdc-query/1 §3.2):
    /// the block is content either way; only the diagnostic differs.
    fn validate_base_query(&mut self, lines: &[String]) {
        let src = lines.join("\n");
        match super::query::validate_base_yaml(&src) {
            Ok(()) => self.body.query_blocks += 1,
            Err(reason) => self.body.query_errors.push(reason),
        }
    }
}

#[derive(Clone)]
struct Fence {
    ch: char,
    count: usize,
    info: String,
}

/// Opening fence: up to three leading spaces, ≥3 of the same marker char,
/// then the info string. Fence info strings are user data (PDC 2 §6.1).
fn opening_fence(line: &str) -> Option<Fence> {
    let trimmed = line.strip_prefix("   ").unwrap_or(line);
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let count = trimmed.chars().take_while(|&c| c == marker).count();
    if count < 3 {
        return None;
    }
    Some(Fence {
        ch: marker,
        count,
        info: trimmed[count..].trim().to_string(),
    })
}

fn is_closing_fence(trimmed: &str, fence: &Fence) -> bool {
    let marker_count = trimmed.chars().take_while(|&c| c == fence.ch).count();
    marker_count >= fence.count && trimmed[fence.count..].trim().is_empty()
}

/// Balanced-paren length of a link target starting after `(`.
fn balanced_parens_len(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 1usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\\' if i + 1 < bytes.len() => i += 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Strip the optional angle-bracket form and a trailing quoted title.
fn normalize_target(raw: &str) -> String {
    let inner = raw.trim();
    let inner = if inner.starts_with('<') && inner.ends_with('>') && inner.len() >= 2 {
        &inner[1..inner.len() - 1]
    } else {
        inner
    };
    // `target "title"` — cut at the last space-quote boundary.
    match inner.rfind(" \"") {
        Some(pos) if inner.ends_with('"') => inner[..pos].trim().to_string(),
        _ => inner.to_string(),
    }
}

/// Trailing caret block ID on a trimmed line: the longest `[A-Za-z0-9-]+`
/// run at the end, preceded by `^` and a whitespace/start boundary
/// (PDC 2 §7.2, Obsidian placement).
fn trailing_caret_id(trimmed: &str) -> Option<String> {
    let bytes = trimmed.as_bytes();
    let mut end = bytes.len();
    while end > 0 && is_caret_id_char(bytes[end - 1]) {
        end -= 1;
    }
    // The walk stops with the caret at `end - 1`; the run `end..` is the ID.
    if end == 0 || end == bytes.len() || bytes[end - 1] != b'^' {
        return None;
    }
    let id = &trimmed[end..];
    if id.is_empty() || !id.bytes().all(is_caret_id_char) {
        return None;
    }
    // Preceded by whitespace or the start of the line.
    if end >= 2 && !bytes[end - 2].is_ascii_whitespace() {
        return None;
    }
    Some(id.to_string())
}

/// `id="b-<uuid>"` / `id='b-<uuid>'` attributes inside raw HTML on a line.
fn html_b_uuid_ids(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\''] {
        let needle = format!("id={quote}b-");
        let mut search = line;
        while let Some(pos) = search.find(&needle) {
            let rest = &search[pos + needle.len()..];
            if let Some(end) = rest.find(quote) {
                let id = &rest[..end];
                if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    out.push(format!("b-{id}"));
                }
            }
            search = &search[pos + needle.len()..];
        }
    }
    out
}

/// GFM task item: list marker + checkbox. `- [/]` and friends are preserved
/// extensions, not tasks (PDC 2 §9.1).
fn task_item(trimmed: &str) -> Option<PdcTask> {
    let rest = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    let mut chars = rest.chars();
    if chars.next() != Some('[') {
        return None;
    }
    let state = chars.next()?;
    let tail = chars.next()?;
    if tail != ']' {
        return None;
    }
    let completed = match state {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None, // preserved extension state
    };
    let body = &rest[3..].strip_prefix(' ').unwrap_or(&rest[3..]);
    // A trailing caret ID on the task line is the task's stable target.
    let (text, id) = match trailing_caret_id(body.trim_end()) {
        Some(id) => {
            let without = body.trim_end();
            let cut = without.len() - id.len() - 1; // `^` + id
            (without[..cut].trim_end().to_string(), Some(id))
        }
        None => (body.trim().to_string(), None),
    };
    Some(PdcTask {
        completed,
        text,
        id,
    })
}

/// Raw-HTML constructs starting with `<`, classified "sufficiently" for the
/// unsafe-content list.
enum RawHtml {
    /// `<scheme:…>` autolink — its byte length.
    Autolink(usize),
    /// A raw tag — its byte length and lowercase element name.
    Tag(usize, String),
    /// Anything else (comment, PI, declaration, lone `<`).
    Other(usize),
}

/// Classify the construct starting at `<` per CommonMark raw-HTML rules.
fn raw_html_construct(s: &str) -> RawHtml {
    let bytes = s.as_bytes();
    let Some(close) = s.find('>') else {
        return RawHtml::Other(bytes.len());
    };
    let inner = &s[1..close];
    // Comment / PI / declaration / CDATA — inert, not a tag.
    if inner.starts_with("!--") || inner.starts_with('?') || inner.starts_with('!') {
        return RawHtml::Other(close + 1);
    }
    // Closing tags carry no active semantics of their own.
    let close_tag = inner.starts_with('/');
    let body = inner.strip_prefix('/').unwrap_or(inner);
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if name.is_empty() || close_tag {
        return RawHtml::Other(close + 1);
    }
    // `<scheme:...>` — autolink, not a tag.
    if body[name.len()..].starts_with(':') {
        return RawHtml::Autolink(close + 1);
    }
    RawHtml::Tag(close + 1, name)
}

/// Active element names (PDC 2 §6.1/§11): script-family and embeds.
/// `<meta …>` is handled separately — only refresh metas are active.
fn is_active_tag(name: &str) -> bool {
    matches!(
        name,
        "script" | "iframe" | "frame" | "object" | "embed" | "applet" | "base"
    )
}

/// A raw `<meta …>` is active only when it forces a refresh (PDC 2 §11.3);
/// charset declarations stay benign.
fn is_refresh_meta(tag_src: &str) -> bool {
    let lower = tag_src.to_ascii_lowercase();
    lower.contains("http-equiv") && lower.contains("refresh")
}

/// First `on…=` event-handler attribute in a raw tag's source.
fn event_handler(tag_src: &str) -> Option<String> {
    let lower = tag_src.to_ascii_lowercase();
    let mut search = lower.as_str();
    while let Some(pos) = search.find("on") {
        let rest = &search[pos..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '=')
            .collect();
        if name.len() > 2 && name.ends_with('=') {
            return Some(name.trim_end_matches('=').to_string());
        }
        search = &search[pos + 2..];
    }
    None
}

fn unsafe_scheme(target: &str) -> bool {
    let lower = target.trim_start().to_ascii_lowercase();
    lower.starts_with("javascript:") || lower.starts_with("vbscript:") || lower.starts_with("data:")
}

/// `<scheme:rest>` autolink target, when it is a `pdc://` URI.
fn autolink_target(inner: &str) -> Option<String> {
    let (scheme, rest) = inner.split_once(':')?;
    if scheme.eq_ignore_ascii_case("pdc") {
        Some(format!("pdc:{rest}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> PdcBody {
        parse_markdown_body(src)
    }

    #[test]
    fn text_is_verbatim_source() {
        let src = "# H\n\n==highlight== and $math$ and <b>bold</b>.\n";
        let b = body(src);
        assert_eq!(b.text, src);
        assert!(b.unsafe_constructs.is_empty(), "benign html is not unsafe");
    }

    #[test]
    fn caret_ids_are_projected() {
        let b = body("# First ^b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32\n\n# Second ^my-note\n");
        assert_eq!(
            b.block_ids,
            vec![
                "b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32".to_string(),
                "my-note".to_string(),
            ]
        );
    }

    #[test]
    fn caret_id_requires_charset_and_boundary() {
        let b = body("text ^not_an_id\nmore plain\n");
        assert!(b.block_ids.is_empty(), "{:?}", b.block_ids);
    }

    #[test]
    fn links_embeds_and_assets() {
        let b = body(
            "[Doc](pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db)\n\
             ![pic](pdc://asset/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef)\n\
             [[note|label]] and ![[other]] and [rel](folder/note.md#^block-id)\n",
        );
        assert_eq!(b.document_links.len(), 1);
        assert_eq!(
            b.document_links[0].uuid,
            "018f47c6-4a77-7c52-9db8-0e5f9bcb17db"
        );
        assert_eq!(
            b.asset_refs,
            vec!["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]
        );
        assert_eq!(b.wiki_links.len(), 3);
        assert_eq!(b.wiki_links[0].target, "note");
        assert!(!b.wiki_links[0].embed);
        assert!(b.wiki_links[1].embed);
        assert_eq!(b.wiki_links[2].block.as_deref(), Some("^block-id"));
    }

    #[test]
    fn tasks_and_extension_states() {
        let b = body(
            "- [ ] Open ^b-018f47c6-c718-728c-9d91-b2bc700814bb\n- [x] Done\n- [/] Extension\n",
        );
        assert_eq!(b.tasks.len(), 2);
        assert!(!b.tasks[0].completed);
        assert_eq!(
            b.tasks[0].id.as_deref(),
            Some("b-018f47c6-c718-728c-9d91-b2bc700814bb")
        );
        assert_eq!(b.tasks[0].text, "Open");
        assert_eq!(b.tasks[1].text, "Done");
    }

    #[test]
    fn active_html_is_flagged_benign_is_not() {
        let b = body("<b>ok</b> <script>alert(1)</script>\n[click](javascript:alert(1))\n");
        assert_eq!(b.unsafe_constructs.len(), 2, "{:?}", b.unsafe_constructs);
    }

    #[test]
    fn base_fences_are_validated_not_executed() {
        let good = body("```base\nfilters: 'a == 1'\n```\n");
        assert_eq!(good.query_blocks, 1);
        assert!(good.query_errors.is_empty());
        assert!(good.text.contains("filters: 'a == 1'"));

        let bad = body("```base\nfilters: [unclosed\n```\n");
        assert_eq!(bad.query_blocks, 0);
        assert_eq!(bad.query_errors.len(), 1);
    }

    #[test]
    fn code_fences_hide_caret_ids_and_links() {
        let b = body("```\n[[not-a-link]] ^fake-id\n```\n");
        assert!(b.wiki_links.is_empty());
        assert!(b.block_ids.is_empty());
    }

    #[test]
    fn fallback_title_is_first_level_one_heading() {
        assert_eq!(
            body("# Title\n\ntext\n").fallback_title.as_deref(),
            Some("Title")
        );
        assert_eq!(body("## only h2\n").fallback_title, None);
    }
}
