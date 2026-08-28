//! The agent-facing op registry (ADR-012, `doc/spec/agent-first-cli-v1.md`).
//!
//! One `OpSpec` per operation. MCP `tools/list`, the CLI `oxibrain <op>`
//! dispatch, `oxibrain schema`, and the generated SKILL.md all derive from
//! this registry — there are no hand-maintained schema duplicates.
//!
//! Phase 1 keeps the schemas static and **byte-identical** to the v2.12
//! hand-written catalogue (guarded by the golden fixture test). The P3
//! cutover makes `schema` registry-aware (live predicate enums), moves the
//! tool set 15 → 14, and makes `space` required.

use serde_json::{Value, json};

/// Declared capability names. Metadata through P5; enforcement
/// (capability-filtered listings) lands in P6.
pub const CAP_READ: &str = "Read";
pub const CAP_INGEST: &str = "Ingest";
pub const CAP_WRITE: &str = "Write";
pub const CAP_REDACT: &str = "Redact";

/// One agent-facing operation: the single source of truth for the MCP tool
/// catalogue, the CLI op dispatch, `oxibrain schema`, and the SKILL.md
/// generator.
pub struct OpSpec {
    pub name: &'static str,
    /// The paragraph an agent reads in `tools/list`. This is contract text:
    /// changes are surface changes and reviewed like schema changes.
    pub summary: &'static str,
    /// JSON Schema for the op payload. Becomes registry-aware (live
    /// predicate enums for `declare`) at P3.
    pub schema: fn() -> Value,
    pub caps: &'static [&'static str],
    /// Mutating ops participate in the dry-run/plan-token rails (P6).
    pub mutating: bool,
}

// ── per-op schemas (bodies verbatim from the v2.12 hand-written catalogue) ──

fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "The search query text." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." },
            "mode": { "type": "string", "enum": ["hybrid","lexical","lexical-vector","graph","community"], "description": "Retrieval mode (default: hybrid)." },
            "planes": { "type": "array", "items": { "type": "string", "enum": ["memory","documents"] }, "description": "Planes to search (default: both)." },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum results per plane (default: 20)." },
            "as_of": { "type": "integer", "description": "Valid-time instant (millis since epoch). Only beliefs true at this instant are returned (default: now)." },
            "known_at": { "type": "integer", "description": "Transaction-time instant (millis since epoch). Only beliefs recorded by this instant are returned (default: now)." },
            "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence floor (default: 0)." }
        },
        "required": ["query"]
    })
}

fn recall_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "What information to assemble." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." },
            "token_budget": { "type": "integer", "minimum": 1, "description": "Maximum tokens for the assembled context (default: 3000)." }
        },
        "required": ["query"]
    })
}

fn brief_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_kind": { "type": "string", "enum": ["entity", "space", "topic"], "description": "Which brief to render. Default: entity." },
            "entity_id": { "type": "string", "description": "Required when target_kind=entity. The entity's content-derived ID." },
            "topic": { "type": "string", "description": "Required when target_kind=topic. A keyword to match against entity surface forms (case-insensitive substring)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": []
    })
}

fn navigate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "from": { "type": "string", "description": "The view/page the link came from (e.g. an entity:// id)." },
            "link": { "type": "string", "description": "The link to follow (entity://<id> or a raw entity id)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": ["from", "link"]
    })
}

fn ingest_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "content": { "type": "string", "description": "The text to ingest." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." },
            "source_path": { "type": "string", "description": "Optional source label, e.g. a file path (default: mcp)." },
            "extract": { "type": "boolean", "description": "If true, extract claims via client sampling immediately (default: false)." },
            "trust": { "type": "string", "enum": ["trusted","semi_trusted","untrusted"], "description": "Requested trust tier. Requires trusted_ingest capability for 'trusted'. Default: trusted (parity with the note path until the policy engine lands)." }
        },
        "required": ["content"]
    })
}

fn declare_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "declaration_json": { "type": "string", "description": "Canonical declaration JSON (op = add_statement | merge | retract)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": ["declaration_json"]
    })
}

fn why_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "statement_id": { "type": "string", "description": "The statement ID." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": ["statement_id"]
    })
}

fn contradictions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        }
    })
}

fn stats_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        }
    })
}

