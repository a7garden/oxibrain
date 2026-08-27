//! Extraction pipeline methods (M10 10.10). Extracted from lib.rs to keep
//! the facade under 1,000 LOC. The methods here are `pub(crate)` impl blocks
//! on `Brain`; the facade wraps them with 1-line delegations.

use super::{Brain, BrainError, LlmPort, LlmRequest};
use std::sync::Arc;

/// The facade's default extractor config — the identity shared by the
/// inline capture path (`remember`) and the backlog drain
/// (`extract_uncached`), so an episode captured-but-not-extracted is
/// always picked up by the same extractor id later.
pub(crate) fn default_extractor_config() -> oxibrain_core::extraction::ExtractorConfig {
    oxibrain_core::extraction::ExtractorConfig {
        model_id: "oxibrain-default".into(),
        prompt_version: 2, // v2: quote-based mentions (ADR-006)
        registry_major: oxibrain_core::registry::CORE_V1_MAJOR,
        mechanism: oxibrain_core::extraction::ExtractMechanism::JsonSchema,
        max_tokens: 8192,
        model_digest: None,
        provider_profile_id: None,
    }
}

impl Brain {
    /// Extract a single episode synchronously with an explicit LLM provider.
    ///
    /// Does NOT use the job queue — directly reads, calls the provided LLM,
    /// validates, projects. Used by the realtime MCP sampling path (§12.3):
    /// the `llm` is a [`SamplingLlmPort`](../../oxibrain_mcp/sampling/struct.SamplingLlmPort.html)
    /// backed by the client's model.
    pub(crate) async fn extract_one_with_impl(
        &self,
        space: &str,
        episode_id: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        llm: Arc<dyn LlmPort>,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let now = self.clock.now();

        // 1. Read episode content [read-only connection].
        let episode = self
            .get_episode(episode_id)
            .await?
            .ok_or_else(|| BrainError::NotFound(format!("episode {episode_id}")))?;

        // 2. Generate schema + prompt [pure]. The system prompt carries the
        //    quote contract (ADR-006) plus the k most similar built-in
        //    few-shot examples (§9.6, 10.8) — selection is trigram Jaccard,
        //    language-independent (P11), deterministic.
        let predicates = oxibrain_core::registry::core_v1();
        let schema = oxibrain_core::extraction::schema_from_registry(predicates);
        let mut system = oxibrain_core::extraction::build_extraction_prompt(predicates);
        let corpus = oxibrain_core::extraction::default_few_shot_corpus();
        let selected = oxibrain_core::extraction::few_shot_examples(&episode.content, &corpus, 2);
        system.push_str(&oxibrain_core::extraction::format_few_shot(&selected));

        // 3. Call LLM [async, no store open anywhere in this phase].
        //    Grammar-capable adapters (the local GGUF path, §9.4 D28) get a
        //    GBNF grammar generated from the registry (P4); everything else
        //    takes schema-and-repair.
        let req = LlmRequest {
            model: config.model_id.clone(),
            system: Some(system),
            prompt: episode.content.clone(),
            json_schema: Some(schema),
            max_tokens: config.max_tokens,
        };
        let grammar = llm
            .capabilities()
            .grammar
            .then(|| oxibrain_core::extraction::grammar_from_registry(predicates));
        let response = match &grammar {
            Some(g) => llm.generate_constrained(req.clone(), g).await?,
            None => llm.complete(req.clone()).await?,
        };

        // 4. Parse + validate [pure]. An unparseable response (truncated
        //    tool call, grammar runaway past the KV budget) is invalid output:
        //    it is recorded in extraction_failures like any other, never
        //    silently dropped, and then fails the episode loudly.
        let parsed: oxibrain_core::extraction::ExtractionResponse =
            match serde_json::from_str(&response.text) {
                Ok(p) => p,
                Err(e) => {
                    let err = BrainError::Extraction(format!("parse LLM response: {e}"));
                    self.record_response_failure(
                        episode_id,
                        &config.id(),
                        &response.text,
                        &err,
                        now,
                    )
                    .await;
                    return Err(err);
                }
            };
        let mut result = oxibrain_core::extraction::validate_claims(
            &parsed.claims,
            &episode.content,
            predicates,
        );

        // 5. Repair loop: one retry if invalid claims exist.
        if !result.invalid.is_empty() && config.max_tokens > 0 {
            let errors_summary: Vec<&oxibrain_core::extraction::ValidationError> = result
                .invalid
                .iter()
                .flat_map(|(_, errs)| errs.iter())
                .collect();
            let repair_prompt = format!(
                "{}\n\nPrevious extraction had these errors: {:?}\nPlease re-extract, fixing \
                 these issues. Every mention — subject AND object — needs a non-empty quote \
                 copied EXACTLY from the text, containing the surface verbatim.",
                episode.content, errors_summary
            );
            let repair_req = LlmRequest {
                prompt: repair_prompt,
                ..req.clone()
            };
            let repair_response = match &grammar {
                Some(g) => llm.generate_constrained(repair_req, g).await,
                None => llm.complete(repair_req).await,
            };
            if let Ok(repair_response) = repair_response {
                if let Ok(repair_parsed) = serde_json::from_str::<
                    oxibrain_core::extraction::ExtractionResponse,
                >(&repair_response.text)
                {
                    result = oxibrain_core::extraction::validate_claims(
                        &repair_parsed.claims,
                        &episode.content,
                        predicates,
                    );
                }
            }
        }

        let invalid_count = result.invalid.len();
        let raw_response = response.text.clone();
        let extractor_id = config.id();
        let space = space.to_string();
        let episode_id = episode_id.to_string();
        let valid = result.valid.clone();
        let invalid = result.invalid.clone();

        // 6. Project [one short write op; fresh resolution cache].
        self.write(move |conn| {
            // Cache the raw response.
            oxibrain_store::extraction::cache_response(
                conn,
                &episode_id,
                &extractor_id,
                &raw_response,
                now,
            )?;
            // Project valid claims. The resolution cache is per-call: the
            // handle-free facade holds no process-lifetime LSH state.
            let mut cache = oxibrain_store::project::ResolutionCache::new();
            let n = oxibrain_store::extraction::project_extraction(
                conn,
                &space,
                &episode_id,
                &extractor_id,
                &valid,
                now,
                &mut cache,
            )?;
            // File invalid claims.
            for (_claim, errors) in &invalid {
                let errors_json = serde_json::to_string(errors).unwrap_or_else(|_| "[]".into());
                oxibrain_store::quarantine::record_failure(
                    conn,
                    &episode_id,
                    &extractor_id,
                    &raw_response,
                    &errors_json,
                    now,
                )?;
            }
            Ok(oxibrain_core::extraction::ExtractSummary {
                extracted: n,
                quarantined: invalid_count,
                episodes_done: 1,
                episodes_failed: 0,
                failures: Vec::new(),
            })
        })
        .await
    }

