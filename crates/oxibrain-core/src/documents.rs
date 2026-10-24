//! Pure document planners and observation types for the document plane
//! (Daemonless Two-Plane spec §6–§7).
//!
//! This module is pure: no I/O, no model, no time. Store and connectors
//! observe files; this module turns the observed data into deterministic
//! reconcile plans and identity-bearing structs. Nothing here opens a database
//! or reads from disk.
//!
//! ## Identity
//!
//! - `document_id(root_alias, locator)` = `blake3(("root_alias", alias),
//!   ("locator", locator))` hex — stable across processes and reruns.
//! - `chunk_id(document_id, revision, ordinal)` =
//!   `blake3(("document_id", id), ("revision", rev), ("ordinal", n))` hex.
//!
//! The chunk id is keyed by revision so that a re-ingest of a changed file
//! produces new chunk ids for the new revision. Old chunks remain addressable
//! through their old id (the store's apply step cascades deletes on Replace).

use serde::{Deserialize, Serialize};

/// One observed file in a configured root. Connectors build these during the
/// scan/gix phases; the planner consumes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileObservation {
    /// Root-relative path with `/` separators.
    pub locator: String,
    /// File size in bytes (raw, pre-decode).
    pub bytes: u64,
    /// Modification time in nanoseconds since the Unix epoch.
    pub modified_ns: i64,
    /// Some git blob oid for clean tracked files; None for plain roots or
    /// dirty worktree files.
    pub revision_hint: Option<String>,
}

/// One cached file as stored in `documents.db`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedFile {
    pub locator: String,
    pub bytes: u64,
    pub modified_ns: i64,
    /// Effective revision at apply time. May be a `git:` oid-string or a
    /// `blake3:` hash for plain roots.
    pub revision: String,
}

/// Config-fingerprint inputs for one root. Equality decides Keep vs Reset
/// in `diff_roots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFingerprint {
    pub alias: String,
    pub canonical_path: String,
    pub space: String,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_file_bytes: u64,
    /// Connector decoder version that produced the cached projection
    /// (`connectors::DECODER_VERSION`). Cached fingerprints persisted before
    /// this field existed deserialize with the empty default, which differs
    /// from the current version — so every root resets exactly once after an
    /// upgrade and the whole cache re-decodes under the new decoder.
    #[serde(default)]
    pub decoder_version: String,
}

/// One canonical `pdc://document/…` link in the projection — a
/// serialization-stable mirror of the connector's `PdcLink`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PdcLinkMeta {
    /// Target document UUID (canonical lowercase form).
    pub uuid: String,
    /// `b-<uuid>` block target when the fragment is present.
    pub block: Option<String>,
    /// True when the link carries the `pdc-embed` class (document embed).
    pub embed: bool,
}

/// Standard decoded metadata of one canonical PDC document, persisted as
/// JSON in the disposable document projection (`documents.pdc_meta`). Pure
/// data: the connectors own decoding, the store serializes this verbatim,
/// and the facade fills it at ingest time. Serialized form is part of the
/// rebuildable cache only — reprojection from source reproduces it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PdcProjectionMeta {
    /// Envelope title; may be empty (use `display_title` for rendering).
    pub title: String,
    /// Envelope title, else body fallback, else filename stem.
    pub display_title: String,
    /// Envelope `profile` free-text value when present.
    pub profile: Option<String>,
    /// Envelope `lang` when present.
    pub lang: Option<String>,
    pub tags: Vec<String>,
    pub aliases: Vec<String>,
    pub favorite: bool,
    /// Envelope deletion state (trash semantics; the source stays indexed).
    pub deleted: bool,
    pub deleted_at: Option<String>,
    pub created: String,
    pub updated: String,
    /// Canonical internal links in source order.
    pub links: Vec<PdcLinkMeta>,
    /// Managed-asset SHA-256 digests referenced by the body.
    pub assets: Vec<String>,
    pub task_count: u32,
    pub block_id_count: u32,
    /// Reasons the body carries unsafe constructs (`unsafe_content`); the
    /// document is still indexed when non-empty.
    pub unsafe_flags: Vec<String>,
}

/// Cached root metadata held in `documents.db`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedRootMeta {
    pub alias: String,
    pub space: String,
    pub fingerprint: RootFingerprint,
    pub generation: i64,
}

/// Action produced by `diff_roots` for a configured alias vs the cached set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RootAction {
    /// Fingerprint unchanged; generation can be reused.
    KeepRoot,
    /// Fingerprint changed; root must be wiped and rebuilt at gen 1.
    ResetRoot,
    /// Alias no longer present in config; root rows must be deleted.
    RemoveRoot,
}

