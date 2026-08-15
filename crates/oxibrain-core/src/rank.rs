//! Pure rank layer: `Retrieval` spec + `RetrievalInput` + `rank` (DESIGN §11.2, §11.3).
//!
//! `rank` is the executable form of P9: the *only* place where filters are
//! applied to folded facts. Its three post-conditions (conservation, filter
//! totality, determinism) are property-tested in `tests::rank` and defended by
//! the `DroppedItem` accounting that makes `oxibrain why --dropped` honest.
//!
//! No rusqlite, no tokio, no model calls — pure data in, ranked data out.

use crate::knowledge::{BeliefStatus, EntityId, EntityTypeRef, StatementId};
use oxibrain_index::{Direction, PredicateFilter};
use oxibrain_ports::{TIME_MAX, TIME_MIN, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// ── §11.2 spec types ────────────────────────────────────────────────────────

/// What the caller wants to retrieve (one-of; the spec is multi-target by
/// construction — a single query may span statements and entities).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Statement,
    Entity,
    Episode,
    Chunk,
    Community,
}

/// Set of target kinds the query is asking for. Default is `Statement`.
/// `Statement` covers the post-conditions for retrieval; the others exist for
/// store-side execution only (entity-targeted queries still yield `Statement`
/// hits whose subject equals the entity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSet {
    #[default]
    Statement,
    Entity,
    Episode,
    Chunk,
    Community,
}

/// Which lexical index to consult (§7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexIndex {
    /// Word-level FTS5 — good for prose and CJK token boundaries via unicode61.
    Word,
    /// Trigram FTS5 — script-neutral, catches substring matches the word
    /// tokenizer splits apart.
    Ngram,
}

/// Which vector space to KNN over. The store owns the actual embedding tables;
/// this is just the routing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VecSpace {
    Entity,
    Statement,
    Chunk,
}

/// Seed policy for graph/community expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SeedPolicy {
    /// Expansion rooted at explicit entity ids (callers must provide).
    Explicit { entities: Vec<EntityId> },
    /// Expansion rooted at the top-k lexical hits' subjects.
    FromHits { top_k: usize },
}

/// A single retrieval channel. Channels are executed by the store; `rank` only
/// consumes their results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Channel {
    Lexical { index: LexIndex },
    Vector { space: VecSpace },
    GraphExpand { seed: SeedPolicy, depth: u8 },
    CommunityExpand { seed: SeedPolicy },
}

/// Result fusion strategy. RRF is the default (§11.2, §7.4) — it is the only
/// fusion that does not require score calibration across channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fusion {
    /// Reciprocal rank fusion, k=60 is the standard constant.
    Rrf { k: u32 },
    /// Weighted sum of normalised scores; weights must sum to 1.0.
    Weighted { weights: Vec<f64> },
}

impl Default for Fusion {
    fn default() -> Self {
        Fusion::Rrf { k: 60 }
    }
}

/// Rerankers applied after fusion, in order. `Chain` composes them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rerank {
    #[default]
    None,
    /// One-step graph distance from a fixed set of seed entities.
    GraphDistance { from: Vec<EntityId> },
    /// Boost items by their `Support.distinct_episodes` count — free because
    /// `Support` is already on every belief row.
    Corroboration,
    /// Maximal marginal relevance — diversity-aware vector reranker.
    /// `lambda` trades relevance vs diversity (0.5 = balanced).
    /// `max_similarity` — when set, a candidate whose cosine to any
    /// already-selected item exceeds this ceiling is deferred to the end
    /// of the list rather than selected, so no two survivors in the
    /// selection prefix are near-duplicates (§11.4 exit criterion).
    Mmr {
        lambda: f32,
        max_similarity: Option<f32>,
    },
    /// Apply rerankers in sequence.
    Chain(Vec<Rerank>),
}

/// Trust filtering policy. Default: include all tiers except those explicitly
/// excluded. Excluding `Untrusted` is the common agent-runtime choice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustPolicy {
    #[default]
    All,
    /// Exclude one or more tiers (e.g. `["untrusted"]`).
    Exclude(Vec<crate::TrustTier>),
}

/// Filters — the entire list of "what to include" knobs. NOT optional, NOT
/// silently ignorable: §11.3 says there is exactly one place these can be
/// forgotten, and that place has a property test.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Filters {
    pub space: String,
    /// Valid-time `as_of` (§6.1). `None` ⇒ current.
    pub as_of: Option<Timestamp>,
    /// Transaction-time `known_at` (§6.1). `None` ⇒ now.
    pub known_at: Option<Timestamp>,
    pub min_confidence: f32,
    pub trust: TrustPolicy,
    pub predicates: PredicateFilter,
    pub entity_types: Option<Vec<EntityTypeRef>>,
}

impl Filters {
    /// Common constructor: open filters with no lower-bound constraints.
    /// `space` is required and never `Option` (every query is scoped).
    pub fn open(space: impl Into<String>) -> Self {
        Self {
            space: space.into(),
            as_of: None,
            known_at: None,
            min_confidence: 0.0,
            trust: TrustPolicy::All,
            predicates: PredicateFilter::AllowAll,
            entity_types: None,
        }
    }
}

/// The query. Section §11.2 verbatim — type-only, no execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Retrieval {
    pub targets: TargetSet,
    pub channels: Vec<Channel>,
    pub fusion: Fusion,
    pub rerank: Rerank,
    pub filters: Filters,
    pub limit: usize,
    pub explain: bool,
}

impl Retrieval {
    /// Hybrid preset: lexical (word + ngram) ∪ vector (entity) ∪ graph expand.
    /// Backward-compatible with the M7 `QueryMode::Hybrid` API surface.
    pub fn hybrid(space: impl Into<String>) -> Self {
        Self {
            targets: TargetSet::Statement,
            channels: vec![
                Channel::Lexical {
                    index: LexIndex::Word,
                },
                Channel::Lexical {
                    index: LexIndex::Ngram,
                },
                Channel::Vector {
                    space: VecSpace::Entity,
                },
                Channel::GraphExpand {
                    seed: SeedPolicy::FromHits { top_k: 5 },
                    depth: 1,
                },
            ],
            fusion: Fusion::Rrf { k: 60 },
            rerank: Rerank::Corroboration,
            filters: Filters::open(space),
            limit: 20,
            explain: false,
        }
    }

