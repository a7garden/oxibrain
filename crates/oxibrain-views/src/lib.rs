//! oxibrain-views — rendered pages over projection data (ARCHITECTURE.md §14).
//!
//! Views are **pure renderers**: they take plain, already-fetched data and
//! return Markdown. They never store anything and never touch SQLite — the
//! §18 rule-4 boundary is satisfied structurally: this crate has no
//! dependencies, so it *cannot* reach the database.
//!
//! `brief(entity|topic|space)` renders a page with followable links;
//! `navigate(from, link)` follows a link to another page. Determinism
//! (§14.2): `render` is a pure function of its input, and every list is
//! sorted by a stable key, so `brief(e)` twice on an unchanged ledger is equal.

use std::fmt::Write as _;

/// The entity id + surface the followable links in a rendered page point at.
/// A link is rendered as `[surface](entity://<id>)`; `navigate` parses it.
pub const LINK_SCHEME: &str = "entity://";

/// Render a link to an entity page in the followable-link format (§14.1).
pub fn link(surface: &str, entity_id: &str) -> String {
    format!("[{surface}]({LINK_SCHEME}{entity_id})")
}

/// Parse a followable link back to the target entity id, if it is an entity
/// link. Returns `None` for a non-link (e.g. a literal or bare id).
pub fn parse_entity_link(link: &str) -> Option<&str> {
    link.strip_prefix(LINK_SCHEME)
}

// ── View model ─────────────────────────────────────────────────────────────

/// A rendered entity page (§14.1). All fields are plain values — already
/// fetched and formatted by the facade — so the renderer is pure.
#[derive(Debug, Clone, Default)]
pub struct EntityBrief {
    /// Canonical surface name (the page title).
    pub surface: String,
    /// Entity type (Person, Organization, …).
    pub ty: String,
    /// Alias surfaces, excluding the canonical one.
    pub aliases: Vec<String>,
    /// Current beliefs with validity, confidence and support.
    pub beliefs: Vec<BeliefView>,
    /// Contradicted statements, with both provenances.
    pub contradictions: Vec<ContradictionView>,
    /// Connected entities, as followable links.
    pub neighbours: Vec<NeighbourView>,
    /// Timeline change points.
    pub timeline: Vec<TimelineView>,
    /// Source episodes backing the beliefs.
    pub sources: Vec<SourceView>,
    /// Derived uncertainty summary (§13.1).
    pub uncertainty: UncertaintyView,
}

