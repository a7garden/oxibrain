//! Portable Document Contract Reader support: `pdc-document/2` (Markdown-
//! first) plus the frozen `pdc-document/1` legacy readers.
//!
//! oxibrain is a Full Reader/indexer: it classifies, validates, decodes, and
//! indexes PDC documents but never writes, repairs, or migrates one. All
//! functions here are pure and deterministic — same bytes in, same result
//! out — and none of them touch the filesystem.
//!
//! Contract pins:
//! - `pdc-document/2` (`v2.0.0-draft.2`, commit `0ee51ea`), corpus format
//!   `pdc-document-conformance/2` revision [`PDC2_CORPUS_REVISION`];
//! - `pdc-document/1` draft 4, corpus revision [`PDC_CORPUS_REVISION`]
//!   (legacy readability, never auto-converted; PDC 2 §6.3);
//! - `pdc-query/1` ([`PDC_QUERY_CONTRACT`]) is recognized read-only:
//!   `.base` files and fenced `base` blocks are preserved as opaque query
//!   definitions, validated for safe YAML, and never executed.
//!
//! The normative text lives in the contract repository; behavior pinned by
//! corpus fixtures is documented next to the code that implements it.

mod diagnostic;
mod djot_body;
mod envelope;
mod html_body;
mod markdown_body;
mod query;
mod transport;
mod yaml_frontmatter;

pub use diagnostic::{PdcDiagnostic, PdcDiagnosticCode};
pub use djot_body::parse_djot_body;
pub use html_body::parse_html_body;
pub use markdown_body::parse_markdown_body;
pub use query::validate_base_query;
pub use transport::{
    TransportSplit, check_size, reject_bom, sniff_html, sniff_markdown, split_djot, split_html,
    split_markdown,
};

/// Conformance corpus revision this implementation is pinned to.
pub const PDC_CORPUS_REVISION: u32 = 3;

/// Conformance corpus format + revision for the PDC 2 reader.
pub const PDC2_CORPUS_FORMAT: &str = "pdc-document-conformance/2";
pub const PDC2_CORPUS_REVISION: u32 = 2;

/// The separately versioned query contract (`PDC-QUERY-1.0.md`). oxibrain
/// never executes queries; it preserves definitions and reports diagnostics.
pub const PDC_QUERY_CONTRACT: &str = "pdc-query/1";

/// Maximum query-definition size: 1 MiB (`pdc-query/1` §3.1).
pub(crate) const PDC_MAX_QUERY_BYTES: usize = 1024 * 1024;

/// Canonical document size cap: 4 MiB including envelope transport and body.
pub(crate) const PDC_MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// Maximum simultaneously open block containers / DOM nesting depth.
pub(crate) const PDC_MAX_CONTAINER_DEPTH: usize = 256;

/// Canonical body profile declared by the envelope's `body` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyProfile {
    Djot,
    Html,
    /// `pdc-markdown/1` — the canonical Obsidian-compatible Markdown body
    /// profile of `pdc-document/2`.
    Markdown,
}

impl BodyProfile {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Djot => "pdc-djot/1",
            Self::Html => "pdc-html/1",
            Self::Markdown => "pdc-markdown/1",
        }
    }

    /// The contract media type for the profile. HTML is version-split: the
    /// same body profile is canonical under both envelope majors
    /// (PDC 2 §4.3), so upserts use [`PdcMetadata::transport_media_type`]
    /// instead of this method.
    pub fn media_type(&self) -> &'static str {
        match self {
            Self::Djot => "application/vnd.pdc.document+djot;version=1",
            Self::Html => "application/vnd.pdc.document+html;version=1",
            Self::Markdown => "application/vnd.pdc.document+markdown;version=2",
        }
    }

    /// Parse an exact `body` envelope value. `None` when the value is not one
    /// of the canonical profile identifiers.
    pub fn from_str_exact(value: &str) -> Option<Self> {
        match value {
            "pdc-djot/1" => Some(Self::Djot),
            "pdc-html/1" => Some(Self::Html),
            "pdc-markdown/1" => Some(Self::Markdown),
            _ => None,
        }
    }
}

