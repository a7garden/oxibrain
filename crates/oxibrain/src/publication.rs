//! Authenticated source-view publication sequencing. No credential is accepted
//! in a serialized request or persisted with a Declaration.
use crate::Brain;
use oxibrain_core::publication::*;
use oxibrain_core::{Capability, EpisodeKind};
use oxibrain_ports::{BrainError, Timestamp};
use oxibrain_store::{
    ledger,
    project::{Declaration, ResolutionCache, project_declaration},
    publication as store, security,
};

/// Two separately authenticated grants, outside operation/model arguments.
/// Deliberately lacks Serialize, Deserialize and a credential-bearing Debug.
pub struct PublicationCredentials {
    origin: String,
    destination: String,
}
impl PublicationCredentials {
    pub fn new(origin: String, destination: String) -> Self {
        Self {
            origin,
            destination,
        }
    }
}
impl std::fmt::Debug for PublicationCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PublicationCredentials([redacted])")
    }
}
fn authority(
    conn: &rusqlite::Connection,
    credentials: &PublicationCredentials,
    request: &PublishViewV1,
    now: Timestamp,
) -> Result<String, BrainError> {
    let (origin_id, origin) =
        security::verify_token_identity(conn, &credentials.origin, now)?.ok_or_else(unavailable)?;
    let (destination_id, destination) =
        security::verify_token_identity(conn, &credentials.destination, now)?
            .ok_or_else(unavailable)?;
    authorize_publication(
        &origin,
        &destination,
        request.origin.space(),
        &request.destination,
        now,
    )?;
    if ledger::get_space(conn, request.origin.space())?.is_none()
        || ledger::get_space(conn, &request.destination)?.is_none()
    {
        return Err(unavailable());
    }
    Ok(exact_digest(&format!(
        "publication-principal/1\0{origin_id}\0{destination_id}"
    )))
}
fn snapshot(
    conn: &rusqlite::Connection,
    origin: &ViewOrigin,
) -> Result<SourceViewSnapshot, BrainError> {
    let text = match origin {
        ViewOrigin::Episode {
            space,
            episode_id,
            revision,
        } => {
            let episode =
                store::source_episode(conn, space, episode_id)?.ok_or_else(unavailable)?;
            if episode.space != *space
                || episode.kind != EpisodeKind::Primary
                || matches!(
                    episode.source,
                    oxibrain_core::SourceRef::Document { .. }
                        | oxibrain_core::SourceRef::DocumentRevision { .. }
                )
                || episode.redacted_at.is_some()
                || exact_digest(&episode.content) != *revision
            {
                return Err(unavailable());
            }
            episode.content
        }
        ViewOrigin::Publication {
            space,
            publication_id,
            revision,
        } => {
            let row =
                store::audience_record(conn, space, publication_id)?.ok_or_else(unavailable)?;
            if row.view_revision != *revision || row.state != ViewState::Current {
                return Err(unavailable());
            }
            row.bytes
        }
        ViewOrigin::Document { .. } => return Err(unavailable()),
    };
    Ok(SourceViewSnapshot {
        origin: origin.clone(),
        text,
        available: true,
    })
}
impl Brain {
    pub async fn publish_source_view(
        &self,
        request: PublishViewV1,
        credentials: PublicationCredentials,
    ) -> Result<PublicationReceipt, BrainError> {
        let now = self.clock.now();
        validate_request(&request, now)?;
        let origin = request.origin.clone();
        let check_request = request.clone();
        let check_credentials = PublicationCredentials::new(
            credentials.origin.clone(),
            credentials.destination.clone(),
        );
        self.read(move |conn| authority(conn, &check_credentials, &check_request, now))
            .await?;
        let prior_request = request.clone();
        let prior_credentials = PublicationCredentials::new(
            credentials.origin.clone(),
            credentials.destination.clone(),
        );
        let prior = self
            .read(move |conn| {
                let principal = authority(conn, &prior_credentials, &prior_request, now)?;
                let id = publication_id(
                    &prior_request.destination,
                    &principal,
                    &prior_request.occurrence_id,
                );
                let prior = store::audience_record(conn, &prior_request.destination, &id)?;
                if prior
                    .as_ref()
                    .is_some_and(|row| row.request_digest != request_digest(&prior_request))
                {
                    return Err(BrainError::Conflict(
                        "publication occurrence already exists".into(),
                    ));
                }
                Ok(prior)
            })
            .await?;
        if let Some(prior) = prior {
            let check_credentials =
                PublicationCredentials::new(credentials.origin, credentials.destination);
            let current = self.clock.now();
            self.read(move |conn| authority(conn, &check_credentials, &request, current))
                .await?;
            return Ok(publication_receipt(&prior));
        }
        if let ViewOrigin::Publication {
            space,
            publication_id,
            ..
        } = &origin
        {
            let space = space.clone();
            let id = publication_id.clone();
            let parent = self
                .read(move |conn| {
                    store::audience_record(conn, &space, &id)?.ok_or_else(unavailable)
                })
                .await?;
            self.refresh_publication_origin(&parent).await?;
        }
        let document = match &origin {
            ViewOrigin::Document { .. } => Some(self.publication_document_snapshot(&origin).await?),
            _ => None,
        };
        let preflight = match document.clone() {
            Some(value) => value,
            None => {
                let origin = origin.clone();
                self.read(move |conn| snapshot(conn, &origin)).await?
            }
        };
        // Validate exact bytes before entering the writer, then materialize
        // episode/publication state again under the writer transaction.
        prepare_publication(&request, &preflight, "preflight", now)?;
        let verify_request = request.clone();
        let verify_credentials = PublicationCredentials::new(
            credentials.origin.clone(),
            credentials.destination.clone(),
        );
        let commit_clock = self.clock.clone();
        let id = self
            .write(move |conn| {
                let now = commit_clock.now();
                let principal = authority(conn, &credentials, &request, now)?;
                let materialized = match document {
                    Some(value) => value,
                    None => snapshot(conn, &request.origin)?,
                };
                let event = prepare_publication(&request, &materialized, &principal, now)?;
                let id = publication_id(&request.destination, &principal, &request.occurrence_id);
                if let Some(prior) = store::audience_record(conn, &request.destination, &id)? {
                    if prior.request_digest != request_digest(&request) {
                        return Err(BrainError::Conflict(
                            "publication occurrence already exists".into(),
                        ));
                    }
                    return Ok(id);
                }
                project_declaration(
                    conn,
                    request.origin.space(),
                    &Declaration::PublicationV1 { event },
                    now,
                    &mut ResolutionCache::new(),
                )?;
                Ok(id)
            })
            .await?;
        // The commit and delivery are separate. An authority/source race may
        // leave an admitted occurrence; reconcile its original receipt.
        let delivery_clock = self.clock.clone();
        let post_request = verify_request.clone();
        self.read(move |conn| {
            authority(
                conn,
                &verify_credentials,
                &post_request,
                delivery_clock.now(),
            )
        })
        .await?;
        if matches!(origin, ViewOrigin::Document { .. }) {
            self.publication_document_snapshot(&origin).await?;
        }
        let destination = verify_request.destination;
        self.read(move |conn| {
            let row = store::audience_record(conn, &destination, &id)?.ok_or_else(unavailable)?;
            Ok(publication_receipt(&row))
        })
        .await
    }

