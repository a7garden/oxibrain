//! PDC diagnostics (`pdc-document/1` §13).
//!
//! Codes mirror the contract's diagnostic vocabulary exactly (`as_str()` is the
//! contract spelling). `LegacyHtml` is a visible classification rather than an
//! error, and `UnresolvedLink` is an oxibrain-side informational report — the
//! contract requires diagnostics to be distinguishable but does not assign it
//! a code. Indexing one bad document must never hide unrelated valid ones, so
//! diagnostics travel alongside results instead of aborting the pass.

/// Diagnostic vocabulary of the contract plus oxibrain's informational code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdcDiagnosticCode {
    InvalidTransport,
    InvalidEnvelope,
    UnsupportedDocumentVersion,
    UnsupportedBodyVersion,
    InvalidDocumentId,
    DuplicateDocumentId,
    DuplicateBlockId,
    DocumentTooLarge,
    DocumentTooComplex,
    MissingAsset,
    AssetDigestMismatch,
    UnsafeContent,
    LegacyHtml,
    /// oxibrain informational report, not a contract code: a `pdc://document/<uuid>`
    /// link whose target UUID is absent from the same root.
    UnresolvedLink,
}

impl PdcDiagnosticCode {
    /// Contract spelling. `UnresolvedLink` maps to `unresolved_link` (oxibrain
    /// extension for reporting; the contract leaves link reporting to apps).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidTransport => "invalid_transport",
            Self::InvalidEnvelope => "invalid_envelope",
            Self::UnsupportedDocumentVersion => "unsupported_document_version",
            Self::UnsupportedBodyVersion => "unsupported_body_version",
            Self::InvalidDocumentId => "invalid_document_id",
            Self::DuplicateDocumentId => "duplicate_document_id",
            Self::DuplicateBlockId => "duplicate_block_id",
            Self::DocumentTooLarge => "document_too_large",
            Self::DocumentTooComplex => "document_too_complex",
            Self::MissingAsset => "missing_asset",
            Self::AssetDigestMismatch => "asset_digest_mismatch",
            Self::UnsafeContent => "unsafe_content",
            Self::LegacyHtml => "legacy_html",
            Self::UnresolvedLink => "unresolved_link",
        }
    }
}

/// One diagnosable condition on one document, with an optional source position
/// (1-based, relative to the file when known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcDiagnostic {
    pub code: PdcDiagnosticCode,
    pub reason: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

impl PdcDiagnostic {
    pub fn new(code: PdcDiagnosticCode, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
            line: None,
            col: None,
        }
    }

    pub fn at(code: PdcDiagnosticCode, reason: impl Into<String>, line: u32, col: u32) -> Self {
        Self {
            code,
            reason: reason.into(),
            line: Some(line),
            col: Some(col),
        }
    }
}

impl std::fmt::Display for PdcDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.reason)?;
        if let (Some(line), Some(col)) = (self.line, self.col) {
            write!(f, " (line {line}, col {col})")?;
        } else if let Some(line) = self.line {
            write!(f, " (line {line})")?;
        }
        Ok(())
    }
}