/// Pure root-set diff. Output sorted by alias; deterministic for equal
/// inputs. An alias present only in the configured set (no cached entry)
/// is treated as `KeepRoot` with no cached generation — the apply stage
/// is responsible for inserting the row at generation 1.
pub fn diff_roots(
    configured: &[RootFingerprint],
    cached: &[CachedRootMeta],
) -> Vec<(String, RootAction)> {
    let mut by_alias: std::collections::BTreeMap<&str, RootAction> =
        std::collections::BTreeMap::new();

    for c in cached {
        match configured.iter().find(|f| f.alias == c.alias) {
            Some(f) if f == &c.fingerprint => {
                by_alias.insert(c.alias.as_str(), RootAction::KeepRoot);
            }
            Some(_) => {
                by_alias.insert(c.alias.as_str(), RootAction::ResetRoot);
            }
            None => {
                by_alias.insert(c.alias.as_str(), RootAction::RemoveRoot);
            }
        }
    }

    for f in configured {
        by_alias
            .entry(f.alias.as_str())
            .or_insert(RootAction::KeepRoot);
    }

    by_alias
        .into_iter()
        .map(|(a, act)| (a.to_owned(), act))
        .collect()
}

/// Action produced by `plan_reconcile` for one locator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileAction {
    /// Locator present in both with matching `(bytes, modified_ns)` and a
    /// cached revision compatible with the observed revision hint (equal
    /// or absent).
    Unchanged,
    /// Locator seen only in `observed`.
    Add(FileObservation),
    /// Locator seen in both but `(bytes, modified_ns)` differ or the
    /// observed revision hint disagrees with the cached revision.
    Replace(FileObservation),
    /// Locator seen only in `cached`.
    Delete { locator: String },
    /// Locator deliberately skipped at the I/O layer (unreadable,
    /// oversize, ignored by gix, …). The pure planner never emits this;
    /// the apply-stage report surfaces it from a separate skipped list
    /// the connector produces.
    Skip { locator: String, reason: String },
}

/// Pure per-root reconcile. Every cached and observed locator lands in
/// exactly one action; output sorted by locator.
///
/// Compatibility rule:
///   - same `(bytes, modified_ns)` AND
///     (`observed.revision_hint == Some(cached.revision)` OR
///     `observed.revision_hint == None`)  ⇒ Unchanged
///   - same `(bytes, modified_ns)` AND `observed.revision_hint` differs
///     from cached revision  ⇒ Replace
///   - `(bytes, modified_ns)` differs  ⇒ Replace
///   - cached-only  ⇒ Delete
///   - observed-only ⇒ Add
pub fn plan_reconcile(cached: &[CachedFile], observed: &[FileObservation]) -> Vec<FileAction> {
    let cached_by_loc: std::collections::BTreeMap<&str, &CachedFile> =
        cached.iter().map(|c| (c.locator.as_str(), c)).collect();
    let observed_by_loc: std::collections::BTreeMap<&str, &FileObservation> =
        observed.iter().map(|o| (o.locator.as_str(), o)).collect();

    let mut all_locators: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for k in cached_by_loc.keys() {
        all_locators.insert(k);
    }
    for k in observed_by_loc.keys() {
        all_locators.insert(k);
    }

    let mut actions = Vec::with_capacity(all_locators.len());
    for loc in all_locators {
        match (cached_by_loc.get(loc), observed_by_loc.get(loc)) {
            (None, Some(o)) => actions.push(FileAction::Add((*o).clone())),
            (Some(c), None) => actions.push(FileAction::Delete {
                locator: c.locator.clone(),
            }),
            (Some(c), Some(o)) => {
                let stat_match = c.bytes == o.bytes && c.modified_ns == o.modified_ns;
                let rev_compat = match (&o.revision_hint, &c.revision) {
                    (Some(h), r) => h == r,
                    (None, _) => true,
                };
                if stat_match && rev_compat {
                    actions.push(FileAction::Unchanged);
                } else {
                    actions.push(FileAction::Replace((*o).clone()));
                }
            }
            (None, None) => unreachable!("locator in union of cached and observed"),
        }
    }

    actions
}

fn derive(fields: &[(&str, &str)]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for (k, v) in fields {
        h.update(k.as_bytes());
        h.update(&[0u8]);
        h.update(v.as_bytes());
        h.update(&[0u8]);
    }
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    out
}

/// Stable document identity.
///
/// `document_id = blake3(("root_alias", alias), ("locator", locator))` hex.
/// The result is reproducible across processes and reruns; changing either
/// input produces a different id.
pub fn document_id(root_alias: &str, locator: &str) -> String {
    hex::encode(derive(&[("root_alias", root_alias), ("locator", locator)]))
}

