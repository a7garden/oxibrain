# oxibrain M1 — Knowledge Core Design Spec

> **Date:** 2026-08-11
> **Authority:** `doc/DESIGN.md` v1.0 (§§3, 5.4–5.8, 6, 7.4, 8, 14.3, 17). This spec
> scopes and concretizes M1. Where this spec and DESIGN.md disagree, DESIGN.md wins
> unless this spec explicitly records a deviation (§10).
> **Predecessor:** M0 Foundation (complete).
> **Status:** Design. Drives the M1 implementation plan.

---

## 1. Goal

The fully deterministic knowledge core: a predicate registry with a representative
ontology, entity identity and lexical resolution, the temporal fold with
contradiction handling, declaration-driven projection, and byte-identical
reprojection. **No LLM, no embeddings, no network.**

## 2. M1 Exit Criteria (DESIGN §17)

1. Fold property tests pass (§10.1).
2. Reprojection determinism holds byte-identically (§9.2).
3. A hand-built graph answers `as_of` and contradiction queries (§12).

---

## 3. Scope

### 3.1 In M1

| Capability | Detail |
|---|---|
| Predicate registry | `PredicateDef` type + `core/v1` ontology (~12 predicates, §5.2) seeded to DB |
| Knowledge types | Entity, EntityKey, EntityMerge, Statement, Assertion, Mention, Belief + enums |
| Content-derived ids | entity_id, entity_key_id, statement_id, assertion_id, mention_id, merge_id (§5.6 of DESIGN) |
| Temporal fold | `fold(def, group, at) → Vec<Belief>` — pure, group-level, property-tested |
| Contradiction handling | `BeliefStatus::Contradicted` — system never silently picks a winner |
| Identity & resolution | normalize → exact-key → Jaro-Winkler + type gate + graph-context → dual thresholds → merge candidates |
| Merge & split | `EntityMerge` data, path-compressed redirect, `split` undoes |
| Declaration episodes | `add_statement`, `merge`, `retract` → Declaration episode → canonical JSON content |
| Projection pipeline | declaration → episode → resolve → assertions → fold → beliefs (one transaction) |
| Reprojection | drop projection tables → replay ledger in canonical order → byte-identical |
| Read queries | `beliefs_for_entity`, `beliefs_as_of`, `contradictions` |
| Facade API | `Brain::declare`, `Brain::merge`, `Brain::beliefs`, `Brain::contradictions`, `Brain::reproject` |

### 3.2 Deferred to M2/M3

| Deferred | Milestone | Why |
|---|---|---|
| Embedding-based resolution scoring | M3 | No embeddings in M1 (§8.2: secondary signal for names) |
| Full ~40-predicate ontology | M3 | ~12 covers all invalidation branches |
| Extraction pipeline (mentions from Primary episodes) | M3 | No LLM in M1 |
| FTS5 / sqlite-vec / HNSW indexes | M2 | Retrieval engine |
| Traversal, RRF, communities, `assemble_context` | M2 | Retrieval engine |
| `timeline`, `diff`, `why --dropped` | M2 | Read-side features |
| Confidence calibration (eval harness) | M3 | No eval data yet |
| Spaces, scopes, tokens, audit, trust tiers, redaction | M4 | Security/tenancy |

### 3.3 What "no extraction" means for M1

Primary episodes are ingested (M0) but produce **no assertions** — extraction is
M3. Only **Declaration episodes** produce assertions, entities, and merges. The
reprojection test therefore replays Declaration episodes exclusively. The fold,
resolution, and reprojection machinery are fully built and tested via declarations
and synthetic proptest data; M3 wires them to extraction output.

---

## 4. Architecture

### 4.1 Layering

```
oxibrain-core      pure logic: types, ids, fold, resolution, registry defs
    ↑
oxibrain-store     persistence + projection orchestration (only rusqlite user)
    ↑
oxibrain           facade: async Brain wrapping store ops as WriteOps
```

**Core is pure.** No `rusqlite`, no I/O. The fold, resolution scoring, id
derivation, and interval algebra are pure functions property-tested in isolation.

**Store orchestrates projection.** The declaration→projection pipeline runs
inside a single write transaction on the writer actor's connection: write
episode → resolve entities (calling core) → write assertions/mentions → re-fold
group (calling core) → update beliefs. This respects P7 (no LLM/embedding in a
transaction — vacuous in M1) and the single-writer model (P8).

**Facade is thin.** Each public method wraps a store function in a `WriteOp`
(closure on `&Connection`) and submits it to the `WriterActor`. Reads go through
the `ReaderPool`.

### 4.2 Module map

