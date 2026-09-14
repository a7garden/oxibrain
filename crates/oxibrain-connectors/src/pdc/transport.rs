//! PDC file transports (§4): the Djot `---` frontmatter transport and the
//! HTML `<!-- … -->` comment transport, plus the shared transport rules
//! (UTF-8, no BOM, 4 MiB cap).
//!
//! Contract pins (corpus revision 3):
//! - a UTF-8 BOM is `invalid_transport` and is never stripped;
//! - frontmatter-free `.djot` is valid upstream Djot but `invalid_transport`;
//! - the serialized HTML envelope must not contain an earlier `-->` or `--!>`
//!   (an HTML parser could close the comment before the PDC parser does);
//! - `.html` files without the exact opening transport are visible legacy
//!   HTML, not malformed PDC.

use super::diagnostic::{PdcDiagnostic, PdcDiagnosticCode};
use super::{HtmlClassification, MarkdownClassification, PDC_MAX_DOCUMENT_BYTES};

/// A document split into its envelope source and body. `envelope_src` is the
/// text between the two transport markers with CRLF normalized to LF (the
/// contract allows envelope-only normalization; the body keeps its bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportSplit {
    pub envelope_src: String,
    pub body: String,
}

fn transport_error(reason: impl Into<String>) -> PdcDiagnostic {
    PdcDiagnostic::new(PdcDiagnosticCode::InvalidTransport, reason)
}
/// UTF-8 byte-order mark (`U+FEFF`). A canonical document never starts with
/// one, and a Reader must reject it rather than strip it (§4.1).
const UTF8_BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

/// One physical source line: its text with the CRLF already normalized away,
/// plus the byte offset where the line starts (for slicing the untouched body
/// back out of the original text).
struct Line<'a> {
    text: &'a str,
    start: usize,
}

/// Split into physical lines on `\n`, accepting `\r\n`. A trailing newline
/// does not produce an extra empty line; empty input produces no lines.
fn scan_lines(s: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let rest = &s[start..];
        let end = match rest.find('\n') {
            Some(nl) => start + nl + 1,
            None => s.len(),
        };
        let raw = &s[start..end];
        let no_lf = raw.strip_suffix('\n').unwrap_or(raw);
        let text = no_lf.strip_suffix('\r').unwrap_or(no_lf);
        lines.push(Line { text, start });
        start = end;
    }
    lines
}

/// First line of `bytes` without its terminator, plus the remainder after it.
/// Only inspects up to the next `\n`, so repeated calls never read past the
/// second line of a document.
fn next_line(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b'\n') {
        Some(nl) => {
            let line = &bytes[..nl];
            (line.strip_suffix(b"\r").unwrap_or(line), &bytes[nl + 1..])
        }
        None => (bytes, &[]),
    }
}

/// Canonical documents are UTF-8 (§4.1); anything else is `invalid_transport`.
fn as_utf8(bytes: &[u8]) -> Result<&str, PdcDiagnostic> {
    std::str::from_utf8(bytes).map_err(|_| transport_error("document is not valid UTF-8"))
}

/// `invalid_transport` with a 1-based source line.
fn transport_at(reason: impl Into<String>, line: u32) -> PdcDiagnostic {
    PdcDiagnostic::at(PdcDiagnosticCode::InvalidTransport, reason, line, 1)
}

/// Reject a UTF-8 byte-order mark (`invalid_transport`, never stripped).
pub fn reject_bom(bytes: &[u8]) -> Result<(), PdcDiagnostic> {
    if bytes.starts_with(&UTF8_BOM) {
        return Err(transport_error(
            "document starts with a UTF-8 byte-order mark; a Reader must reject it, not strip it",
        ));
    }
    Ok(())
}

/// Enforce the 4 MiB document size cap (envelope transport + body).
pub fn check_size(bytes: &[u8]) -> Result<(), PdcDiagnostic> {
    if bytes.len() > PDC_MAX_DOCUMENT_BYTES {
        return Err(PdcDiagnostic::new(
            PdcDiagnosticCode::DocumentTooLarge,
            format!(
                "document is {} bytes; the contract caps a canonical document at {} bytes (4 MiB)",
                bytes.len(),
                PDC_MAX_DOCUMENT_BYTES
            ),
        ));
    }
    Ok(())
}

