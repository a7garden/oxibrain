//! Brief rendering: maps store data to view models and renders Markdown.
//!
//! Extracted from `lib.rs` to keep the facade under the 1,000 LOC cap (M10
//! 10.10). Pure functions only — no I/O, no model calls.

/// Map a store brief data struct to the view model and render Markdown.
pub(crate) fn render_entity_brief(data: &oxibrain_store::brief::EntityBriefData) -> String {
    use oxibrain_views as views;
    let brief = views::EntityBrief {
        surface: data.canonical_surface.clone(),
        ty: data.entity.ty.clone(),
        aliases: data.aliases.clone(),
        beliefs: data
            .beliefs
            .iter()
            .map(|b| views::BeliefView {
                predicate: b.predicate.clone(),
                object: b.object.clone(),
                object_entity: b.object_entity.clone(),
                valid_from: fmt_ts(b.valid_from),
                valid_to: fmt_ts(b.valid_to),
                confidence: b.confidence,
                affirm: b.affirm,
                deny: b.deny,
                episodes: b.episodes,
                status: b.status.clone(),
            })
            .collect(),
        contradictions: data
            .contradictions
            .iter()
            .map(|c| views::ContradictionView {
                predicate: c.predicate.clone(),
                object: c.object.clone(),
                affirm_episodes: c.affirm_episodes.clone(),
                deny_episodes: c.deny_episodes.clone(),
            })
            .collect(),
        neighbours: data
            .neighbours
            .iter()
            .map(|n| views::NeighbourView {
                surface: n.surface.clone(),
                entity: n.entity.clone(),
                predicate: n.predicate.clone(),
                direction: n.direction.clone(),
            })
            .collect(),
        timeline: data
            .timeline
            .iter()
            .map(|t| views::TimelineView {
                at: fmt_ts(t.valid_from),
                predicate: t.predicate.clone(),
                object: t.object_repr.clone(),
                object_entity: t.object_entity.clone(),
                status: t.status.clone(),
            })
            .collect(),
        sources: data
            .sources
            .iter()
            .map(|s| views::SourceView {
                episode: s.episode.clone(),
                kind: s.kind.clone(),
                at: fmt_ts(s.occurred_at),
            })
            .collect(),
        uncertainty: uncertainty_for(data),
    };
    views::render_entity(&brief)
}

/// Map store data to the view model and render Markdown for a space brief.
pub(crate) fn render_space_brief(data: &oxibrain_store::brief::SpaceBriefData) -> String {
    use oxibrain_views as views;
    let brief = views::SpaceBrief {
        space_name: data.space_name.clone(),
        stats: views::SpaceStatsView {
            episodes: data.stats.episodes,
            entities: data.stats.entities,
            statements: data.stats.statements,
            contradictions: data.stats.contradictions,
        },
        top_entities: data
            .top_entities
            .iter()
            .map(|e| views::EntityLink {
                surface: e.surface.clone(),
                entity_id: e.entity_id.clone(),
                predicate_count: e.predicate_count,
            })
            .collect(),
    };
    views::render_space(&brief)
}

/// Map store data to the view model and render Markdown for a topic brief.
pub(crate) fn render_topic_brief(data: &oxibrain_store::brief::TopicBriefData) -> String {
    use oxibrain_views as views;
    let brief = views::TopicBrief {
        topic: data.topic.clone(),
        matched_entities: data
            .matched_entities
            .iter()
            .map(|e| views::EntityLink {
                surface: e.surface.clone(),
                entity_id: e.entity_id.clone(),
                predicate_count: e.predicate_count,
            })
            .collect(),
    };
    views::render_topic(&brief)
}

fn uncertainty_for(
    data: &oxibrain_store::brief::EntityBriefData,
) -> oxibrain_views::UncertaintyView {
    let contradicted = data
        .beliefs
        .iter()
        .filter(|b| b.status == "contradicted")
        .count();
    let single_source = data
        .beliefs
        .iter()
        .filter(|b| b.episodes <= 1 && b.affirm <= 1)
        .count();
    let note = if contradicted > 0 {
        format!("{contradicted} belief(s) contradicted — treat as unresolved")
    } else if single_source > 0 {
        format!("{single_source} belief(s) from a single source")
    } else {
        String::new()
    };
    oxibrain_views::UncertaintyView {
        contradicted,
        single_source,
        note,
    }
}

/// Format a timestamp as `YYYY-MM-DD`; open intervals (TIME_MIN/TIME_MAX)
/// render as empty.
fn fmt_ts(t: oxibrain_ports::Timestamp) -> String {
    if t.is_min() || t.is_max() {
        String::new()
    } else {
        oxibrain_core::short_ts(t)
    }
}
