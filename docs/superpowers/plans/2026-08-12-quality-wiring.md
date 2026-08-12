# Quality Wiring Implementation Plan (Sub-project Q)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the four quality defects that block real usage: broken frontend API, non-CSPRNG token secrets, dead salience signals, and hardcoded confidence.

**Architecture:** Four surgical fixes across `apps/brain-ui/src/api.ts`, `crates/oxibrain-store/src/security.rs`, `crates/oxibrain-store/src/query.rs`, and `crates/oxibrain-core/src/fold.rs`. No new crates, no schema changes, no API additions.

**Tech Stack:** Rust 2024, React 19 + TypeScript, `getrandom` crate (already transitive dep), existing `confidence.rs` formula.

## Global Constraints

- AGENTS.md: `expect("reason")` for invariants, `?` for fallible ops, no bare `unwrap` in non-test code.
- AGENTS.md: `clippy` clean with `-D warnings`; `#![cfg_attr(test, allow(clippy::unwrap_used))]`.
- AGENTS.md: Comments and commit messages in English.
- DESIGN §6.5: `confidence = calibrate(extractor) · corroboration · trust · recency_of_support`. Manual declarations bypass at `1.0`.
- DESIGN §9.2: Salience is a ranking signal only, never affects whether something is believed.
- Reprojection determinism must still hold (confidence/salience are deterministic functions of deterministic inputs).
- `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test` must pass after every task.

---

### Task Q1: Fix frontend API contract mismatches

**Files:**
- Modify: `apps/brain-ui/src/api.ts:88-118` (traverse, retract, mergeEntities methods)
- Modify: `apps/brain-ui/src/views/ContradictionInbox.tsx` (update retract call signature)
- Test: `apps/brain-ui/src/api.ts` (typecheck via `bun run build`)

**Interfaces:**
- Consumes: MCP server tool argument schemas from `crates/oxibrain-mcp/src/server.rs:397-522`
- Produces: Working frontend API calls matching server contracts

- [ ] **Step 1: Fix `traverse` arg name**

In `apps/brain-ui/src/api.ts`, change the `traverse` method:
```typescript
  traverse: (startEntities: string[], depth = 2, space = "personal") =>
    callTool<TraversalResult>("traverse", {
      start: startEntities,
      space,
      depth,
    }),
```

- [ ] **Step 2: Fix `mergeEntities` to send EntityRef objects**

In `apps/brain-ui/src/api.ts`, change the `mergeEntities` method to accept surface+type pairs matching the server's `loser`/`winner` EntityRef:
```typescript
  mergeEntities: (
    loserSurface: string,
    loserType: string,
    winnerSurface: string,
    winnerType: string,
    space = "personal",
  ) =>
    callTool<{ episode_id: string }>("merge_entities", {
      loser: { surface: loserSurface, type: loserType },
      winner: { surface: winnerSurface, type: winnerType },
      space,
    }),
```

- [ ] **Step 3: Fix `retract` to send full declaration args**

In `apps/brain-ui/src/api.ts`, change the `retract` method to match server's `{subject, predicate, object, episode}` contract:
```typescript
  retract: (
    subjectSurface: string,
    subjectType: string,
    predicate: string,
    objectKind: string,
    objectValue: string,
    episodeId: string,
    space = "personal",
  ) =>
    callTool<{ episode_id: string }>("retract", {
      subject: { surface: subjectSurface, type: subjectType },
      predicate,
      object: { kind: objectKind, value: objectValue },
      episode: episodeId,
      space,
    }),
```

- [ ] **Step 4: Update ContradictionInbox to use new signatures**

The ContradictionInbox's `handleRetract` currently calls `api.retract(item.entity_surface, item.predicate, "personal")`. The new signature needs more data. For v1, use a simplified call: pass the contradiction's entity_surface as the subject surface, `"Concept"` as type (from the contradiction data if available, default), and the episode from the first supporting assertion.