fn traverse_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "start": { "type": "array", "items": { "type": "string" }, "description": "Entity IDs to start from (at least one required)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." },
            "depth": { "type": "integer", "minimum": 1, "description": "Max traversal depth (default: 3)." },
            "max_nodes": { "type": "integer", "minimum": 1, "description": "Max nodes to return (default: 256)." },
            "direction": { "type": "string", "enum": ["out","in","both"], "description": "Edge direction (default: both)." },
            "valid_at": { "type": "integer", "description": "Valid-time instant (millis since epoch). Walk the graph as believed at this instant (default: now)." },
            "min_confidence": { "type": "number", "minimum": 0, "maximum": 1, "description": "Confidence floor for edges (default: 0)." }
        },
        "required": ["start"]
    })
}

fn review_merges_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "section": { "type": "string", "enum": ["merges", "failures", "sources"], "description": "What to list (default: merges)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        }
    })
}

fn remember_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "content": { "type": "string", "description": "The fact or note to remember." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." },
            "source_path": { "type": "string", "description": "Optional source label (default: remember)." },
            "trust": { "type": "string", "enum": ["trusted","semi_trusted","untrusted"], "description": "Requested trust tier. Requires trusted_ingest capability for 'trusted'. Default: trusted (parity with the note path until the policy engine lands)." }
        },
        "required": ["content"]
    })
}

fn retract_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "statement_id": { "type": "string", "description": "Statement to retract — takes precedence over subject/predicate/object." },
            "subject": { "type": "object", "description": "Entity ref: {\"surface\":\"...\",\"type\":\"...\"} (legacy path).", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} } },
            "predicate": { "type": "string", "description": "Predicate name (legacy path)." },
            "object": { "type": "object", "description": "Entity or literal object (legacy path).", "properties": { "kind": {"type":"string","enum":["entity","literal"]} } },
            "episode": { "type": "string", "description": "Originating episode id (audit context)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": []
    })
}

fn merge_entities_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "loser": { "type": "object", "description": "Entity to merge away: {\"surface\":\"...\",\"type\":\"...\"}", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} }, "required": ["surface","type"] },
            "winner": { "type": "object", "description": "Entity to keep: {\"surface\":\"...\",\"type\":\"...\"}", "properties": { "surface": {"type":"string"}, "type": {"type":"string"} }, "required": ["surface","type"] },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": ["loser", "winner"]
    })
}

fn redact_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_kind": { "type": "string", "enum": ["episode","entity","predicate"], "description": "What to redact." },
            "target_id": { "type": "string", "description": "Episode ID, entity ID, or 'entity_id/predicate' for predicate kind." },
            "reason": { "type": "string", "description": "Audit reason (default: 'mcp redact')." },
            "dry_run": { "type": "boolean", "description": "Preview the closure without modifying anything (default: false)." },
            "space": { "type": "string", "description": "Space name (default: the configured default space)." }
        },
        "required": ["target_kind", "target_id"]
    })
}

// ── the registry (order is the tools/list order) ────────────────────────────

