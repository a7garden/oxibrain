//! `pdc-html/1` body parsing (§6.2): the body is user-authored HTML source.
//! The source bytes are canonical; parsing here is a projection for indexing
//! only and never reserializes anything.
//!
//! Reader artifacts: safe plain-text extraction (tags stripped), stable block
//! targets (`id="b-<uuid>"`), canonical `pdc://document/…` links, managed
//! asset references, task items (`li.pdc-task` + `data-pdc-task-state`), and
//! unsafe-construct detection. Authored inline `<style>` and `style=`
//! attributes are ALLOWED (layout is a purpose of this profile) and are not
//! unsafe; active constructs (`<script>`, event handlers, `javascript:` /
//! `vbscript:` URLs, `<base>`, meta refresh, nested browsing contexts,
//! plugin embeds) are flagged `unsafe_content` but never fail the parse.
//! DOM nesting depth > 256 is enforced by the caller as
//! `document_too_complex`.

use super::{PdcBody, PdcLink, PdcTask};

/// Parse a `pdc-html/1` body slice (everything after the `-->` wrapper line).
///
/// One deterministic pass over the source. Structural problems land in
/// [`PdcBody::unsafe_constructs`] and never fail the parse; the caller turns
/// duplicate block ids and `container_depth > 256` into hard errors.
pub fn parse_html_body(src: &str) -> PdcBody {
    HtmlScan::new(src).run()
}

/// Open-element bookkeeping for one `li.pdc-task` whose text is still being
/// captured. `li_depth` counts nested `<li>` opens so the capture ends at the
/// task item's own `</li>`.
struct TaskCapture {
    completed: bool,
    id: Option<String>,
    /// Offset into [`HtmlScan::text`] where the item's text begins.
    text_start: usize,
    li_depth: usize,
}

/// Single-pass scanner accumulating the [`PdcBody`] artifacts.
struct HtmlScan<'a> {
    src: &'a str,
    /// Raw text under construction: entities decoded, markup stripped,
    /// script/style/comment content excluded, block tags separating words.
    /// Collapsed to single spaces once, at the end.
    text: String,
    block_ids: Vec<String>,
    document_links: Vec<PdcLink>,
    asset_refs: Vec<String>,
    tasks: Vec<PdcTask>,
    unsafe_constructs: Vec<String>,
    /// Lowercase names of currently open non-void elements (DOM depth model).
    open_elements: Vec<String>,
    container_depth: usize,
    open_tasks: Vec<TaskCapture>,
    /// Offset where the first `<h1>`'s text starts, while it is open.
    h1_start: Option<usize>,
    h1_text: Option<String>,
    /// Same capture slot for the `<title>` element.
    title_start: Option<usize>,
    title_text: Option<String>,
}