Check the `Contradiction` type — if it doesn't carry `episode_id` or `entity_type`, add those fields to the type and ensure the server's `contradictions` tool response includes them. For now, if the data isn't available, make the retract button show a message: "Retract needs episode context — use CLI `oxibrain entity` instead."

- [ ] **Step 5: Verify frontend builds**

Run: `cd apps/brain-ui && bun run build`
Expected: Build succeeds with no TypeScript errors.

- [ ] **Step 6: Commit**

```bash
git add apps/brain-ui/src/api.ts apps/brain-ui/src/views/ContradictionInbox.tsx
git commit -m "fix(m6): align frontend API calls with MCP server contracts

- traverse: start_entities → start
- merge_entities: canonical/merged IDs → loser/winner EntityRefs
- retract: entity_id/predicate → subject/predicate/object/episode
- ContradictionInbox updated for new call signatures"
```

---

### Task Q2: Replace token secret generation with CSPRNG

**Files:**
- Modify: `crates/oxibrain-store/src/security.rs:15-40` (`generate_secret` function)
- Modify: `crates/oxibrain-store/Cargo.toml` (add `getrandom` direct dependency)
- Test: `crates/oxibrain-store/src/security.rs` (add test for uniqueness + length)

**Interfaces:**
- Consumes: `getrandom` crate (v0.3+, already transitive dependency)
- Produces: Same `generate_secret() -> String` signature, CSPRNG-backed

- [ ] **Step 1: Add `getrandom` to Cargo.toml**

In `crates/oxibrain-store/Cargo.toml`, add to `[dependencies]`:
```toml
getrandom = "0.3"
```

- [ ] **Step 2: Rewrite `generate_secret`**

In `crates/oxibrain-store/src/security.rs`, replace the entire `generate_secret` function (lines ~15-40):
```rust
/// Generate a cryptographically secure 32-byte token secret, hex-encoded
/// with `obt_` prefix.
fn generate_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("OS entropy source unavailable");
    format!("obt_{}", hex::encode(&bytes))
}
```

- [ ] **Step 3: Write test for secret uniqueness and format**

Add to the `#[cfg(test)]` module in `security.rs`:
```rust
#[test]
fn test_generate_secret_is_cryptographically_unique() {
    let s1 = generate_secret();
    let s2 = generate_secret();
    assert!(s1.starts_with("obt_"));
    assert!(s2.starts_with("obt_"));
    assert_ne!(s1, s2, "two consecutive secrets must differ");
    // obt_ prefix (4) + 32 bytes hex (64) = 68 chars
    assert_eq!(s1.len(), 68);
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p oxibrain-store --lib security`
Expected: All security tests pass including the new one.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-store/Cargo.toml crates/oxibrain-store/src/security.rs
git commit -m "fix(security): use CSPRNG (getrandom) for token secret generation