```
oxibrain-core/src/
  lib.rs              index — re-exports
  types.rs            EXISTING — ledger types (Space, Episode, SourceRef, TrustTier, EpisodeKind)
  knowledge.rs        NEW — Entity, EntityKey, EntityMerge, Statement, Assertion, Mention,
                        Belief, Object, TypedValue, Polarity, BeliefStatus, Support,
                        ResolutionMethod, KeyOrigin, MergeDecision
  registry.rs         NEW — PredicateDef, ObjectKind, Cardinality, Temporality,
                        Invalidation, LiteralType, EntityTypeRef, core_v1() → &[PredicateDef]
  fold.rs             NEW — StatementEntry, fold(def, group, at) → Vec<Belief>
  interval.rs         NEW — Interval, merge_overlapping, clip, overlaps
  resolution.rs       NEW — normalize, jaro_winkler, score, Decision, ResolutionConfig
  id.rs               EXTEND — entity_id, entity_key_id, statement_id, assertion_id,
                        mention_id, entity_merge_id
  canonical.rs        EXISTING — (extend for TypedValue canonicalization if needed)

oxibrain-store/src/
  lib.rs              EXISTING
  ledger.rs           EXISTING — episode/space CRUD
  knowledge.rs        NEW — entity/key/merge/statement/assertion/mention/belief CRUD (&Connection)
  registry.rs         NEW — load predicates table → HashMap<String, PredicateDef>;
                        seed core/v1 from Rust const array
  project.rs          NEW — project_declaration(conn, decl, clock): episode → resolve →
                        assertions → fold group → update beliefs (one transaction)
  reproject.rs        NEW — drop projection tables → replay episodes by seq → re-derive
  query.rs            NEW — beliefs_for_entity, beliefs_as_of, contradictions, get_entity
  migration.rs        EXTEND — v2 step (mentions FK fix + predicate seeding)
  migrations/
    v2.sql            NEW — mentions table fix (DROP/CREATE without bad FK)
  schema.rs           EXTEND — bump LEDGER_SCHEMA_VERSION to 2

oxibrain/src/
  lib.rs              EXTEND — declare(), merge(), split(), retract(),
                        beliefs(), beliefs_as_of(), contradictions(), reproject()
  config.rs           EXISTING — (add ResolutionConfig if needed)
```

### 4.3 New workspace dependency

`strsim = "0.11"` added to `[workspace.dependencies]` and pulled into
`oxibrain-core` for Jaro-Winkler distance. Well-maintained, zero-dep, ~hand-rolled
equivalent. Avoids a from-scratch implementation that could have edge-case bugs.

---

## 5. Data types

### 5.1 core/v1 ontology (core/registry.rs)

**Entity types** (10 — all of DESIGN §5.5, cheap to define):

`Person`, `Organization`, `Project`, `Concept`, `Place`, `Event`, `Artifact`,
`Document`, `Code`, `Task`.

**Predicates** (~12 — covering every invalidation × cardinality × temporality
branch):

| # | name | subject → object | card. | invalid. | temp. | sym | inverse | exercises |
|---|---|---|---|---|---|---|---|---|
| 1 | `employed_by` | Person → Organization | Func | Supersede | Interval | — | — | classic supersession |
| 2 | `works_on` | Person → Project | Multi | Coexist | Interval | — | — | multi-value coexist |
| 3 | `born_in` | Person → Place | Func | Supersede | Static | — | — | Static → contradiction |
| 4 | `full_name` | Person → Text | Func | Supersede | Interval | — | — | literal object, name change |
| 5 | `died_at` | Person → DateTime | Func | ExplicitOnly | Static | — | — | ExplicitOnly + Static |
| 6 | `knows` | Person → Person | Multi | Coexist | Interval | yes | — | symmetric |
| 7 | `member_of` | Person → Organization | Multi | Coexist | Interval | — | — | |
| 8 | `part_of` | Organization → Organization | Func | Supersede | Static | — | — | self-referential type |
| 9 | `located_in` | Place → Place | Func | Supersede | Static | — | — | self-referential type |
| 10 | `has_skill` | Person → Concept | Multi | Coexist | Interval | — | — | |
| 11 | `created_by` | Artifact → Person | Func | Supersede | Static | — | `author_of` | inverse_of |
| 12 | `aliases` | Person → Text | Multi | Coexist | Static | — | — | Static + MultiValued (coexist OK) |

**Coverage check:** Functional/Supersede/Interval ✓(1,4), Functional/Supersede/Static
✓(3,8,9,11), Functional/ExplicitOnly/Static ✓(5), MultiValued/Coexist/Interval
✓(2,6,7,10), MultiValued/Coexist/Static ✓(12), symmetric ✓(6), inverse_of ✓(11),
literal objects ✓(4 Text, 5 DateTime, 12 Text), entity objects ✓(9 predicates).