    /// Best-effort recording of an unparseable LLM response into
    /// extraction_failures (invalid output is never silently dropped).
    async fn record_response_failure(
        &self,
        episode_id: &str,
        extractor_id: &str,
        raw_response: &str,
        error: &BrainError,
        now: oxibrain_ports::Timestamp,
    ) {
        let episode_id = episode_id.to_string();
        let extractor_id = extractor_id.to_string();
        let raw = raw_response.to_string();
        let msg = error.to_string();
        let res = self
            .write(move |conn| {
                let errors_json = serde_json::to_string(&[msg]).unwrap_or_else(|_| "[]".into());
                oxibrain_store::quarantine::record_failure(
                    conn,
                    &episode_id,
                    &extractor_id,
                    &raw,
                    &errors_json,
                    now,
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = res {
            eprintln!("warn: recording response failure: {e}");
        }
    }

    /// Drain the queue-less memory-plane backlog: read up to `limit`
    /// uncached episodes across **all** spaces and extract each with the
    /// configured LLM. Returns the number of episodes *processed*
    /// (successes + failures) — a provider failure still counts as work
    /// done, matching `ExtractSummary::episodes_done + episodes_failed`.
    ///
    /// Each episode runs read → LLM (no store open) → write, so the model
    /// call is never inside a transaction (§7.2).
    pub async fn extract_uncached(&self, limit: usize) -> Result<usize, BrainError> {
        let _llm = self.require_llm()?;
        let config = default_extractor_config();
        let extractor_id = config.id();

        // 1. List spaces, then take the backlog per space under one global
        //    limit (read-only connection).
        let targets: Vec<(String, String)> = self
            .read(move |conn| {
                let mut stmt = conn
                    .prepare("SELECT id FROM spaces ORDER BY id")
                    .map_err(|e| BrainError::Storage(format!("space list: {e}")))?;
                let spaces: Vec<String> = stmt
                    .query_map([], |r| r.get(0))
                    .map_err(|e| BrainError::Storage(format!("space list: {e}")))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| BrainError::Storage(format!("space list: {e}")))?;
                drop(stmt);
                let mut out: Vec<(String, String)> = Vec::new();
                for space in spaces {
                    if out.len() >= limit {
                        break;
                    }
                    let remaining = limit - out.len();
                    let ids = oxibrain_store::extraction::uncached_memory_episodes(
                        conn,
                        &space,
                        &extractor_id,
                    )?;
                    out.extend(ids.into_iter().take(remaining).map(|id| (space.clone(), id)));
                }
                Ok(out)
            })
            .await?;

        // 2. Extract each; `cache_response` inside `extract_one` removes it
        //    from the backlog. Failures count as processed.
        let mut processed: usize = 0;
        for (space, ep_id) in targets {
            match self.extract_one(&space, &ep_id, &config).await {
                Ok(_) | Err(_) => processed += 1,
            }
        }
        Ok(processed)
    }

    /// Re-extract all primary episodes with a new extractor config.
    /// Old cache entries are preserved (different extractor_id = different PK).
    pub(crate) async fn reextract_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let _llm = self.require_llm()?;
        let space = space.to_string();
        let query_space = space.clone();
        let extractor_id = config.id();

        let episode_ids = self
            .read(move |conn| {
                oxibrain_store::extraction::uncached_memory_episodes(
                    conn,
                    &query_space,
                    &extractor_id,
                )
            })
            .await?;

        // Extract each.
        let mut total = oxibrain_core::extraction::ExtractSummary::default();
        for ep_id in episode_ids {
            match self.extract_one(&space, &ep_id, config).await {
                Ok(s) => {
                    total.extracted += s.extracted;
                    total.quarantined += s.quarantined;
                    total.episodes_done += 1;
                }
                Err(e) => {
                    total.episodes_failed += 1;
                    total.failures.push((ep_id.clone(), e.to_string()));
                }
            }
        }
        Ok(total)
    }

    /// Consolidate related episodes into Derived episodes with cached summaries (§10).
    /// Clusters episodes by shared entities → LLM summarize → Derived episode.
    ///
    /// Determinism contract (Task 5, §13):
    ///
    /// * `find_episode_clusters` / `hash_member_set` are unchanged and
    ///   deterministic — their output is sorted and the iteration order is
    ///   the cluster's sorted episode ids.
    /// * The cache key is `(scope_kind, member_set_hash, extractor_id)`. The
    ///   `extractor_id` already folds `model_id`, `prompt_version`,
    ///   `registry_major`, `mechanism`, optional `model_digest`, and (since
    ///   Task 5) optional `provider_profile_id`. Foundation profile binding
    ///   therefore invalidates the cache; legacy compat env does not, so
    ///   existing caches keep hitting.
    /// * Truth-half persisted identifiers (`episode_id`) are NEVER extended
    ///   with profile display names, Keychain locators, wall-clock values,
    ///   or map iteration order — those only ever enter `extractor_id`
    ///   (cache half) and `uncertainty_json` (computed fold, not an
    ///   identifier).
    /// * Profile failures may leave an in-progress checkpoint but never an
    ///   uncited summary and never a mutated source episode: the LLM call
    ///   happens outside any store transaction, and the cache write /
    ///   derived episode write / checkpoint-complete land in one write op
    ///   so all three are atomic together.
    pub(crate) async fn consolidate_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<Vec<String>, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read clusters + filter to pending ones [read-only].
        let clusters = {
            let space_owned = space_owned.clone();
            let extractor_id = extractor_id.clone();
            self.read(move |conn| {
                let all =
                    oxibrain_store::consolidation::find_episode_clusters(conn, &space_owned)?;
                oxibrain_store::consolidation::filter_pending_clusters(
                    conn,
                    &extractor_id,
                    &all,
                )
            })
            .await?
        };

        // 2. For each pending cluster: establish checkpoint FIRST (one short
        //    write op), then check cache, then build prompt + call LLM only
        //    on a miss. The LLM call is NEVER inside a store transaction;
        //    the cache write, derived-episode write, and checkpoint-complete
        //    happen together in step 3.
        let mut summaries: Vec<(Vec<String>, String)> = Vec::new();
        for cluster in clusters {
            let episode_ids = cluster.episode_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_member_set(&episode_ids);

            // 2a. Establish the in-progress checkpoint BEFORE any model
            //     work. A profile failure between this point and step 3
            //     leaves a resumable `in_progress` row; the next call to
            //     `consolidate_impl` re-attempts the cluster because
            //     `filter_pending_clusters` only filters `completed` ones.
            {
                let extractor_id = extractor_id.clone();
                self.write(move |conn| {
                    oxibrain_store::consolidation::checkpoint_begin(
                        conn,
                        &member_hash,
                        &extractor_id,
                        now,
                    )
                })
                .await?;
            }

            // 2b. Cache check + (on miss) prompt build — both reads, no
            //     store lock held across the model call.
            let cached = {
                let extractor_id = extractor_id.clone();
                self.read(move |conn| {
                    oxibrain_store::consolidation::get_cached_summary(
                        conn,
                        "consolidation",
                        &member_hash,
                        &extractor_id,
                    )
                })
                .await?
            };

            if let Some(text) = cached {
                summaries.push((episode_ids.clone(), text));
                continue;
            }

            let prompt = {
                let space_owned = space_owned.clone();
                let prompt_ids = episode_ids.clone();
                self.read(move |conn| {
                    oxibrain_store::consolidation::build_consolidation_prompt(
                        conn,
                        &space_owned,
                        &oxibrain_store::consolidation::EpisodeCluster {
                            episode_ids: prompt_ids,
                            shared_entities: Vec::new(),
                        },
                    )
                })
                .await?
            };

            // 2c. LLM call — OUTSIDE any store transaction, no Keychain
            //     access on this path (the Keychain lookup is in the
            //     ProviderLlm resolver, long before this point).
            let response = llm
                .complete(LlmRequest {
                    model: config.model_id.clone(),
                    system: Some("Summarize related episodes concisely.".into()),
                    prompt,
                    json_schema: None,
                    max_tokens: config.max_tokens,
                })
                .await?;
            summaries.push((episode_ids, response.text));
        }

        // 3. One single transaction holds cache_summary + write_derived_episode
        //    (with Uncertainty) + checkpoint_complete. Either all three rows
        //    land or none do — so a profile failure cannot leave the cache
        //    half pointing at a derived episode that isn't in the ledger.
        self.write(move |conn| {
            let mut ids = Vec::new();
            for (episode_ids, text) in &summaries {
                let member_hash = oxibrain_store::consolidation::hash_member_set(episode_ids);
                let shared_entities =
                    oxibrain_store::consolidation::entities_for_episodes(
                        conn,
                        &space_owned,
                        episode_ids,
                    )?;
                let uncertainty = oxibrain_store::consolidation::uncertainty_for_cluster(
                    conn,
                    &space_owned,
                    &shared_entities,
                    now,
                )?;
                oxibrain_store::consolidation::cache_summary(
                    conn,
                    "consolidation",
                    &member_hash,
                    &extractor_id,
                    text,
                    now,
                )?;
                let id = oxibrain_store::consolidation::write_derived_episode(
                    conn,
                    &space_owned,
                    text,
                    episode_ids,
                    Some(&uncertainty),
                    now,
                )?;
                oxibrain_store::consolidation::checkpoint_complete(conn, &member_hash, now)?;
                ids.push(id);
            }
            Ok(ids)
        })
        .await
    }