/// Coarse classification of a raw `.md` file: a PDC Markdown transport
/// attempt (declares a `pdc-document/*` format) or visible plain Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkdownClassification {
    Pdc,
    Legacy,
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
    /// Markdown bodies are the verbatim post-frontmatter source: the legacy
    /// Markdown scan, so every construct (wiki syntax, math, raw HTML) stays
    /// indexed as inert text.
    pub text: String,
    /// Stable block targets in source order: `b-<uuid>` for djot/html, the
    /// caret ID charset `[A-Za-z0-9-]+` for markdown (PDC 2 §7.2).
    pub block_ids: Vec<String>,
    /// Canonical `pdc://document/…` links in source order.
    pub document_links: Vec<PdcLink>,
    /// Vault-relative links/embeds (wiki links, relative Markdown links) —
    /// projection-only: recorded as written, never resolved or executed.
    pub wiki_links: Vec<WikiLink>,
    /// Managed-asset SHA-256 digests referenced by the body.
    pub asset_refs: Vec<String>,
    /// Task items in source order.
    pub tasks: Vec<PdcTask>,
    /// Reasons the body carries active/unsafe constructs (`unsafe_content`).
    pub unsafe_constructs: Vec<String>,
    /// Deepest simultaneously open container nesting (djot) / DOM depth (html)
    /// / leading-indentation proxy (markdown).
    pub container_depth: usize,
    /// Title fallback from the body: first level-one heading (djot/markdown)
    /// or first `<h1>` then `<title>` (html), as plain text.
    pub fallback_title: Option<String>,
    /// Count of `pdc-query/1` fenced `base` blocks that validated as safe
    /// YAML (markdown bodies; PDC 2 §9.2). Content stays in `text`.
    pub query_blocks: usize,
    /// Reasons a fenced `base` block failed safe-YAML validation
    /// (`invalid_query`): preserved as content, never executed.
    pub query_errors: Vec<String>,
}

/// One vault-relative link or embed, recorded as written (PDC 2 §8.1):
/// `[[note]]`, `[[note|label]]`, `[[note#heading]]`, `[[note#^block-id]]`,
/// `![[note]]`, and relative Markdown links `[label](folder/note.md#^id)`.
/// oxibrain projects these verbatim and never resolves them (vault-confined
/// by policy: no resolution pass exists to escape the root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// Vault-relative target as written (may be empty for `[[#^id]]`).
    pub target: String,
    /// Fragment after `#`, as written (heading text or `^block-id`).
    pub block: Option<String>,
    /// True for embeds (`![[…]]`, `![alt](…)` images).
    pub embed: bool,
}

/// Standard metadata parsed and validated from the envelope (§5.1–§5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdcMetadata {
    /// Envelope major: 1 for the frozen contract, 2 for `pdc-document/2`.
    pub contract_version: u32,
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
    /// `pdc-document/2` §5.2 CSS classes (v1 envelopes never carry them).
    pub cssclasses: Vec<String>,
    pub favorite: bool,
    pub deleted: bool,
    pub deleted_at: Option<String>,
}