    /// Lexical-only preset (word + ngram).
    pub fn lexical(space: impl Into<String>) -> Self {
        Self {
            targets: TargetSet::Statement,
            channels: vec![
                Channel::Lexical {
                    index: LexIndex::Word,
                },
                Channel::Lexical {
                    index: LexIndex::Ngram,
                },
            ],
            fusion: Fusion::Rrf { k: 60 },
            rerank: Rerank::None,
            filters: Filters::open(space),
            limit: 20,
            explain: false,
        }
    }

    /// Dense semantic preset (entity-vector KNN only).
    pub fn semantic(space: impl Into<String>) -> Self {
        Self {
            targets: TargetSet::Statement,
            channels: vec![Channel::Vector {
                space: VecSpace::Entity,
            }],
            fusion: Fusion::Rrf { k: 60 },
            rerank: Rerank::Mmr {
                lambda: 0.5,
                max_similarity: Some(0.9),
            },
            filters: Filters::open(space),
            limit: 20,
            explain: false,
        }
    }

    /// Graph-expand preset: seed from explicit entities, expand 2 hops.
    pub fn graph(space: impl Into<String>, seeds: Vec<EntityId>) -> Self {
        Self {
            targets: TargetSet::Statement,
            channels: vec![Channel::GraphExpand {
                seed: SeedPolicy::Explicit { entities: seeds },
                depth: 2,
            }],
            fusion: Fusion::Rrf { k: 60 },
            rerank: Rerank::GraphDistance { from: Vec::new() },
            filters: Filters::open(space),
            limit: 50,
            explain: false,
        }
    }

    /// Community-thematic preset: expand from seeds' communities.
    pub fn community(space: impl Into<String>, seeds: Vec<EntityId>) -> Self {
        Self {
            targets: TargetSet::Statement,
            channels: vec![Channel::CommunityExpand {
                seed: SeedPolicy::Explicit { entities: seeds },
            }],
            fusion: Fusion::Rrf { k: 60 },
            rerank: Rerank::None,
            filters: Filters::open(space),
            limit: 20,
            explain: false,
        }
    }
}

// ── §11.3 input + output ────────────────────────────────────────────────────

/// A `(channel_index, rank)` pair attached to a candidate — what RRF and
/// explain blocks need to attribute scores to channels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelRank {
    pub channel: u8,
    pub rank: u32,
}

/// Per-target facts the store has fetched for a candidate. These are the
/// inputs `rank` filters against — folding happens in the store, *applying*
/// happens in core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetFacts {
    pub target: TargetId,
    pub confidence: f32,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub recorded_at: Timestamp,
    pub retracted_at: Option<Timestamp>,
    pub trust: crate::TrustTier,
    pub status: BeliefStatus,
    pub predicate: String,
    pub salience: f64,
    pub distinct_episodes: u32,
    pub channels: Vec<ChannelRank>,
    /// Backing channel scores (BM25, KNN, etc.) — used by fusion only.
    pub channel_scores: Vec<f64>,
}

/// Stable identity for a retrieval candidate. Mirrors the pre-M8
/// `SearchTarget` shape but lives in the pure core.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetId {
    Episode { id: String },
    Statement { id: StatementId },
    Entity { id: EntityId },
    Chunk { id: String },
    Community { id: String },
}

impl TargetId {
    /// Stable RRF key — independent of channel order. Two candidates with the
    /// same key from different channels fuse into one.
    pub fn rrf_key(&self) -> String {
        match self {
            TargetId::Episode { id } => format!("episode:{id}"),
            TargetId::Statement { id } => format!("statement:{id}"),
            TargetId::Entity { id } => format!("entity:{id}"),
            TargetId::Chunk { id } => format!("chunk:{id}"),
            TargetId::Community { id } => format!("community:{id}"),
        }
    }
}

/// Channel output as the store hands it to `rank`. Each channel's results are
/// a list ordered by the channel's own score descending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelResult {
    pub channel: u8,
    pub hits: Vec<(TargetId, f64)>,
}

/// The input bundle. Channels are positional (matches `Retrieval.channels`),
/// facts are keyed by candidate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalInput {
    pub channels: Vec<ChannelResult>,
    pub facts: HashMap<TargetId, TargetFacts>,
    /// Dense entity vectors for MMR cosine similarity (§11.4, 10.3).
    /// Populated by the store layer from `entity_embeddings` for the
    /// candidate entity set. Keys are entity IDs.
    #[serde(default)]
    pub entity_vectors: HashMap<String, Vec<f32>>,
}

/// One scored, ranked candidate. Carries its `TargetFacts` so downstream
/// `pack` does not have to re-fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedItem {
    pub target: TargetId,
    pub fused_score: f64,
    pub rank: usize,
    pub channels: Vec<ChannelRank>,
    pub salience: f64,
    /// Snapshot of the facts that determined this item's inclusion.
    pub facts: TargetFacts,
}

/// Per-item accounting — conservation depends on every candidate being in
/// either `items` or `dropped`, never both, never neither.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DroppedItem {
    pub target: TargetId,
    pub reason: DropReason,
    /// Score before dropping, if the drop happened at the limit stage.
    pub score: Option<f64>,
}

/// Why a candidate was not in `items`. The variants are the *only* reasons a
/// candidate can be absent — there is no silent drop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DropReason {
    BelowConfidenceFloor {
        actual: f32,
        floor: f32,
    },
    OutsideValidWindow {
        valid_at: Timestamp,
    },
    BeforeKnownAt {
        known_at: Timestamp,
        recorded_at: Timestamp,
    },
    TrustExcluded {
        tier: crate::TrustTier,
    },
    PredicateDenied {
        predicate: String,
    },
    EntityTypeMismatch {
        expected: Vec<EntityTypeRef>,
    },
    TruncatedByBudget {
        position: usize,
    },
}