/// Split the Djot transport: first line exactly `---`, a later line exactly
/// `---` closes the envelope, the remainder is the body. A file with no
/// envelope lines at all (frontmatter-free Djot) or without a closer is
/// `invalid_transport`. CRLF is normalized in the envelope slice only.
pub fn split_djot(bytes: &[u8]) -> Result<TransportSplit, PdcDiagnostic> {
    let s = as_utf8(bytes)?;
    let lines = scan_lines(s);
    if lines.first().is_none_or(|l| l.text != "---") {
        return Err(transport_at(
            "first line must be exactly `---` (frontmatter-free Djot is not a PDC transport)",
            1,
        ));
    }
    let Some(close) = lines
        .iter()
        .skip(1)
        .position(|l| l.text == "---")
        .map(|i| i + 1)
    else {
        return Err(transport_error("envelope closer `---` line not found"));
    };
    let body_start = lines.get(close + 1).map_or(s.len(), |l| l.start);
    Ok(TransportSplit {
        envelope_src: lines[1..close]
            .iter()
            .map(|l| l.text)
            .collect::<Vec<_>>()
            .join("\n"),
        body: s[body_start..].to_string(),
    })
}

/// Split the Markdown transport (`pdc-markdown/1`, PDC 2 §4.2): the first
/// line is exactly `---` at byte 0, a later line exactly `---` closes the
/// envelope, the remainder is the Markdown body. A missing closer is
/// `invalid_transport` ("missing or unclosed envelope"). CRLF is normalized
/// in the envelope slice only.
pub fn split_markdown(bytes: &[u8]) -> Result<TransportSplit, PdcDiagnostic> {
    let s = as_utf8(bytes)?;
    let lines = scan_lines(s);
    if lines.first().is_none_or(|l| l.text != "---") {
        return Err(transport_at(
            "first line must be exactly `---` (the Markdown transport is a fenced frontmatter block)",
            1,
        ));
    }
    let Some(close) = lines
        .iter()
        .skip(1)
        .position(|l| l.text == "---")
        .map(|i| i + 1)
    else {
        return Err(transport_error(
            "unclosed Markdown frontmatter: no closing `---` line",
        ));
    };
    let body_start = lines.get(close + 1).map_or(s.len(), |l| l.start);
    Ok(TransportSplit {
        envelope_src: lines[1..close]
            .iter()
            .map(|l| l.text)
            .collect::<Vec<_>>()
            .join("\n"),
        body: s[body_start..].to_string(),
    })
}

/// Cheap classification of raw `.md` bytes (PDC 2 §3.2/§4.2): a PDC Markdown
/// transport attempt, or visible plain Markdown. A file starts a PDC attempt
/// when (after an optional BOM, which the parse then rejects per §4.1) its
/// first line is exactly `---` and a closer is present. The attempt is only
/// routed to the PDC parser when the envelope declares a `pdc-document/*`
/// `format`; any other frontmatter is ordinary user YAML and stays plain
/// Markdown. An opener without a closer is still an attempt (the transport
/// requires one) so the diagnostic surfaces instead of silently indexing.
pub fn sniff_markdown(bytes: &[u8]) -> MarkdownClassification {
    let candidate = bytes.strip_prefix(&UTF8_BOM).unwrap_or(bytes);
    let Ok(s) = std::str::from_utf8(candidate) else {
        return MarkdownClassification::Legacy;
    };
    let lines = scan_lines(s);
    if lines.first().is_none_or(|l| l.text != "---") {
        return MarkdownClassification::Legacy;
    }
    let Some(close) = lines
        .iter()
        .skip(1)
        .position(|l| l.text == "---")
        .map(|i| i + 1)
    else {
        // The transport shape demands a closer; its absence is a malformed
        // PDC attempt, not plain Markdown.
        return MarkdownClassification::Pdc;
    };
    let envelope = lines[1..close]
        .iter()
        .map(|l| l.text)
        .collect::<Vec<_>>()
        .join("\n");
    match super::envelope::observed_format(&envelope) {
        Some(v) if v.starts_with("pdc-document/") => MarkdownClassification::Pdc,
        _ => MarkdownClassification::Legacy,
    }
}

/// Cheap classification of raw `.html` bytes: canonical PDC HTML transport
/// (first line exactly `<!--` AND second line exactly `---`) or visible
/// legacy HTML. I/O-free; only inspects the first bytes.
pub fn sniff_html(bytes: &[u8]) -> HtmlClassification {
    let (first, rest) = next_line(bytes);
    if first != b"<!--" {
        return HtmlClassification::Legacy;
    }
    let (second, _) = next_line(rest);
    if second == b"---" {
        HtmlClassification::Pdc
    } else {
        HtmlClassification::Legacy
    }
}