    /// Generate community summary text as cached Derived episodes (§9.4, §5.3).
    ///
    /// Mirrors [`consolidate_impl`] so community summaries satisfy the
    /// same deterministic consolidation invariants:
    ///
    /// 1. `checkpoint_begin` runs in its own write op BEFORE the LLM call
    ///    so a profile / LLM failure leaves a resumable `in_progress`
    ///    row instead of writing an uncited summary.
    /// 2. `cache_summary + write_derived_episode(sources, uncertainty) +
    ///    checkpoint_complete` run atomically in a single final write op
    ///    so the cache can never land without the derived episode row.
    /// 3. Sources are the primary episodes that cite the group's entities
    ///    (sorted, deterministic via `episodes_for_entities`), and the
    ///    persisted Uncertainty is computed from the group's belief
    ///    stats (`uncertainty_for_cluster`), so the summary is never
    ///    uncited and never Uncertainty-less.
    /// 4. The community member-set hash is namespaced (mixes the literal
    ///    `"community"` tag) so it cannot collide with an episode-cluster
    ///    hash for the same extractor — no migration needed.
    /// 5. LLM work sits between two transactions, never holding a store
    ///    transaction across model or Keychain work.
    pub(crate) async fn summarize_communities_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<usize, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read community groups [read-only].
        let groups = {
            let space_owned = space_owned.clone();
            self.read(move |conn| {
                oxibrain_store::consolidation::load_community_entities(conn, &space_owned)
            })
            .await?
        };