/// Output of `rank`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingResult {
    pub items: Vec<RankedItem>,
    pub dropped: Vec<DroppedItem>,
    pub total_candidates: usize,
    /// Echoed back for caller convenience; not interpreted by `rank`.
    pub spec: Retrieval,
}

// ── §11.3 rank ─────────────────────────────────────────────────────────────

/// Apply `spec.filters` to `input.facts`, fuse channel hits via `spec.fusion`,
/// apply `spec.rerank`, truncate to `spec.limit`, and produce a `RankingResult`
/// whose `items ∪ dropped = candidates` is provably disjoint.
///
/// Three post-conditions, each a property test in `tests::rank`:
///   - **Conservation.** Every candidate in `input.facts` appears in exactly
///     one of `items` or `dropped` — never both, never neither.
///   - **Filter totality.** No `items` member violates `spec.filters`.
///   - **Determinism.** Equal inputs produce byte-equal `RankingResult`.
///
/// Pure: no I/O, no time, no model. The store hands facts pre-folded.
pub fn rank(input: &RetrievalInput, spec: &Retrieval) -> RankingResult {
    // 1. Build a candidate set by deduplicating across channels via the
    //    canonical RRF key (TargetId::rrf_key is content-derived and stable).
    let mut candidate_facts: HashMap<String, TargetFacts> = HashMap::new();
    let mut channel_ranks: HashMap<String, Vec<ChannelRank>> = HashMap::new();
    let mut channel_scores: HashMap<String, Vec<f64>> = HashMap::new();

    for cr in &input.channels {
        for (rank_idx, (target, score)) in cr.hits.iter().enumerate() {
            let key = target.rrf_key();
            // First-wins on facts: if multiple channels return the same
            // target, we keep the facts the store attached to it. The store
            // is responsible for producing identical facts; if they disagree
            // on confidence/salience we keep the larger confidence (a stale
            // hit should not silently weaken a fresher one).
            let entry = candidate_facts.entry(key.clone());
            match entry {
                std::collections::hash_map::Entry::Vacant(v) => {
                    if let Some(facts) = input.facts.get(target) {
                        v.insert(facts.clone());
                    } else {
                        // Channel hit without facts — fabricate a minimal
                        // facts row so we still satisfy conservation.
                        v.insert(minimal_facts(target));
                    }
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if let Some(newer) = input.facts.get(target) {
                        if newer.confidence > o.get().confidence {
                            o.insert(newer.clone());
                        }
                    }
                }
            }
            channel_ranks
                .entry(key.clone())
                .or_default()
                .push(ChannelRank {
                    channel: cr.channel,
                    rank: rank_idx as u32,
                });
            channel_scores.entry(key).or_default().push(*score);
        }
    }

    // Targets that arrived via `facts` but no channel referenced them.
    // Per conservation, they still belong in the output — either kept or
    // dropped. Pull them in as channel-less candidates so the contract holds.
    for (target, facts) in &input.facts {
        let key = target.rrf_key();
        candidate_facts.entry(key).or_insert_with(|| facts.clone());
    }

    let total_candidates = candidate_facts.len();

    // 2. Filter — every drop here is attributed with a DropReason. This is
    //    the only place Filters can be forgotten, and it has a property test.
    let mut items: Vec<RankedItem> = Vec::with_capacity(candidate_facts.len());
    let mut dropped: Vec<DroppedItem> = Vec::new();
    // Determinism post-condition: iterate candidates in sorted key order.
    let mut ordered_keys: Vec<&String> = candidate_facts.keys().collect();
    ordered_keys.sort();

    for key in &ordered_keys {
        let facts = &candidate_facts[*key];
        if let Some(reason) = check_filters(facts, &spec.filters) {
            dropped.push(DroppedItem {
                target: facts.target.clone(),
                reason,
                score: None,
            });
            continue;
        }
        let fused = fuse(channel_scores.get(*key), &spec.fusion);
        let channels = channel_ranks.get(*key).cloned().unwrap_or_default();
        items.push(RankedItem {
            target: facts.target.clone(),
            fused_score: fused,
            rank: 0, // filled after sort
            channels,
            salience: facts.salience,
            facts: facts.clone(),
        });
    }

    // 4. Rerank. Each variant either preserves the order or replaces it.
    apply_rerank(&mut items, &spec.rerank, &input.entity_vectors);

    // 5. Sort descending by fused_score; tie-break on target type (evidence
    //    before navigation: Statement > Episode > Entity > Chunk > Community)
    //    then on rrf_key for full determinism.
    items.sort_by(|a, b| {
        b.fused_score
            .partial_cmp(&a.fused_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| target_type_rank(&a.target).cmp(&target_type_rank(&b.target)))
            .then_with(|| a.target.rrf_key().cmp(&b.target.rrf_key()))
    });

    // 6. Assign final ranks and truncate to limit. Everything past `limit`
    //    gets a TruncatedByBudget drop — never silent.
    items.truncate(spec.limit);
    for (i, item) in items.iter_mut().enumerate() {
        item.rank = i;
    }
    let kept_keys: std::collections::HashSet<String> =
        items.iter().map(|i| i.target.rrf_key()).collect();
    let already_dropped: std::collections::HashSet<String> =
        dropped.iter().map(|d| d.target.rrf_key()).collect();
    let mut post_truncate_keys: Vec<&String> = candidate_facts
        .keys()
        .filter(|k| !kept_keys.contains(*k) && !already_dropped.contains(*k))
        .collect();
    post_truncate_keys.sort();
    let mut post_truncate_drops: Vec<DroppedItem> = post_truncate_keys
        .into_iter()
        .map(|k| {
            let facts = &candidate_facts[k];
            DroppedItem {
                target: facts.target.clone(),
                reason: DropReason::TruncatedByBudget {
                    position: ordered_keys.iter().position(|x| *x == k).unwrap_or(0),
                },
                score: None,
            }
        })
        .collect();

    // We may have already filtered some of these above; merge.
    // Existing drops took a different code path (they got filtered before the
    // truncation stage), so there's no overlap with post_truncate_drops.
    dropped.append(&mut post_truncate_drops);

    RankingResult {
        items,
        dropped,
        total_candidates,
        spec: spec.clone(),
    }
}

