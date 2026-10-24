//! Scope-aware read orchestration. SQL fetches finish before pure fold/packing.
use crate::{Brain, BrainError, DocumentFreshness, SearchResponse, Timestamp};
use oxibrain_core::context::{ContextBudget, LayerKind};
use oxibrain_core::evidence::{EvidenceResult, EvidenceUnit, SourceCoordinate};
pub use oxibrain_core::read_scope::{ReadPolicy, ReadScope};
use oxibrain_core::retrieval::{Query, QueryMode, SearchPlane};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub text: String,
    pub scope: ReadScope,
    pub planes: BTreeSet<SearchPlane>,
    pub mode: QueryMode,
    pub limit: usize,
}
impl From<Query> for SearchRequest {
    fn from(q: Query) -> Self {
        Self {
            text: q.text,
            scope: ReadScope {
                space: q.space,
                as_of: q.as_of,
                known_at: None,
                min_confidence: q.min_confidence,
            },
            planes: q.planes,
            mode: q.mode,
            limit: q.limit,
        }
    }
}
#[derive(Debug, Clone)]
pub struct RecallRequest {
    pub text: String,
    pub scope: ReadScope,
    pub planes: BTreeSet<SearchPlane>,
    pub budget: ContextBudget,
    pub session_id: Option<String>,
    pub require_exact_tokens: bool,
    pub explain: bool,
    pub hints: crate::RecallHints,
}
impl RecallRequest {
    pub fn new(space: impl Into<String>, text: impl Into<String>, budget: usize) -> Self {
        Self {
            text: text.into(),
            scope: ReadScope::current(space),
            planes: [SearchPlane::Memory, SearchPlane::Documents]
                .into_iter()
                .collect(),
            budget: ContextBudget { max_tokens: budget },
            session_id: None,
            require_exact_tokens: false,
            explain: false,
            hints: crate::RecallHints::default(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResponse<T> {
    pub data: T,
    pub meta: ReadMeta,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReadMeta {
    pub scope: serde_json::Value,
    pub tokens: Option<serde_json::Value>,
    pub layers_kept: usize,
    pub dropped: BTreeMap<String, usize>,
    pub decisions: Vec<oxibrain_core::evidence::UnitDecision>,
    pub freshness: DocumentFreshness,
    pub index_coverage: serde_json::Value,
    pub execution: serde_json::Value,
    #[serde(default)]
    pub channels: Vec<oxibrain_core::read_channels::ChannelReceipt>,
    pub truth_selection: Option<oxibrain_core::truth_selection::TruthSelectionReceipt>,
}
pub type RecallResponse = ReadResponse<EvidenceResult>;

fn coordinate(reference: String, revision: String, start: usize, end: usize) -> SourceCoordinate {
    let is_statement = reference.starts_with("statement://");
    SourceCoordinate {
        reference,
        view_digest: (!is_statement).then(|| revision.clone()),
        decoder: None,
        revision,
        start,
        end,
        view: if is_statement {
            "fold/1"
        } else {
            "episode-utf8/1"
        }
        .into(),
        document_uuid: None,
    }
}

impl Brain {
    /// Bind a local tokenizer independently of extraction. Does not load/download a model.
    pub fn with_tokenizer(
        mut self,
        tokenizer: std::sync::Arc<dyn oxibrain_ports::TokenizerPort>,
    ) -> Self {
        self.tokenizer = tokenizer;
        self
    }

    pub fn with_read_policy(mut self, policy: ReadPolicy) -> Result<Self, BrainError> {
        policy.validate()?;
        self.read_policy = policy;
        Ok(self)
    }
    pub fn read_policy(&self) -> &ReadPolicy {
        &self.read_policy
    }

    async fn visible_memory(
        &self,
        scope: &ReadScope,
        now: Timestamp,
        query: &str,
    ) -> Result<
        (
            Vec<oxibrain_core::visible_memory::VisibleClaim>,
            BTreeMap<String, String>,
            BTreeMap<String, String>,
            oxibrain_core::truth_selection::TruthSelectionReceipt,
        ),
        BrainError,
    > {
        let space = scope.space.clone();
        let known = scope.known_at.unwrap_or(now);
        let maximum = self.read_policy.truth_bytes;
        let query = query.to_string();
        let cap = self.read_policy.truth_group_cap;
        let (input, mut receipt) = self
            .read(move |c| {
                use oxibrain_core::truth_selection::{
                    TruthSelectionReceipt, consider_groups, select_complete_groups,
                };
                let started = std::time::Instant::now();
                let mut receipt = TruthSelectionReceipt::default();
                let mut pool = Vec::new();
                let mut after = None;
                loop {
                    let cursor = after
                        .as_ref()
                        .map(|(a, b): &(String, String)| (a.as_str(), b.as_str()));
                    let page =
                        oxibrain_store::truth_groups::catalog(c, &space, known, cursor, 128)?;
                    if page.is_empty() {
                        break;
                    }
                    let last = page.last().expect("nonempty page");
                    after = Some((last.subject.clone(), last.predicate.clone()));
                    receipt.examined += page.len();
                    receipt.truncated_seeds += page.iter().filter(|g| g.seed_truncated).count();
                    consider_groups(&mut pool, page, &query, cap);
                }
                let (selected, bytes) = select_complete_groups(pool, maximum);
                receipt.materialized = selected.len();
                receipt.bytes = bytes;
                receipt.omitted = receipt.examined.saturating_sub(selected.len());
                let input =
                    oxibrain_store::read_evidence::truth_input(c, &space, known, &selected)?;
                receipt.sources = input.live_sources.len();
                receipt.selection_ms = started.elapsed().as_secs_f64() * 1000.0;
                Ok((input, receipt))
            })
            .await?;
        let fold_started = std::time::Instant::now();
        let claims = oxibrain_core::visible_memory::visible_claims(
            &input.groups,
            &input.definitions,
            &input.live_sources,
            scope,
            now,
        );
        let names = oxibrain_core::visible_memory::scoped_names(
            input.surfaces,
            &input.merges,
            &input.merge_surfaces,
            &input.live_sources,
            known,
        );
        receipt.fold_ms = fold_started.elapsed().as_secs_f64() * 1000.0;
        Ok((claims, names, input.entity_types, receipt))
    }

    /// Resume a versioned raw-source build. Each batch and cursor share a write transaction.
    pub async fn index_memory(&self) -> Result<usize, BrainError> {
        let spaces = self.read(oxibrain_store::ledger::list_spaces).await?;
        let mut indexed = 0;
        for row in spaces {
            loop {
                let space = row.id.clone();
                let (n,done)=self.write(move |c| {
                    let key=format!("raw-index/{space}/2");
                    let mut saved=oxibrain_store::meta::get(c,&key)?.and_then(|v|serde_json::from_str::<serde_json::Value>(&v).ok());
                    if saved.as_ref().is_some_and(|v|v["complete"]==true) && oxibrain_store::read_evidence::missing_source_count(c,&space)? > 0 {saved=None;}
                    let target=saved.as_ref().and_then(|v|v["target"].as_i64()).unwrap_or(oxibrain_store::read_evidence::frontier(c,&space)?);
                    let after=saved.as_ref().and_then(|v|v["after"].as_i64()).unwrap_or(-1);
                    if saved.as_ref().is_some_and(|v|v["complete"]==true) {return Ok((0,true));}
                    let batch=oxibrain_store::read_evidence::source_batch(c,&space,after,target,64)?;
                    let n=batch.len();let next=batch.last().map_or(target,|r|r.0);
                    for (_,id,body) in batch {oxibrain_store::read_evidence::index_source(c,&space,&id,&body)?;}
                    let done=n==0;
                    oxibrain_store::meta::set(c,&key,&serde_json::json!({"target":target,"after":next,"complete":done,"policy":oxibrain_store::source_vectors::WINDOW_POLICY}).to_string())?;
                    Ok((n,done))
                }).await?;
                indexed += n;
                if done {
                    break;
                }
            }
        }
        Ok(indexed)
    }

    pub async fn search(&self, q: Query) -> Result<SearchResponse, BrainError> {
        Ok(self.search_with_scope(q.into()).await?.data)
    }

    pub async fn search_with_scope(
        &self,
        req: SearchRequest,
    ) -> Result<ReadResponse<SearchResponse>, BrainError> {
        let now = self.clock.now();
        req.scope.validate(&req.planes, now)?;
        for attempt in 0..2 {
            let epoch = self
                .read(|c| oxibrain_store::meta::get(c, "exposure_epoch"))
                .await?;
            let response = self.search_once(&req, now).await?;
            let unchanged = self
                .read(move |c| Ok(oxibrain_store::meta::get(c, "exposure_epoch")? == epoch))
                .await?;
            if unchanged && self.document_hits_live(&response.data.documents).await? {
                return Ok(response);
            }
            if attempt == 1 {
                return Err(BrainError::Busy("source changed during search".into()));
            }
        }
        Err(BrainError::Busy("source changed during search".into()))
    }

    async fn search_once(
        &self,
        req: &SearchRequest,
        now: Timestamp,
    ) -> Result<ReadResponse<SearchResponse>, BrainError> {
        let mut meta = ReadMeta {
            scope: serde_json::json!({"requested":req.scope,"planes":req.planes,"request_time":now,"policy_version":ReadPolicy::VERSION,"limits":self.read_policy}),
            ..Default::default()
        };
        // Document sequencing retains revision validation and native PDC decoding.
        let mut data = if req.planes.contains(&SearchPlane::Documents) {
            self.search_unscoped(
                Query {
                    text: req.text.clone(),
                    space: req.scope.space.clone(),
                    mode: req.mode,
                    as_of: None,
                    limit: req.limit,
                    min_confidence: 0.0,
                    planes: [SearchPlane::Documents].into_iter().collect(),
                },
                false,
            )
            .await?
            .0
        } else {
            SearchResponse {
                memory: Vec::new(),
                documents: Vec::new(),
                freshness: DocumentFreshness::default(),
            }
        };
        if req.planes.contains(&SearchPlane::Memory) {
            let (claims, names, types, omitted) =
                self.visible_memory(&req.scope, now, &req.text).await?;
            meta.truth_selection = Some(omitted.clone());
            let (hits, degradation, capped) = self
                .scoped_claim_hits(
                    &claims,
                    &names,
                    &req.text,
                    req.mode,
                    req.limit.max(64),
                    &mut meta.channels,
                )
                .await?;
            let mut by_entity = BTreeMap::new();
            for h in hits {
                let Some(claim) = claims.iter().find(|c| c.statement.id == h.statement) else {
                    continue;
                };
                let mut entities = vec![h.subject.clone()];
                if let oxibrain_core::Object::Entity(id) = &claim.statement.object {
                    entities.push(id.clone());
                }
                for id in entities {
                    by_entity.entry(id.clone()).or_insert_with(|| {
                        oxibrain_store::query::SearchResult {
                            entity_id: id.clone(),
                            entity_surface: names.get(&id).cloned().unwrap_or(id.clone()),
                            entity_type: types
                                .get(&id)
                                .cloned()
                                .unwrap_or_else(|| "Unknown".into()),
                            score: h.score,
                            snippet: h.text.clone(),
                        }
                    });
                }
            }
            data.memory = by_entity.into_values().collect();
            data.memory.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.entity_id.cmp(&b.entity_id))
            });
            data.memory.truncate(req.limit);
            meta.dropped.insert("dense_candidate_cap".into(), capped);
            if let Some(error) = degradation {
                meta.execution["dense_fallback"] = error.into();
            }
            meta.dropped
                .insert("truth_group_incomplete".into(), omitted.omitted);
        }
        meta.channels.extend(data.freshness.channels.clone());
        meta.freshness = data.freshness.clone();
        meta.scope["document_generations"] = serde_json::to_value(
            data.documents
                .iter()
                .map(|h| (h.root.clone(), h.root_generation))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("generation serializes");
        let degradation = meta.execution.get("dense_fallback").cloned().or_else(|| {
            meta.channels
                .iter()
                .find_map(|c| c.fallback.clone().map(serde_json::Value::String))
        });
        meta.execution = serde_json::json!({"partial":meta.channels.iter().any(|c|c.partial())||meta.truth_selection.as_ref().is_some_and(|r|r.truncated_seeds>0)||degradation.is_some()||meta.dropped.values().any(|n|*n>0)||!data.freshness.skipped_roots.is_empty()||data.freshness.unavailable_hits>0,"answerability":"not_evaluated","dense_fallback":degradation,"dense_capability":self.embedder.is_some()});
        Ok(ReadResponse { data, meta })
    }

    pub async fn recall_with_request(
        &self,
        req: RecallRequest,
    ) -> Result<RecallResponse, BrainError> {
        let now = self.clock.now();
        req.scope.validate(&req.planes, now)?;
        let exact = self.tokenizer.quality() == oxibrain_ports::tokenizer::TokenizerQuality::Exact;
        if req.require_exact_tokens && !exact {
            return Err(BrainError::TokenizerUnavailable(
                "an exact configured tokenizer is required".into(),
            ));
        }
        for attempt in 0..2 {
            let epoch = self
                .read(|c| oxibrain_store::meta::get(c, "exposure_epoch"))
                .await?;
            let mut units = Vec::new();
            let mut document_receipts = Vec::new();
            let mut meta = ReadMeta {
                scope: serde_json::json!({"requested":req.scope,"planes":req.planes,"request_time":now,"policy_version":ReadPolicy::VERSION,"limits":self.read_policy}),
                ..Default::default()
            };
            if req.planes.contains(&SearchPlane::Memory) {
                let space = req.scope.space.clone();
                let frontier = self
                    .read(move |c| oxibrain_store::read_evidence::frontier(c, &space))
                    .await?;
                meta.scope["memory_frontier"] = frontier.into();
                let (claims, names, _types, omitted) =
                    self.visible_memory(&req.scope, now, &req.text).await?;
                meta.truth_selection = Some(omitted.clone());
                meta.dropped
                    .insert("truth_group_incomplete".into(), omitted.omitted);
                for c in claims.iter().filter(|c| c.profile_relevant).take(30) {
                    let mut u = EvidenceUnit::new(
                        LayerKind::Profile,
                        oxibrain_core::visible_memory::render_claim(c, &names),
                        coordinate(
                            format!("statement://{}", c.statement.id),
                            oxibrain_core::visible_memory::claim_revision(c, &names),
                            0,
                            0,
                        ),
                        "belief",
                    );
                    u.support = c.sources.clone();
                    units.push(u);
                }
                if !req.hints.skip_beliefs {
                    let (hits, degradation, capped) = self
                        .scoped_claim_hits(
                            &claims,
                            &names,
                            &req.text,
                            if self.read_policy.semantic_sources {
                                QueryMode::Hybrid
                            } else {
                                QueryMode::Lexical
                            },
                            96,
                            &mut meta.channels,
                        )
                        .await?;
                    meta.dropped.insert("dense_candidate_cap".into(), capped);
                    if let Some(error) = degradation {
                        meta.execution["dense_fallback"] = error.into();
                    }
                    for hit in hits {
                        let mut u = EvidenceUnit::new(
                            if hit.score == 0.0 {
                                LayerKind::QueryNeighborhood
                            } else {
                                LayerKind::HighSalienceBeliefs
                            },
                            hit.text,
                            coordinate(
                                format!("statement://{}", hit.statement),
                                claims
                                    .iter()
                                    .find(|c| c.statement.id == hit.statement)
                                    .map(|c| {
                                        oxibrain_core::visible_memory::claim_revision(c, &names)
                                    })
                                    .expect("scoped claim hit"),
                                0,
                                0,
                            ),
                            "belief",
                        );
                        u.support = hit.sources;
                        units.push(u);
                    }
                }
                let space = req.scope.space.clone();
                let missing = self
                    .read(move |c| oxibrain_store::read_evidence::missing_source_count(c, &space))
                    .await?;
                meta.index_coverage = serde_json::json!({"pending_sources":missing,"complete":missing==0,"policy":oxibrain_store::source_vectors::WINDOW_POLICY});
                if !req.hints.skip_summaries {
                    let space = req.scope.space.clone();
                    let known = req.scope.known_at.unwrap_or(now);
                    let valid = req.scope.as_of.unwrap_or(Timestamp(i64::MAX));
                    for summary in self
                        .read(move |c| {
                            oxibrain_store::read_evidence::summaries(c, &space, known, valid)
                        })
                        .await?
                    {
                        if req.scope.as_of.is_some() || req.scope.known_at.is_some() {
                            *meta
                                .dropped
                                .entry("summary_scope_unavailable".into())
                                .or_default() += 1;
                            continue;
                        }
                        if !oxibrain_core::evidence::support_is_live(&summary.support_states) {
                            *meta.dropped.entry("missing_support".into()).or_default() += 1;
                            continue;
                        }
                        let len = summary.text.len();
                        let text = format!(
                            "{}\n  uncertainty: {}",
                            summary.text,
                            summary
                                .uncertainty
                                .unwrap_or_else(|| "unknown (legacy summary)".into())
                        );
                        let mut u = EvidenceUnit::new(
                            LayerKind::Summaries,
                            text,
                            coordinate(
                                format!("episode://{}", summary.id),
                                summary.revision,
                                0,
                                len,
                            ),
                            "summary",
                        );
                        u.support = summary.sources;
                        units.push(u);
                    }
                }
                let mut channels = Vec::new();
                let channel_depth = self.read_policy.channel_depth;
                let term_plan = oxibrain_core::query_candidates::RawSourceTermPlan::new(
                    &req.text,
                    self.read_policy.query_terms,
                );
                for term in &term_plan.terms {
                    for ngram in [false, true] {
                        let (space, term, exclude) = (
                            req.scope.space.clone(),
                            term.to_string(),
                            req.session_id.clone(),
                        );
                        let (known, valid) = (
                            req.scope.known_at.unwrap_or(now),
                            req.scope.as_of.unwrap_or(Timestamp(i64::MAX)),
                        );
                        channels.push(
                            self.read(move |c| {
                                oxibrain_store::read_evidence::source_channel(
                                    c,
                                    &oxibrain_store::read_evidence::SourceReadScope {
                                        space: &space,
                                        known_at: known,
                                        as_of: valid,
                                        excluded_session: exclude.as_deref(),
                                    },
                                    &term,
                                    ngram,
                                    channel_depth,
                                )
                            })
                            .await?,
                        );
                    }
                }
                for (offset, name) in [(0, "word"), (1, "ngram")] {
                    let channel_hits: Vec<_> = channels.iter().skip(offset).step_by(2).collect();
                    let considered = channel_hits
                        .iter()
                        .flat_map(|hits| hits.iter().map(|hit| hit.id.clone()))
                        .collect::<BTreeSet<_>>()
                        .len();
                    let mut receipt = oxibrain_core::read_channels::ChannelReceipt::lexical(
                        "memory_sources",
                        name,
                        considered,
                        channel_depth,
                    );
                    receipt.executed = !channels.is_empty();
                    receipt.limit_reached = channels
                        .iter()
                        .skip(offset)
                        .step_by(2)
                        .any(|c| c.len() >= channel_depth);
                    receipt.capped = term_plan.capped;
                    receipt.algorithm_version =
                        Some(oxibrain_core::query_candidates::RAW_SOURCE_TERM_PLAN_VERSION.into());
                    receipt.query_limit = Some(term_plan.query_limit);
                    receipt.plan_candidates_examined = term_plan.terms_examined;
                    receipt.plan_candidates_selected = term_plan.terms_selected;
                    receipt.queries_run = term_plan.terms_selected;
                    meta.channels.push(receipt);
                }
                let (semantic, receipt) = self
                    .semantic_source_hits(&req.scope, &req.text, req.session_id.as_deref(), now)
                    .await?;
                meta.channels.push(receipt);
                let channel_ids: Vec<Vec<String>> = channels
                    .iter()
                    .map(|ch| ch.iter().map(|h| h.id.clone()).collect())
                    .collect();
                let dense_ids: Vec<String> = semantic.iter().map(|h| h.id.clone()).collect();
                let (selected, capped) = oxibrain_core::evidence::fuse_query_source_families(
                    &channel_ids,
                    &dense_ids,
                    self.read_policy.candidate_cap,
                );
                meta.dropped.insert("candidate_cap".into(), capped.len());
                let mut rows = BTreeMap::new();
                for h in channels.into_iter().flatten().chain(semantic) {
                    rows.entry(h.id.clone()).or_insert(h);
                }
                let mut raw: Vec<_> = selected.iter().filter_map(|id| rows.remove(id)).collect();
                let space = req.scope.space.clone();
                let exclude = req.session_id.clone();
                let known = req.scope.known_at.unwrap_or(now);
                let valid = req.scope.as_of.unwrap_or(Timestamp(i64::MAX));
                let recent = self
                    .read(move |c| {
                        oxibrain_store::read_evidence::recent_sessions(
                            c,
                            &oxibrain_store::read_evidence::SourceReadScope {
                                space: &space,
                                known_at: known,
                                as_of: valid,
                                excluded_session: exclude.as_deref(),
                            },
                        )
                    })
                    .await?;
                let query_count = raw.len();
                raw.extend(recent);
                for (position, h) in raw.into_iter().enumerate() {
                    if h.source_kind != "session" && req.hints.skip_episodes {
                        *meta.dropped.entry("hint_excluded".into()).or_default() += 1;
                        continue;
                    }
                    let reference = if h.source_kind == "session" {
                        format!(
                            "session://{}",
                            h.source_ref
                                .as_deref()
                                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                                .and_then(|v| v["session_id"].as_str().map(str::to_string))
                                .unwrap_or(h.episode.clone())
                        )
                    } else {
                        format!("episode://{}", h.episode)
                    };
                    let layer = if h.source_kind == "session" {
                        LayerKind::SessionSummaries
                    } else {
                        LayerKind::RecentEpisodes
                    };
                    let mut u = EvidenceUnit::new(
                        layer,
                        format!(
                            "[observed_at={}; trust={}] {}",
                            h.occurred_at.0, h.trust, h.content
                        ),
                        coordinate(reference, h.revision, h.start, h.end),
                        if position >= query_count {
                            "recency_observation"
                        } else {
                            "untrusted_observation"
                        },
                    );
                    u.variants = oxibrain_core::evidence::quotation_variants(
                        &h.content, &u.source, &req.text,
                    );
                    for variant in &mut u.variants {
                        variant.text = format!(
                            "[observed_at={}; trust={}] {}",
                            h.occurred_at.0, h.trust, variant.text
                        );
                    }
                    u.support.push(h.episode);
                    units.push(u);
                }
            }
            if req.planes.contains(&SearchPlane::Documents) {
                let (response, document_units) = self
                    .search_unscoped(
                        Query {
                            text: req.text.clone(),
                            space: req.scope.space.clone(),
                            mode: if self.read_policy.semantic_sources {
                                QueryMode::Hybrid
                            } else {
                                QueryMode::Lexical
                            },
                            as_of: None,
                            limit: 64,
                            min_confidence: 0.0,
                            planes: [SearchPlane::Documents].into_iter().collect(),
                        },
                        true,
                    )
                    .await?;
                meta.channels.extend(response.freshness.channels.clone());
                meta.freshness = response.freshness;
                meta.scope["document_generations"] = serde_json::to_value(
                    response
                        .documents
                        .iter()
                        .map(|h| (h.root.clone(), h.root_generation))
                        .collect::<BTreeMap<_, _>>(),
                )
                .expect("generation serializes");
                document_receipts = response.documents;
                units.extend(document_units);
            }
            let counter = crate::read_counter::ReadCounter::new(self.tokenizer.as_ref());
            let mut result = oxibrain_core::evidence_pack::pack_evidence_with_policy(
                &units,
                req.budget.max_tokens,
                &counter,
                &self.read_policy,
            );
            counter.check()?;
            if exact {
                let text = oxibrain_core::context::render_context(&result.context);
                let counted = self
                    .tokenizer
                    .count_exact(&text)?
                    .ok_or_else(|| BrainError::TokenizerUnavailable("exact count failed".into()))?;
                if counted > req.budget.max_tokens {
                    return Err(BrainError::Budget(
                        "canonical context exceeded exact token budget".into(),
                    ));
                }
            }
            // Revalidate live source support after optional adapter/filesystem work.
            let ids: Vec<_> = result
                .evidence
                .iter()
                .flat_map(|e| e.support.clone())
                .collect();
            let space = req.scope.space.clone();
            let valid = self
                .read(move |c| {
                    for id in ids {
                        if oxibrain_store::ledger::get_episode(c, &id)?
                            .is_none_or(|e| e.space != space || e.redacted_at.is_some())
                        {
                            return Ok(false);
                        }
                    }
                    Ok(oxibrain_store::meta::get(c, "exposure_epoch")? == epoch)
                })
                .await?;
            let selected_refs: BTreeSet<_> = result
                .evidence
                .iter()
                .map(|e| e.source.reference.as_str())
                .collect();
            document_receipts
                .retain(|h| selected_refs.contains(h.text.provenance.reference.as_str()));
            let documents_live = self.document_hits_live(&document_receipts).await?;
            if !valid || !documents_live {
                if attempt == 0 {
                    continue;
                }
                return Err(BrainError::Busy("source changed during recall".into()));
            }
            for d in &result.decisions {
                if !d.reason.starts_with("selected") {
                    *meta.dropped.entry(d.reason.clone()).or_insert(0) += 1;
                }
            }
            if req.explain {
                let mut bytes = 0;
                for d in &result.decisions {
                    bytes += d.unit_id.len() + d.reason.len() + 40;
                    if bytes > self.read_policy.explanation_bytes {
                        break;
                    }
                    meta.decisions.push(d.clone());
                }
            }
            result.context.truncated |= meta.dropped.get("candidate_cap").copied().unwrap_or(0) > 0
                || meta
                    .dropped
                    .get("dense_candidate_cap")
                    .copied()
                    .unwrap_or(0)
                    > 0
                || meta.freshness.unavailable_hits > 0;
            meta.layers_kept = result.context.layers.len();
            meta.tokens = Some(
                serde_json::json!({"spent":result.context.total_tokens,"budget":req.budget.max_tokens,"counted_by":self.tokenizer.id(),"artifact_digest":self.tokenizer.artifact_digest(),"exact":exact,"render":if self.read_policy.compact_evidence {"context_render/2"} else {"context_render/1"}}),
            );
            let degradation = meta.execution.get("dense_fallback").cloned().or_else(|| {
                meta.channels
                    .iter()
                    .find_map(|c| c.fallback.clone().map(serde_json::Value::String))
            });
            let source_channels: BTreeSet<_> = meta
                .channels
                .iter()
                .filter(|c| c.executed && (c.plane == "memory_sources" || c.plane == "documents"))
                .map(|c| c.channel.as_str())
                .collect();
            meta.execution = serde_json::json!({"partial":meta.channels.iter().any(|c|c.partial())||meta.truth_selection.as_ref().is_some_and(|r|r.truncated_seeds>0)||degradation.is_some()||meta.index_coverage["complete"]==false||meta.dropped.get("truth_group_incomplete").copied().unwrap_or(0)>0||!meta.freshness.skipped_roots.is_empty()||meta.freshness.unavailable_hits>0,"answerability":"not_evaluated","dense_configured":self.embedder.is_some(),"dense_fallback":degradation,"source_channels":source_channels,"loss_scope":"normalized considered candidates; unseen index tails are not enumerated"});
            return Ok(ReadResponse { data: result, meta });
        }
        Err(BrainError::Busy("source changed during recall".into()))
    }
    /// Optional inference happens after the truth fetch has released its DB handle.
    async fn scoped_claim_hits(
        &self,
        claims: &[oxibrain_core::visible_memory::VisibleClaim],
        names: &BTreeMap<String, String>,
        text: &str,
        mode: QueryMode,
        maximum: usize,
        receipts: &mut Vec<oxibrain_core::read_channels::ChannelReceipt>,
    ) -> Result<
        (
            Vec<oxibrain_core::visible_memory::ClaimHit>,
            Option<String>,
            usize,
        ),
        BrainError,
    > {
        let lexical = if mode == QueryMode::Dense {
            Vec::new()
        } else {
            oxibrain_core::visible_memory::search_claims(claims, names, text, maximum)
        };
        let mut lexical_receipt = oxibrain_core::read_channels::ChannelReceipt::lexical(
            "memory_claims",
            "ngram",
            lexical.len(),
            maximum,
        );
        lexical_receipt.eligible = mode != QueryMode::Dense;
        lexical_receipt.executed = mode != QueryMode::Dense;
        receipts.push(lexical_receipt);
        let mut receipt = oxibrain_core::read_channels::ChannelReceipt {
            plane: "memory_claims".into(),
            channel: "dense".into(),
            eligible: mode != QueryMode::Lexical && self.embedder.is_some(),
            model_identity: self.embedding_identity.clone(),
            total: Some(claims.len()),
            ..Default::default()
        };
        let mut dense = Vec::new();
        let mut degradation = None;
        let mut capped = 0;
        if mode != QueryMode::Lexical {
            if let Some(embedder) = &self.embedder {
                let mut texts = vec![text.to_string()];
                let mut ids = Vec::new();
                let mut bytes = text.len();
                for c in claims.iter().take(256) {
                    let rendered = oxibrain_core::visible_memory::render_claim(c, names);
                    if bytes + rendered.len() > 1024 * 1024 {
                        break;
                    }
                    bytes += rendered.len();
                    ids.push(c.statement.id.clone());
                    texts.push(rendered);
                }
                capped = claims.len().saturating_sub(ids.len());
                receipt.indexed = Some(ids.len());
                receipt.considered = ids.len();
                receipt.capped = capped > 0;
                if !ids.is_empty() {
                    let embedder = embedder.clone();
                    let dimension = embedder.dim();
                    let expected = texts.len();
                    let result = tokio::task::spawn_blocking(move || {
                        let refs: Vec<_> = texts.iter().map(String::as_str).collect();
                        embedder.embed(&refs)
                    })
                    .await
                    .map_err(|e| BrainError::Config(format!("embedding worker: {e}")))?;
                    match result {
                        Ok(vectors)
                            if vectors.len() == expected
                                && vectors.iter().all(|v| {
                                    v.len() == dimension
                                        && !v.is_empty()
                                        && v.iter().all(|x| x.is_finite())
                                }) =>
                        {
                            receipt.executed = true;
                            dense = oxibrain_core::visible_memory::cosine_scores(&ids, &vectors)
                        }
                        Ok(_) => {
                            degradation = Some("invalid embedding output; lexical fallback".into())
                        }
                        Err(e) => degradation = Some(e.to_string()),
                    }
                }
            } else if mode == QueryMode::Dense {
                return Err(BrainError::Config(
                    "dense search requires an embedding provider".into(),
                ));
            }
        }
        receipt.fallback = degradation.clone();
        receipts.push(receipt);
        Ok((
            oxibrain_core::visible_memory::combine_claim_hits(
                claims,
                names,
                lexical,
                &dense,
                mode == QueryMode::Hybrid,
                maximum,
            ),
            degradation,
            capped,
        ))
    }
}