impl<'a> HtmlScan<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            text: String::with_capacity(src.len() / 2 + 16),
            block_ids: Vec::new(),
            document_links: Vec::new(),
            asset_refs: Vec::new(),
            tasks: Vec::new(),
            unsafe_constructs: Vec::new(),
            open_elements: Vec::new(),
            container_depth: 0,
            open_tasks: Vec::new(),
            h1_start: None,
            h1_text: None,
            title_start: None,
            title_text: None,
        }
    }

    fn run(mut self) -> PdcBody {
        let src = self.src;
        let bytes = src.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'<' if src[i..].starts_with("<!--") => {
                    // HTML comment: source content, never visible text (§6.2).
                    i = match src[i + 4..].find("-->") {
                        Some(end) => i + 4 + end + 3,
                        None => bytes.len(),
                    };
                }
                b'<' => match bytes.get(i + 1) {
                    // Doctype, CDATA section, or processing instruction.
                    Some(b'!') | Some(b'?') => {
                        i = match src[i..].find('>') {
                            Some(end) => i + end + 1,
                            None => bytes.len(),
                        };
                    }
                    Some(b'/') => i = self.scan_close_tag(i),
                    Some(c) if c.is_ascii_alphabetic() => i = self.scan_open_tag(i),
                    // Stray `<` (e.g. "a < b") is literal text.
                    _ => {
                        self.text.push('<');
                        i += 1;
                    }
                },
                b'&' => match decode_entity(&src[i..]) {
                    Some((ch, used)) => {
                        self.text.push(ch);
                        i += used;
                    }
                    None => {
                        self.text.push('&');
                        i += 1;
                    }
                },
                _ => {
                    let ch = src[i..]
                        .chars()
                        .next()
                        .expect("index is on a char boundary");
                    self.text.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        let HtmlScan {
            text,
            block_ids,
            document_links,
            asset_refs,
            tasks,
            unsafe_constructs,
            container_depth,
            h1_text,
            title_text,
            ..
        } = self;
        // Display fallback (§6.2 + envelope rules): first `<h1>` text, else
        // `<title>` text, else nothing. An empty capture is not a usable
        // display title, so it falls through to the next candidate.
        let fallback_title = match h1_text {
            Some(h1) if !h1.is_empty() => Some(h1),
            _ => title_text.filter(|t| !t.is_empty()),
        };
        PdcBody {
            text: collapse_spaces(&text),
            block_ids,
            document_links,
            asset_refs,
            tasks,
            unsafe_constructs,
            container_depth,
            fallback_title,
        }
    }

    /// Scan an open tag starting at `start` (`bytes[start] == b'<'` followed
    /// by an ASCII letter), process its attributes, and return the index just
    /// past the tag's `>`. `<script>`/`<style>` additionally consume their
    /// raw text content and end tag here, because tag scanning must never
    /// resume inside them.
    fn scan_open_tag(&mut self, start: usize) -> usize {
        let src = self.src;
        let bytes = src.as_bytes();
        let mut i = start + 1;
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            i += 1;
        }
        let name = src[name_start..i].to_ascii_lowercase();

        // Attributes as raw source slices: (name, value).
        let mut attrs: Vec<(&str, &str)> = Vec::new();
        loop {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let Some(&b) = bytes.get(i) else { break };
            if b == b'>' {
                i += 1;
                break;
            }
            if b == b'/' {
                // Stray slash or the self-closing flag. HTML5 keeps non-void
                // elements open across `/>`, so this never closes anything.
                i += 1;
                continue;
            }
            let attr_start = i;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && bytes[i] != b'='
                && bytes[i] != b'>'
                && bytes[i] != b'/'
            {
                i += 1;
            }
            let attr_name = &src[attr_start..i];
            let mut value = "";
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                match bytes.get(i) {
                    Some(q @ (b'"' | b'\'')) => {
                        let quote = *q;
                        i += 1;
                        let value_start = i;
                        while i < bytes.len() && bytes[i] != quote {
                            i += 1;
                        }
                        value = &src[value_start..i];
                        if i < bytes.len() {
                            i += 1; // closing quote
                        }
                    }
                    _ => {
                        let value_start = i;
                        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>'
                        {
                            i += 1;
                        }
                        value = &src[value_start..i];
                    }
                }
            }
            if !attr_name.is_empty() {
                attrs.push((attr_name, value));
            }
        }

        // Pass 1: element-level flags that later attribute decisions need.
        let mut embed_class = false;
        let mut task_class = false;
        let mut task_completed = false;
        let mut task_id: Option<String> = None;
        let mut meta_refresh = false;
        for (raw_name, raw_value) in &attrs {
            let key = raw_name.to_ascii_lowercase();
            let value = decode_entities(raw_value);
            match key.as_str() {
                "class" => {
                    embed_class |= value.split_whitespace().any(|c| c == "pdc-embed");
                    task_class |= value.split_whitespace().any(|c| c == "pdc-task");
                }
                "id" => {
                    // `b-…` ids are stable targets (§7.2); the caller checks
                    // for duplicates and enforces the UUID shape.
                    if value.starts_with("b-") {
                        self.block_ids.push(value.clone());
                    }
                    if !value.is_empty() {
                        task_id = Some(value);
                    }
                }
                "data-pdc-task-state" => task_completed = value == "completed",
                "http-equiv" => meta_refresh |= value.trim().eq_ignore_ascii_case("refresh"),
                _ => {}
            }
        }

        // Pass 2: per-attribute artifacts, in source order.
        for (raw_name, raw_value) in &attrs {
            let key = raw_name.to_ascii_lowercase();
            if key.starts_with("on") {
                self.unsafe_constructs
                    .push(format!("event-handler attribute `{key}` on <{name}>"));
                continue;
            }
            if !is_url_attribute(&key) {
                continue;
            }
            let value = decode_entities(raw_value);
            if let Some(scheme) = unsafe_url_scheme(&value) {
                self.unsafe_constructs
                    .push(format!("{scheme} URL in `{key}` attribute of <{name}>"));
            }
            if key == "href"
                && let Some((uuid, block)) = split_document_link(&value)
            {
                self.document_links.push(PdcLink {
                    uuid,
                    block,
                    embed: embed_class,
                });
            }
            if (key == "href" || key == "src")
                && let Some(digest) = asset_digest(&value)
            {
                self.asset_refs.push(digest);
            }
        }

        // Active constructs carried by the element itself.
        match name.as_str() {
            "script" => {
                self.unsafe_constructs.push("<script> element".to_string());
                return skip_raw_text(src, "script", i);
            }
            "style" => return skip_raw_text(src, "style", i),
            "base" => self.unsafe_constructs.push("<base> element".to_string()),
            "iframe" => self.unsafe_constructs.push("<iframe> element".to_string()),
            "object" => self.unsafe_constructs.push("<object> element".to_string()),
            "embed" => self.unsafe_constructs.push("<embed> element".to_string()),
            "frame" => self.unsafe_constructs.push("<frame> element".to_string()),
            _ => {}
        }
        if meta_refresh {
            self.unsafe_constructs
                .push("meta refresh (<meta http-equiv=\"refresh\">)".to_string());
        }

        if is_block_tag(&name) {
            self.text.push(' ');
        }
        if !is_void(&name) {
            self.open_elements.push(name.clone());
            let depth = self.open_elements.len();
            if depth > self.container_depth {
                self.container_depth = depth;
            }
        }
        if name == "li" {
            if let Some(open) = self.open_tasks.last_mut() {
                open.li_depth += 1;
            }
            if task_class {
                self.open_tasks.push(TaskCapture {
                    completed: task_completed,
                    id: task_id,
                    text_start: self.text.len(),
                    li_depth: 1,
                });
            }
        }
        if name == "h1" && self.h1_start.is_none() && self.h1_text.is_none() {
            self.h1_start = Some(self.text.len());
        }
        if name == "title" && self.title_text.is_none() {
            self.title_start = Some(self.text.len());
        }
        i
    }

    /// Scan an end tag starting at `start` (`bytes[start..]` begins with
    /// `</`), close any matching text capture, and pop the nearest matching
    /// open element. End-tag attributes are ignored, as in HTML5.
    fn scan_close_tag(&mut self, start: usize) -> usize {
        let src = self.src;
        let bytes = src.as_bytes();
        let mut i = start + 2;
        let name_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
            i += 1;
        }
        let name = src[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i] != b'>' {
            i += 1;
        }
        let next = (i + 1).min(bytes.len());

        if is_block_tag(&name) {
            self.text.push(' ');
        }
        if name == "li" {
            let mut finished = false;
            if let Some(open) = self.open_tasks.last_mut() {
                open.li_depth -= 1;
                finished = open.li_depth == 0;
            }
            if finished && let Some(done) = self.open_tasks.pop() {
                self.finish_task(done);
            }
        }
        if name == "h1"
            && let Some(begin) = self.h1_start.take()
        {
            self.h1_text = Some(collapse_spaces(&self.text[begin..]));
        }
        if name == "title"
            && let Some(begin) = self.title_start.take()
        {
            self.title_text = Some(collapse_spaces(&self.text[begin..]));
        }
        // Pop the nearest matching open element; unmatched closes are ignored.
        if let Some(pos) = self.open_elements.iter().rposition(|open| open == &name) {
            self.open_elements.truncate(pos);
        }
        next
    }

    fn finish_task(&mut self, capture: TaskCapture) {
        let raw = collapse_spaces(&self.text[capture.text_start..]);
        self.tasks.push(PdcTask {
            completed: capture.completed,
            text: strip_task_glyph(&raw).to_string(),
            id: capture.id,
        });
    }
}