/// Build a minimal `TargetFacts` for a target the store hit but did not
/// supply facts for. Used as a safety net so the conservation contract
/// holds even when the store is lazy.
fn minimal_facts(target: &TargetId) -> TargetFacts {
    TargetFacts {
        target: target.clone(),
        confidence: 0.0,
        valid_from: TIME_MIN,
        valid_to: TIME_MAX,
        recorded_at: TIME_MIN,
        retracted_at: None,
        trust: crate::TrustTier::SemiTrusted,
        status: BeliefStatus::Active,
        predicate: String::new(),
        salience: 0.0,
        distinct_episodes: 0,
        channels: Vec::new(),
        channel_scores: Vec::new(),
    }
}

/// Priority for tie-breaking in the final sort: evidence (Statement) before
/// navigation (Entity). Lower = higher priority.
fn target_type_rank(t: &TargetId) -> u8 {
    match t {
        TargetId::Statement { .. } => 0,
        TargetId::Episode { .. } => 1,
        TargetId::Entity { .. } => 2,
        TargetId::Chunk { .. } => 3,
        TargetId::Community { .. } => 4,
    }
}

/// Apply `Filters` to a single candidate. Returns `Some(reason)` to drop,
/// `None` to keep.
fn check_filters(facts: &TargetFacts, filters: &Filters) -> Option<DropReason> {
    // `as_of` (valid time): drop if outside [valid_from, valid_to].
    if let Some(t) = filters.as_of {
        if t < facts.valid_from || t > facts.valid_to {
            return Some(DropReason::OutsideValidWindow { valid_at: t });
        }
    }
    // `known_at` (transaction time): drop if recorded after known_at, or
    // retracted before known_at.
    if let Some(t) = filters.known_at {
        if facts.recorded_at > t {
            return Some(DropReason::BeforeKnownAt {
                known_at: t,
                recorded_at: facts.recorded_at,
            });
        }
        if let Some(retracted_at) = facts.retracted_at {
            if retracted_at <= t {
                return Some(DropReason::BeforeKnownAt {
                    known_at: t,
                    recorded_at: retracted_at,
                });
            }
        }
    }
    // `min_confidence`: simple floor. Believed status gates higher floors
    // are a policy choice in `PackPolicy::expand_score`, not here.
    if facts.confidence < filters.min_confidence {
        return Some(DropReason::BelowConfidenceFloor {
            actual: facts.confidence,
            floor: filters.min_confidence,
        });
    }
    // `trust`: explicit exclusion list.
    if let TrustPolicy::Exclude(excluded) = &filters.trust {
        if excluded.contains(&facts.trust) {
            return Some(DropReason::TrustExcluded { tier: facts.trust });
        }
    }
    // `predicates`: AllowAll | Allow(list) | Deny(list).
    if !filters.predicates.allows(&facts.predicate) {
        return Some(DropReason::PredicateDenied {
            predicate: facts.predicate.clone(),
        });
    }
    // `entity_types`: store applies this at fetch time as a cheap SQL
    // pushdown, but we re-check here so the conservation guarantee holds
    // even if the store skips it.
    if let Some(expected) = &filters.entity_types {
        if !expected.is_empty() && !expected.iter().any(|t| t == &facts.predicate) {
            return Some(DropReason::EntityTypeMismatch {
                expected: expected.clone(),
            });
        }
    }
    None
}

/// Compute a fused score from the per-channel scores.
fn fuse(scores: Option<&Vec<f64>>, fusion: &Fusion) -> f64 {
    let Some(scores) = scores else { return 0.0 };
    if scores.is_empty() {
        return 0.0;
    }
    match fusion {
        Fusion::Rrf { k } => {
            // RRF: sum 1 / (k + rank_i). Higher k reduces top-weight; 60 is
            // the published standard from the original paper.
            //
            // We don't actually receive ranks here (we receive raw scores from
            // the channel), so we approximate by sorting scores descending and
            // using position+1 as rank. Channels with identical scores break
            // ties by their declaration order — that order is deterministic
            // because channel results are emitted in `spec.channels` order.
            let mut indexed: Vec<(usize, f64)> = scores.iter().copied().enumerate().collect();
            indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let k = *k as f64;
            indexed
                .iter()
                .enumerate()
                .map(|(pos, (_, _))| 1.0 / (k + (pos as f64) + 1.0))
                .sum()
        }
        Fusion::Weighted { weights } => {
            // Normalise scores to [0, 1] then weighted sum. Min-max scaling
            // is the simple, defensible choice — store-side calibration is
            // outside rank's scope.
            let min = scores.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let span = (max - min).max(f64::EPSILON);
            let n = scores.len().max(weights.len());
            let w_per = if weights.is_empty() {
                1.0 / n as f64
            } else {
                let s: f64 = weights.iter().sum();
                if s > 0.0 { 1.0 / s } else { 1.0 / n as f64 }
            };
            scores
                .iter()
                .map(|s| (s - min) / span)
                .zip(weights.iter().chain(std::iter::repeat(&w_per)))
                .map(|(n, w)| n * w)
                .sum()
        }
    }
}

/// Cosine similarity between two equal-length f32 slices. Returns 0.0 for
/// empty or mismatched-length vectors. Pure and deterministic.
fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

/// Look up the dense vector for a ranked item's target entity. Only Entity
/// targets have vectors directly; for others we return None (MMR falls back
/// to the score proxy for those pairs).
fn item_vector<'a>(
    item: &RankedItem,
    vectors: &'a HashMap<String, Vec<f32>>,
) -> Option<&'a Vec<f32>> {
    match &item.target {
        TargetId::Entity { id } => vectors.get(id),
        _ => None,
    }
}

