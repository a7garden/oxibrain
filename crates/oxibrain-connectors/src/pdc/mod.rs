//! Portable Document Contract (`pdc-document/1`) Reader support.
//!
//! oxibrain is a Full Reader/indexer: it classifies, validates, decodes, and
//! indexes PDC documents but never writes, repairs, or migrates one. All
//! functions here are pure and deterministic — same bytes in, same result out —
//! and none of them touch the filesystem.
//!
//! Contract pin: `portable-document-contract` draft 4, corpus revision
//! [`PDC_CORPUS_REVISION`]. The normative text lives in the contract repository;
//! behavior pinned by corpus fixtures is documented next to the code that
//! implements it.

mod diagnostic;
mod djot_body;
mod envelope;
mod html_body;
mod transport;

pub use diagnostic::{PdcDiagnostic, PdcDiagnosticCode};
pub use djot_body::parse_djot_body;
pub use html_body::parse_html_body;
pub use transport::{TransportSplit, check_size, reject_bom, sniff_html, split_djot, split_html};

/// Conformance corpus revision this implementation is pinned to.
pub const PDC_CORPUS_REVISION: u32 = 3;

/// Canonical document size cap: 4 MiB including envelope transport and body.
pub(crate) const PDC_MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// Maximum simultaneously open block containers / DOM nesting depth.
pub(crate) const PDC_MAX_CONTAINER_DEPTH: usize = 256;

/// Canonical body profile declared by the envelope's `body` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyProfile {
    Djot,
    Html,
}

impl BodyProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Djot => "pdc-djot/1",
            Self::Html => "pdc-html/1",
        }
    }

    /// The contract media type for the profile.
    pub fn media_type(&self) -> &'static str {
        match self {
            Self::Djot => "application/vnd.pdc.document+djot;version=1",
            Self::Html => "application/vnd.pdc.document+html;version=1",
        }
    }

    /// Parse an exact `body` envelope value. `None` when the value is not one
    /// of the two canonical profile identifiers.
    pub fn from_str_exact(value: &str) -> Option<Self> {
        match value {
            "pdc-djot/1" => Some(Self::Djot),
            "pdc-html/1" => Some(Self::Html),
            _ => None,
        }
    }
}

/// Coarse classification of a raw `.html` file: canonical PDC HTML transport
/// or visible legacy HTML (which keeps flowing through the legacy adapter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlClassification {
    Pdc,
    Legacy,
}

/// One canonical internal link (`pdc://document/<uuid>[#b-<uuid>]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcLink {
    /// Target document UUID (lowercase canonical spelling preserved).
    pub uuid: String,
    /// `b-<uuid>` block target when the fragment is present.
    pub block: Option<String>,
    /// True when the link carries the `pdc-embed` class (document embed).
    pub embed: bool,
}

/// One task item. HTML tasks carry `id` when the `<li>` has a stable target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcTask {
    pub completed: bool,
    pub text: String,
    pub id: Option<String>,
}

/// Body-derived index artifacts of one document. Structural problems that the
/// contract reports as `unsafe_content` land in `unsafe_constructs` and do NOT
/// fail the parse; `duplicate_block_id` and `document_too_complex` DO fail it
/// (enforced in [`parse_djot_document`] / [`parse_html_document`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcBody {
    /// Indexable plain text of the body (envelope excluded, markup stripped).
    pub text: String,
    /// Stable `b-<uuid>` targets in source order.
    pub block_ids: Vec<String>,
    /// Canonical `pdc://document/…` links in source order.
    pub document_links: Vec<PdcLink>,
    /// Managed-asset SHA-256 digests referenced by the body.
    pub asset_refs: Vec<String>,
    /// Task items in source order.
    pub tasks: Vec<PdcTask>,
    /// Reasons the body carries active/unsafe constructs (`unsafe_content`).
    pub unsafe_constructs: Vec<String>,
    /// Deepest simultaneously open container nesting (djot) / DOM depth (html).
    pub container_depth: usize,
    /// Title fallback from the body: first level-one heading (djot) or first
    /// `<h1>` then `<title>` (html), as plain text.
    pub fallback_title: Option<String>,
}