        // 2. Filter pending groups (skip already-completed cache entries,
        //    but keep in-progress rows so a crash resumes). Done in a
        //    single read op so we can short-circuit the LLM for done work.
        let pending_groups: Vec<oxibrain_store::consolidation::CommunityGroup> = {
            let extractor_id = extractor_id.clone();
            self.read(move |conn| {
                let done =
                    oxibrain_store::consolidation::completed_clusters(conn, &extractor_id)?;
                let mut kept = Vec::new();
                for g in groups {
                    let h =
                        oxibrain_store::consolidation::hash_community_member_set(&g.entity_ids);
                    if !done.contains(&hex::encode(h)) {
                        kept.push(g);
                    }
                }
                Ok::<_, BrainError>(kept)
            })
            .await?
        };

        // 3. For each pending group: checkpoint_begin BEFORE LLM call so
        //    a crash here leaves a resumable in_progress row.
        let mut checkpointed_hashes: Vec<[u8; 32]> = Vec::with_capacity(pending_groups.len());
        for group in &pending_groups {
            let entity_ids = group.entity_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_community_member_set(&entity_ids);
            let extractor_id = extractor_id.clone();
            self.write(move |conn| {
                oxibrain_store::consolidation::checkpoint_begin(
                    conn,
                    &member_hash,
                    &extractor_id,
                    now,
                )
            })
            .await?;
            checkpointed_hashes.push(member_hash);
        }