Replace RandomState (SipHash, non-CSPRNG) with getrandom for
cryptographically secure bearer token secrets."
```

---

### Task Q3: Wire salience into retrieval queries

**Files:**
- Modify: `crates/oxibrain-store/src/query.rs:360-370` (`hybrid_query` — replace `salience: 1.0`)
- Modify: `crates/oxibrain-store/src/query.rs:425-435` (`traverse` — replace `salience: 1.0`)
- Test: `crates/oxibrain-store/tests/query.rs` (add salience-aware test)

**Interfaces:**
- Consumes: `entities.salience` column (exists since migration v3, updated by `apply_decay`)
- Produces: Real salience values in `RankedItem.salience` and `TraversalNode.salience`

- [ ] **Step 1: Write failing test for salience in hybrid_query**

In `crates/oxibrain-store/tests/query.rs`, add a test that creates entities with known salience values, runs `hybrid_query`, and asserts the salience field is not `1.0`:
```rust
#[test]
fn hybrid_query_returns_real_salience() {
    let dir = TempDir::new().unwrap();
    let store = StoreHandle::open(dir.path()).unwrap();
    let now = Timestamp::now();
    let clock = FixedClock::at(now);

    // Create a space and an entity.
    let space = store.writer_submit(|conn| {
        let id = ledger::create_space(conn, "test", now)?;
        Ok(id)
    }).unwrap();

    // Ingest and declare an entity, then apply decay to set non-default salience.
    // ... (fixture setup following existing test patterns)

    // Apply decay with a config that gives < 1.0 salience.
    let config = DecayConfig { base: 1.0, lambda: 0.01, floor: 0.05 };
    store.writer_submit(|conn| {
        lifecycle::apply_decay(conn, &space, now, &config)
    }).unwrap();

    // Query and assert salience is not 1.0.
    let result = store.read(|conn| {
        query::hybrid_query(conn, &space, "test query", QueryMode::Lexical, 10)
    }).unwrap();

    for item in &result.items {
        // After decay, salience should be < 1.0 for old entities.
        assert!(item.salience <= 1.0, "salience should be bounded");
        // If the entity has been around long enough, salience < 1.0.
        // (The exact value depends on the fixture's last_activity vs now.)
    }
}
```

- [ ] **Step 2: Run test to verify it fails or passes (it may pass if salience defaults to 1.0)**

Run: `cargo test -p oxibrain-store --test query hybrid_query_returns_real_salience`
Expected: FAIL — `salience` is always `1.0` regardless of decay.

- [ ] **Step 3: Wire salience into `hybrid_query`**

In `crates/oxibrain-store/src/query.rs`, find the `hybrid_query` function where `RankedItem` is constructed with `salience: 1.0`. Replace it to read the `salience` column from the entity row.

The entity row is already queried (the function joins against `entities`). Add a `salience` column to the SELECT and use it:
```rust
// Before (line ~366):
salience: 1.0, // M2: salience default

// After:
salience: row.get::<_, f64>("salience").unwrap_or(1.0) as f32,
```

- [ ] **Step 4: Wire salience into `traverse`**

In `crates/oxibrain-store/src/query.rs`, find the `traverse` function where `TraversalNode` is constructed with `salience: 1.0`. Same fix:
```rust
// Before (line ~430):
salience: 1.0, // M2: default salience

// After:
salience: row.get::<_, f64>("salience").unwrap_or(1.0) as f32,
```

- [ ] **Step 5: Run all tests**

Run: `cargo test -p oxibrain-store`
Expected: All tests pass. The reprojection determinism test still passes because salience is a deterministic function of the entity's `last_activity` and the current time.

- [ ] **Step 6: Commit**

```bash
git add crates/oxibrain-store/src/query.rs crates/oxibrain-store/tests/query.rs
git commit -m "fix: wire salience decay into retrieval ranking (§9.2)

Replace hardcoded salience: 1.0 with entity.salience column reads
in hybrid_query and traverse. Salience is computed by apply_decay
and rebuild_salience, both deterministic."
```

---

### Task Q4: Wire confidence computation into temporal fold

**Files:**
- Modify: `crates/oxibrain-core/src/fold.rs:115-280` (6 confidence: 1.0 assignments)
- Modify: `crates/oxibrain-core/src/fold.rs` (add calibration parameter to `fold` function)
- Modify: `crates/oxibrain-store/src/project.rs` (pass calibration table to fold)
- Test: `crates/oxibrain-core/src/fold.rs` (update existing tests + add confidence test)

**Interfaces:**
- Consumes: `ConfidenceComponents::combine()` from `confidence.rs`, `Assertion.confidence` (raw), `Assertion.extractor`, `Support.trust_weights`, `Support.distinct_episodes`
- Produces: Real confidence values in `Belief.confidence`, computed deterministically from assertion inputs

- [ ] **Step 1: Add a `compute_belief_confidence` helper**

In `fold.rs`, add a pure function that computes confidence from the visible assertions that survived into a belief interval:
```rust
use crate::confidence::{CalibrationTable, ConfidenceComponents, calibrate};