### 5.2 Registry types

```rust
/// EntityTypeRef — a string name from the ontology ("Person", "Project", ...).
pub type EntityTypeRef = String;
/// PredicateRef — a string name ("employed_by", "works_on", ...).
pub type PredicateRef = String;

pub enum ObjectKind {
    Entity(EntityTypeRef),
    Literal(LiteralType),
    Enum(Vec<String>),
}

pub enum LiteralType {
    Text, Date, DateTime,
    Quantity { unit: String },
    Number, Bool,
}

pub enum Cardinality { Functional, MultiValued }
pub enum Temporality { Static, Interval, Point }
pub enum Invalidation { Supersede, Coexist, ExplicitOnly }

pub struct PredicateDef {
    pub name: PredicateRef,
    pub object_kind: ObjectKind,
    pub subject_types: Vec<EntityTypeRef>,
    pub cardinality: Cardinality,
    pub temporality: Temporality,
    pub invalidation: Invalidation,
    pub symmetric: bool,
    pub inverse_of: Option<PredicateRef>,
    pub description: String,
    pub examples: Vec<String>,
    pub deprecated_by: Option<PredicateRef>,
}

/// The shipped ontology. Serialize each entry to canonical JSON, insert into
/// predicates table with major_version=1, minor_version=0.
pub fn core_v1() -> &'static [PredicateDef] { &CORE_V1 }
```

### 5.3 Knowledge types (core/knowledge.rs)

```rust
pub type EntityId = String;
pub type EntityKeyId = String;
pub type StatementId = String;
pub type AssertionId = String;
pub type MentionId = String;

/// Opaque, permanent identity. Names live in EntityKey, not here (P3).
pub struct Entity {
    pub id: EntityId,
    pub space: String,
    pub ty: EntityTypeRef,
    pub canonical_key: Option<EntityKeyId>,
    pub created_at: Timestamp,
    pub merged_into: Option<EntityId>,
}

/// A (type, normalized name) handle. Aliases are additional keys on one entity.
pub struct EntityKey {
    pub id: EntityKeyId,
    pub space: String,
    pub entity: EntityId,
    pub ty: EntityTypeRef,
    pub normalized: String,
    pub surface: String,
    pub origin: KeyOrigin,
}

pub enum KeyOrigin { Extracted, UserDeclared, Imported }

pub struct EntityMerge {
    pub id: String,
    pub loser: EntityId,
    pub winner: EntityId,
    pub decided_by: MergeDecision,
    pub provenance: String,       // Declaration episode id
    pub evidence: Vec<MentionId>,
    pub decided_at: Timestamp,
    pub undone_at: Option<Timestamp>,
}

pub enum MergeDecision { Rule { score: f64 }, User, Import }

/// An atemporal proposition. Content-addressed → deduplicated by construction.
pub struct Statement {
    pub id: StatementId,
    pub space: String,
    pub subject: EntityId,
    pub predicate: PredicateRef,
    pub object: Object,
}

pub enum Object {
    Entity(EntityId),
    Literal(TypedValue),
}

pub enum TypedValue {
    Text(String),
    Date(String),         // RFC-3339 date
    DateTime(String),     // RFC-3339 datetime
    Quantity { value: f64, unit: String },
    Number(f64),
    Bool(bool),
    Enum(String),
}

/// "Episode E, via extractor R, claimed S held over I, with confidence c."
pub struct Assertion {
    pub id: AssertionId,
    pub statement: StatementId,
    pub episode: String,           // EpisodeId — provenance, mandatory
    pub extractor: Option<String>, // None = manual declaration
    pub polarity: Polarity,
    pub claimed_from: Timestamp,   // TIME_MIN when unbounded
    pub claimed_to: Timestamp,     // TIME_MAX when still true
    pub confidence: f32,
    pub recorded_at: Timestamp,
    pub retracted_at: Option<Timestamp>,
}

pub enum Polarity { Affirm, Deny }

/// The verbatim text this assertion came from.
pub struct Mention {
    pub id: MentionId,
    pub assertion: AssertionId,
    pub role: MentionRole,
    pub surface: String,
    pub span: (u32, u32),
    pub resolved_to: Option<EntityId>,
    pub method: ResolutionMethod,
}

pub enum MentionRole { Subject, Object }

pub enum ResolutionMethod {
    ExactKey, Alias, Lexical { score: f64 },
    Embedding { score: f64 },  // not produced in M1; type exists for forward compat
    New, User,
}

/// Current-slice cache of the temporal fold. Fully derived.
pub struct Belief {
    pub statement: StatementId,
    pub valid_from: Timestamp,
    pub valid_to: Timestamp,
    pub support: Support,
    pub confidence: f32,
    pub status: BeliefStatus,
}

pub struct Support {
    pub affirm_count: u32,
    pub deny_count: u32,
    pub distinct_episodes: u32,
    pub trust_weights: Vec<(TrustTier, u32)>,  // tier → count of supporting episodes
}

// Support serializes to `beliefs.support_json`. For byte-identical reprojection
// (§9.2), the fold MUST produce trust_weights sorted by TrustTier ordinal
// (Trusted < SemiTrusted < Untrusted). The canonical JSON serializer sorts object
// keys, but Vec element order is significant — the fold controls it deterministically.

pub enum BeliefStatus { Active, Superseded, Contradicted, Retracted }
```