        // 4. Cache check + LLM call OUTSIDE any transaction. Cache
        //    hits short-circuit the LLM (and the final write op), only
        //    completing the checkpoint that the previous run already
        //    began.
        let mut ltm_results: Vec<(Vec<String>, String)> = Vec::new();
        for group in pending_groups.iter() {
            let entity_ids = group.entity_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_community_member_set(&entity_ids);
            let cached = {
                let extractor_id = extractor_id.clone();
                self.read(move |conn| {
                    oxibrain_store::consolidation::get_cached_summary(
                        conn,
                        "community",
                        &member_hash,
                        &extractor_id,
                    )
                })
                .await?
            };

            if let Some(text) = cached {
                ltm_results.push((entity_ids, text));
                continue;
            }

            // Build prompt and call LLM.
            let prompt = {
                let space_owned = space_owned.clone();
                let group = group.clone();
                self.read(move |conn| {
                    oxibrain_store::consolidation::build_community_prompt(
                        conn,
                        &space_owned,
                        &group,
                    )
                })
                .await?
            };

            let response = llm
                .complete(LlmRequest {
                    model: config.model_id.clone(),
                    system: Some("Summarize the themes among these entities.".into()),
                    prompt,
                    json_schema: None,
                    max_tokens: config.max_tokens,
                })
                .await?;
            ltm_results.push((entity_ids, response.text));
        }

        // 5. Final write op: gather sources + Uncertainty per group, then
        //    atomically cache_summary + write_derived_episode +
        //    checkpoint_complete. Same atomicity boundary as
        //    consolidate_impl — one sqlite transaction.
        let count = ltm_results.len();
        if count > 0 {
            self.write(move |conn| {
                for ((entity_ids, text), member_hash) in
                    ltm_results.iter().zip(checkpointed_hashes.iter())
                {
                    let sources = oxibrain_store::consolidation::episodes_for_entities(
                        conn,
                        &space_owned,
                        entity_ids,
                    )?;
                    let uncertainty = oxibrain_store::consolidation::uncertainty_for_cluster(
                        conn,
                        &space_owned,
                        entity_ids,
                        now,
                    )?;
                    oxibrain_store::consolidation::cache_summary(
                        conn,
                        "community",
                        member_hash,
                        &extractor_id,
                        text,
                        now,
                    )?;
                    oxibrain_store::consolidation::write_derived_episode(
                        conn,
                        &space_owned,
                        text,
                        &sources,
                        Some(&uncertainty),
                        now,
                    )?;
                    oxibrain_store::consolidation::checkpoint_complete(conn, member_hash, now)?;
                }
                Ok(())
            })
            .await?;
        }
        Ok(count)
    }
}
