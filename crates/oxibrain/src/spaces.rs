//! Space lifecycle, registry reads, and per-space counts. Facade split —
//! the `Brain` type stays the one public surface; its methods live in
//! focused submodules (M10 exit: facade under 1,000 LOC).

use crate::{Brain, Episode, SpaceInfo};
use oxibrain_ports::{BrainError, Timestamp};
use oxibrain_store::documents::DocumentCache;
use oxibrain_store::ledger;
use std::collections::HashMap;

impl Brain {
    /// Ensure a space exists. Returns its id.
    pub async fn ensure_space(&self, name: &str) -> Result<String, BrainError> {
        let now = self.clock.now();
        let name = name.to_string();
        self.write(move |conn| ledger::create_space(conn, &name, now))
            .await
    }

    /// Look up a space id by name without creating it (read-only). `Ok(None)`
    /// when the name has no row. Scope gates call this instead of
    /// `ensure_space` so denied reads do not create shadow rows.
    pub async fn lookup_space(&self, name: &str) -> Result<Option<String>, BrainError> {
        let name = name.to_string();
        self.read(move |conn| ledger::lookup_space_by_name(conn, &name))
            .await
    }

    /// Drop a space row (verified empty by the caller). Spec §4.5.
    pub async fn drop_space(&self, space_id: &str) -> Result<(), BrainError> {
        let space_id = space_id.to_string();
        self.write(move |conn| ledger::drop_space(conn, &space_id))
            .await
    }

    /// List all spaces with live counts, ordered by (created_at, id).
    pub async fn list_spaces(&self) -> Result<Vec<SpaceInfo>, BrainError> {
        self.read(move |conn| {
            let rows = ledger::list_spaces(conn)?;
            Ok(rows
                .into_iter()
                .map(|r| SpaceInfo {
                    id: r.id,
                    name: r.name,
                    created_at: Timestamp::from_millis(r.created_at),
                    episode_count: r.episode_count,
                    entity_count: r.entity_count,
                })
                .collect())
        })
        .await
    }

    /// Predicate registry as persisted in the store — the core/v1 seed plus
    /// every custom registration — ordered by name. The read path for
    /// registry introspection (`admin predicate list`); semantics stay in
    /// the registry (P4).
    pub async fn list_predicates(&self) -> Result<Vec<oxibrain_core::PredicateDef>, BrainError> {
        self.read(move |conn| {
            let mut defs: Vec<oxibrain_core::PredicateDef> =
                oxibrain_store::registry::load_all_predicates(conn)?
                    .into_values()
                    .collect();
            defs.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(defs)
        })
        .await
    }

    /// Episode count for one space id (space-remove empty check, spec §4.5).
    pub async fn episode_count_for_space(&self, space_id: &str) -> Result<i64, BrainError> {
        let space_id = space_id.to_string();
        self.read(move |conn| ledger::episode_count_for_space(conn, &space_id))
            .await
    }

    /// Document rows in the documents cache for a space NAME (listing, spec §4.2).
    pub async fn document_count_for_space(&self, space_name: &str) -> Result<u64, BrainError> {
        let dir = self.config.dir.clone();
        let name = space_name.to_string();
        tokio::task::spawn_blocking(move || match DocumentCache::open_ro(&dir) {
            Ok(cache) => cache.document_count_for_space(&name),
            Err(BrainError::NotFound(_)) => Ok(0),
            Err(e) => Err(e),
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Chunk count for a space NAME (empty check, spec §4.5).
    pub async fn chunk_count_for_space(&self, space_name: &str) -> Result<u64, BrainError> {
        let dir = self.config.dir.clone();
        let name = space_name.to_string();
        tokio::task::spawn_blocking(move || match DocumentCache::open_ro(&dir) {
            Ok(cache) => cache.chunk_count_for_space(&name),
            Err(BrainError::NotFound(_)) => Ok(0),
            Err(e) => Err(e),
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Purge every documents-cache row for a space NAME (purge step 3).
    pub async fn purge_documents_for_space(&self, space_name: &str) -> Result<(), BrainError> {
        let dir = self.config.dir.clone();
        let name = space_name.to_string();
        tokio::task::spawn_blocking(move || {
            let cache = DocumentCache::open_rw(&dir)?;
            cache.purge_space(&name)
        })
        .await
        .map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Ingest a note episode. Returns the episode id (content-derived).
    pub async fn ingest_note(
        &self,
        space: &str,
        path: &str,
        content: String,
        occurred_at: Timestamp,
    ) -> Result<String, BrainError> {
        self.ingest_note_impl(space, path, content, occurred_at)
            .await
    }

    /// Note content hashes per source path (live episodes only), for sync
    /// classification. Read-only; the decision lives in
    /// [`oxibrain_core::classify_sync`].
    pub async fn note_hashes(&self, space: &str) -> Result<oxibrain_core::KnownNotes, BrainError> {
        let space = space.to_string();
        self.read(move |conn| ledger::note_hashes_by_path(conn, &space))
            .await
    }

    /// Latest event-path episode state per locator for a source.
    /// Used by sync to derive occurrence chains (§4.2 pull mode).
    pub async fn locator_states(
        &self,
        space: &str,
        source_id: &str,
    ) -> Result<HashMap<String, oxibrain_core::sync::LocatorState>, BrainError> {
        let space = space.to_string();
        let source_id = source_id.to_string();
        self.read(move |conn| ledger::locator_states(conn, &space, &source_id))
            .await
    }

    /// Current time from the configured clock. Exposed for callers that need
    /// a Timestamp without going through an ingest method.
    pub fn clock_now(&self) -> Timestamp {
        self.clock.now()
    }

    /// Fetch only within the caller's already authorized canonical space.
    pub async fn get_episode_in_space(
        &self,
        space: &str,
        id: &str,
    ) -> Result<Option<Episode>, BrainError> {
        let space = space.to_owned();
        let id = id.to_owned();
        self.read(move |conn| {
            let episode = ledger::get_episode_in_space(conn, &space, &id)?;
            Ok(episode.filter(|ep| {
                !matches!(
                    oxibrain_store::project::parse_declaration(&ep.content),
                    Ok(oxibrain_store::project::Declaration::PublicationV1 { .. })
                )
            }))
        })
        .await
    }

    pub async fn get_episode(&self, id: &str) -> Result<Option<Episode>, BrainError> {
        let id = id.to_string();
        self.read(move |conn| {
            let episode = ledger::get_episode(conn, &id)?;
            // Publication control records are an internal ledger format, not
            // raw evidence. Ordinary authorized declarations remain readable.
            Ok(episode.filter(|ep| {
                !matches!(
                    oxibrain_store::project::parse_declaration(&ep.content),
                    Ok(oxibrain_store::project::Declaration::PublicationV1 { .. })
                )
            }))
        })
        .await
    }

    pub async fn episode_count(&self) -> Result<i64, BrainError> {
        self.read(ledger::episode_count).await
    }
}
