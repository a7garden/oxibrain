//! Extraction pipeline methods (M10 10.10). Extracted from lib.rs to keep
//! the facade under 1,000 LOC. The methods here are `pub(crate)` impl blocks
//! on `Brain`; the facade wraps them with 1-line delegations.

use super::{Brain, BrainError, LlmPort, LlmRequest};
use std::sync::Arc;

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

        // 1. Read episode content [reader].
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

        // 3. Call LLM [async, off-actor]. Grammar-capable adapters (the local
        //    GGUF path, §9.4 D28) get a GBNF grammar generated from the
        //    registry (P4); everything else takes schema-and-repair.
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
        // tool call, grammar runaway past the KV budget) is invalid output:
        // it is recorded in extraction_failures like any other, never
        // silently dropped, and then fails the episode loudly.
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

        // 6. Project [WriteOp].
        let h = self.handle.clone();
        let cache = self.cache.clone();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                // Cache the raw response.
                oxibrain_store::extraction::cache_response(
                    conn,
                    &episode_id,
                    &extractor_id,
                    &raw_response,
                    now,
                )?;
                // Project valid claims with the persistent resolution cache.
                let mut cache = cache.lock().expect("resolution cache poisoned");
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
                let summary = oxibrain_core::extraction::ExtractSummary {
                    extracted: n,
                    quarantined: invalid_count,
                    episodes_done: 1,
                    episodes_failed: 0,
                    failures: Vec::new(),
                };
                let _ = tx.send(summary);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("extract_one channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
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
        let h = self.handle.clone();
        let episode_id = episode_id.to_string();
        let extractor_id = extractor_id.to_string();
        let raw = raw_response.to_string();
        let msg = error.to_string();
        let res = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let errors_json = serde_json::to_string(&[msg]).unwrap_or_else(|_| "[]".into());
                oxibrain_store::quarantine::record_failure(
                    conn,
                    &episode_id,
                    &extractor_id,
                    &raw,
                    &errors_json,
                    now,
                )?;
                let _ = tx.send(());
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("record_failure channel dropped".into()))
        })
        .await;
        if let Err(e) = res {
            eprintln!("warn: recording response failure: {e}");
        }
    }
    /// Process pending extraction jobs in batch. Claims up to
    /// `budget.max_episodes_per_batch` ready jobs and extracts each.
    pub(crate) async fn extract_pending_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
        budget: &oxibrain_core::extraction::ExtractionBudget,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let _llm = self.require_llm()?;
        let now = self.clock.now();
        let extractor_id = config.id();

        // 1. Claim jobs.
        let h = self.handle.clone();
        let lease_timeout = budget.lease_timeout_secs;
        let batch_limit = budget.max_episodes_per_batch;
        let jobs = tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let _ = oxibrain_store::extraction::reclaim_expired(conn, now);
                let jobs = oxibrain_store::extraction::claim_jobs(
                    conn,
                    &extractor_id,
                    lease_timeout,
                    batch_limit,
                    now,
                )?;
                let _ = tx.send(jobs);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("claim_jobs channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. Process each job via extract_one.
        let mut total = oxibrain_core::extraction::ExtractSummary::default();
        for job in jobs {
            match self.extract_one(space, &job.episode_id, config).await {
                Ok(summary) => {
                    total.extracted += summary.extracted;
                    total.quarantined += summary.quarantined;
                    total.episodes_done += 1;
                    // Complete the job.
                    let h = self.handle.clone();
                    let job_id = job.id.clone();
                    let now = self.clock.now();
                    let _ = tokio::task::spawn_blocking(move || {
                        let (tx, rx) = std::sync::mpsc::channel();
                        if let Some(w) = &h.writer {
                            let _ = w.submit(Box::new(move |conn| {
                                let _ = tx.send(oxibrain_store::extraction::complete_job(
                                    conn, &job_id, now,
                                ));
                                Ok(())
                            }));
                            let _ = w.flush();
                        }
                        rx.recv()
                    })
                    .await;
                }
                Err(e) => {
                    total.episodes_failed += 1;
                    total.failures.push((job.episode_id.clone(), e.to_string()));
                    // Fail the job.
                    let h = self.handle.clone();
                    let job_id = job.id.clone();
                    let now = self.clock.now();
                    let max_attempts = budget.max_repair_attempts + 1;
                    let _ = tokio::task::spawn_blocking(move || {
                        let (tx, rx) = std::sync::mpsc::channel();
                        if let Some(w) = &h.writer {
                            let _ = w.submit(Box::new(move |conn| {
                                let _ = tx.send(oxibrain_store::extraction::fail_job(
                                    conn,
                                    &job_id,
                                    &e.to_string(),
                                    max_attempts,
                                    now,
                                ));
                                Ok(())
                            }));
                            let _ = w.flush();
                        }
                        rx.recv()
                    })
                    .await;
                }
            }
        }
        Ok(total)
    }

    /// Re-extract all primary episodes with a new extractor config.
    /// Old cache entries are preserved (different extractor_id = different PK).
    pub(crate) async fn reextract_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<oxibrain_core::extraction::ExtractSummary, BrainError> {
        let _llm = self.require_llm()?;
        let h = self.handle.clone();
        let space = space.to_string();
        let query_space = space.clone();
        let extractor_id = config.id();

        // Find primary episodes that don't have a cache entry for this extractor.
        let episode_ids = tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| {
                oxibrain_store::extraction::uncached_episodes(conn, &query_space, &extractor_id)
            })
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

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
    pub(crate) async fn consolidate_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<Vec<String>, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let h = self.handle.clone();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read episode clusters [reader].
        let clusters = tokio::task::spawn_blocking({
            let h = h.clone();
            let space_owned = space_owned.clone();
            move || {
                h.readers.read(|conn| {
                    oxibrain_store::consolidation::find_episode_clusters(conn, &space_owned)
                })
            }
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. For each cluster: check cache, call LLM if miss.
        let mut summaries: Vec<(Vec<String>, String)> = Vec::new();
        for cluster in clusters {
            let episode_ids = cluster.episode_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_member_set(&episode_ids);
            let cached = tokio::task::spawn_blocking({
                let h = h.clone();
                let extractor_id = extractor_id.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::get_cached_summary(
                            conn,
                            "consolidation",
                            &member_hash,
                            &extractor_id,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            if let Some(text) = cached {
                summaries.push((episode_ids.clone(), text));
            } else {
                // Build prompt and call LLM.
                let prompt = tokio::task::spawn_blocking({
                    let h = h.clone();
                    let space_owned = space_owned.clone();
                    let prompt_ids = episode_ids.clone();
                    let prompt_shared = cluster.shared_entities.clone();
                    move || {
                        h.readers.read(|conn| {
                            oxibrain_store::consolidation::build_consolidation_prompt(
                                conn,
                                &space_owned,
                                &oxibrain_store::consolidation::EpisodeCluster {
                                    episode_ids: prompt_ids,
                                    shared_entities: prompt_shared,
                                },
                            )
                        })
                    }
                })
                .await
                .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

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
        }

        // 3. Write Derived episodes + cache [WriteOp].
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer()?.submit(Box::new(move |conn| {
                let mut ids = Vec::new();
                for (episode_ids, text) in &summaries {
                    let member_hash = oxibrain_store::consolidation::hash_member_set(episode_ids);
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
                        None,
                        now,
                    )?;
                    ids.push(id);
                }
                let _ = tx.send(ids);
                Ok(())
            }))?;
            h.writer()?.flush()?;
            rx.recv()
                .map_err(|_| BrainError::Storage("consolidate channel dropped".into()))
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Generate community summary text as cached Derived episodes (§9.4, §5.3).
    pub(crate) async fn summarize_communities_impl(
        &self,
        space: &str,
        config: &oxibrain_core::extraction::ExtractorConfig,
    ) -> Result<usize, BrainError> {
        let llm = self.require_llm()?.clone();
        let now = self.clock.now();
        let h = self.handle.clone();
        let space_owned = space.to_string();
        let extractor_id = config.id();

        // 1. Read community groups [reader].
        let groups = tokio::task::spawn_blocking({
            let h = h.clone();
            let space_owned = space_owned.clone();
            move || {
                h.readers.read(|conn| {
                    oxibrain_store::consolidation::load_community_entities(conn, &space_owned)
                })
            }
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

        // 2. For each group: check cache, call LLM if miss.
        let mut summaries: Vec<(Vec<String>, String)> = Vec::new();
        for group in groups {
            let entity_ids = group.entity_ids.clone();
            let member_hash = oxibrain_store::consolidation::hash_member_set(&group.entity_ids);
            let cached = tokio::task::spawn_blocking({
                let h = h.clone();
                let extractor_id = extractor_id.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::get_cached_summary(
                            conn,
                            "community",
                            &member_hash,
                            &extractor_id,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            if cached.is_some() {
                continue; // cache hit
            }

            // Build prompt and call LLM.
            let prompt = tokio::task::spawn_blocking({
                let h = h.clone();
                let space_owned = space_owned.clone();
                move || {
                    h.readers.read(|conn| {
                        oxibrain_store::consolidation::build_community_prompt(
                            conn,
                            &space_owned,
                            &group,
                        )
                    })
                }
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;

            let response = llm
                .complete(LlmRequest {
                    model: config.model_id.clone(),
                    system: Some("Summarize the themes among these entities.".into()),
                    prompt,
                    json_schema: None,
                    max_tokens: config.max_tokens,
                })
                .await?;
            summaries.push((entity_ids, response.text));
        }

        // 3. Write Derived episodes + cache [WriteOp].
        let count = summaries.len();
        if count > 0 {
            tokio::task::spawn_blocking(move || {
                let (tx, rx) = std::sync::mpsc::channel();
                h.writer()?.submit(Box::new(move |conn| {
                    for (entity_ids, text) in &summaries {
                        let member_hash =
                            oxibrain_store::consolidation::hash_member_set(entity_ids);
                        oxibrain_store::consolidation::cache_summary(
                            conn,
                            "community",
                            &member_hash,
                            &extractor_id,
                            text,
                            now,
                        )?;
                        // Write as a Derived episode linking to episodes mentioning these entities.
                        oxibrain_store::consolidation::write_derived_episode(
                            conn,
                            &space_owned,
                            text,
                            &[],
                            None,
                            now,
                        )?;
                    }
                    let _ = tx.send(());
                    Ok(())
                }))?;
                h.writer()?.flush()?;
                rx.recv().map_err(|_| {
                    BrainError::Storage("summarize_communities channel dropped".into())
                })
            })
            .await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        }
        Ok(count)
    }
}
