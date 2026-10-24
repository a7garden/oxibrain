//! Optional source semantics. Index explicitly; read only query vectors at recall.
use crate::{Brain, BrainError, ReadScope, Timestamp};
use oxibrain_core::read_channels::ChannelReceipt;
use oxibrain_store::{
    read_evidence::{RawSourceHit, SourceReadScope},
    source_vectors,
};
use std::sync::Arc;

pub(crate) async fn checked_embeddings(
    port: Arc<dyn oxibrain_ports::EmbeddingPort>,
    texts: Vec<String>,
) -> Result<Vec<Vec<f32>>, BrainError> {
    let dimension = port.dim();
    let expected = texts.len();
    let vectors = tokio::task::spawn_blocking(move || {
        port.embed(&texts.iter().map(String::as_str).collect::<Vec<_>>())
    })
    .await
    .map_err(|e| BrainError::Model(format!("embedding worker: {e}")))??;
    if dimension == 0
        || vectors.len() != expected
        || vectors
            .iter()
            .any(|v| v.len() != dimension || v.iter().any(|x| !x.is_finite()))
    {
        return Err(BrainError::Model(
            "invalid embedding output dimensions/count/values".into(),
        ));
    }
    Ok(vectors)
}

impl Brain {
    /// Explicit resumable indexing; no extraction and no model work under a lock.
    pub async fn embed_pending_memory(&self, budget: usize) -> Result<usize, BrainError> {
        let port = self
            .embedder
            .clone()
            .ok_or_else(|| BrainError::Config("no embedding port configured".into()))?;
        let model = self
            .embedding_identity
            .clone()
            .expect("configured embedder has identity");
        let spaces = self.read(oxibrain_store::ledger::list_spaces).await?;
        let mut written = 0;
        let mut attempted = 0;
        for space in spaces {
            while attempted < budget {
                let (space, model) = (space.id.clone(), model.clone());
                let limit = (budget - attempted).min(32);
                let pending = self
                    .read(move |c| source_vectors::pending(c, &space, &model, limit))
                    .await?;
                if pending.is_empty() {
                    break;
                }
                attempted += pending.len();
                let vectors = checked_embeddings(
                    port.clone(),
                    pending.iter().map(|p| p.text.clone()).collect(),
                )
                .await?;
                let rows = pending.into_iter().zip(vectors).collect::<Vec<_>>();
                let model = model_identity(self);
                written += self
                    .write(move |c| source_vectors::upsert(c, &model, &rows))
                    .await?;
            }
        }
        Ok(written)
    }

    pub(crate) async fn semantic_source_hits(
        &self,
        scope: &ReadScope,
        query: &str,
        session: Option<&str>,
        now: Timestamp,
    ) -> Result<(Vec<RawSourceHit>, ChannelReceipt), BrainError> {
        let mut receipt = ChannelReceipt {
            plane: "memory_sources".into(),
            channel: "dense".into(),
            eligible: self.embedder.is_some() && self.read_policy.semantic_sources,
            model_identity: self.embedding_identity.clone(),
            ..Default::default()
        };
        let Some(port) = self
            .embedder
            .clone()
            .filter(|_| self.read_policy.semantic_sources)
        else {
            return Ok((Vec::new(), receipt));
        };
        let (space, session, model) = (
            scope.space.clone(),
            session.map(str::to_owned),
            model_identity(self),
        );
        let known = scope.known_at.unwrap_or(now);
        let valid = scope.as_of.unwrap_or(Timestamp(i64::MAX));
        let coverage = {
            let (space, session, model) = (space.clone(), session.clone(), model.clone());
            self.read(move |c| {
                if !source_vectors::available(c)? {
                    return Ok(None);
                }
                source_vectors::coverage(
                    c,
                    &SourceReadScope {
                        space: &space,
                        known_at: known,
                        as_of: valid,
                        excluded_session: session.as_deref(),
                    },
                    &model,
                )
                .map(Some)
            })
            .await?
        };
        let Some((total, indexed)) = coverage else {
            receipt.fallback = Some(
                "source vector projection unavailable; run admin index --memory --embed".into(),
            );
            return Ok((Vec::new(), receipt));
        };
        receipt.total = Some(total);
        receipt.indexed = Some(indexed);
        if indexed == 0 {
            if total > 0 {
                receipt.fallback = Some(
                    "no source vectors for this model; run admin index --memory --embed".into(),
                );
            }
            return Ok((Vec::new(), receipt));
        }
        let vectors = match checked_embeddings(port, vec![query.to_string()]).await {
            Ok(v) => v,
            Err(e) => {
                receipt.fallback = Some(e.to_string());
                return Ok((Vec::new(), receipt));
            }
        };
        let query_vector = &vectors[0];
        receipt.executed = true;
        let mut after = String::new();
        let mut scored = Vec::new();
        while receipt.considered < self.read_policy.dense_scan_vectors {
            let limit = (self.read_policy.dense_scan_vectors - receipt.considered).min(128);
            let (space, session, model, cursor) =
                (space.clone(), session.clone(), model.clone(), after.clone());
            let batch = self
                .read(move |c| {
                    source_vectors::batch(
                        c,
                        &SourceReadScope {
                            space: &space,
                            known_at: known,
                            as_of: valid,
                            excluded_session: session.as_deref(),
                        },
                        &model,
                        &cursor,
                        limit,
                    )
                })
                .await;
            let batch = match batch {
                Ok(b) => b,
                Err(e) => {
                    receipt.fallback = Some(e.to_string());
                    return Ok((Vec::new(), receipt));
                }
            };
            if batch.is_empty() {
                break;
            }
            after = batch.last().expect("nonempty batch").id.clone();
            receipt.considered += batch.len();
            if batch
                .iter()
                .any(|row| row.vector.len() != query_vector.len())
            {
                receipt.fallback = Some("source vector model dimension mismatch".into());
                return Ok((Vec::new(), receipt));
            }
            let ids = batch.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
            let mut vectors = vec![query_vector.clone()];
            vectors.extend(batch.into_iter().map(|row| row.vector));
            scored.extend(oxibrain_core::visible_memory::cosine_scores(&ids, &vectors));
            oxibrain_core::visible_memory::retain_dense_candidates(
                &mut scored,
                self.read_policy.channel_depth,
            );
        }
        receipt.capped = receipt.considered >= self.read_policy.dense_scan_vectors
            && receipt.considered < indexed;
        if !receipt.capped && receipt.considered < indexed {
            receipt.fallback = Some("source vector index changed during scan".into());
        }
        let ids = scored.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        let hits = self
            .read(move |c| {
                source_vectors::hits(
                    c,
                    &SourceReadScope {
                        space: &space,
                        known_at: known,
                        as_of: valid,
                        excluded_session: session.as_deref(),
                    },
                    &ids,
                )
            })
            .await?;
        Ok((hits, receipt))
    }
}
fn model_identity(brain: &Brain) -> String {
    brain
        .embedding_identity
        .clone()
        .expect("configured embedder identity")
}