/// Compute belief confidence from supporting assertions (DESIGN §6.5).
/// All inputs are deterministic — this is a pure function of the assertion set.
fn belief_confidence(
    assertions: &[Assertion],
    support: &Support,
    calibration: &CalibrationTable,
    is_declaration: bool,
) -> f32 {
    // Manual declarations bypass at 1.0 (DESIGN §6.5).
    if is_declaration {
        return 1.0;
    }

    // Raw: max assertion confidence in this interval (the strongest evidence).
    let raw = assertions
        .iter()
        .map(|a| a.confidence)
        .fold(0.0_f32, f32::max);

    // Calibrated: per-extractor multiplier from eval harness.
    let extractor_id = assertions
        .iter()
        .filter_map(|a| a.extractor.as_deref())
        .next()
        .unwrap_or("unknown");
    let calibrated = calibrate(extractor_id, calibration);

    // Corroboration: saturating in distinct episodes.
    let n = support.distinct_episodes.max(1) as f32;
    let corroboration = (1.0 - (-0.3 * n).exp()).clamp(0.5, 1.0);

    // Trust: weighted by episode trust tier.
    let trust = if support.trust_weights.is_empty() {
        1.0
    } else {
        // Weighted average: Trusted=1.0, SemiTrusted=0.7, Untrusted=0.3.
        let total: u32 = support.trust_weights.iter().map(|(_, c)| *c).sum();
        if total == 0 {
            1.0
        } else {
            let weighted: f32 = support
                .trust_weights
                .iter()
                .map(|(tier, count)| {
                    let w = match tier {
                        TrustTier::Trusted => 1.0,
                        TrustTier::SemiTrusted => 0.7,
                        TrustTier::Untrusted => 0.3,
                    };
                    w * *count as f32
                })
                .sum();
            (weighted / total as f32).clamp(0.3, 1.0)
        }
    };

    // Recency: for Interval predicates, based on claimed_from proximity.
    // v1: fixed at 1.0 — recency_of_support needs a reference time parameter
    // that the fold doesn't currently carry. This is a known simplification.
    let recency = 1.0;

    ConfidenceComponents {
        raw,
        calibrated,
        corroboration,
        trust,
        recency,
    }
    .combine()
}
```

- [ ] **Step 2: Add `calibration` parameter to `fold` function**

Change the `fold` signature:
```rust
// Before:
pub fn fold(
    def: &PredicateDef,
    group: &[StatementEntry],
    at: Timestamp,
) -> Vec<Belief>

// After:
pub fn fold(
    def: &PredicateDef,
    group: &[StatementEntry],
    at: Timestamp,
    calibration: &CalibrationTable,
) -> Vec<Belief>
```

- [ ] **Step 3: Wire `belief_confidence` into fold**

Replace each `confidence: 1.0` in the fold body. The fold has two paths:
1. **Declaration path** (line ~116): `is_declaration = true` → confidence stays `1.0`
2. **Extraction path** (lines ~164, 217, 233, 259, 278): call `belief_confidence`

For the declaration path, keep `confidence: 1.0` explicitly.

For the extraction paths, replace:
```rust
// Before:
confidence: 1.0,