/// HTML void elements never open a container, so they never push the element
/// stack.
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Block-level elements separate words in the indexable text, mirroring
/// [`crate::html::html_to_text`] so both HTML paths produce comparable FTS
/// text.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "hr"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "blockquote"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "pre"
            | "figure"
            | "figcaption"
            | "form"
            | "fieldset"
            | "address"
            | "html"
            | "body"
            | "head"
            | "title"
            | "details"
            | "summary"
            | "dialog"
    )
}

/// Attributes whose values browsers resolve as URLs. The unsafe-scheme check
/// covers all of them; only `href` carries document links, and only
/// `href`/`src` carry managed-asset references.
fn is_url_attribute(name: &str) -> bool {
    matches!(
        name,
        "href"
            | "src"
            | "action"
            | "formaction"
            | "poster"
            | "background"
            | "cite"
            | "longdesc"
            | "data"
            | "srcset"
            | "xlink:href"
    )
}

/// Decode one entity at the start of `s` (which begins with `&`). Returns the
/// decoded scalar and the consumed byte count. Covers the named forms
/// `&amp; &lt; &gt; &quot; &apos; &nbsp;` plus decimal `&#…;` and hexadecimal
/// `&#x…;`; anything else stays literal. The scan window fits the longest
/// valid numeric form (`&#4294967295;`).
fn decode_entity(s: &str) -> Option<(char, usize)> {
    let bytes = s.as_bytes();
    let limit = bytes.len().min(13);
    let mut semi = None;
    for (j, &b) in bytes.iter().enumerate().take(limit).skip(1) {
        match b {
            b';' => {
                semi = Some(j);
                break;
            }
            b'&' | b'<' => break,
            _ => {}
        }
    }
    let semi = semi?;
    let name = &s[1..semi];
    let ch = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = if let Some(hex) = digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                digits.parse::<u32>().ok()?
            };
            // Surrogates and out-of-range values keep the literal source.
            char::from_u32(code)?
        }
    };
    Some((ch, semi + 1))
}

