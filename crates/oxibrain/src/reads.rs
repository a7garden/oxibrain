//! Read-path delegations (truth + ranking halves). Facade split — the
//! `Brain` type stays the one public surface; its methods live in focused
//! submodules (M10 exit: facade under 1,000 LOC).

use crate::Brain;
use oxibrain_ports::BrainError;
use oxibrain_store::query;

impl Brain {
    /// Current beliefs for an entity (follows merge chain).
    pub async fn beliefs(
        &self,
        space: &str,
        entity_id: &str,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| query::beliefs_for_entity(conn, &space, &entity_id))
            .await
    }

    /// Canonical display surface for an entity id (follows merge chain).
    /// Used by MCP projections that render human-readable object names.
    pub async fn entity_surface(&self, space: &str, entity_id: &str) -> Result<String, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| {
            oxibrain_store::brief::surface_of(conn, &entity_id)
                .map_err(|_| BrainError::NotFound(format!("entity {entity_id} in {space}")))
        })
        .await
    }

    /// Rebuild a Retract declaration's inputs from a stored statement —
    /// the statement-first retract path (the conflicts inbox holds statement
    /// ids, not resolvable surfaces). Pure read; the caller declares.
    pub async fn retract_parts(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<
        (
            oxibrain_store::project::EntityRef,
            String,
            oxibrain_store::project::DeclObject,
        ),
        BrainError,
    > {
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        self.read(move |conn| query::retract_parts(conn, &space, &statement_id))
            .await
    }

    /// Beliefs as of a valid-time point.
    pub async fn beliefs_as_of(
        &self,
        space: &str,
        entity_id: &str,
        valid_at: oxibrain_ports::Timestamp,
    ) -> Result<Vec<oxibrain_core::Belief>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| query::beliefs_as_of(conn, &space, &entity_id, Some(valid_at), None))
            .await
    }

    /// All contradicted statements in a space.
    pub async fn contradictions(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::Statement>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::contradictions(conn, &space))
            .await
    }

    /// Contradicted statements with surfaces and supporting episodes (UI DTO).
    pub async fn contradiction_details(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::query::ContradictionDetail>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::query::contradiction_details(conn, &space))
            .await
    }

    /// Aggregate counts for a space (episodes, entities, statements,
    /// contradicted statements). Used by the `stats` MCP tool and dashboards.
    pub async fn stats(&self, space: &str) -> Result<oxibrain_core::SpaceStats, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::space_stats(conn, &space))
            .await
    }

    /// Hybrid (or mode-specific) query. Returns ranked results with provenance.
    pub async fn query(
        &self,
        q: oxibrain_core::retrieval::Query,
    ) -> Result<oxibrain_core::retrieval::RankingResult, BrainError> {
        let embedder = self.embedder.clone();
        self.read(move |conn| query::hybrid_query(conn, &q, embedder.as_deref()))
            .await
    }

    /// Memory-plane search for the planes-aware `search` (document_plane).
    /// Hybrid query projected to UI-ready entity hits. Legacy
    /// `document` / `document_revision` episodes are excluded at the FTS
    /// fetch (see `query::fts_search`).
    pub(crate) async fn search_memory(
        &self,
        q: &oxibrain_core::retrieval::Query,
    ) -> Result<Vec<oxibrain_store::query::SearchResult>, BrainError> {
        let embedder = self.embedder.clone();
        let q = q.clone();
        self.read(move |conn| {
            let ranking = query::hybrid_query(conn, &q, embedder.as_deref())?;
            query::search_results(conn, &q.space, &ranking)
        })
        .await
    }

    pub async fn rebuild_indexes(&self, space: &str) -> Result<(), BrainError> {
        let space = space.to_string();
        self.write(move |conn| oxibrain_store::index_ops::rebuild_indexes(conn, &space))
            .await
    }

    /// Bounded subgraph traversal over a space's statement graph.
    pub async fn traverse(
        &self,
        space: &str,
        spec: oxibrain_core::retrieval::TraversalSpec,
    ) -> Result<oxibrain_core::retrieval::TraversalResult, BrainError> {
        let space = space.to_string();
        self.read(move |conn| query::traverse(conn, &space, &spec))
            .await
    }

    /// Look up the entity_id for a surface form + type within a space. Returns
    /// `None` if the entity hasn't been declared yet.
    pub async fn resolve_entity_id(
        &self,
        space: &str,
        ty: &str,
        surface: &str,
    ) -> Result<Option<String>, BrainError> {
        let space = space.to_string();
        let ty = ty.to_string();
        let surface = surface.to_string();
        self.read(move |conn| query::resolve_entity_id(conn, &space, &ty, &surface))
            .await
    }

    pub async fn list_entities(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_core::Entity>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_entities(conn, &space, limit))
            .await
    }

    /// Entities with canonical surfaces resolved, for the `space://` resource.
    /// One SQL join — no N+1.
    pub async fn list_entity_cards(
        &self,
        space: &str,
        limit: usize,
    ) -> Result<Vec<oxibrain_store::knowledge::EntityCard>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_entity_cards(conn, &space, limit))
            .await
    }

    /// List merge records in a space, most recent first.
    /// Used by the `review_merges` MCP tool.
    pub async fn list_merges(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_core::EntityMerge>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::knowledge::list_merges(conn, &space))
            .await
    }

    /// List extraction failures in a space, most recent first.
    /// Used by the `review_merges` MCP tool (`section: "failures"`).
    pub async fn list_failures(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::quarantine::ExtractionFailure>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::quarantine::list_failures(conn, Some(&space)))
            .await
    }

    /// List registered sources in a space.
    /// Used by the `review_merges` MCP tool (`section: "sources"`).
    pub async fn list_sources(
        &self,
        space: &str,
    ) -> Result<Vec<oxibrain_store::ledger::SourceRow>, BrainError> {
        let space = space.to_string();
        self.read(move |conn| oxibrain_store::ledger::list_sources(conn, &space))
            .await
    }

    /// Source-registry rows that never produced an episode (doctor report).
    pub async fn orphan_source_count(&self) -> Result<i64, BrainError> {
        self.read(oxibrain_store::ledger::count_orphan_sources)
            .await
    }

    pub async fn timeline(
        &self,
        space: &str,
        entity_id: &str,
        from: Option<oxibrain_ports::Timestamp>,
        to: Option<oxibrain_ports::Timestamp>,
    ) -> Result<Vec<oxibrain_store::timeline::TimelineEntry>, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| {
            oxibrain_store::timeline::timeline(conn, &space, &entity_id, from, to)
        })
        .await
    }

    pub async fn diff(
        &self,
        space: &str,
        entity_id: &str,
        at_a: oxibrain_ports::Timestamp,
        at_b: oxibrain_ports::Timestamp,
    ) -> Result<oxibrain_store::timeline::DiffResult, BrainError> {
        let space = space.to_string();
        let entity_id = entity_id.to_string();
        self.read(move |conn| oxibrain_store::timeline::diff(conn, &space, &entity_id, at_a, at_b))
            .await
    }

    pub async fn why(
        &self,
        space: &str,
        statement_id: &str,
    ) -> Result<oxibrain_store::explain::ExplainBlock, BrainError> {
        let space = space.to_string();
        let statement_id = statement_id.to_string();
        self.read(move |conn| oxibrain_store::explain::why(conn, &space, &statement_id))
            .await
    }
}