### 5.4 Fold types (core/fold.rs)

```rust
/// A statement and its assertions — input to the fold for one (subject, predicate) group.
pub struct StatementEntry {
    pub statement: Statement,
    pub assertions: Vec<Assertion>,
}
```

---

## 6. The temporal fold

### 6.1 Group-level semantics (deviation D1, §10)

DESIGN §6.3 says "for one statement," but §6.4 defines supersession as acting on
"the same subject+predicate." Since `StatementId = blake3(space, subject,
predicate, object)`, two values for one predicate (`employed_by(Alice, CorpA)`
and `employed_by(Alice, CorpB)`) are **different statements**. A per-statement
fold cannot see that the second must close the first. The fold therefore operates
over the **(subject, predicate) group** — all statements sharing that key.

### 6.2 Cross-object rules

When 2+ objects in the group have affirming assertions:

| cardinality | invalidation | temporality | Behavior |
|---|---|---|---|
| Functional | Supersede | Static | **Contradicted** — a timeless fact has one value |
| Functional | Supersede | Interval / Point | **Superseded** — newer closes older at its `valid_from` |
| Functional | ExplicitOnly | any | **Active** — system never auto-closes; both stay Active until explicit deny |
| Functional | Coexist | any | **Active** — unusual; treat as MultiValued |
| MultiValued | any | any | **Active** — independent per-statement; no cross-object effect |

**Rule for Static + Functional:** any temporality=Static + cardinality=Functional
predicate with 2+ concurrent affirming objects → all overlapping beliefs are
**Contradicted**. The system never picks a winner (DESIGN §6.4).

**Supersession mechanics:** collect all affirming intervals across the group.
Sort by `valid_from`. Walk in order: when an interval for object B starts while
object A's interval is still open (valid_to > B.valid_from), clip A's interval
to `[A.valid_from, B.valid_from)` and mark it **Superseded**. B becomes the
current object. If A and B start at the same `valid_from`, both are
**Contradicted** (ambiguous ordering).

### 6.3 Algorithm

```rust
/// Fold a (subject, predicate) group into current-slice beliefs.
/// Pure function. Property-tested (§9.1).
///
/// `at` is the transaction-time cutoff: only assertions with
/// `recorded_at <= at && (retracted_at.is_none() || retracted_at > at)` are visible.
pub fn fold(def: &PredicateDef, group: &[StatementEntry], at: Timestamp) -> Vec<Belief> {
    // 1. Filter by transaction time.
    // 2. Per-statement: build affirming intervals from affirm assertions;
    //    apply deny assertions to clip them.
    // 3. Merge overlapping/adjacent affirming intervals per object (interval algebra).
    // 4. Apply cross-object rules (§6.2): supersession / contradiction / coexist.
    // 5. Compute support (affirm/deny counts, distinct episodes, trust mix).
    // 6. Compute confidence (§6.5).
    // 7. Set status per belief.
    // 8. Return Vec<Belief> sorted by (statement_id, valid_from).
}
```

**Denial clipping (step 2):** a denial is always about a specific statement
(subject+predicate+object). Its `[claimed_from, claimed_to]` interval specifies
when the denial says it is not true. Subtract the denial interval from the
affirming intervals of that statement. A point denial (`claimed_from ==
claimed_to`) clips from its `recorded_at` — it asserts "this was false as of when
I learned it."

**Interval algebra (core/interval.rs):**

```rust
pub struct Interval { pub start: Timestamp, pub end: Timestamp }

/// Merge overlapping or adjacent intervals into disjoint ones. Sorted by start.
pub fn merge_overlapping(intervals: &mut Vec<Interval>);

/// Clip affirming intervals by a denial interval. Returns the remaining pieces.
pub fn clip(affirming: &[Interval], denial: &Interval) -> Vec<Interval>;

/// True if the intervals share any valid-time point.
pub fn overlaps(a: &Interval, b: &Interval) -> bool;
```

### 6.4 Confidence (DESIGN §6.5, M1 version)