/// Stable chunk identity.
///
/// `chunk_id = blake3(("document_id", id), ("revision", rev),
///  ("ordinal", n))` hex.
///
/// The revision is part of the id so that a re-ingest of a changed file
/// produces new chunk ids for the new revision while old chunks remain
/// addressable through their old id.
pub fn chunk_id(document_id: &str, revision: &str, ordinal: u32) -> String {
    let ord = ordinal.to_string();
    hex::encode(derive(&[
        ("document_id", document_id),
        ("revision", revision),
        ("ordinal", ord.as_str()),
    ]))
}

/// A heading's UTF-8 byte range in the same decoded view as document chunks.
/// This transient retrieval metadata never changes the source or cached text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentHeading {
    pub start: usize,
    pub end: usize,
    pub level: u8,
}

/// Select a complete section for a heading or body hit. Descendant sections
/// belong to their parent; the next peer or ancestor ends the range. No partial
/// section is manufactured to fit a budget. Short heading-free documents can
/// be read whole, while unsafe body-only fallbacks are rejected.
pub fn recall_document_range(
    text: &str,
    headings: &[DocumentHeading],
    hit: std::ops::Range<usize>,
    query: &str,
    short_document_bytes: usize,
) -> Option<std::ops::Range<usize>> {
    let mut ranges = recall_document_ranges(text, headings, hit, query, short_document_bytes)?;
    (ranges.len() == 1).then(|| ranges.remove(0))
}

/// Select complete sections for a selected source hit. A chunk spanning peers
/// may yield several distinct query matches; a whole-view chunk with no lexical
/// section signal remains whole so dense-only retrieval keeps its uncertainty.
pub fn recall_document_ranges(
    text: &str,
    headings: &[DocumentHeading],
    hit: std::ops::Range<usize>,
    query: &str,
    short_document_bytes: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    text.get(hit.clone())?;
    if headings.is_empty() {
        return Some(vec![if text.len() <= short_document_bytes {
            0..text.len()
        } else {
            hit
        }]);
    }

    // A lexical chunk can hit the heading itself or any body text below it.
    // Select the outermost complete section containing the whole hit. This
    // keeps parent-level conditions and all child subsections together.
    let mut sections = Vec::new();
    for (index, heading) in headings.iter().enumerate() {
        if heading.level == 0
            || heading.start >= heading.end
            || text.get(heading.start..heading.end).is_none()
        {
            continue;
        }
        let end = headings[index + 1..]
            .iter()
            .find(|next| {
                next.level > 0
                    && next.start >= heading.end
                    && next.start <= text.len()
                    && text.is_char_boundary(next.start)
                    && next.level <= heading.level
            })
            .map_or(text.len(), |next| next.start);
        let section = heading.start..end;
        sections.push((heading.level, section));
    }

    let mut selected: Option<(u8, std::ops::Range<usize>)> = None;
    for (level, section) in &sections {
        if section.start <= hit.start
            && hit.end <= section.end
            && selected.as_ref().is_none_or(|(best_level, best_range)| {
                level < best_level || (level == best_level && section.len() > best_range.len())
            })
        {
            selected = Some((*level, section.clone()));
        }
    }
    if let Some((_, section)) = selected {
        return Some(vec![section]);
    }

    // A chunk can itself straddle peer sections. In that case use the same
    // bounded, language-neutral query terms and trigrams to select the section
    // that actually matched; never return the mixed chunk as a body-only
    // fallback. Byte-source coordinates remain those of this decoded view.
    // This also narrows a whole-document chunk when it contains a lexical hit;
    // only a query with no section signal keeps the whole source intact.
    let whole_document_hit = hit.start == 0 && hit.end == text.len();
    let plan = crate::query_candidates::LexicalQueryPlan::new(query);
    let mut scored_sections = Vec::new();
    for (_, section) in sections {
        let start = hit.start.max(section.start);
        let end = hit.end.min(section.end);
        if start >= end {
            continue;
        }
        let Some(body) = text.get(section.clone()) else {
            continue;
        };
        let score = plan.relevance(body);
        if score == crate::query_candidates::LexicalRelevance::default() {
            continue;
        }
        scored_sections.push((score, section));
    }
    // A stronger query match ranks a candidate first; it does not establish
    // that another independently matched section is redundant. Preserve every
    // lexical-signal peer so opposing conditions remain available as evidence.
    let mut selected: Vec<_> = scored_sections
        .into_iter()
        .map(|(_, section)| section)
        .collect();
    // Prefer the complete outer section when a parent and one of its children
    // both carry the query signal. That preserves the parent condition without
    // duplicating a nested subsection as a second evidence unit.
    let all_selected = selected.clone();
    selected.retain(|section| {
        !all_selected.iter().any(|other| {
            other.start <= section.start
                && section.end <= other.end
                && (other.start < section.start || section.end < other.end)
        })
    });
    selected.sort_by_key(|section| section.start);
    if !selected.is_empty() {
        Some(selected)
    } else {
        whole_document_hit.then_some(vec![hit])
    }
}