/// Split the HTML comment transport: first line exactly `<!--`, second line
/// exactly `---`, a later exact `---` line closes the envelope, and the
/// immediately following line is exactly `-->`. The serialized envelope
/// between the opening `<!--` line and the closing `-->` line must not
/// contain `-->` or `--!>`. Anything else is `invalid_transport`.
///
/// The body is the bytes after the line ending following `-->` (a canonical
/// writer emits that line ending even for an empty body). CRLF is normalized
/// in the envelope slice only.
pub fn split_html(bytes: &[u8]) -> Result<TransportSplit, PdcDiagnostic> {
    let s = as_utf8(bytes)?;
    let lines = scan_lines(s);
    if lines.first().is_none_or(|l| l.text != "<!--") {
        return Err(transport_at("first line must be exactly `<!--`", 1));
    }
    if lines.get(1).is_none_or(|l| l.text != "---") {
        return Err(transport_at("second line must be exactly `---`", 2));
    }
    let Some(close) = lines
        .iter()
        .skip(2)
        .position(|l| l.text == "---")
        .map(|i| i + 2)
    else {
        return Err(transport_error("envelope closer `---` line not found"));
    };
    match lines.get(close + 1) {
        Some(l) if l.text == "-->" => {}
        Some(_) => {
            return Err(transport_at(
                "expected a `-->` line immediately after the envelope closer",
                (close + 2) as u32,
            ));
        }
        None => {
            return Err(transport_error(
                "document ends before the closing `-->` line",
            ));
        }
    }
    // Premature close: between the opening `<!--` line and the wrapper's
    // closing `-->` line, any literal `-->` or `--!>` would let an HTML
    // parser terminate the comment before the PDC parser does (§4.3). The
    // delimiter lines themselves are exact matches, so scanning the envelope
    // interior line by line covers the whole serialized envelope; newlines
    // keep the sequences from spanning lines.
    for (i, line) in lines[2..close].iter().enumerate() {
        if line.text.contains("-->") || line.text.contains("--!>") {
            return Err(transport_at(
                "envelope contains `-->` or `--!>`, which closes the HTML comment before the PDC parser does",
                (i + 3) as u32,
            ));
        }
    }
    let body_start = lines.get(close + 2).map_or(s.len(), |l| l.start);
    Ok(TransportSplit {
        envelope_src: lines[2..close]
            .iter()
            .map(|l| l.text)
            .collect::<Vec<_>>()
            .join("\n"),
        body: s[body_start..].to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_is_rejected_not_stripped() {
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(b"---\nformat: x\n---\n");
        let err = reject_bom(&bytes).unwrap_err();
        assert_eq!(err.code, PdcDiagnosticCode::InvalidTransport);
    }

    #[test]
    fn size_cap_is_enforced() {
        let ok = vec![b'a'; PDC_MAX_DOCUMENT_BYTES];
        assert!(check_size(&ok).is_ok());
        let too_big = vec![b'a'; PDC_MAX_DOCUMENT_BYTES + 1];
        assert_eq!(
            check_size(&too_big).unwrap_err().code,
            PdcDiagnosticCode::DocumentTooLarge
        );
    }

    #[test]
    fn djot_transport_splits_and_rejects_frontmatter_free() {
        let doc = b"---\nformat: pdc-document/1\n---\n# Body\n";
        let split = split_djot(doc).unwrap();
        assert_eq!(split.envelope_src, "format: pdc-document/1");
        assert_eq!(split.body, "# Body\n");

        let free = b"# Not a PDC document\n\nplain djot\n";
        assert_eq!(
            split_djot(free).unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );

        let unclosed = b"---\nformat: pdc-document/1\n# never closed\n";
        assert_eq!(
            split_djot(unclosed).unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
    }

    #[test]
    fn html_transport_sniffs_and_splits() {
        let legacy = b"<!doctype html>\n<html><body>legacy</body></html>\n";
        assert_eq!(sniff_html(legacy), HtmlClassification::Legacy);

        let doc = b"<!--\n---\nformat: pdc-document/1\n---\n-->\n<h1>hi</h1>\n";
        assert_eq!(sniff_html(doc), HtmlClassification::Pdc);
        let split = split_html(doc).unwrap();
        assert_eq!(split.envelope_src, "format: pdc-document/1");
        assert_eq!(split.body, "<h1>hi</h1>\n");
    }

    #[test]
    fn premature_comment_close_is_invalid_transport() {
        let doc = b"<!--\n---\nformat: pdc-document/1\ntitle: premature --> close\n---\n-->\n<h1>x</h1>\n";
        assert_eq!(
            split_html(doc).unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
    }

    #[test]
    fn unclosed_html_envelope_is_invalid_transport() {
        let doc = b"<!--\n---\nformat: pdc-document/1\ntitle: Unclosed envelope\n<h1>x</h1>\n";
        assert_eq!(
            split_html(doc).unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
    }

    #[test]
    fn crlf_is_normalized_in_envelope_only() {
        let doc = b"---\r\nformat: pdc-document/1\r\n---\r\n# Body\r\n";
        let split = split_djot(doc).unwrap();
        assert_eq!(split.envelope_src, "format: pdc-document/1");
        assert_eq!(split.body, "# Body\r\n");
    }
    #[test]
    fn bom_only_at_start_is_rejected() {
        assert!(reject_bom(&[0xef, 0xbb, 0xbf]).is_err());
        assert!(reject_bom(b"---\nformat: x\n---\n").is_ok());
        // A U+FEFF after the first byte is ordinary content, not a transport BOM.
        assert!(reject_bom("a \u{feff} b".as_bytes()).is_ok());
    }

    #[test]
    fn non_utf8_is_invalid_transport() {
        assert_eq!(
            split_djot(b"---\n\xff\xfe\n---\nbody\n").unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
        assert_eq!(
            split_html(b"<!--\n---\n\xff\n---\n-->\n").unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
    }

    #[test]
    fn sniff_never_errors_and_checks_only_the_opener() {
        assert_eq!(sniff_html(b""), HtmlClassification::Legacy);
        assert_eq!(sniff_html(b"<!--"), HtmlClassification::Legacy);
        assert_eq!(sniff_html(b"<!--\n---"), HtmlClassification::Pdc);
        assert_eq!(sniff_html(b"<!--\r\n---\r\n"), HtmlClassification::Pdc);
        assert_eq!(sniff_html(b"<!--\n-- second\n"), HtmlClassification::Legacy);
        // Invalid UTF-8 must classify as legacy, never panic.
        assert_eq!(sniff_html(b"\xff\xfe<html>"), HtmlClassification::Legacy);
    }

    #[test]
    fn djot_transport_edge_shapes() {
        assert_eq!(
            split_djot(b"").unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
        // Closer as the final line without a terminator: empty body.
        assert_eq!(split_djot(b"---\nformat: x\n---").unwrap().body, "");
        // An indented `---` inside a literal block does not close the
        // envelope; only an exact `---` line does.
        let split = split_djot(b"---\nnotes: |\n  ---\n---\nbody\n").unwrap();
        assert_eq!(split.envelope_src, "notes: |\n  ---");
        assert_eq!(split.body, "body\n");
    }

    #[test]
    fn html_transport_edge_shapes() {
        // Canonical writers emit the line ending after `-->` even for an
        // empty body.
        assert_eq!(
            split_html(b"<!--\n---\nformat: x\n---\n-->\n")
                .unwrap()
                .body,
            ""
        );
        // `-->` with no following terminator still yields an empty body.
        assert_eq!(
            split_html(b"<!--\n---\nformat: x\n---\n-->").unwrap().body,
            ""
        );
        // A non-PDC opener is invalid transport for the split (sniff routes
        // it to the legacy path first).
        assert_eq!(
            split_html(b"<html>\n").unwrap_err().code,
            PdcDiagnosticCode::InvalidTransport
        );
        // `--!>` is also an early HTML comment close.
        assert_eq!(
            split_html(b"<!--\n---\ntitle: a --!> b\n---\n-->\n")
                .unwrap_err()
                .code,
            PdcDiagnosticCode::InvalidTransport
        );
        // CRLF: the envelope is normalized, the body keeps its bytes.
        let split = split_html(b"<!--\r\n---\r\nformat: x\r\n---\r\n-->\r\n<p>a</p>\r\n").unwrap();
        assert_eq!(split.envelope_src, "format: x");
        assert_eq!(split.body, "<p>a</p>\r\n");
    }
}
