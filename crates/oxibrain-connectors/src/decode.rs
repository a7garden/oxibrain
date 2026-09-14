//! Versioned decoders: turn raw file bytes into the indexable UTF-8 text the
//! documents plane stores.
//!
//! `DECODER_VERSION` is bumped whenever decode semantics change in a way that
//! invalidates already-cached chunks (tokenization rewrite, frontmatter rule
//! change, etc.). The facade persists the value alongside each document row so
//! `documents.db` rebuilds can detect stale entries.
//!
//! All decoders are deterministic and pure — same bytes in, same text out —
//! so `decode(Markdown, b1) == decode(Markdown, b1)` always holds. Markdown
//! and HTML frontmatter stripping are delegated to [`crate::markdown`] and
//! [`crate::html`], which already implement the spec rules; keeping one
//! source of truth avoids drift between the legacy vault import path and the
//! new document plane.

use oxibrain_ports::BrainError;

use crate::html::html_note_to_text;

/// Bumped when decode semantics change in a way that invalidates cached text.
/// v2: PDC Reader support — `.djot` documents decode through the
/// `pdc-djot/1` body profile and canonical `.html` through `pdc-html/1`.
/// v3: `pdc-document/2` Markdown-first Reader — canonical `.md` decodes
/// through the `pdc-markdown/1` body profile, canonical `.html` may declare
/// `pdc-document/2`, and `.base` query definitions decode as opaque source
/// (`doc/spec/pdc-adoption-v2.md`, corpus revision 2).
pub const DECODER_VERSION: &str = "3";

/// Coarse content type used to pick a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Markdown,
    Html,
    /// Canonical PDC Djot documents (`.djot`). Decoded through the
    /// `pdc-djot/1` body profile; envelope text never reaches the index.
    Djot,
    PlainText,
    /// `pdc-query/1` `.base` query definitions — opaque, read-only source.
    /// Never executed; preserved verbatim and indexed as source text.
    BaseQuery,
}

impl MediaType {
    /// Map a file extension (without the leading `.`) to a media type.
    /// Unknown extensions return `None` so the scanner can skip them.
    pub fn from_extension(ext: &str) -> Option<Self> {
        if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            Some(Self::Markdown)
        } else if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
            Some(Self::Html)
        } else if ext.eq_ignore_ascii_case("djot") {
            Some(Self::Djot)
        } else if ext.eq_ignore_ascii_case("base") {
            Some(Self::BaseQuery)
        } else if ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("text") {
            Some(Self::PlainText)
        } else {
            None
        }
    }

    /// Stable MIME-style string used for FTS dispatch and telemetry.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Markdown => "text/markdown",
            Self::Html => "text/html",
            Self::Djot => "application/vnd.pdc.document+djot;version=1",
            Self::PlainText => "text/plain",
            Self::BaseQuery => "application/vnd.pdc.query+yaml;version=1",
        }
    }

    /// Inverse of the stored media_type strings: legacy values (`text/*`)
    /// plus the PDC contract media types. `None` for anything else so
    /// callers can fall back to the locator extension.
    pub fn from_stored_str(stored: &str) -> Option<Self> {
        match stored {
            "text/markdown" => Some(Self::Markdown),
            "text/html" => Some(Self::Html),
            "text/plain" => Some(Self::PlainText),
            "application/vnd.pdc.document+djot;version=1" => Some(Self::Djot),
            "application/vnd.pdc.query+yaml;version=1" => Some(Self::BaseQuery),
            // Canonical PDC HTML documents decode through the PDC parser under
            // either envelope major; the caller uses the stored media type /
            // `pdc_body_profile` to pick the parse. Canonical Markdown
            // documents likewise carry their contract media type, not
            // `text/markdown`.
            "application/vnd.pdc.document+html;version=1"
            | "application/vnd.pdc.document+html;version=2" => Some(Self::Html),
            "application/vnd.pdc.document+markdown;version=2" => Some(Self::Markdown),
            _ => None,
        }
    }
}

/// The result of decoding a document: the chosen media type plus the
/// indexable UTF-8 body. `text` is guaranteed valid UTF-8; lossy paths
/// replace invalid sequences with U+FFFD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedDocument {
    pub media_type: MediaType,
    pub text: String,
}

/// Decode `bytes` into a [`DecodedDocument`] per the rules for `media_type`.
///
/// Errors are limited to "the input cannot be made valid UTF-8 in any
/// meaningful way" (replaced with U+FFFD) and the conversion being
/// inconsistent with the chosen media type.
pub fn decode(media_type: MediaType, bytes: &[u8]) -> Result<DecodedDocument, BrainError> {
    let text = match media_type {
        // Markdown: YAML frontmatter (if present) is metadata and must not
        // bleed into the FTS index. The shared markdown helper already does
        // this for both `.md` and `.mdx` variants. This is the LEGACY
        // Markdown path — canonical `pdc-markdown/1` documents bypass it in
        // the facade and decode through the PDC parser instead.
        MediaType::Markdown => {
            let raw = decode_utf8(bytes)?;
            crate::markdown::strip_markdown_frontmatter(&raw)
        }
        MediaType::Html => {
            // The HTML frontmatter convention follows oximemo's `<!-- +++ … +++ -->`
            // comment syntax; the html module already strips it before html→text.
            let raw = decode_utf8(bytes)?;
            html_note_to_text(&raw)
        }
        MediaType::Djot => {
            // Canonical PDC Djot: decode through the `pdc-djot/1` profile so
            // cached text is identical to the ingest-time body text. Only
            // valid documents carry the `Djot` media type in the cache —
            // classification happens before an upsert exists.
            crate::pdc::parse_djot_document("", bytes)
                .map(|doc| doc.body.text)
                .map_err(|d| BrainError::Invalid(format!("pdc document: {d}")))?
        }
        MediaType::BaseQuery => {
            // pdc-query/1: opaque, read-only query source. Never executed.
            decode_utf8(bytes)?
        }
        MediaType::PlainText => decode_utf8(bytes)?,
    };
    Ok(DecodedDocument { media_type, text })
}

/// Decode bytes to a UTF-8 string; invalid sequences are replaced with the
/// Unicode replacement character so downstream chunking + FTS still see text.
fn decode_utf8(bytes: &[u8]) -> Result<String, BrainError> {
    Ok(String::from_utf8_lossy(bytes).into_owned())
}
