//! Source-view publication decisions. Ledger writes and authentication stay at
//! their existing store/facade boundaries; these types never carry credentials.
use crate::{Capability, Scope, canonical_json_value};
use oxibrain_ports::{BrainError, Timestamp};
use serde::{Deserialize, Serialize};

pub const PUBLICATION_VERSION: &str = "source-view-publication/1";
pub const MAX_VIEW_BYTES: usize = 65_536;
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PROJECTION_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_LINEAGE_DEPTH: usize = 64;
pub const MAX_CLOSURE_VIEWS: usize = 4096;

/// Internal origin identity. Never serialize it into an audience response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ViewOrigin {
    Episode {
        space: String,
        episode_id: String,
        revision: String,
    },
    Document {
        space: String,
        root: String,
        document_id: String,
        document_uuid: Option<String>,
        revision: String,
        decoder: String,
    },
    Publication {
        space: String,
        publication_id: String,
        revision: String,
    },
}
impl ViewOrigin {
    pub fn space(&self) -> &str {
        match self {
            Self::Episode { space, .. }
            | Self::Document { space, .. }
            | Self::Publication { space, .. } => space,
        }
    }
    pub fn revision(&self) -> &str {
        match self {
            Self::Episode { revision, .. }
            | Self::Document { revision, .. }
            | Self::Publication { revision, .. } => revision,
        }
    }
    /// Stable source occurrence, without a content revision. Equal bytes never
    /// merge independent occurrences or independent documents.
    pub fn identity(&self) -> String {
        let value = match self {
            Self::Episode {
                space, episode_id, ..
            } => serde_json::json!(["episode", space, episode_id]),
            Self::Document {
                space,
                document_uuid: Some(uuid),
                ..
            } => serde_json::json!(["pdc-document", space, uuid]),
            Self::Document {
                space,
                root,
                document_id,
                ..
            } => serde_json::json!(["document", space, root, document_id]),
            Self::Publication {
                space,
                publication_id,
                ..
            } => serde_json::json!(["publication", space, publication_id]),
        };
        canonical_json_value(&value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishViewV1 {
    pub occurrence_id: String,
    pub origin: ViewOrigin,
    pub range: ViewRange,
    pub bytes: String,
    pub digest: String,
    pub destination: String,
    pub policy_revision: String,
    pub occurred_at: Timestamp,
}

/// Checked observation supplied by the facade from a live source. This is not
/// accepted from an untrusted operation payload.
#[derive(Debug, Clone)]
pub struct SourceViewSnapshot {
    pub origin: ViewOrigin,
    pub text: String,
    pub available: bool,
}

/// Reserved typed Declaration payload; generic declare must refuse admission.
/// Re-citations retain only the accessible parent publication reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicationEventV1 {
    Published {
        request: PublishViewV1,
        principal: String,
    },
    Withdrawn {
        occurrence_id: String,
        destination: String,
        publication_id: String,
        principal: String,
        occurred_at: Timestamp,
    },
    SourceObserved {
        occurrence_id: String,
        origin_identity: String,
        previous_revision: String,
        observed_revision: Option<String>,
        principal: String,
        occurred_at: Timestamp,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewState {
    Current,
    Closed,
    Redacted,
}

/// The only publication DTO intended for a destination audience. Source range
/// coordinates are deliberately replaced by view-relative byte coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedView {
    pub version: String,
    pub publication_id: String,
    pub citation: String,
    pub view_revision: String,
    pub range: ViewRange,
    pub bytes: String,
    pub state: ViewState,
    pub extractable: bool,
    pub independent_support: bool,
    pub uncertainty: crate::Uncertainty,
}

/// Stable operation receipt. It contains no source identity or selected bytes,
/// so reconciliation never resurrects content after closure or withdrawal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub version: String,
    pub publication_id: String,
    pub citation: String,
    pub view_revision: String,
}
pub fn publication_receipt(record: &PublicationRecord) -> PublicationReceipt {
    PublicationReceipt {
        version: PUBLICATION_VERSION.into(),
        publication_id: record.id.clone(),
        citation: format!("publication://{}", record.id),
        view_revision: record.view_revision.clone(),
    }
}

pub fn exact_digest(bytes: &str) -> String {
    blake3::hash(bytes.as_bytes()).to_hex().to_string()
}
fn hash_json(value: serde_json::Value) -> String {
    exact_digest(&canonical_json_value(&value))
}
pub fn publication_id(destination: &str, principal: &str, occurrence: &str) -> String {
    hash_json(serde_json::json!([
        PUBLICATION_VERSION,
        destination,
        principal,
        occurrence
    ]))
}
pub fn root_family(origin: &ViewOrigin) -> String {
    hash_json(serde_json::json!([
        "source-evidence-family/1",
        origin.identity()
    ]))
}
pub fn request_digest(request: &PublishViewV1) -> String {
    hash_json(serde_json::to_value(request).expect("publication request is serializable"))
}
pub fn unavailable() -> BrainError {
    BrainError::Unauthorized("publication unavailable".into())
}

/// Check two exact grants independently. Combining them into a Cartesian Scope
/// would grant additional source writes or destination reads.
pub fn authorize_publication(
    origin: &Scope,
    destination: &Scope,
    origin_space: &str,
    destination_space: &str,
    now: Timestamp,
) -> Result<(), BrainError> {
    if !origin.permits(Capability::Read, origin_space, now)
        || !destination.permits(Capability::Write, destination_space, now)
        || origin.predicate_filter.is_some()
        || origin.entity_type_filter.is_some()
        || destination.predicate_filter.is_some()
        || destination.entity_type_filter.is_some()
    {
        return Err(unavailable());
    }
    Ok(())
}

pub fn validate_request(request: &PublishViewV1, now: Timestamp) -> Result<(), BrainError> {
    let bounded = |value: &str, max: usize| !value.is_empty() && value.len() <= max;
    let origin_valid = match &request.origin {
        ViewOrigin::Episode {
            space,
            episode_id,
            revision,
        } => bounded(space, 256) && bounded(episode_id, 256) && bounded(revision, 1024),
        ViewOrigin::Document {
            space,
            root,
            document_id,
            document_uuid,
            revision,
            decoder,
        } => {
            bounded(space, 256)
                && bounded(root, 256)
                && bounded(document_id, 256)
                && document_uuid.as_ref().is_none_or(|uuid| bounded(uuid, 64))
                && bounded(revision, 1024)
                && bounded(decoder, 256)
        }
        ViewOrigin::Publication {
            space,
            publication_id,
            revision,
        } => bounded(space, 256) && bounded(publication_id, 256) && bounded(revision, 1024),
    };
    if !origin_valid
        || !bounded(&request.destination, 256)
        || !bounded(&request.occurrence_id, 256)
        || !bounded(&request.policy_revision, 256)
        || request.occurred_at > now
        || request.bytes.is_empty()
        || request.bytes.len() > MAX_VIEW_BYTES
        || request.range.start >= request.range.end
        || request.range.end > MAX_SOURCE_BYTES
        || request.range.end - request.range.start != request.bytes.len()
        || request.digest != exact_digest(&request.bytes)
    {
        return Err(unavailable());
    }
    Ok(())
}

pub fn prepare_publication(
    request: &PublishViewV1,
    snapshot: &SourceViewSnapshot,
    principal: &str,
    now: Timestamp,
) -> Result<PublicationEventV1, BrainError> {
    validate_request(request, now)?;
    if !snapshot.available
        || snapshot.origin != request.origin
        || request.origin.space().is_empty()
        || request.origin.revision().is_empty()
        || request.destination.is_empty()
        || request.destination.len() > 256
        || principal.is_empty()
        || principal.len() > 256
        || request.occurrence_id.is_empty()
        || request.occurrence_id.len() > 256
        || request.policy_revision.is_empty()
        || request.policy_revision.len() > 256
        || request.occurred_at > now
        || request.bytes.is_empty()
        || request.bytes.len() > MAX_VIEW_BYTES
        || request.range.start >= request.range.end
        || snapshot.text.get(request.range.start..request.range.end) != Some(request.bytes.as_str())
        || exact_digest(&request.bytes) != request.digest
    {
        return Err(unavailable());
    }
    Ok(PublicationEventV1::Published {
        request: request.clone(),
        principal: principal.into(),
    })
}

pub fn audience_view(id: &str, bytes: &str, revision: &str, state: ViewState) -> PublishedView {
    let visible = if state == ViewState::Current {
        bytes
    } else {
        ""
    };
    PublishedView {
        version: PUBLICATION_VERSION.into(),
        publication_id: id.into(),
        citation: format!("publication://{id}"),
        view_revision: revision.into(),
        range: ViewRange {
            start: 0,
            end: visible.len(),
        },
        bytes: visible.into(),
        state,
        extractable: false,
        independent_support: false,
        uncertainty: crate::Uncertainty {
            contradiction_rate: 1.0,
            single_source_fraction: 1.0,
            staleness: 1.0,
            trust_exclusion_fraction: 1.0,
        },
    }
}

/// Private projection row, reconstructed solely from reserved Declarations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicationRecord {
    pub id: String,
    pub destination: String,
    pub principal: String,
    pub occurrence_id: String,
    pub request_digest: String,
    pub source_identity: String,
    pub source_revision: String,
    pub parent_id: Option<String>,
    pub root_family: String,
    pub view_revision: String,
    pub bytes: String,
    pub state: ViewState,
    pub declaration_id: String,
}

pub fn plan_record(
    request: &PublishViewV1,
    principal: &str,
    declaration_id: &str,
    parent: Option<&PublicationRecord>,
) -> Result<PublicationRecord, BrainError> {
    let (parent_id, family) = match &request.origin {
        ViewOrigin::Publication {
            space,
            publication_id,
            revision,
        } => {
            let parent = parent.ok_or_else(unavailable)?;
            if parent.id != *publication_id
                || parent.destination != *space
                || parent.view_revision != *revision
                || parent.state != ViewState::Current
            {
                return Err(unavailable());
            }
            (Some(parent.id.clone()), parent.root_family.clone())
        }
        _ => (None, root_family(&request.origin)),
    };
    Ok(PublicationRecord {
        id: publication_id(&request.destination, principal, &request.occurrence_id),
        destination: request.destination.clone(),
        principal: principal.into(),
        occurrence_id: request.occurrence_id.clone(),
        request_digest: request_digest(request),
        source_identity: request.origin.identity(),
        source_revision: request.origin.revision().into(),
        parent_id,
        root_family: family,
        view_revision: request.digest.clone(),
        bytes: request.bytes.clone(),
        state: ViewState::Current,
        declaration_id: declaration_id.into(),
    })
}

/// Bounded transitive closure, independent of SQL recursion limits and row order.
/// Corrupt cycles or overly deep/wide graphs cannot silently truncate closure.
pub fn descendant_closure(
    records: &[PublicationRecord],
    initial: &[String],
) -> Result<Vec<String>, BrainError> {
    use std::collections::{BTreeMap, BTreeSet};
    if records.len() > MAX_CLOSURE_VIEWS {
        return Err(BrainError::Budget(
            "publication closure exceeds bound".into(),
        ));
    }
    let parents: BTreeMap<_, _> = records
        .iter()
        .map(|row| (row.id.as_str(), row.parent_id.as_deref()))
        .collect();
    for id in parents.keys() {
        let mut path = BTreeSet::new();
        let mut current = Some(*id);
        while let Some(node) = current {
            if !path.insert(node) || path.len() > MAX_LINEAGE_DEPTH {
                return Err(BrainError::Corruption("invalid publication lineage".into()));
            }
            current = parents.get(node).copied().flatten();
        }
    }
    let mut selected: BTreeSet<String> = initial.iter().cloned().collect();
    loop {
        let before = selected.len();
        for row in records {
            if row
                .parent_id
                .as_ref()
                .is_some_and(|parent| selected.contains(parent))
            {
                selected.insert(row.id.clone());
            }
        }
        if selected.len() > MAX_CLOSURE_VIEWS {
            return Err(BrainError::Budget(
                "publication closure exceeds bound".into(),
            ));
        }
        if before == selected.len() {
            return Ok(selected.into_iter().collect());
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceViewObservation {
    pub origin_identity: String,
    pub previous_revision: String,
    pub observed_revision: Option<String>,
}
pub fn revision_is_closed(origin: &ViewOrigin, observations: &[SourceViewObservation]) -> bool {
    observations.iter().any(|event| {
        event.origin_identity == origin.identity()
            && event.previous_revision == origin.revision()
            && event.observed_revision.as_deref() != Some(origin.revision())
    })
}

pub struct PublicationProjectionPlan {
    pub insert: Option<PublicationRecord>,
    pub close: Vec<String>,
}
pub fn plan_projection(
    event: &PublicationEventV1,
    declaration: &str,
    declaration_space: &str,
    rows: &[PublicationRecord],
    observations: &[SourceViewObservation],
) -> Result<PublicationProjectionPlan, BrainError> {
    if rows.len() > MAX_CLOSURE_VIEWS
        || observations.len() > MAX_CLOSURE_VIEWS
        || rows.iter().map(|row| row.bytes.len()).sum::<usize>() > MAX_PROJECTION_BYTES
    {
        return Err(BrainError::Budget(
            "publication projection exceeds bound".into(),
        ));
    }
    match event {
        PublicationEventV1::Published { request, principal } => {
            if request.origin.space() != declaration_space {
                return Err(unavailable());
            }
            let parent = match &request.origin {
                ViewOrigin::Publication { publication_id, .. } => {
                    rows.iter().find(|row| row.id == *publication_id)
                }
                _ => None,
            };
            if revision_is_closed(&request.origin, observations) {
                return Err(unavailable());
            }
            let record = plan_record(request, principal, declaration, parent)?;
            if let Some(existing) = rows.iter().find(|row| row.id == record.id) {
                if existing.request_digest != record.request_digest {
                    return Err(BrainError::Conflict(
                        "publication occurrence already exists".into(),
                    ));
                }
                return Ok(PublicationProjectionPlan {
                    insert: None,
                    close: vec![],
                });
            }
            let encoded_bytes = rows
                .iter()
                .map(|row| {
                    serde_json::to_string(row)
                        .expect("publication record is serializable")
                        .len()
                })
                .sum::<usize>()
                .saturating_add(
                    serde_json::to_string(&record)
                        .expect("publication record is serializable")
                        .len(),
                );
            if encoded_bytes > MAX_PROJECTION_BYTES {
                return Err(BrainError::Budget(
                    "publication projection materialization bound exceeded".into(),
                ));
            }
            let mut next = rows.to_vec();
            next.push(record.clone());
            descendant_closure(&next, &[])?;
            Ok(PublicationProjectionPlan {
                insert: Some(record),
                close: vec![],
            })
        }
        PublicationEventV1::Withdrawn {
            destination,
            publication_id,
            ..
        } => {
            if destination != declaration_space
                || !rows
                    .iter()
                    .any(|row| row.id == *publication_id && row.destination == *destination)
            {
                return Err(unavailable());
            }
            Ok(PublicationProjectionPlan {
                insert: None,
                close: descendant_closure(rows, std::slice::from_ref(publication_id))?,
            })
        }
        PublicationEventV1::SourceObserved {
            origin_identity,
            previous_revision,
            observed_revision,
            ..
        } => {
            let identity: serde_json::Value =
                serde_json::from_str(origin_identity).map_err(|_| unavailable())?;
            if identity.get(1).and_then(serde_json::Value::as_str) != Some(declaration_space) {
                return Err(unavailable());
            }
            // A verified observation of the same revision is a no-op. The
            // facade, not this declaration payload, proves source authority.
            let initial: Vec<_> = rows
                .iter()
                .filter(|row| {
                    row.source_identity == *origin_identity
                        && row.source_revision == *previous_revision
                        && observed_revision.as_ref() != Some(previous_revision)
                })
                .map(|row| row.id.clone())
                .collect();
            Ok(PublicationProjectionPlan {
                insert: None,
                close: descendant_closure(rows, &initial)?,
            })
        }
    }
}

/// Pure conservative closure over copies. Predicate/entity redaction may pass
/// supporting episode IDs without tombstoning those original raw episodes.
pub fn publication_redaction_declarations(
    rows: &[PublicationRecord],
    episodes: &[String],
    spaces: &[String],
) -> Result<Vec<String>, BrainError> {
    let mut initial = Vec::new();
    for row in rows {
        let identity: serde_json::Value = serde_json::from_str(&row.source_identity)
            .map_err(|_| BrainError::Corruption("invalid publication source identity".into()))?;
        let source_space = identity.get(1).and_then(serde_json::Value::as_str);
        let episode = (identity.get(0).and_then(serde_json::Value::as_str) == Some("episode"))
            .then(|| identity.get(2).and_then(serde_json::Value::as_str))
            .flatten();
        if episodes.contains(&row.declaration_id)
            || episode.is_some_and(|id| episodes.iter().any(|value| value == id))
            || spaces.contains(&row.destination)
            || source_space.is_some_and(|space| spaces.iter().any(|value| value == space))
        {
            initial.push(row.id.clone());
        }
    }
    let ids = descendant_closure(rows, &initial)?;
    Ok(rows
        .iter()
        .filter(|row| ids.contains(&row.id))
        .map(|row| row.declaration_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn origin(id: &str) -> ViewOrigin {
        ViewOrigin::Episode {
            space: "private".into(),
            episode_id: id.into(),
            revision: "verified-revision".into(),
        }
    }
    fn request(text: &str) -> PublishViewV1 {
        PublishViewV1 {
            occurrence_id: "occurrence".into(),
            origin: origin("one"),
            range: ViewRange {
                start: 0,
                end: text.len(),
            },
            bytes: text.into(),
            digest: exact_digest(text),
            destination: "shared".into(),
            policy_revision: "policy-v1".into(),
            occurred_at: Timestamp(1),
        }
    }
    #[test]
    fn grants_remain_separate_and_equal_bytes_do_not_merge_occurrences() {
        let read = Scope {
            spaces: vec!["private".into()],
            caps: Capability::parse_set("read"),
            ..Default::default()
        };
        let write = Scope {
            spaces: vec!["shared".into()],
            caps: Capability::parse_set("write"),
            ..Default::default()
        };
        authorize_publication(&read, &write, "private", "shared", Timestamp(2)).unwrap();
        assert!(!read.permits(Capability::Write, "private", Timestamp(2)));
        assert!(!write.permits(Capability::Read, "shared", Timestamp(2)));
        assert!(authorize_publication(&write, &read, "private", "shared", Timestamp(2)).is_err());
        assert_ne!(root_family(&origin("one")), root_family(&origin("two")));
        assert_ne!(
            publication_id("shared", "principal", "one"),
            publication_id("shared", "principal", "two")
        );
    }
    #[test]
    fn pdc_source_family_uses_canonical_uuid_instead_of_a_path_cache_key() {
        let one = ViewOrigin::Document {
            space: "private".into(),
            root: "root".into(),
            document_id: "old-path-key".into(),
            document_uuid: Some("018f47c6-4a77-7c52-9db8-0e5f9bcb1705".into()),
            revision: "one".into(),
            decoder: "decoder".into(),
        };
        let two = ViewOrigin::Document {
            space: "private".into(),
            root: "root".into(),
            document_id: "new-path-key".into(),
            document_uuid: Some("018f47c6-4a77-7c52-9db8-0e5f9bcb1705".into()),
            revision: "two".into(),
            decoder: "decoder".into(),
        };
        assert_eq!(root_family(&one), root_family(&two));
    }
    #[test]
    fn wrong_revision_digest_and_non_utf8_ranges_are_opaque() {
        let snapshot = SourceViewSnapshot {
            origin: origin("one"),
            text: "선택 view".into(),
            available: true,
        };
        let valid = request(&snapshot.text);
        prepare_publication(&valid, &snapshot, "principal", Timestamp(2)).unwrap();
        for invalid in [
            PublishViewV1 {
                digest: "forged".into(),
                ..valid.clone()
            },
            PublishViewV1 {
                origin: origin("other-private-id"),
                ..valid.clone()
            },
            PublishViewV1 {
                range: ViewRange { start: 1, end: 3 },
                ..valid.clone()
            },
        ] {
            let error = prepare_publication(&invalid, &snapshot, "principal", Timestamp(2))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "unauthorized: publication unavailable");
            assert!(!error.contains("other-private-id"));
        }
    }
    #[test]
    fn destination_dto_has_no_origin_and_closed_views_have_no_bytes() {
        let view = audience_view(
            "public-id",
            "selected bytes",
            "revision",
            ViewState::Current,
        );
        assert!(!view.extractable);
        assert!(!view.independent_support);
        assert_eq!(view.range, ViewRange { start: 0, end: 14 });
        let json = serde_json::to_value(view).unwrap();
        for key in [
            "origin",
            "root_family",
            "locator",
            "source",
            "principal",
            "policy_revision",
        ] {
            assert!(json.get(key).is_none());
        }
        for state in [ViewState::Closed, ViewState::Redacted] {
            assert!(
                audience_view("public-id", "selected bytes", "revision", state)
                    .bytes
                    .is_empty()
            );
        }
    }
    proptest! {
        #[test]
        fn exact_source_views_are_deterministic(text in ".{1,200}") {
            let request = request(&text);
            let snapshot = SourceViewSnapshot { origin: request.origin.clone(), text, available: true };
            let one = prepare_publication(&request, &snapshot, "principal", Timestamp(2)).unwrap();
            let two = prepare_publication(&request, &snapshot, "principal", Timestamp(2)).unwrap();
            prop_assert_eq!(one, two);
            let mutated = PublishViewV1 { bytes: format!("{}x", request.bytes), ..request.clone() };
            prop_assert!(prepare_publication(&mutated, &snapshot, "principal", Timestamp(2)).is_err());
        }
    }
}

#[cfg(test)]
mod closure_tests {
    use super::*;
    fn row(id: &str, parent: Option<&str>) -> PublicationRecord {
        PublicationRecord {
            id: id.into(),
            destination: "audience".into(),
            principal: "principal".into(),
            occurrence_id: id.into(),
            request_digest: "digest".into(),
            source_identity: "source".into(),
            source_revision: "revision".into(),
            parent_id: parent.map(str::to_owned),
            root_family: "family".into(),
            view_revision: "view".into(),
            bytes: "bytes".into(),
            state: ViewState::Current,
            declaration_id: "declaration".into(),
        }
    }
    #[test]
    fn closure_is_transitive_sorted_and_cycle_safe() {
        let rows = vec![
            row("child", Some("root")),
            row("root", None),
            row("last", Some("child")),
            row("other", None),
        ];
        assert_eq!(
            descendant_closure(&rows, &["root".into()]).unwrap(),
            vec!["child", "last", "root"]
        );
        assert!(
            descendant_closure(&[row("a", Some("b")), row("b", Some("a"))], &["a".into()]).is_err()
        );
        let chain: Vec<_> = (0..=MAX_LINEAGE_DEPTH)
            .map(|i| {
                row(
                    &i.to_string(),
                    (i > 0).then(|| (i - 1).to_string()).as_deref(),
                )
            })
            .collect();
        assert!(descendant_closure(&chain, &["0".into()]).is_err());
    }
}
