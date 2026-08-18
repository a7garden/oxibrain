//! HTML note primitives: frontmatter-in-comment parsing and text extraction.
//!
//! Mirrors oximemo's HTML note contract (spec §3) so oxibrain can ingest notes
//! written by oximemo:
//!
//! ```html
//! <!--
//! +++
//! id = "…"
//! +++
//! -->
//! <h1>Title</h1>
//! ```
//!
//! All functions are pure text scanners — no external HTML parser, keeping the
//! connector dependency footprint flat. The goal is indexing-quality text
//! extraction (FTS + downstream recall), not DOM fidelity.

/// Result of splitting an HTML note into frontmatter + body.
#[derive(Debug, PartialEq, Eq)]
pub enum HtmlFrontmatterSplit<'a> {
    /// A leading comment containing a `+++ … +++` TOML block.
    Some { toml_text: &'a str, body: &'a str },
    /// No frontmatter comment. The whole content is the body.
    None { body: &'a str },
}

/// Split an HTML note's content into frontmatter + body.
///
/// Rules:
/// 1. Leading whitespace is skipped.
/// 2. If the content does not start with `<!--`, there is no frontmatter.
/// 3. The comment runs to the first `-->`. Its inner text must start with a
///    `+++` line and contain a closing `+++` line to count as frontmatter
///    (a plain comment — e.g. a normal web page's license banner — does not).
/// 4. The body is everything after the comment, with one leading newline
///    trimmed.
pub fn split_frontmatter(content: &str) -> HtmlFrontmatterSplit<'_> {
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("<!--") else {
        return HtmlFrontmatterSplit::None { body: content };
    };
    let Some(end) = rest.find("-->") else {
        // Unterminated comment: treat the whole file as body — the file is
        // malformed HTML, but we never lose user content.
        return HtmlFrontmatterSplit::None { body: content };
    };
    let inner = &rest[..end];
    let after = &rest[end + 3..];
    let body = after.strip_prefix('\n').unwrap_or(after);

    // The inner text must be a `+++ … +++` block.
    let inner_trimmed = inner.trim_start_matches('\n');
    let Some(toml_text) = inner_trimmed.strip_prefix("+++\n") else {
        return HtmlFrontmatterSplit::None { body: content };
    };
    let toml_text = match toml_text.strip_suffix("+++\n") {
        Some(t) => t,
        None => match toml_text.strip_suffix("+++") {
            Some(t) => t.strip_suffix('\n').unwrap_or(t),
            None => return HtmlFrontmatterSplit::None { body: content },
        },
    };

    HtmlFrontmatterSplit::Some { toml_text, body }
}