```
confidence = calibrate(extractor) × corroboration × trust × recency
```

- `calibrate(extractor)` — M1: always `1.0` (no eval harness). Declarations
  (`extractor = None`) get `1.0` by convention: "a user statement always outranks
  an extracted one."
- `corroboration` — saturating function of **distinct episodes** affirming.
- `trust` — weighted by trust tier of supporting episodes.
- `recency` — Interval/Point predicates only; Static predicates skip this factor.

The formula is implemented and exercised with synthetic extractors in proptests.
The calibration multiplier arrives in M3 with eval data.

### 6.5 BeliefStatus semantics

| Status | When | Meaning |
|---|---|---|
| Active | Currently affirmed, not superseded or contradicted | "the brain believes this now" |
| Superseded | Closed by a newer affirmation (Functional/Supersede) | "was true, replaced" |
| Contradicted | Conflicting evidence (Static overlap, or same-valid_from collision) | "conflicting claims; needs resolution" |
| Retracted | Supporting assertion was retracted (transaction-time view) | "was believed, then retracted" |

The current-time fold (`at = now`) produces Active, Superseded, Contradicted.
Retracted arises in the transaction-time view: a belief that was active before
`at` but whose sole support was retracted after the original `recorded_at` and
before `at`. The fold filters retracted assertions at step 1, so a belief with no
surviving support simply disappears from the output. Retracted status is set
during incremental belief-cache updates when a retraction removes the last
support (the row is marked Retracted rather than deleted, preserving the audit
trail).

---

## 7. Identity and resolution

### 7.1 M1 pipeline (no embeddings)

```
mention (surface, type, span, episode)
  → normalize           NFKC, casefold, collapse ws, strip honorifics/suffixes per type
  → block               exact entity_keys hit + all keys of same type (lexical candidates)
  → score               weighted: exact_key + alias + jaro_winkler + type_gate(hard) + graph_context
  → decide              ≥ τ_high → link;  ≤ τ_low → new;  between → new + merge candidate
  → record              mention with method + score
```

**Blocking in M1:** no FTS5 index (that is M2). The blocker queries
`entity_keys WHERE type_name = ?` and scores all candidates of the same type.
This is O(n_keys_for_type) — fine for M1's declaration-only scale.

### 7.2 Scoring

```rust
pub struct ResolutionConfig {
    pub tau_high: f64,   // default 0.85
    pub tau_low: f64,    // default 0.55
    pub w_exact: f64,    // 1.0 (binary)
    pub w_alias: f64,    // 0.8 (binary)
    pub w_jw: f64,       // 0.6 (Jaro-Winkler [0,1])
    pub w_graph: f64,    // 0.4 (graph context overlap [0,1])
}

/// score = type_gate × (w_exact·is_exact + w_alias·is_alias + w_jw·jw + w_graph·ctx)
/// type_gate = 0.0 if types disagree (hard reject), 1.0 if they match.
pub fn score(
    candidate: &EntityKey,
    mention_normalized: &str,
    graph_context: f64,
    config: &ResolutionConfig,
) -> f64;
```

- **Exact key** (`is_exact`): candidate.normalized == mention.normalized → 1.0.
- **Alias** (`is_alias`): candidate belongs to an entity that already has this
  surface as another key → 1.0.
- **Jaro-Winkler** (`jw`): `strsim::jaro_winkler(candidate.normalized,
  mention_normalized)`.
- **Graph context** (`ctx`): fraction of shared neighbors (subjects/objects in
  statements) between the candidate entity and the mention's episode context. In
  M1, computed from the statements table (one adjacency lookup per candidate).
- **Type gate**: if the candidate's type != the mention's type → score = 0.0,
  regardless of lexical similarity.

### 7.3 Decision

```rust
pub enum Decision {
    Link(EntityId, ResolutionMethod, f64),  // score ≥ τ_high
    New(EntityId, ResolutionMethod, f64),   // score ≤ τ_low → new entity created
    Candidate(EntityId, EntityId, f64),     // between → new entity + merge candidate
}
```

- `≥ τ_high` → link the mention to the existing entity (method = ExactKey or
  Alias or Lexical{score}).
- `≤ τ_low` → create a new entity with this mention as its first key.
- between → create a new entity AND record a merge candidate
  (`EntityMerge` with `decided_by = Rule{score}`, `undone_at = None`). These
  surface in review tooling (M4). The system never guesses.

### 7.4 Merge and split

**Merge** (`store::project.rs`): writes a Declaration episode, then writes an
`EntityMerge` row and sets `loser.merged_into = winner`. No statements are
rewritten. Query-time follows the `merged_into` chain, path-compressed on read.