/// Decode every entity in an attribute value or text run.
fn decode_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let rest = &value[i..];
        if rest.starts_with('&')
            && let Some((ch, used)) = decode_entity(rest)
        {
            out.push(ch);
            i += used;
            continue;
        }
        let ch = rest.chars().next().expect("index is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Unsafe URL scheme check with browser-comparable tolerance: leading C0
/// controls and spaces are stripped, ASCII tab/CR/LF are removed anywhere,
/// and the scheme is ASCII case-insensitive. Returns the offending scheme
/// for the report. Ordinary internal spaces are kept, as browsers do.
fn unsafe_url_scheme(decoded: &str) -> Option<&'static str> {
    let compact: String = decoded
        .trim_start_matches(|c: char| c.is_ascii() && c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let lower = compact.to_ascii_lowercase();
    if lower.starts_with("javascript:") {
        Some("javascript:")
    } else if lower.starts_with("vbscript:") {
        Some("vbscript:")
    } else {
        None
    }
}

/// `pdc://document/<uuid>[#b-<block>]` → `(uuid, block-with-b-prefix)`.
/// Only `b-` fragments are stable targets (§7.2); any other fragment still
/// leaves a plain document link. Canonical spelling only: the scheme and
/// host segments are matched exactly as written.
fn split_document_link(decoded: &str) -> Option<(String, Option<String>)> {
    let rest = decoded.strip_prefix("pdc://document/")?;
    let (uuid, fragment) = match rest.split_once('#') {
        Some((uuid, fragment)) => (uuid, Some(fragment)),
        None => (rest, None),
    };
    if uuid.is_empty() {
        return None;
    }
    let block = fragment
        .filter(|fragment| fragment.starts_with("b-"))
        .map(str::to_string);
    Some((uuid.to_string(), block))
}

/// 64-hex digest of a `pdc://asset/sha256/<digest>` URI (§8.2), taken up to
/// an optional fragment or query.
fn asset_digest(decoded: &str) -> Option<String> {
    let rest = decoded.strip_prefix("pdc://asset/sha256/")?;
    let end = rest.find(['#', '?']).unwrap_or(rest.len());
    let digest = &rest[..end];
    if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(digest.to_string())
    } else {
        None
    }
}

/// Consume script/style raw text: everything up to and including the matching
/// case-insensitive end tag's `>`. A missing end tag consumes the rest of the
/// input, as browsers do.
fn skip_raw_text(src: &str, name: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    let needle = name.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' && bytes.get(i + 1) == Some(&b'/') {
            let after = i + 2;
            if after + needle.len() <= bytes.len()
                && bytes[after..after + needle.len()].eq_ignore_ascii_case(needle)
            {
                let mut k = after + needle.len();
                let boundary = k >= bytes.len()
                    || bytes[k].is_ascii_whitespace()
                    || bytes[k] == b'/'
                    || bytes[k] == b'>';
                if boundary {
                    while k < bytes.len() && bytes[k] != b'>' {
                        k += 1;
                    }
                    return (k + 1).min(bytes.len());
                }
            }
        }
        i += 1;
    }
    bytes.len()
}