/// Standard metadata parsed and validated from the envelope (§5.1–§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcMetadata {
    pub document_uuid: String,
    pub body: BodyProfile,
    pub created: String,
    pub updated: String,
    /// Envelope title; may be empty (display falls back to `display_title`).
    pub title: String,
    pub profile: Option<String>,
    pub lang: Option<String>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub favorite: bool,
    pub deleted: bool,
    pub deleted_at: Option<String>,
}

/// A fully parsed canonical PDC document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcDocument {
    pub metadata: PdcMetadata,
    pub body: PdcBody,
    /// Display title: envelope title when nonempty, else the body fallback,
    /// else the filename stem.
    pub display_title: String,
}

/// Classify raw `.html` bytes: canonical PDC HTML transport or visible
/// legacy HTML. A thin alias over [`sniff_html`], kept as the named
/// classification entry point for the corpus runner and the facade.
pub fn classify_html_transport(bytes: &[u8]) -> HtmlClassification {
    sniff_html(bytes)
}

/// Parse a canonical `.djot` document (transport + envelope + body).
pub fn parse_djot_document(file_stem: &str, bytes: &[u8]) -> Result<PdcDocument, PdcDiagnostic> {
    check_size(bytes)?;
    reject_bom(bytes)?;
    let split = split_djot(bytes)?;
    let raw = envelope::parse_envelope(&split.envelope_src).map_err(|e| {
        PdcDiagnostic::at(
            PdcDiagnosticCode::InvalidEnvelope,
            e.message,
            e.line as u32,
            1,
        )
    })?;
    let metadata = envelope::validate_fields(&raw, BodyProfile::Djot)?;
    let body = parse_djot_body(&split.body);
    finish_document(
        file_stem,
        metadata,
        body,
        PdcDiagnosticCode::DocumentTooComplex,
    )
}

/// Parse a canonical `.html` document (comment transport + envelope + body).
pub fn parse_html_document(file_stem: &str, bytes: &[u8]) -> Result<PdcDocument, PdcDiagnostic> {
    check_size(bytes)?;
    reject_bom(bytes)?;
    let split = split_html(bytes)?;
    let raw = envelope::parse_envelope(&split.envelope_src).map_err(|e| {
        PdcDiagnostic::at(
            PdcDiagnosticCode::InvalidEnvelope,
            e.message,
            e.line as u32,
            1,
        )
    })?;
    let metadata = envelope::validate_fields(&raw, BodyProfile::Html)?;
    let body = parse_html_body(&split.body);
    finish_document(
        file_stem,
        metadata,
        body,
        PdcDiagnosticCode::DocumentTooComplex,
    )
}

/// Shared post-body steps: duplicate block ids, depth cap, display title.
fn finish_document(
    file_stem: &str,
    metadata: PdcMetadata,
    body: PdcBody,
    depth_code: PdcDiagnosticCode,
) -> Result<PdcDocument, PdcDiagnostic> {
    let mut seen = std::collections::BTreeSet::new();
    for id in &body.block_ids {
        if !seen.insert(id.as_str()) {
            return Err(PdcDiagnostic::new(
                PdcDiagnosticCode::DuplicateBlockId,
                format!("block target `{id}` appears more than once in the document"),
            ));
        }
    }
    if body.container_depth > PDC_MAX_CONTAINER_DEPTH {
        return Err(PdcDiagnostic::new(
            depth_code,
            format!(
                "container nesting depth {} exceeds the maximum of {PDC_MAX_CONTAINER_DEPTH}",
                body.container_depth
            ),
        ));
    }
    let display_title = if !metadata.title.is_empty() {
        metadata.title.clone()
    } else {
        body.fallback_title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| file_stem.to_string())
    };
    Ok(PdcDocument {
        metadata,
        body,
        display_title,
    })
}