#[derive(Debug, Clone)]
pub struct BeliefView {
    pub predicate: String,
    /// Rendered object: surface name for entity objects, literal otherwise.
    pub object: String,
    /// Present when the object is an entity (→ followable link).
    pub object_entity: Option<String>,
    /// `YYYY-MM-DD` valid-from; empty string = open.
    pub valid_from: String,
    /// `YYYY-MM-DD` valid-to; empty string = open.
    pub valid_to: String,
    pub confidence: f32,
    pub affirm: u32,
    pub deny: u32,
    pub episodes: u32,
    /// active | superseded | contradicted | retracted
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct ContradictionView {
    pub predicate: String,
    pub object: String,
    pub affirm_episodes: Vec<String>,
    pub deny_episodes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NeighbourView {
    pub surface: String,
    pub entity: String,
    pub predicate: String,
    /// `out` = this entity is the subject; `in` = this entity is the object.
    pub direction: String,
}

#[derive(Debug, Clone)]
pub struct TimelineView {
    /// `YYYY-MM-DD` of the change point.
    pub at: String,
    pub predicate: String,
    pub object: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct SourceView {
    pub episode: String,
    pub kind: String,
    pub at: String,
}

#[derive(Debug, Clone, Default)]
pub struct UncertaintyView {
    pub contradicted: usize,
    pub single_source: usize,
    pub note: String,
}

// ── Renderer ──────────────────────────────────────────────────────────────

/// Render an entity brief to Markdown. Pure and deterministic: every list is
/// sorted by a stable key before rendering, so input order does not matter.
pub fn render_entity(brief: &EntityBrief) -> String {
    let mut out = String::new();

    // Header: canonical surface + type.
    let _ = writeln!(out, "# {} ({})", brief.surface, brief.ty);
    if !brief.aliases.is_empty() {
        let mut aliases = brief.aliases.clone();
        aliases.sort();
        let _ = writeln!(out, "**Aliases:** {}", aliases.join(", "));
    }
    out.push('\n');

    // Beliefs.
    if !brief.beliefs.is_empty() {
        out.push_str("## Beliefs\n\n");
        let mut beliefs = brief.beliefs.clone();
        beliefs.sort_by(|a, b| {
            (&a.predicate, &a.object)
                .cmp(&(&b.predicate, &b.object))
        });
        for b in &beliefs {
            let obj = match &b.object_entity {
                Some(id) => link(&b.object, id),
                None => format!("`{}`", b.object),
            };
            let validity = match (b.valid_from.is_empty(), b.valid_to.is_empty()) {
                (false, false) => format!("{} → {}", b.valid_from, b.valid_to),
                (false, true) => format!("since {}", b.valid_from),
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "- **{}** {} — {:.2} · {} affirm / {} deny ({} ep) · {}{}",
                b.predicate,
                obj,
                b.confidence,
                b.affirm,
                b.deny,
                b.episodes,
                b.status,
                if validity.is_empty() {
                    String::new()
                } else {
                    format!(" · {validity}")
                }
            );
        }
        out.push('\n');
    }

    // Contradictions.
    if !brief.contradictions.is_empty() {
        out.push_str("## Contradictions\n\n");
        let mut cs = brief.contradictions.clone();
        cs.sort_by(|a, b| (&a.predicate, &a.object).cmp(&(&b.predicate, &b.object)));
        for c in &cs {
            let _ = writeln!(
                out,
                "- **{}** {} — affirmed by {} · denied by {}",
                c.predicate,
                c.object,
                join_refs(&c.affirm_episodes),
                join_refs(&c.deny_episodes)
            );
        }
        out.push('\n');
    }

    // Neighbours — followable links (§14.1).
    if !brief.neighbours.is_empty() {
        out.push_str("## Neighbours\n\n");
        let mut ns = brief.neighbours.clone();
        ns.sort_by(|a, b| {
            (&a.surface, &a.predicate, &a.direction).cmp(&(
                &b.surface,
                &b.predicate,
                &b.direction,
            ))
        });
        for n in &ns {
            let dir = if n.direction == "in" { "←" } else { "→" };
            let _ = writeln!(out, "- {} {} {}", link(&n.surface, &n.entity), n.predicate, dir);
        }
        out.push('\n');
    }

    // Timeline.
    if !brief.timeline.is_empty() {
        out.push_str("## Timeline\n\n");
        let mut ts = brief.timeline.clone();
        ts.sort_by(|a, b| {
            (&a.at, &a.predicate, &a.object).cmp(&(&b.at, &b.predicate, &b.object))
        });
        for t in &ts {
            let _ = writeln!(out, "- {}: **{}** {} ({})", t.at, t.predicate, t.object, t.status);
        }
        out.push('\n');
    }

    // Sources.
    if !brief.sources.is_empty() {
        out.push_str("## Sources\n\n");
        let mut ss = brief.sources.clone();
        ss.sort_by(|a, b| (&a.episode, &a.kind).cmp(&(&b.episode, &b.kind)));
        for s in &ss {
            let _ = writeln!(out, "- `{}` ({}, {})", s.episode, s.kind, s.at);
        }
        out.push('\n');
    }

    // Uncertainty.
    let u = &brief.uncertainty;
    if u.contradicted > 0 || u.single_source > 0 || !u.note.is_empty() {
        out.push_str("## Uncertainty\n\n");
        let _ = writeln!(out, "- contradicted: {}", u.contradicted);
        let _ = writeln!(out, "- single-source: {}", u.single_source);
        if !u.note.is_empty() {
            let _ = writeln!(out, "- {}", u.note);
        }
    }

    out
}

fn join_refs(ids: &[String]) -> String {
    let mut v = ids.to_vec();
    v.sort();
    v.iter().map(|s| format!("`{s}`")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> EntityBrief {
        EntityBrief {
            surface: "Alice".into(),
            ty: "Person".into(),
            aliases: vec!["Alicia".into(), "A. Smith".into()],
            beliefs: vec![BeliefView {
                predicate: "works_on".into(),
                object: "Project X".into(),
                object_entity: Some("proj-x".into()),
                valid_from: "2023-01-01".into(),
                valid_to: String::new(),
                confidence: 0.9,
                affirm: 3,
                deny: 0,
                episodes: 2,
                status: "active".into(),
            }],
            contradictions: vec![],
            neighbours: vec![NeighbourView {
                surface: "Bob".into(),
                entity: "bob".into(),
                predicate: "works_with".into(),
                direction: "out".into(),
            }],
            timeline: vec![],
            sources: vec![SourceView {
                episode: "ep1".into(),
                kind: "note".into(),
                at: "2023-01-01".into(),
            }],
            uncertainty: UncertaintyView {
                contradicted: 0,
                single_source: 1,
                note: String::new(),
            },
        }
    }

    #[test]
    fn render_is_deterministic_and_has_links() {
        let a = render_entity(&brief());
        let b = render_entity(&brief());
        assert_eq!(a, b, "render must be deterministic");
        assert!(a.contains("# Alice (Person)"));
        assert!(a.contains("**Aliases:** A. Smith, Alicia"));
        assert!(a.contains("[Project X](entity://proj-x)"));
        assert!(a.contains("[Bob](entity://bob)"));
        assert!(a.contains("single-source: 1"));
    }

    #[test]
    fn link_round_trips() {
        let l = link("Bob", "abc123");
        assert_eq!(l, "[Bob](entity://abc123)");
        assert_eq!(parse_entity_link("entity://abc123"), Some("abc123"));
        assert_eq!(parse_entity_link("literal"), None);
    }
}
