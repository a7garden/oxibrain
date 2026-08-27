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
pub const DECODER_VERSION: &str = "1";

/// Coarse content type used to pick a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Markdown,
    Html,
    PlainText,
}

impl MediaType {
    /// Map a file extension (without the leading `.`) to a media type.
    /// Unknown extensions return `None` so the scanner can skip them.
    pub fn from_extension(ext: &str) -> Option<Self> {
        if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            Some(Self::Markdown)
        } else if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
            Some(Self::Html)
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
            Self::PlainText => "text/plain",
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
        // this for both `.md` and `.mdx` variants.
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
        MediaType::PlainText => decode_utf8(bytes)?,
    };
    Ok(DecodedDocument { media_type, text })
}

/// Decode bytes to a UTF-8 string; invalid sequences are replaced with the
/// Unicode replacement character so downstream chunking + FTS still see text.
fn decode_utf8(bytes: &[u8]) -> Result<String, BrainError> {
    Ok(String::from_utf8_lossy(bytes).into_owned())
}