**Split** (`store::project.rs`): sets `undone_at` on the `EntityMerge` and
re-runs resolution for affected mentions only, using stored surface forms.
Because every mention retains its verbatim text (P3), this is exact.

**Merge does not trigger re-folding.** The fold always operates on original
EntityIds. When querying beliefs for entity A, the query follows the merge chain
to collect all entities merged into A, then reads beliefs for all of them. This
keeps the fold simple and avoids cascading re-folds.

---

## 8. Declaration → projection

### 8.1 Declaration episode content

A declaration creates a Declaration episode whose `content` is canonical JSON
(sorted keys, compact). The `SourceRef` is `Declaration`. Three operations:

**`add_statement`:**
```json
{
  "op": "add_statement",
  "subject": { "surface": "Alice", "type": "Person" },
  "predicate": "employed_by",
  "object": { "kind": "entity", "surface": "Acme", "type": "Organization" },
  "polarity": "affirm",
  "valid_from": 1704067200000,
  "valid_to": 9223372036854775806
}
```
Literal object variant:
```json
  "object": { "kind": "literal", "literal_type": "date", "value": "1990-05-15" }
```

**`merge`:**
```json
{
  "op": "merge",
  "loser": { "surface": "Alice", "type": "Person" },
  "winner": { "surface": "Alice Smith", "type": "Person" }
}
```

**`retract`:**
```json
{
  "op": "retract",
  "subject": { "surface": "Alice", "type": "Person" },
  "predicate": "employed_by",
  "object": { "kind": "entity", "surface": "Acme", "type": "Organization" },
  "episode": "<episode_id of the assertion to retract>"
}
```

The episode `content_hash` is BLAKE3 over the canonical JSON bytes. The
`EpisodeId` is derived from `(space, content_hash, source_ref, occurred_at)`.
This makes declarations idempotent: re-declaring the same claim is a no-op
(`UNIQUE(space_id, content_hash)`).

### 8.2 Projection pipeline (`store::project::project_declaration`)

```
project_declaration(conn, space, decl_json, clock):
  1. Parse decl_json → Declaration struct.
  2. Build the canonical content, derive content_hash + episode_id.
  3. Insert the Declaration episode (idempotent via UNIQUE constraint).
  4. For add_statement:
     a. Resolve subject mention → EntityId (create if new).
     b. Resolve object (entity mention → EntityId, or literal → TypedValue).
     c. Derive StatementId, insert statement if new (idempotent).
     d. Capture subject + object mentions verbatim.
     e. Derive AssertionId, insert assertion.
     f. Read the full (subject, predicate) group: all statements + assertions.
     g. Call core::fold::fold(def, group, now) → Vec<Belief>.
     h. Delete old beliefs for all statements in the group; insert new beliefs.
  5. For merge:
     a. Resolve loser + winner mentions → EntityIds.
     b. Insert EntityMerge; set loser.merged_into = winner.
  6. For retract:
     a. Find matching assertions (statement + episode).
     b. Set retracted_at = now.
     c. Re-fold the affected group(s).
```