    async fn refresh_publication_origin(&self, row: &PublicationRecord) -> Result<(), BrainError> {
        let row = row.clone();
        let origin = self
            .read(move |conn| store::root_origin(conn, &row))
            .await?;
        if !matches!(origin, ViewOrigin::Document { .. }) {
            return Ok(());
        }
        let observed = self.publication_document_observed(&origin).await?;
        let next = observed.map(|snapshot| snapshot.origin.revision().to_owned());
        if next.as_deref() == Some(origin.revision()) {
            return Ok(());
        }
        let now = self.clock.now();
        let source_space = origin.space().to_owned();
        let identity = origin.identity();
        let previous = origin.revision().to_owned();
        let occurrence_id = exact_digest(&format!(
            "verified-source-observation/1\0{identity}\0{previous}\0{next:?}"
        ));
        self.write(move |conn| {
            let event = PublicationEventV1::SourceObserved {
                occurrence_id,
                origin_identity: identity,
                previous_revision: previous,
                observed_revision: next,
                principal: "verified-source-observer".into(),
                occurred_at: now,
            };
            project_declaration(
                conn,
                &source_space,
                &Declaration::PublicationV1 { event },
                now,
                &mut ResolutionCache::new(),
            )?;
            Ok(())
        })
        .await
    }

    pub async fn read_published_view(
        &self,
        destination: &str,
        publication_id: &str,
        credential: String,
    ) -> Result<PublishedView, BrainError> {
        let now = self.clock.now();
        let destination = destination.to_owned();
        let id = publication_id.to_owned();
        let secret = credential.clone();
        let checked_destination = destination.clone();
        let mut row = self
            .read(move |conn| {
                let scope = security::verify_token(conn, &secret, now)?.ok_or_else(unavailable)?;
                if !scope.permits(Capability::Read, &checked_destination, now)
                    || scope.predicate_filter.is_some()
                    || scope.entity_type_filter.is_some()
                {
                    return Err(unavailable());
                }
                store::audience_record(conn, &checked_destination, &id)?.ok_or_else(unavailable)
            })
            .await?;
        if row.state == ViewState::Current {
            self.refresh_publication_origin(&row)
                .await
                .map_err(|_| unavailable())?;
            let refreshed = row.clone();
            row = self
                .read(move |conn| {
                    store::audience_record(conn, &refreshed.destination, &refreshed.id)?
                        .ok_or_else(unavailable)
                })
                .await?;
        }
        let checked_row = row.clone();
        let origin = self
            .read(move |conn| store::root_origin(conn, &checked_row))
            .await?;
        let document_fence =
            if row.state == ViewState::Current && matches!(origin, ViewOrigin::Document { .. }) {
                Some(
                    self.publication_document_delivery_fence(&origin)
                        .map_err(|_| unavailable())?,
                )
            } else {
                None
            };
        let directory = self.config.dir.clone();
        let delivery_clock = self.clock.clone();
        self.read(move |conn| {
            let now = delivery_clock.now();
            let scope = security::verify_token(conn, &credential, now)?.ok_or_else(unavailable)?;
            if !scope.permits(Capability::Read, &destination, now)
                || scope.predicate_filter.is_some()
                || scope.entity_type_filter.is_some()
            {
                return Err(unavailable());
            }
            let latest =
                store::audience_record(conn, &destination, &row.id)?.ok_or_else(unavailable)?;
            if latest.state == ViewState::Current
                && let Some(fence) = document_fence
            {
                fence.verify(conn, &directory).map_err(|_| unavailable())?;
            }
            Ok(audience_view(
                &latest.id,
                &latest.bytes,
                &latest.view_revision,
                latest.state,
            ))
        })
        .await
    }