pub const OPS: &[OpSpec] = &[
    OpSpec {
        name: "search",
        summary: "Search the brain across two planes and return the envelope {memory, documents, freshness}. Memory-plane hits are entity targets: entity_id, entity_surface, entity_type, score, snippet. Documents-plane hits carry verbatim text slices with their doc:// provenance (root, locator, revision, ordinal). as_of (valid time) and min_confidence filter beliefs; a belief that is retracted or contradicted at as_of is excluded.",
        schema: search_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "recall",
        summary: "Assemble context for a query within a token budget — the per-turn call for agents. Returns layered context: profile, high-salience beliefs (with subjects, validity, support), query neighborhood, summaries with their sources, and recent episodes.",
        schema: recall_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "brief",
        summary: "Render a page as Markdown with followable links. Three target kinds: `entity` (an entity page: identity, aliases, current beliefs, contradictions, neighbours, timeline, sources — the M9 §9.2 brief); `space` (counts + top entities); `topic` (keyword search over entity surfaces). The target_kind discriminator is purely additive; existing callers using only `entity_id` keep working.",
        schema: brief_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "navigate",
        summary: "Follow a followable link from a rendered page to another entity page. Returns the target's brief.",
        schema: navigate_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "ingest",
        summary: "Ingest text content as a new Primary episode. Set extract:true to trigger realtime extraction via client sampling (§12.3) — the server asks the client's model to extract claims. Requires the Sample capability on authenticated sessions.",
        schema: ingest_schema,
        caps: &[CAP_INGEST],
        mutating: false,
    },
    OpSpec {
        name: "declare",
        summary: "Declare a statement deterministically (no LLM). Takes a declaration JSON: {op, subject, predicate, object, polarity, valid_from, valid_to}. Writes a Declaration episode.",
        schema: declare_schema,
        caps: &[CAP_WRITE],
        mutating: true,
    },
    OpSpec {
        name: "why",
        summary: "Get provenance for a statement — supporting/denying assertions with confidence breakdown, extractors, and source episodes.",
        schema: why_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "contradictions",
        summary: "List all contradicted statements in a space — statements with both affirming and denying support.",
        schema: contradictions_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "stats",
        summary: "Aggregate counts for a space: episodes, entities, statements, and contradicted statements.",
        schema: stats_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "traverse",
        summary: "Bounded subgraph traversal from a set of start entities. Returns nodes and edges within the depth/node budget. The graph is belief-filtered: retracted and contradicted edges are excluded (valid_at filters valid time). Useful for multi-hop recall (ToG driver).",
        schema: traverse_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "review_merges",
        summary: "Console data tool. `section` selects what to list: `merges` (default) — entity merge records, by whom (rule/user/import), and when; `failures` — extraction failures (episode, extractor, raw response, errors); `sources` — registered sources (name, kind, mode, claims). All sections return JSON arrays.",
        schema: review_merges_schema,
        caps: &[CAP_READ],
        mutating: false,
    },
    OpSpec {
        name: "remember",
        summary: "One-shot ingest + sync extraction for short user facts. Ingests text as a Primary episode and immediately extracts claims via client sampling. Requires the Sample capability on authenticated sessions.",
        schema: remember_schema,
        caps: &[CAP_WRITE],
        mutating: true,
    },
    OpSpec {
        name: "retract",
        summary: "Retract a statement. Prefer the statement_id form — the declaration is rebuilt from the stored statement (no surfaces/types needed); this retracts ALL assertions of the statement. The subject/predicate/object form is the legacy resubmission path. Creates a Declaration episode.",
        schema: retract_schema,
        caps: &[CAP_WRITE],
        mutating: true,
    },
    OpSpec {
        name: "merge_entities",
        summary: "Merge two entities: the loser is redirected to the winner. Creates a Declaration episode. Both refs use surface form + type.",
        schema: merge_entities_schema,
        caps: &[CAP_WRITE],
        mutating: true,
    },
    OpSpec {
        name: "redact",
        summary: "Destructive: remove episodes, entities, or predicates from the brain. Writes audit first, then tombstones. Use dry_run to preview the closure first.",
        schema: redact_schema,
        caps: &[CAP_REDACT],
        mutating: true,
    },
];

/// The registry, in tools/list order.
pub fn ops() -> &'static [OpSpec] {
    OPS
}

/// Look up one op by name.
pub fn find(name: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|op| op.name == name)
}

/// The `tools/list` payload. Byte-identical to the v2.12 hand-written
/// catalogue (ADR-012 migration guard: the golden fixture test).
pub fn tools_list() -> Value {
    json!({
        "tools": OPS
            .iter()
            .map(|op| {
                json!({
                    "name": op.name,
                    "description": op.summary,
                    "inputSchema": (op.schema)(),
                })
            })
            .collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry's first output must be byte-identical to the
    /// pre-refactor hand-written catalogue (ADR-012 migration guard). The
    /// fixture was blessed from `oxibrain-mcp`'s catalogue before the move
    /// and is never hand-edited afterwards.
    #[test]
    fn tools_list_matches_blessed_fixture() {
        let generated = serde_json::to_string_pretty(&tools_list()).unwrap();
        let fixture = include_str!("../tests/golden_tools_list.json");
        assert_eq!(generated.trim(), fixture.trim());
    }

    #[test]
    fn registry_has_fifteen_unique_ops_with_schemas() {
        let list = tools_list();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(
            tools.len(),
            15,
            "P1 keeps the current 15; the P3 cutover moves to 14"
        );
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "tool names must be unique");
        for op in ops() {
            let tool = tools
                .iter()
                .find(|t| t["name"] == op.name)
                .unwrap_or_else(|| panic!("{} missing from tools/list", op.name));
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["description"], op.summary);
        }
    }

    #[test]
    fn find_rejects_unknown_and_finds_known() {
        assert!(find("search").is_some());
        assert!(find("not-an-op").is_none());
    }
}