/// Extract indexable plain text from HTML: comments, `<script>` and
/// `<style>` contents are dropped, tags are stripped, entities decoded,
/// and runs of whitespace collapsed to single spaces (block tags act as
/// word separators via the surrounding whitespace they introduce).
pub fn html_to_text(html: &str) -> String {
    let no_comments = strip_comments(html);
    let mut out = String::with_capacity(no_comments.len());
    let chars: Vec<char> = no_comments.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            // Find the tag name.
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let name_start = j;
            while j < chars.len()
                && (chars[j].is_alphanumeric()
                    || chars[j] == '-'
                    || chars[j] == '!'
                    || chars[j] == '/')
            {
                j += 1;
            }
            let name: String = chars[name_start..j]
                .iter()
                .filter(|c| c.is_alphanumeric() || **c == '-')
                .collect::<String>()
                .to_ascii_lowercase();
            // Consume up to '>'.
            let mut k = j;
            while k < chars.len() && chars[k] != '>' {
                k += 1;
            }
            if k >= chars.len() {
                // Unterminated tag: drop the rest.
                break;
            }
            // For script/style, skip until the matching closing tag.
            if name == "script" || name == "style" {
                let close = format!("</{name}");
                let rest: String = chars[k + 1..].iter().collect();
                if let Some(pos) = rest.to_ascii_lowercase().find(&close) {
                    // Continue scanning after the closing tag's '>'.
                    let after_close = k + 1 + rest[..pos].chars().count() + close.chars().count();
                    let mut m = after_close;
                    while m < chars.len() && chars[m] != '>' {
                        m += 1;
                    }
                    i = if m < chars.len() { m + 1 } else { chars.len() };
                    // Block elements separate words.
                    out.push(' ');
                } else {
                    break; // No closing tag: drop the rest.
                }
            } else {
                i = k + 1;
                // Block-level tags act as separators.
                if is_block_tag(&name) {
                    out.push(' ');
                }
            }
        } else if c == '&' {
            // Entity: try named/numeric forms.
            let (decoded, next) = decode_entity(&chars[i..]);
            match decoded {
                Some(text) => {
                    out.push_str(&text);
                    i += next;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    // Collapse whitespace runs into single spaces, then trim.
    let mut collapsed = String::with_capacity(out.len());
    let mut last_space = true; // leading trim
    for c in out.chars() {
        if c.is_whitespace() {
            if !last_space {
                collapsed.push(' ');
                last_space = true;
            }
        } else {
            collapsed.push(c);
            last_space = false;
        }
    }
    collapsed.trim_end().to_string()
}

/// Remove all HTML comments (`<!-- … -->`). Unterminated comments swallow
/// the remainder. Used before wiki-link scanning so frontmatter comments
/// cannot hide `[[…]]` text.
pub fn strip_comments(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' && html[i..].starts_with("<!--") {
            match html[i + 4..].find("-->") {
                Some(end) => {
                    i = i + 4 + end + 3;
                }
                None => break,
            }
        } else {
            let ch = html[i..].chars().next().expect("non-empty slice");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Convert raw HTML note bytes into the indexable form oxibrain stores in
/// `MarkdownFile::content`:
///
/// 1. Frontmatter comment is stripped (TOML is metadata — `ingest_note`
///    ingests only the body).
/// 2. The remaining body is run through [`html_to_text`] so FTS does not see
///    tags, scripts, styles, or entities.
///
/// If the file does not begin with a frontmatter comment, the whole content
/// is treated as body and run through [`html_to_text`].
pub fn html_note_to_text(content: &str) -> String {
    let body = match split_frontmatter(content) {
        HtmlFrontmatterSplit::Some { body, .. } => body,
        HtmlFrontmatterSplit::None { body } => body,
    };
    html_to_text(body)
}

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

/// Decode an HTML entity at the start of `chars` (which begins with `&`).
/// Returns the decoded text and the number of chars consumed, or `None`
/// when the sequence is not a recognized entity.
fn decode_entity(chars: &[char]) -> (Option<String>, usize) {
    let max = chars.len().min(12);
    let mut semi = None;
    for (j, &c) in chars.iter().enumerate().take(max).skip(1) {
        if c == ';' {
            semi = Some(j);
            break;
        }
        if c == '&' || c == '<' {
            break;
        }
    }
    let Some(semi) = semi else {
        return (None, 0);
    };
    let name: String = chars[1..semi].iter().collect();
    let decoded = match name.as_str() {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        "nbsp" => Some("\u{a0}".to_string()),
        _ => {
            if let Some(num) = name.strip_prefix('#') {
                let code =
                    if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                code.and_then(char::from_u32).map(|c| c.to_string())
            } else {
                None
            }
        }
    };
    match decoded {
        Some(text) => (Some(text), semi + 1),
        None => (None, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<!--\n+++\nid = \"x\"\n+++\n-->\n<h1>제목</h1>\n<p>본문</p>";

    #[test]
    fn split_frontmatter_roundtrip() {
        // The parser preserves the trailing newline of the TOML block
        // (between the inner `+++\n` opener and the closing `+++`).
        let toml = "id = \"01957a3b\"\ncreated_at = \"2026-08-18T14:30:52Z\"";
        let body = "<h1>제목</h1>\n<p>본문</p>";
        let serialized = format!("<!--\n+++\n{toml}\n+++\n-->\n{body}");
        match split_frontmatter(&serialized) {
            HtmlFrontmatterSplit::Some { toml_text, body: b } => {
                assert_eq!(toml_text, format!("{toml}\n"));
                assert_eq!(b, body);
            }
            HtmlFrontmatterSplit::None { .. } => panic!("expected frontmatter"),
        }
    }

    #[test]
    fn split_frontmatter_plain_comment_is_not_frontmatter() {
        // License banner style: comment without +++ fences.
        let content = "<!-- (c) 2026 -->\n<h1>hi</h1>";
        match split_frontmatter(content) {
            HtmlFrontmatterSplit::None { body } => assert_eq!(body, content),
            HtmlFrontmatterSplit::Some { .. } => panic!("plain comment must not be frontmatter"),
        }
    }

    #[test]
    fn split_frontmatter_unterminated_is_not_frontmatter() {
        let content = "<!-- forever\n<h1>hi</h1>";
        match split_frontmatter(content) {
            HtmlFrontmatterSplit::None { body } => assert_eq!(body, content),
            HtmlFrontmatterSplit::Some { .. } => panic!("unterminated must not be frontmatter"),
        }
    }

    #[test]
    fn split_frontmatter_skips_leading_whitespace() {
        let content = "   \n<!--\n+++\nk = 1\n+++\n-->\nbody";
        match split_frontmatter(content) {
            HtmlFrontmatterSplit::Some { toml_text, body } => {
                assert_eq!(toml_text, "k = 1\n");
                assert_eq!(body, "body");
            }
            HtmlFrontmatterSplit::None { .. } => panic!("expected frontmatter"),
        }
    }

    #[test]
    fn to_text_drops_script_and_style_contents() {
        let html = "<style>p { color: red }</style><p>keep</p><script>bad()</script><p>this</p>";
        assert_eq!(html_to_text(html), "keep this");
    }

    #[test]
    fn to_text_drops_comments() {
        assert_eq!(html_to_text("<p>a</p><!-- hidden -->"), "a");
    }

    #[test]
    fn to_text_named_entities() {
        // Spec §3.3 named-entity set: &amp; &lt; &gt; &quot; &#39; &nbsp;.
        // &nbsp; is non-breaking space and is treated as whitespace by the
        // collapser, so we assert the other five decoded characters plus
        // that &nbsp; does not panic and contributes a space.
        assert_eq!(
            html_to_text("&amp; &lt; &gt; &quot; &#39;"),
            "& < > \" '",
            "named entities must decode to their canonical characters"
        );
        assert_eq!(
            html_to_text("a&nbsp;b"),
            "a b",
            "&nbsp; must be treated as whitespace"
        );
    }

    #[test]
    fn to_text_numeric_entities() {
        assert_eq!(html_to_text("&#65;&#x42;"), "AB");
    }

    #[test]
    fn to_text_separates_inline_tags() {
        assert_eq!(html_to_text("<b>bold</b><i>it</i>"), "boldit");
        assert_eq!(html_to_text("<b>bold</b> <i>it</i>"), "bold it");
    }

    #[test]
    fn to_text_unterminated_comment_drops_rest() {
        assert_eq!(html_to_text("a<!-- forever"), "a");
    }

    #[test]
    fn html_note_to_text_strips_frontmatter() {
        let text = html_note_to_text(SAMPLE);
        // Frontmatter gone, only body text remains.
        assert!(text.contains("제목"));
        assert!(text.contains("본문"));
        assert!(!text.contains("+++\n"));
        assert!(!text.contains("id ="));
    }

    #[test]
    fn html_note_to_text_whole_file_when_no_frontmatter() {
        let content = "<p>plain html</p>";
        assert_eq!(html_note_to_text(content), "plain html");
    }

    #[test]
    fn strip_comments_removes_all() {
        assert_eq!(strip_comments("<!--a--><p>x</p><!--b-->"), "<p>x</p>");
    }
}