All steps run inside one transaction (the `WriteOp` closure on the writer
actor's connection). No LLM, no network, no embedding — the transaction-spanning
rule (DESIGN §7.2) is vacuously satisfied.

### 8.3 Entity creation determinism

`EntityId = blake3(space, entity_type, first_episode_id, first_span_start)`.

For declarations, the "first mention" is in the declaration episode itself. The
span is a byte offset into the canonical JSON content — deterministic because the
content is canonical. The `first_episode_id` is the declaration episode's
content-derived id. Therefore the same declaration always produces the same
EntityId, whether created incrementally or during reprojection.

---

## 9. Reprojection

### 9.1 Mechanism (`store::reproject`)

```
reproject(conn):
  1. Delete all rows from: beliefs, mentions, assertions, statements,
     entity_merges, entity_keys, entities. (Order respects FK constraints.)
  2. Read all episodes WHERE kind = 'declaration' ORDER BY seq ASC.
  3. For each episode:
     a. Parse content → Declaration.
     b. Call project_declaration(conn, space, content, clock=ingested_at).
        (project_declaration is idempotent; replaying produces identical rows.)
  4. Verify: row counts match expectations; no orphan FKs.
```

Replay order is canonical: `episode.seq ASC`. Within a declaration episode, the
order of entity creation / assertion creation follows the JSON field order
(deterministic because the content is canonical JSON with sorted keys — though
for single-claim declarations this is trivially one operation).

### 9.2 Byte-identical test

The reprojection determinism test (DESIGN §14.3 — the single most valuable test):

```
1. Generate a random sequence of N declarations (proptest).
   - Mix of add_statement (varied predicates, objects, valid intervals),
     merge, and retract.
   - Multiple assertions on the same (subject, predicate) with different objects
     to exercise supersession and contradiction.
2. Apply each declaration through project_declaration (incremental path).
3. Dump all projection tables: SELECT * ORDER BY <primary key>.
   Serialize each row as canonical JSON; concatenate per table.
4. Call reproject (full replay path).
5. Dump again.
6. Assert: the two dumps are byte-identical for every table.
```

The dump includes `beliefs.support_json`, so the Support struct must serialize as
**canonical JSON** (sorted keys) — the fold must produce identical support_json
on both paths. This is guaranteed because the fold is a pure function and the
input (assertions + group) is identical.

---

## 10. Testing strategy

### 10.1 Fold property tests (core/fold.rs, `#[cfg(test)]`)

- **Disjoint and ordered:** for every output, belief intervals for one statement
  are disjoint and sorted by `valid_from`.
- **Retraction monotone:** adding a retraction can only remove or shrink beliefs,
  never add or grow them.
- **Prefix-suffix:** `fold(def, group[0..k], at)` + `fold(def, group[k..], at)` at
  the assertion level is consistent with `fold(def, group, at)` (the fold is
  compositional over assertions within a group, modulo cross-object effects —
  tested by comparing belief sets).
- **Idempotent:** `fold(def, group, at) == fold(def, fold_result_as_group, at)`.
  Running the fold on its own output produces the same beliefs.
- **Supersession:** for Functional/Supersede/Interval, asserting a second object
  after the first's start marks the first Superseded.
- **Contradiction:** for Static/Functional, two concurrent objects → both
  Contradicted.
- **Coexist:** for MultiValued, N objects → N Active beliefs.
- **Denial clipping:** a denial that fully covers an affirming interval removes
  the belief; a partial denial splits it.

### 10.2 Interval algebra property tests (core/interval.rs)

- `merge_overlapping` output is disjoint, sorted, and covers the same total span.
- `clip(a, d)` ⊆ a for any d.
- `overlaps` is symmetric.
- `clip(merge_overlapping(a), d) == merge_overlapping(clip(a, d))` (commutativity
  of merge and clip — or a documented counterexample if they don't commute).

### 10.3 Resolution property/decision tests (core/resolution.rs)

- Exact key match → score 1.0 → Link.
- Type mismatch → score 0.0 → New (hard gate).
- Jaro-Winkler: identical strings → 1.0; completely different → near 0.
- Dual threshold boundary: score exactly τ_high → Link; exactly τ_low → New.
- Merge candidate band: τ_low < score < τ_high → Candidate.

### 10.4 Id derivation tests (core/id.rs, extend existing)

- Same inputs → same id (determinism).
- Different object → different StatementId.
- Different first-mention location → different EntityId.
- Rename (different surface, same entity) → same EntityId.

### 10.5 Integration tests (store)

- **Declaration round-trip:** declare a statement, read it back as a belief.
- **Supersession scenario:** declare employed_by(Alice, Acme), then
  employed_by(Alice, Globex). Assert Acme is Superseded, Globex is Active.
- **Contradiction scenario:** declare born_in(Alice, Seoul), then
  born_in(Alice, Busan). Assert both are Contradicted.
- **Coexist scenario:** declare works_on(Alice, P1), works_on(Alice, P2). Assert
  both Active.
- **Merge scenario:** merge two entities, query beliefs for the winner, assert
  both entities' beliefs are visible.
- **Retraction scenario:** retract an assertion, re-fold, assert belief removed.
- **as_of query:** declare with valid intervals, query beliefs as of a past time.
- **Reprojection determinism:** the byte-identical test (§9.2).

### 10.6 Canonical serialization tests (extend existing)

- TypedValue canonicalization: Number 1.0 == 1, dates in RFC-3339 UTC.
- Declaration content canonicalization: same claim → same bytes regardless of
  field order in the input.

---

## 11. Schema changes (v2 migration)

### 11.1 mentions table FK fix

**Bug in v1.sql:** `id TEXT PRIMARY KEY REFERENCES assertions(id)` — the mention
id FKs to assertions, but `MentionId = blake3(assertion_id, role, span)` ≠
`AssertionId`. The FK is wrong and would reject valid inserts.

**Fix (v2.sql):** drop and recreate the mentions table without the spurious FK
on `id`. The `assertion_id` column retains its FK to assertions.

```sql
-- v2.sql
DROP TABLE IF EXISTS mentions;
CREATE TABLE mentions (
  id           TEXT PRIMARY KEY,
  assertion_id TEXT NOT NULL REFERENCES assertions(id) ON DELETE CASCADE,
  role         TEXT NOT NULL,
  surface      TEXT NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  resolved_to  TEXT,
  method       TEXT NOT NULL
);
CREATE INDEX idx_mention_assert ON mentions(assertion_id);
```

### 11.2 Predicate seeding

No schema change — the `predicates` table already exists (v1.sql). Seeding is a
**data migration** run as a Rust step after `v2.sql`:

```rust
// In migration.rs, after v2.sql:
if new_version >= 2 {
    seed_core_v1(conn)?;  // INSERT OR IGNORE each PredicateDef as canonical JSON
}
```

`seed_core_v1` iterates `core_v1()`, serializes each `PredicateDef` to canonical
JSON, and inserts with `(name, major_version=1, minor_version=0, def_json)`.
`INSERT OR IGNORE` makes it idempotent: re-running on an already-seeded DB is a
no-op.

### 11.3 Version bump

`schema::LEDGER_SCHEMA_VERSION` bumps from `1` to `2`. The migration chain test
extends to cover v1→v2 (open a v1 fixture, run migrations, assert v2 state).

---

## 12. Facade API (oxibrain/src/lib.rs)

```rust
impl Brain {
    /// Declare a statement. Writes a Declaration episode, projects it.
    /// Returns the episode id.
    pub async fn declare(&self, space: &str, claim: DeclareRequest) -> Result<String, BrainError>;

    /// Merge two entities by surface form.
    pub async fn merge(&self, space: &str, loser: EntityRef, winner: EntityRef) -> Result<String, BrainError>;

    /// Retract an assertion.
    pub async fn retract(&self, space: &str, target: RetractRequest) -> Result<(), BrainError>;

    /// Current beliefs for an entity (follows merge chain).
    pub async fn beliefs(&self, space: &str, entity: EntityRef) -> Result<Vec<Belief>, BrainError>;

    /// Beliefs as of a valid-time and/or transaction-time point.
    pub async fn beliefs_as_of(
        &self, space: &str, entity: EntityRef,
        valid_at: Option<Timestamp>, transaction_at: Option<Timestamp>,
    ) -> Result<Vec<Belief>, BrainError>;

    /// All contradicted statements in the space.
    pub async fn contradictions(&self, space: &str) -> Result<Vec<Statement>, BrainError>;

    /// Drop and rebuild the entire projection from the ledger.
    pub async fn reproject(&self) -> Result<(), BrainError>;
}
```

`DeclareRequest`, `EntityRef`, and `RetractRequest` are request types built in
the facade or core. `EntityRef` is `(surface: String, ty: EntityTypeRef)` — the
caller names entities by surface form; the facade resolves them.

---

## 13. Deviations from DESIGN.md

| # | Deviation | DESIGN says | M1 does | Reason |
|---|---|---|---|---|
| D1 | Fold granularity | §6.3: "for one statement" | Group-level: fold takes the full (subject, predicate) group | §6.4 supersession acts on "same subject+predicate" across different objects (different StatementIds). Per-statement fold silently breaks Functional/Supersede and Static/contradiction. |
| D2 | Ontology size | §5.5: "~40 predicates" | ~12 representative predicates | All invalidation branches covered; full ontology is M3. |
| D3 | Resolution embeddings | §8.1: embedding kNN in blocking | Lexical-only (exact + Jaro-Winkler + graph context) | No embeddings in M1. Embeddings are a secondary signal for names (§8.2). |
| D4 | Confidence calibration | §6.5: `calibrate(extractor)` from eval harness | `calibrate = 1.0` always | No eval data until M3. Formula skeleton implemented; multiplier deferred. |
| D5 | Retraction in fold | BeliefStatus::Retracted listed | Current-time fold produces Active/Superseded/Contradicted only; Retracted set during incremental cache updates | Retracted assertions are filtered at fold step 1; the status is a cache-level concern for the audit trail. |

---

## 14. Open questions (M1 defaults)

1. **Graph context overlap cost.** Computing shared neighbors for each candidate
   is O(candidates × adjacency). For M1's declaration-only scale this is fine.
   If it becomes a bottleneck, add an adjacency cache (M2 index). *Default:
   compute on demand from the statements table.*

2. **Multi-claim declarations.** Should one Declaration episode carry multiple
   claims, or is it strictly one-claim-per-episode? *Default: one claim per
   episode in M1 (simpler canonical content, simpler span semantics). The API
   can accept a batch and write N episodes. Revisit if declaration batches
   become common.*

3. **Symmetric predicate materialization.** When `knows(A, B)` is asserted, do we
   automatically create `knows(B, A)`? *Default: yes, write the reverse assertion
   in the same projection step. This is deterministic (symmetric is declared in
   the registry). M2's traversal can also follow symmetric edges virtually, but
   materializing avoids traversal-time special-casing.*