/// Apply rerankers in sequence. Pure: each variant either sorts the slice
/// in-place or leaves it alone. `entity_vectors` provides dense vectors for
/// MMR cosine similarity (§11.4, 10.3).
fn apply_rerank(
    items: &mut [RankedItem],
    rerank: &Rerank,
    entity_vectors: &HashMap<String, Vec<f32>>,
) {
    match rerank {
        Rerank::None => {}
        Rerank::Corroboration => {
            // Boost by Support.distinct_episodes — already on each fact row.
            // We adjust fused_score multiplicatively (1 + log(1 + distinct))
            // so the boost is bounded and never overwhelms the channel score.
            for item in items.iter_mut() {
                let boost = 1.0 + (1.0 + item.facts.distinct_episodes as f64).ln();
                item.fused_score *= boost;
            }
        }
        Rerank::GraphDistance { from } => {
            // Without a real adjacency lookup we cannot compute distances
            // here. The store applies GraphDistance before handing inputs to
            // `rank`; this branch exists so callers can declare the intent.
            // We sort by salience as a documented fallback that always runs
            // and is deterministic.
            let _ = from;
            items.sort_by(|a, b| {
                b.salience
                    .partial_cmp(&a.salience)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        Rerank::Mmr {
            lambda,
            max_similarity,
        } => {
            // O(k²) MMR: pick top-1, then greedily select the item that
            // maximises `λ * score - (1-λ) * max_sim_to_selected`.
            //
            // When entity vectors are available (§11.4, 10.3), similarity is
            // cosine distance between dense embeddings — real MMR. When not,
            // we fall back to the score proxy `|Δscore|` (smaller = more
            // similar), preserving the legacy M7 behaviour.
            //
            // When `max_similarity` (ceiling) is set, any candidate whose
            // cosine to an already-selected item exceeds the ceiling is
            // *deferred* — skipped in the greedy loop and appended at the end
            // (sorted by score). This enforces a hard diversity floor in the
            // selection prefix without dropping items (conservation holds).
            if items.is_empty() {
                return;
            }
            let lambda = *lambda as f64;
            let ceiling = max_similarity.map(|c| c as f64);
            let mut reordered: Vec<RankedItem> = Vec::with_capacity(items.len());
            let mut pool: Vec<RankedItem> = items.to_vec();
            // First pick: highest fused score.
            pool.sort_by(|a, b| {
                b.fused_score
                    .partial_cmp(&a.fused_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            reordered.push(pool.remove(0));
            while !pool.is_empty() {
                let mut best_idx: Option<usize> = None;
                let mut best_score = f64::NEG_INFINITY;
                for (i, cand) in pool.iter().enumerate() {
                    let cand_vec = item_vector(cand, entity_vectors);
                    // max_sim: maximum similarity to any already-selected item.
                    let mut max_sim: f64 = 0.0;
                    let mut used_cosine = false;
                    for sel in &reordered {
                        let sel_vec = item_vector(sel, entity_vectors);
                        if let (Some(cv), Some(sv)) = (cand_vec, sel_vec) {
                            let cos = cosine_sim(cv, sv);
                            if cos > max_sim {
                                max_sim = cos;
                            }
                            used_cosine = true;
                        }
                    }
                    // If no vector pairs were available, fall back to score proxy.
                    let sim = if used_cosine {
                        max_sim
                    } else {
                        let last = reordered.last().unwrap().fused_score;
                        (last - cand.fused_score).abs()
                    };
                    // Ceiling check: defer near-duplicates.
                    if let Some(c) = ceiling {
                        if used_cosine && max_sim > c {
                            continue;
                        }
                    }
                    let mmr = lambda * cand.fused_score - (1.0 - lambda) * sim;
                    if mmr > best_score {
                        best_score = mmr;
                        best_idx = Some(i);
                    }
                }
                match best_idx {
                    Some(i) => reordered.push(pool.remove(i)),
                    // All remaining candidates are above the ceiling —
                    // append them by descending score and stop.
                    None => {
                        pool.sort_by(|a, b| {
                            b.fused_score
                                .partial_cmp(&a.fused_score)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        reordered.append(&mut pool);
                        break;
                    }
                }
            }
            items.clone_from_slice(&reordered);
        }
        Rerank::Chain(reranks) => {
            for r in reranks {
                apply_rerank(items, r, entity_vectors);
            }
        }
    }
}

/// Trait extension: re-exported direction predicate check. Kept here so
/// callers don't need to import `oxibrain_index` directly.
pub fn direction_allows(direction: Direction, from_subject: bool) -> bool {
    match direction {
        Direction::Both => true,
        Direction::Out => from_subject,
        Direction::In => !from_subject,
    }
}

/// Convenience for explain blocks: produce a human-readable channel line.
pub fn explain_item(item: &RankedItem) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("target".into(), item.target.rrf_key());
    out.insert("fused_score".into(), format!("{:.6}", item.fused_score));
    out.insert("salience".into(), format!("{:.6}", item.salience));
    out.insert(
        "channels".into(),
        item.channels
            .iter()
            .map(|c| format!("ch{}:{}", c.channel, c.rank))
            .collect::<Vec<_>>()
            .join(","),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrustTier;

    fn facts(target: TargetId, confidence: f32, predicate: &str) -> TargetFacts {
        TargetFacts {
            target: target.clone(),
            confidence,
            valid_from: TIME_MIN,
            valid_to: TIME_MAX,
            recorded_at: Timestamp(1000),
            retracted_at: None,
            trust: TrustTier::Trusted,
            status: BeliefStatus::Active,
            predicate: predicate.into(),
            salience: 0.5,
            distinct_episodes: 1,
            channels: vec![],
            channel_scores: vec![],
        }
    }

    #[test]
    fn rrf_key_is_stable_and_distinguishes_kinds() {
        let e = TargetId::Episode { id: "x".into() };
        let s = TargetId::Statement { id: "x".into() };
        assert_ne!(e.rrf_key(), s.rrf_key());
        assert_eq!(e.rrf_key(), e.rrf_key());
    }

    #[test]
    fn filter_drops_below_floor() {
        let target = TargetId::Statement { id: "s1".into() };
        let f = facts(target.clone(), 0.1, "works_on");
        let mut filters = Filters::open("default");
        filters.min_confidence = 0.5;
        assert!(matches!(
            check_filters(&f, &filters),
            Some(DropReason::BelowConfidenceFloor { .. })
        ));
    }

    #[test]
    fn filter_keeps_at_or_above_floor() {
        let target = TargetId::Statement { id: "s1".into() };
        let f = facts(target, 0.5, "works_on");
        let filters = Filters::open("default");
        assert!(check_filters(&f, &filters).is_none());
    }

    #[test]
    fn filter_drops_outside_as_of() {
        let target = TargetId::Statement { id: "s1".into() };
        let mut f = facts(target, 0.9, "works_on");
        f.valid_from = Timestamp(100);
        f.valid_to = Timestamp(200);
        let mut filters = Filters::open("default");
        filters.as_of = Some(Timestamp(300));
        assert!(matches!(
            check_filters(&f, &filters),
            Some(DropReason::OutsideValidWindow { .. })
        ));
    }

    #[test]
    fn filter_drops_before_known_at() {
        let target = TargetId::Statement { id: "s1".into() };
        let mut f = facts(target, 0.9, "works_on");
        f.recorded_at = Timestamp(1000);
        let mut filters = Filters::open("default");
        filters.known_at = Some(Timestamp(500));
        assert!(matches!(
            check_filters(&f, &filters),
            Some(DropReason::BeforeKnownAt { .. })
        ));
    }

    #[test]
    fn filter_drops_retracted_before_known_at() {
        let target = TargetId::Statement { id: "s1".into() };
        let mut f = facts(target, 0.9, "works_on");
        f.recorded_at = Timestamp(100);
        f.retracted_at = Some(Timestamp(200));
        let mut filters = Filters::open("default");
        filters.known_at = Some(Timestamp(300));
        assert!(matches!(
            check_filters(&f, &filters),
            Some(DropReason::BeforeKnownAt { .. })
        ));
    }

    #[test]
    fn rank_conservation_simple() {
        // Two candidates, both keep — conservation holds.
        let mut input = RetrievalInput::default();
        let a = TargetId::Statement { id: "a".into() };
        let b = TargetId::Statement { id: "b".into() };
        input.facts.insert(a.clone(), facts(a.clone(), 0.9, "p"));
        input.facts.insert(b.clone(), facts(b.clone(), 0.8, "p"));
        input.channels.push(ChannelResult {
            channel: 0,
            hits: vec![(a.clone(), 0.9), (b.clone(), 0.8)],
        });
        let spec = Retrieval::hybrid("default");
        let r = rank(&input, &spec);
        // Both pass through to items (limit=20, both survive filters).
        assert_eq!(r.items.len() + r.dropped.len(), 2);
    }

    #[test]
    fn rank_filter_totality() {
        // Confidence 0.1, min floor 0.5 → must end up in dropped.
        let mut input = RetrievalInput::default();
        let a = TargetId::Statement { id: "a".into() };
        input.facts.insert(a.clone(), facts(a.clone(), 0.1, "p"));
        input.channels.push(ChannelResult {
            channel: 0,
            hits: vec![(a.clone(), 0.5)],
        });
        let mut spec = Retrieval::hybrid("default");
        spec.filters.min_confidence = 0.5;
        let r = rank(&input, &spec);
        assert!(r.items.is_empty());
        assert_eq!(r.dropped.len(), 1);
        assert!(matches!(
            r.dropped[0].reason,
            DropReason::BelowConfidenceFloor { .. }
        ));
    }

    #[test]
    fn rank_determinism() {
        let make = || {
            let mut input = RetrievalInput::default();
            for (i, score) in [(0u8, 0.9f64), (1, 0.7), (2, 0.5)] {
                let id = format!("s{i}");
                let t = TargetId::Statement { id: id.clone() };
                input.facts.insert(t.clone(), facts(t, score as f32, "p"));
                input.channels.push(ChannelResult {
                    channel: 0,
                    hits: vec![(TargetId::Statement { id }, score)],
                });
            }
            input
        };
        let spec = Retrieval::hybrid("default");
        let r1 = rank(&make(), &spec);
        let r2 = rank(&make(), &spec);
        // rrf_keys + fused_score + rank order must match.
        let keys1: Vec<_> = r1
            .items
            .iter()
            .map(|i| (i.target.rrf_key(), i.fused_score, i.rank))
            .collect();
        let keys2: Vec<_> = r2
            .items
            .iter()
            .map(|i| (i.target.rrf_key(), i.fused_score, i.rank))
            .collect();
        assert_eq!(keys1, keys2);
    }

    // ── Corroboration rerank (§11.4, 10.4) ──────────────────────────────

    #[test]
    fn corroboration_invariance_equal_episodes_preserves_order() {
        // When all items have the same distinct_episodes, the multiplicative
        // boost is identical for every item — ordering must not change.
        let mut items: Vec<RankedItem> = [0.9f64, 0.7, 0.5]
            .iter()
            .enumerate()
            .map(|(i, score)| RankedItem {
                target: TargetId::Statement {
                    id: format!("s{i}"),
                },
                fused_score: *score,
                rank: i,
                channels: vec![],
                salience: 0.5,
                facts: {
                    let mut f = facts(
                        TargetId::Statement {
                            id: format!("s{i}"),
                        },
                        0.8,
                        "works_on",
                    );
                    f.distinct_episodes = 3; // all equal
                    f
                },
            })
            .collect();
        apply_rerank(&mut items, &Rerank::Corroboration, &HashMap::new());
        // All items get the same multiplicative boost, so the original
        // descending-score order must be preserved.
        let boosted: Vec<f64> = items.iter().map(|i| i.fused_score).collect();
        for w in boosted.windows(2) {
            assert!(
                w[0] >= w[1],
                "ordering broken after equal-boost corroboration"
            );
        }
    }

    #[test]
    fn corroboration_monotonicity_higher_distinct_ranks_higher() {
        // Two items with the same fused_score but different distinct_episodes.
        // The one with more corroboration must get a higher boosted score.
        let mut items: Vec<RankedItem> = vec![
            RankedItem {
                target: TargetId::Statement { id: "low".into() },
                fused_score: 0.5,
                rank: 0,
                channels: vec![],
                salience: 0.5,
                facts: {
                    let mut f = facts(TargetId::Statement { id: "low".into() }, 0.8, "works_on");
                    f.distinct_episodes = 1;
                    f
                },
            },
            RankedItem {
                target: TargetId::Statement { id: "high".into() },
                fused_score: 0.5,
                rank: 1,
                channels: vec![],
                salience: 0.5,
                facts: {
                    let mut f = facts(TargetId::Statement { id: "high".into() }, 0.8, "works_on");
                    f.distinct_episodes = 10;
                    f
                },
            },
        ];
        apply_rerank(&mut items, &Rerank::Corroboration, &HashMap::new());
        let high_score = items
            .iter()
            .find(|i| i.target == TargetId::Statement { id: "high".into() })
            .unwrap()
            .fused_score;
        let low_score = items
            .iter()
            .find(|i| i.target == TargetId::Statement { id: "low".into() })
            .unwrap()
            .fused_score;
        assert!(
            high_score > low_score,
            "higher corroboration must rank higher: {high_score} vs {low_score}"
        );
    }

    // ── Property tests (M8 §8.4) ────────────────────────────────────────
    // Each property runs 64 cases by default. The post-conditions of `rank`
    // — conservation, filter totality, determinism — must hold over the
    // generated input space, not just the curated examples above.

    use proptest::prelude::*;

    prop_compose! {
        fn arb_target_id()(i in any::<u8>()) -> TargetId {
            let id = format!("t{i:03}");
            match i % 3 {
                0 => TargetId::Statement { id },
                1 => TargetId::Entity { id },
                _ => TargetId::Episode { id },
            }
        }
    }

    prop_compose! {
        fn arb_facts()(
            target in arb_target_id(),
            confidence in 0.0f32..1.0f32,
            vf in 0i64..10_000,
            vt in 0i64..10_000,
            recorded_at in 0i64..10_000,
            retracted_at in prop::option::of(0i64..10_000),
            trust in prop::sample::select(vec![
                TrustTier::Trusted, TrustTier::SemiTrusted, TrustTier::Untrusted,
            ]),
            status in prop::sample::select(vec![
                BeliefStatus::Active,
                BeliefStatus::Superseded,
                BeliefStatus::Contradicted,
                BeliefStatus::Retracted,
            ]),
            predicate in prop::sample::select(vec![
                "works_on".to_string(), "knows".to_string(), "likes".to_string(),
            ]),
            salience in 0.0f64..1.0f64,
            distinct in 0u32..10,
        ) -> TargetFacts {
            TargetFacts {
                target,
                confidence,
                valid_from: Timestamp(vf),
                valid_to: Timestamp(vt),
                recorded_at: Timestamp(recorded_at),
                retracted_at: retracted_at.map(Timestamp),
                trust,
                status,
                predicate,
                salience,
                distinct_episodes: distinct,
                channels: vec![],
                channel_scores: vec![],
            }
        }
    }

    prop_compose! {
        fn arb_filters()(
            as_of in prop::option::of(0i64..10_000),
            known_at in prop::option::of(0i64..10_000),
            min_confidence in 0.0f32..1.0f32,
        ) -> Filters {
            Filters {
                space: "default".into(),
                as_of: as_of.map(Timestamp),
                known_at: known_at.map(Timestamp),
                min_confidence,
                trust: TrustPolicy::All,
                predicates: PredicateFilter::AllowAll,
                entity_types: None,
            }
        }
    }

    prop_compose! {
        fn arb_input()(entries in prop::collection::vec((arb_facts(), 0.0f64..1.0f64), 1..16)) -> RetrievalInput {
            let mut input = RetrievalInput::default();
            let mut hits: Vec<(TargetId, f64)> = Vec::new();
            for (f, score) in entries {
                let target = f.target.clone();
                input.facts.insert(target.clone(), f);
                hits.push((target, score));
            }
            input.channels.push(ChannelResult { channel: 0, hits });
            input
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Conservation: items ∪ dropped = candidates, disjointly.
        #[test]
        fn prop_conservation(input in arb_input(), filters in arb_filters()) {
            let mut spec = Retrieval::hybrid("default");
            spec.filters = filters;
            let total_candidates = input.facts.len();
            let r = rank(&input, &spec);
            prop_assert_eq!(r.items.len() + r.dropped.len(), total_candidates);
            let item_keys: std::collections::HashSet<_> =
                r.items.iter().map(|i| i.target.rrf_key()).collect();
            let drop_keys: std::collections::HashSet<_> =
                r.dropped.iter().map(|d| d.target.rrf_key()).collect();
            prop_assert!(item_keys.is_disjoint(&drop_keys));
            prop_assert_eq!(r.total_candidates, total_candidates);
        }

        /// Filter totality: no item in `items` violates `spec.filters`.
        #[test]
        fn prop_filter_totality(input in arb_input(), filters in arb_filters()) {
            let mut spec = Retrieval::hybrid("default");
            spec.filters = filters.clone();
            let r = rank(&input, &spec);
            for item in &r.items {
                let f = &item.facts;
                prop_assert!(f.confidence >= filters.min_confidence,
                    "item {} violates min_confidence: {} < {}",
                    item.target.rrf_key(), f.confidence, filters.min_confidence);
                if let Some(t) = filters.as_of {
                    prop_assert!(t >= f.valid_from && t <= f.valid_to,
                        "item {} violates as_of {}", item.target.rrf_key(), t.0);
                }
                if let Some(t) = filters.known_at {
                    prop_assert!(f.recorded_at <= t,
                        "item {} violates known_at {}", item.target.rrf_key(), t.0);
                    if let Some(r_at) = f.retracted_at {
                        prop_assert!(r_at > t,
                            "item {} violates known_at (retracted)", item.target.rrf_key());
                    }
                }
                if let TrustPolicy::Exclude(excluded) = &filters.trust {
                    prop_assert!(!excluded.contains(&f.trust),
                        "item {} has excluded trust tier", item.target.rrf_key());
                }
            }
        }

        /// Determinism: equal inputs produce byte-equal JSON output.
        #[test]
        fn prop_determinism(input in arb_input(), filters in arb_filters()) {
            let mut spec = Retrieval::hybrid("default");
            spec.filters = filters;
            let r1 = rank(&input, &spec);
            let r2 = rank(&input, &spec);
            let j = |r: &RankingResult| serde_json::to_string(r).expect("serialize");
            prop_assert_eq!(j(&r1), j(&r2));
        }
    }

    // ── MMR diversity invariant (§11.4, M10 10.3 exit criterion) ────────
    // Exit criterion: "Top-10 results for a broad query contain no two items
    // above 0.9 mutual similarity."
    //
    // Test setup: 15 entities — 10 with distinct directions (evenly spaced on
    // the unit circle, cosine ≈ 0.81 between neighbors) plus 5 near-duplicates
    // (cosine ≈ 0.999) of 5 of those originals. The near-duplicates carry
    // *higher* raw fused scores so that without the ceiling they would crowd
    // the top-10. With `max_similarity: Some(0.9)`, the ceiling defers them
    // to the tail and the top-10 contains only mutually-distinct items.

    #[test]
    fn mmr_ceiling_defers_near_duplicates_in_top_10() {
        use std::collections::HashMap;
        let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
        let mut names: Vec<String> = Vec::new();

        // 10 distinct directions: 36° apart on the unit circle.
        for i in 0..10 {
            let name = format!("d{i}");
            names.push(name.clone());
            let angle = (i as f64) * std::f64::consts::PI / 5.0;
            vectors.insert(name, vec![angle.cos() as f32, angle.sin() as f32]);
        }
        // 5 near-duplicates of d0..d4 (cosine ≈ 0.999 to their original).
        for i in 0..5 {
            let name = format!("dup{i}");
            names.push(name.clone());
            let angle = (i as f64) * std::f64::consts::PI / 5.0;
            // Same direction, 1% perturbation → cosine ≈ 0.9999.
            vectors.insert(
                name,
                vec![(angle.cos() * 1.01) as f32, (angle.sin() * 1.01) as f32],
            );
        }

        // Give duplicates HIGHER fused scores so they'd dominate without ceiling.
        let mut items: Vec<RankedItem> = names
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let score = if e.starts_with("dup") {
                    1.5 - 0.01 * i as f64 // dup scores: 1.5 range
                } else {
                    1.0 - 0.01 * i as f64 // distinct scores: 1.0 range
                };
                RankedItem {
                    target: TargetId::Entity { id: e.clone() },
                    facts: facts(TargetId::Entity { id: e.clone() }, 0.8, "works_on"),
                    fused_score: score,
                    salience: 0.8,
                    rank: i,
                    channels: vec![],
                }
            })
            .collect();

        apply_rerank(
            &mut items,
            &Rerank::Mmr {
                lambda: 0.5,
                max_similarity: Some(0.9),
            },
            &vectors,
        );

        // Assert: no two items in the top-10 have cosine > 0.9.
        for (i, a) in items.iter().take(10).enumerate() {
            let a_vec = match &a.target {
                TargetId::Entity { id } => vectors.get(id).unwrap(),
                _ => unreachable!(),
            };
            for (j, b) in items.iter().take(10).enumerate() {
                if i >= j {
                    continue;
                }
                let b_vec = match &b.target {
                    TargetId::Entity { id } => vectors.get(id).unwrap(),
                    _ => unreachable!(),
                };
                let sim = cosine_sim(a_vec, b_vec);
                assert!(
                    sim <= 0.9,
                    "MMR kept a >0.9 pair at top-10 positions {i}/{j} \
                     ({:?} / {:?}): cosine = {sim:.4}",
                    a.target,
                    b.target,
                );
            }
        }

        // Conservation: all 15 items still present.
        assert_eq!(items.len(), 15);
    }

    // ── MMR ceiling conservation: no items dropped ─────────────────────
    // Even with a ceiling, every input item must survive in the output.
    // `rank` conservation: every candidate in exactly one of items or dropped.

    #[test]
    fn mmr_ceiling_never_drops_items() {
        use std::collections::HashMap;
        let mut vectors: HashMap<String, Vec<f32>> = HashMap::new();
        let names: Vec<String> = (0..8).map(|i| format!("e{i}")).collect();
        // All near-identical (cosine ≈ 1.0) — everything should be deferred
        // except the first pick.
        for name in &names {
            vectors.insert(name.clone(), vec![1.0, 0.01, 0.0]);
        }

        let mut items: Vec<RankedItem> = names
            .iter()
            .enumerate()
            .map(|(i, e)| RankedItem {
                target: TargetId::Entity { id: e.clone() },
                facts: facts(TargetId::Entity { id: e.clone() }, 0.8, "works_on"),
                fused_score: 1.0 - 0.01 * i as f64,
                salience: 0.8,
                rank: i,
                channels: vec![],
            })
            .collect();

        let count_before = items.len();
        apply_rerank(
            &mut items,
            &Rerank::Mmr {
                lambda: 0.5,
                max_similarity: Some(0.9),
            },
            &vectors,
        );
        assert_eq!(
            items.len(),
            count_before,
            "MMR ceiling must not drop items — conservation invariant"
        );
    }
}