    pub async fn withdraw_published_view(
        &self,
        destination: String,
        id: String,
        occurrence_id: String,
        credential: String,
    ) -> Result<(), BrainError> {
        let clock = self.clock.clone();
        self.write(move |conn| {
            let now = clock.now();
            let (principal, scope) =
                security::verify_token_identity(conn, &credential, now)?.ok_or_else(unavailable)?;
            if !scope.permits(Capability::Write, &destination, now)
                || scope.predicate_filter.is_some()
                || scope.entity_type_filter.is_some()
                || occurrence_id.is_empty()
                || occurrence_id.len() > 256
            {
                return Err(unavailable());
            }
            if let Some(PublicationEventV1::Withdrawn { publication_id, .. }) =
                store::operation_event(conn, &destination, &principal, &occurrence_id, "withdrawn")?
            {
                if publication_id != id {
                    return Err(BrainError::Conflict(
                        "publication operation occurrence already exists".into(),
                    ));
                }
                return Ok(());
            }
            let event = PublicationEventV1::Withdrawn {
                occurrence_id,
                destination: destination.clone(),
                publication_id: id,
                principal,
                occurred_at: now,
            };
            project_declaration(
                conn,
                &destination,
                &Declaration::PublicationV1 { event },
                now,
                &mut ResolutionCache::new(),
            )?;
            Ok(())
        })
        .await
    }
    /// Explicit authenticated source retraction. The event itself is the real
    /// source-view lifecycle operation; callers cannot submit a replacement
    /// revision or assert that a filesystem correction occurred.
    pub async fn retract_source_views(
        &self,
        origin: ViewOrigin,
        occurrence_id: String,
        credential: String,
    ) -> Result<(), BrainError> {
        if matches!(origin, ViewOrigin::Publication { .. })
            || occurrence_id.is_empty()
            || occurrence_id.len() > 256
        {
            return Err(unavailable());
        }
        let now = self.clock.now();
        let checked_origin = origin.clone();
        let checked_credential = credential.clone();
        self.read(move |conn| {
            let scope =
                security::verify_token(conn, &checked_credential, now)?.ok_or_else(unavailable)?;
            if !scope.permits(Capability::Write, checked_origin.space(), now)
                || scope.predicate_filter.is_some()
                || scope.entity_type_filter.is_some()
            {
                return Err(unavailable());
            }
            Ok(())
        })
        .await?;
        let materialized_document = if matches!(origin, ViewOrigin::Document { .. }) {
            Some(self.publication_document_snapshot(&origin).await?)
        } else {
            None
        };
        let commit_clock = self.clock.clone();
        self.write(move |conn| {
            let now = commit_clock.now();
            let (principal, scope) =
                security::verify_token_identity(conn, &credential, now)?.ok_or_else(unavailable)?;
            if !scope.permits(Capability::Write, origin.space(), now)
                || scope.predicate_filter.is_some()
                || scope.entity_type_filter.is_some()
            {
                return Err(unavailable());
            }
            if let Some(previous) = store::operation_event(
                conn,
                origin.space(),
                &principal,
                &occurrence_id,
                "source_observed",
            )? {
                if let PublicationEventV1::SourceObserved {
                    origin_identity,
                    previous_revision,
                    observed_revision,
                    ..
                } = previous
                    && origin_identity == origin.identity()
                    && previous_revision == origin.revision()
                    && observed_revision.is_none()
                {
                    return Ok(());
                }
                return Err(BrainError::Conflict(
                    "publication operation occurrence already exists".into(),
                ));
            }
            if materialized_document.is_none() {
                snapshot(conn, &origin)?;
            }
            let event = PublicationEventV1::SourceObserved {
                occurrence_id,
                origin_identity: origin.identity(),
                previous_revision: origin.revision().into(),
                observed_revision: None,
                principal,
                occurred_at: now,
            };
            project_declaration(
                conn,
                origin.space(),
                &Declaration::PublicationV1 { event },
                now,
                &mut ResolutionCache::new(),
            )?;
            Ok(())
        })
        .await
    }
}