impl PdcMetadata {
    /// Media type stored for the document's transport. HTML is
    /// envelope-version-split (`;version=1` under v1, `;version=2` under
    /// v2 — PDC 2 §4.3); djot is frozen at v1; markdown exists only under
    /// v2 (PDC 2 §4.2).
    pub fn transport_media_type(&self) -> String {
        match self.body {
            BodyProfile::Djot => BodyProfile::Djot.media_type().to_string(),
            BodyProfile::Html => match self.contract_version {
                2 => "application/vnd.pdc.document+html;version=2".to_string(),
                _ => BodyProfile::Html.media_type().to_string(),
            },
            BodyProfile::Markdown => BodyProfile::Markdown.media_type().to_string(),
        }
    }
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
/// The HTML transport carries both envelope majors: `pdc-document/1` keeps
/// the frozen constrained grammar and stays legacy-readable; `pdc-document/2`
/// uses safe general YAML (PDC 2 §4.3).
pub fn parse_html_document(file_stem: &str, bytes: &[u8]) -> Result<PdcDocument, PdcDiagnostic> {
    check_size(bytes)?;
    reject_bom(bytes)?;
    let split = split_html(bytes)?;
    let metadata = parse_envelope_by_version(&split.envelope_src, BodyProfile::Html)?;
    let body = parse_html_body(&split.body);
    finish_document(
        file_stem,
        metadata,
        body,
        PdcDiagnosticCode::DocumentTooComplex,
    )
}

/// Parse a canonical `.md` document (frontmatter transport + envelope + body,
/// PDC 2 §4.2). Only `pdc-document/2` envelopes carry the Markdown profile;
/// a v1 envelope in this transport is `invalid_transport` (§2).
pub fn parse_markdown_document(
    file_stem: &str,
    bytes: &[u8],
) -> Result<PdcDocument, PdcDiagnostic> {
    check_size(bytes)?;
    reject_bom(bytes)?;
    let split = split_markdown(bytes)?;
    let metadata = parse_envelope_by_version(&split.envelope_src, BodyProfile::Markdown)?;
    let body = parse_markdown_body(&split.body);
    finish_document(
        file_stem,
        metadata,
        body,
        PdcDiagnosticCode::DocumentTooComplex,
    )
}

/// Envelope grammar dispatch by the observed `format` value: v1 → the frozen
/// constrained parser, v2 → safe general YAML. Everything else fails through
/// the v1 validator so its diagnostics (missing `format`, unknown versions)
/// keep their pinned wording — except the v1-declares-markdown case, which
/// is a transport error because the Markdown profile does not exist under
/// PDC 1 (PDC 2 §2).
fn parse_envelope_by_version(
    envelope_src: &str,
    transport: BodyProfile,
) -> Result<PdcMetadata, PdcDiagnostic> {
    match envelope::observed_format(envelope_src).as_deref() {
        Some("pdc-document/2") => {
            let fields = yaml_frontmatter::parse_safe_yaml(
                envelope_src,
                PdcDiagnosticCode::InvalidEnvelope,
            )?;
            envelope::validate_fields_v2(&fields, transport)
        }
        Some(f) if f.starts_with("pdc-document/") => {
            if transport == BodyProfile::Markdown {
                // PDC 2 §2: a v1 envelope cannot declare the Markdown profile
                // (transport error); an unknown major is unsupported, not
                // malformed.
                return Err(if f == "pdc-document/1" {
                    PdcDiagnostic::new(
                        PdcDiagnosticCode::InvalidTransport,
                        "`pdc-document/1` cannot declare the Markdown profile: it exists only \
                         under `pdc-document/2` (PDC 2 §2)"
                            .to_string(),
                    )
                } else {
                    PdcDiagnostic::new(
                        PdcDiagnosticCode::UnsupportedDocumentVersion,
                        format!("unsupported document version `{f}`"),
                    )
                });
            }
            let raw = envelope::parse_envelope(envelope_src).map_err(|e| {
                PdcDiagnostic::at(
                    PdcDiagnosticCode::InvalidEnvelope,
                    e.message,
                    e.line as u32,
                    1,
                )
            })?;
            envelope::validate_fields(&raw, transport)
        }
        _ => {
            let raw = envelope::parse_envelope(envelope_src).map_err(|e| {
                PdcDiagnostic::at(
                    PdcDiagnosticCode::InvalidEnvelope,
                    e.message,
                    e.line as u32,
                    1,
                )
            })?;
            envelope::validate_fields(&raw, transport)
        }
    }
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