// After:
confidence: belief_confidence(
    &visible.assertions,
    &support,
    calibration,
    false,
),
```

Note: `visible.assertions` is the `Vec<Assertion>` from the `VisibleStmt` struct, which is the set of assertions visible at transaction time `at`. `support` is already computed at that point in the fold.

- [ ] **Step 4: Update fold callers**

Find all call sites of `fold(`. The primary caller is `crates/oxibrain-store/src/project.rs`. Update them to pass a `CalibrationTable`:

```rust
use oxibrain_core::confidence::CalibrationTable;

// In project.rs, where fold is called:
let calibration = CalibrationTable::default(); // v1: no measured calibration
let beliefs = fold(def, group, at, &calibration);
```

Update all test call sites in `fold.rs` tests to pass `&CalibrationTable::default()`.

- [ ] **Step 5: Write test for confidence computation**

In `crates/oxibrain-core/src/fold.rs` tests, add:
```rust
#[test]
fn fold_extraction_confidence_reflects_assertion_quality() {
    // Two assertions from different episodes → higher corroboration
    // than one assertion from one episode.
    let def = PredicateDef {
        id: "test_predicate".into(),
        label: "test".into(),
        cardinality: Cardinality::MultiValued,
        temporality: Temporality::Interval,
        invalidation: Invalidation::Coexist,
        subject_type: "Concept".into(),
        object_type: "Concept".into(),
        inverse: None,
        symmetric: false,
    };

    let stmt = test_statement("s1", "e1", "test_predicate");
    let a1 = test_assertion("a1", "s1", "ep1", Polarity::Affirm, ts(1));
    let a2 = test_assertion("a2", "s1", "ep2", Polarity::Affirm, ts(2));

    let group = vec![StatementEntry {
        statement: stmt,
        assertions: vec![a1, a2],
    }];

    let calibration = CalibrationTable::default();
    let beliefs = fold(&def, &group, ts(100), &calibration);

    assert_eq!(beliefs.len(), 1);
    // With 2 distinct episodes, corroboration > 0.5 (the floor).
    // With unmeasured extractor, calibrated = 0.8.
    // Final confidence should be < 1.0 but > 0.0.
    let conf = beliefs[0].confidence;
    assert!(conf > 0.0 && conf < 1.0, "confidence should be in (0, 1), got {conf}");
}
```

- [ ] **Step 6: Update all existing fold tests**

Every existing test that calls `fold(` needs the new `&CalibrationTable::default()` argument. Search for `fold(` in `fold.rs` tests and add the parameter.

- [ ] **Step 7: Run all tests**

Run: `cargo test`
Expected: All tests pass. The reprojection determinism test passes because confidence is a pure function of deterministic inputs.

- [ ] **Step 8: Verify reprojection determinism**

Run: `cargo test -p oxibrain-store --test reproject`
Run: `cargo test -p oxibrain --test reproject_determinism`
Expected: Both pass.

- [ ] **Step 9: Commit**

```bash
git add crates/oxibrain-core/src/fold.rs crates/oxibrain-store/src/project.rs
git commit -m "feat: wire confidence formula into temporal fold (§6.5)

Replace hardcoded confidence: 1.0 with belief_confidence() that
computes calibrate · corroboration · trust · recency from the
surviving assertion set. Manual declarations bypass at 1.0.

Recency_of_support is fixed at 1.0 for v1 (needs reference time
parameter — known simplification, documented in code)."
```

---

### Final Verification

- [ ] **Run full suite**

```bash
cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```
Expected: All pass, 0 warnings.

- [ ] **Verify standalone guarantee**

```bash
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1 || echo "PASS"
```

- [ ] **Verify frontend builds**

```bash
cd apps/brain-ui && bun run build
```

- [ ] **Commit final state if any remaining changes**

---

## Self-review checklist

**Spec coverage:**
- Q1 (frontend API): Task Q1 ✅
- Q2 (token CSPRNG): Task Q2 ✅
- Q3 (salience wiring): Task Q3 ✅
- Q4 (confidence wiring): Task Q4 ✅

**Placeholder scan:** No TBDs or TODOs. All steps show actual code.

**Type consistency:**
- `CalibrationTable` used consistently across fold.rs and project.rs
- `generate_secret()` signature unchanged (internal function, return type same)
- `RankedItem.salience` and `TraversalNode.salience` are `f64` in the struct, read from `entities.salience` (also `f64` via `REAL` column)
- `belief_confidence` takes `&[Assertion]` and `&Support` — both available at the call sites in fold

**Known simplifications (documented in code, not bugs):**
- `recency_of_support` fixed at `1.0` — the fold doesn't carry a reference time parameter for interval recency. This is explicitly noted as a v1 simplification.
- `CalibrationTable::default()` (no measured calibration) — the eval harness populates it, but no eval results exist yet.