/// Collapse whitespace runs to single spaces and trim, so block tags acting
/// as separators yield deterministic indexable text.
fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Drop the leading `☐`/`☑` fallback glyph (§9.1) from a task label. Labels
/// written without the glyph are kept verbatim.
fn strip_task_glyph(label: &str) -> &str {
    let trimmed = label.trim_start();
    match trimmed
        .strip_prefix('☐')
        .or_else(|| trimmed.strip_prefix('☑'))
    {
        Some(rest) => rest.trim_start(),
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantics_fixture_artifacts() {
        let src = "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <title>HTML semantics</title>\n  <style>article { max-width: 42rem; margin: auto; }</style>\n</head>\n<body>\n  <article>\n    <h1 id=\"b-018f47c6-7dbe-7a14-9f67-6f89a5e3c170\">HTML semantics</h1>\n    <p><a href=\"pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db\">Related document</a></p>\n    <ul><li class=\"pdc-task\" data-pdc-task-state=\"open\" id=\"b-018f47c6-c718-728c-9d91-b2bc70081170\">☐ Open task</li></ul>\n    <img src=\"pdc://asset/sha256/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\" alt=\"Diagram\">\n  </article>\n</body>\n</html>\n";
        let body = parse_html_body(src);
        assert!(
            body.block_ids
                .contains(&"b-018f47c6-7dbe-7a14-9f67-6f89a5e3c170".to_string()),
            "h1 id is a stable target: {:?}",
            body.block_ids
        );
        assert_eq!(body.document_links.len(), 1);
        assert_eq!(body.tasks.len(), 1);
        assert!(!body.tasks[0].completed);
        assert!(body.tasks[0].text.contains("Open task"));
        assert_eq!(
            body.asset_refs,
            vec!["0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string()]
        );
        assert!(
            body.unsafe_constructs.is_empty(),
            "inline style is authored layout, not unsafe"
        );
        assert_eq!(body.fallback_title.as_deref(), Some("HTML semantics"));
        assert!(body.text.contains("Related document"));
        assert!(
            !body.text.contains('<'),
            "tags must not leak: {:?}",
            body.text
        );
    }

    #[test]
    fn unsafe_constructs_are_flagged_but_parse_succeeds() {
        let src = "<!doctype html>\n<html><body onclick=\"alert(1)\"><script>alert(1)</script><a href=\"javascript:alert(1)\">Unsafe link</a></body></html>\n";
        let body = parse_html_body(src);
        assert!(
            !body.unsafe_constructs.is_empty(),
            "script/event-handler/javascript: flagged"
        );
        assert!(body.text.contains("Unsafe link"), "labels stay readable");
        assert!(!body.text.contains("alert(1)</script>"));
    }

    #[test]
    fn completed_task_state() {
        let src =
            "<ul><li class=\"pdc-task\" data-pdc-task-state=\"completed\">☑ Done task</li></ul>";
        let body = parse_html_body(src);
        assert_eq!(body.tasks.len(), 1);
        assert!(body.tasks[0].completed);
    }

    #[test]
    fn duplicate_block_ids_are_reported_to_caller() {
        let src = "<h1 id=\"b-018f47c6-9c8a-79af-b060-7a6a3b79016c\">A</h1><h2 id=\"b-018f47c6-9c8a-79af-b060-7a6a3b79016c\">B</h2>";
        let body = parse_html_body(src);
        assert_eq!(body.block_ids.len(), 2, "caller dedupes and errors");
    }

    #[test]
    fn title_fallback_prefers_h1_over_title() {
        let src =
            "<html><head><title>Doc title</title></head><body><h1>First heading</h1></body></html>";
        let body = parse_html_body(src);
        assert_eq!(body.fallback_title.as_deref(), Some("First heading"));
        let src2 =
            "<html><head><title>Doc title</title></head><body><p>no heading</p></body></html>";
        assert_eq!(
            parse_html_body(src2).fallback_title.as_deref(),
            Some("Doc title")
        );
    }

    #[test]
    fn deep_dom_depth_is_counted() {
        let mut src = String::new();
        for _ in 0..257 {
            src.push_str("<div>");
        }
        src.push_str("<p>x</p>");
        for _ in 0..257 {
            src.push_str("</div>");
        }
        let body = parse_html_body(&src);
        assert_eq!(body.container_depth, 258, "div stack plus the p");
    }

    #[test]
    fn embed_links_and_block_fragments() {
        let src = "<a class=\"pdc-embed\" href=\"pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db#b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32\">Embedded</a><a href=\"pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db\">Plain</a>";
        let body = parse_html_body(src);
        assert_eq!(body.document_links.len(), 2);
        assert!(body.document_links[0].embed);
        assert_eq!(
            body.document_links[0].block.as_deref(),
            Some("b-018f47c6-7dbe-7a14-9f67-6f89a5e3cc32")
        );
        assert!(!body.document_links[1].embed);
        assert_eq!(body.document_links[1].block, None);
    }

    #[test]
    fn comments_script_and_style_content_stay_out_of_text() {
        let src = "<p>before</p><!-- <p>hidden note</p> --><script>var x = \"<p>not text</p>\";</script><style>p { color: red }</style><p>after</p>";
        let body = parse_html_body(src);
        assert_eq!(body.text, "before after");
        assert_eq!(body.unsafe_constructs.len(), 1, "script flagged, style not");
        assert!(body.unsafe_constructs[0].contains("<script>"));
    }

    #[test]
    fn entities_decode_in_text_and_attribute_values() {
        let src = "<p>Fish &amp; chips &lt;3 &#x1F600;</p><a href=\"pdc://document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db\">A &quot;quote&quot;</a>";
        let body = parse_html_body(src);
        assert!(
            body.text.contains("Fish & chips <3 \u{1F600}"),
            "{:?}",
            body.text
        );
        assert!(body.text.contains("A \"quote\""));
        // Numeric entity hiding the scheme must not hide the link.
        let encoded = "<a href=\"pdc&#58;//document/018f47c6-4a77-7c52-9db8-0e5f9bcb17db\">x</a>";
        assert_eq!(parse_html_body(encoded).document_links.len(), 1);
    }

    #[test]
    fn unsafe_url_schemes_are_whitespace_and_case_tolerant() {
        let src = "<a href=\" jAvA\tscript:alert(1)\">x</a><a href=\"vbscript:MsgBox\">y</a>\
                   <a href=\"\njavascript:alert(1)\">z</a><a href=\"https://example.com\">ok</a>\
                   <a href=\"not javascript:x\">ok2</a>";
        let body = parse_html_body(src);
        assert_eq!(
            body.unsafe_constructs.len(),
            3,
            "{:?}",
            body.unsafe_constructs
        );
        assert!(
            body.unsafe_constructs
                .iter()
                .any(|e| e.starts_with("javascript:"))
        );
        assert!(
            body.unsafe_constructs
                .iter()
                .any(|e| e.starts_with("vbscript:"))
        );
        assert!(body.document_links.is_empty());
    }

    #[test]
    fn void_elements_and_unmatched_closes_do_not_count_depth() {
        let src = "<div><img src=\"x.png\"><br><input type=\"text\"></div></div></div><p></p>";
        let body = parse_html_body(src);
        assert_eq!(body.container_depth, 1);
    }

    #[test]
    fn nested_browsing_and_plugin_elements_are_flagged() {
        let src = "<base href=\"x\"><meta http-equiv=\"Refresh\" content=\"5\"><iframe></iframe>\
                   <object data=\"a\"></object><embed src=\"b\"><frame src=\"c\">";
        let body = parse_html_body(src);
        let joined = body.unsafe_constructs.join("; ");
        for needle in [
            "<base>",
            "meta refresh",
            "<iframe>",
            "<object>",
            "<embed>",
            "<frame>",
        ] {
            assert!(joined.contains(needle), "missing {needle}: {joined}");
        }
    }

    #[test]
    fn task_text_strips_glyph_and_keeps_id() {
        let src = "<ul><li class=\"pdc-task\" data-pdc-task-state=\"completed\" id=\"b-018f47c6-c718-728c-9d91-b2bc700814bb\">☑ Buy oat milk</li><li>plain item</li></ul>";
        let body = parse_html_body(src);
        assert_eq!(body.tasks.len(), 1, "plain li is not a task");
        assert_eq!(body.tasks[0].text, "Buy oat milk");
        assert!(body.tasks[0].completed);
        assert_eq!(
            body.tasks[0].id.as_deref(),
            Some("b-018f47c6-c718-728c-9d91-b2bc700814bb")
        );
    }

    #[test]
    fn task_without_glyph_keeps_text() {
        let src = "<li class=\"pdc-task\" data-pdc-task-state=\"open\">Call mother</li>";
        let body = parse_html_body(src);
        assert_eq!(body.tasks[0].text, "Call mother");
        assert!(!body.tasks[0].completed);
        assert_eq!(body.tasks[0].id, None);
    }
}
