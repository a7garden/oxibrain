# Parallel Session Note — M2 Test Coverage & Budget Measurement

> **Date:** 2026-08-11
> **Branch:** `main` (merged)
> **Commits:** `22d4c75`, `c2d5a9d`, `85b0745`, `ed9cef3`, merges `bbc5e0d`
> **Context:** Written by a parallel session while M3 (extraction + evaluation) was
> in progress. All work is in non-overlapping files — zero merge conflicts.
>
> **Bug found and fixed:** the `supersede_no_overlapping_active` property test
> found a boundary-condition bug in `fold_supersede` — when one interval ends
> exactly where another begins (`valid_to == new_start`), the clipping condition
> used strict `>` instead of `>=`, leaving two Active beliefs overlapping at a
> single point. Fixed in `ed9cef3`. See §5 below.

---

## What Was Done

This session filled the AGENTS.md testing-expectation gaps that existed after M2,
and completed the deferred §13.2 budget measurement.

### 1. Property Tests for the Temporal Fold (8 tests)

**File:** `crates/oxibrain-core/tests/fold_property.rs` (NEW)

AGENTS.md requires "Property tests for the temporal fold." The fold module had
8 example-based tests but zero `proptest` coverage. Added invariant checks:

| Property | What it verifies |
|---|---|
| `output_always_sorted` | Output sorted by (statement_id, valid_from) across all fold modes |
| `intervals_well_formed` | `valid_from <= valid_to` for every belief |
| `supersede_no_overlapping_active` | Functional+Supersede: no two Active beliefs from different statements overlap |
| `contradiction_no_overlapping_active` | Functional+Static: same exclusivity for Active beliefs |
| `multivalued_all_active` | MultiValued: every belief is Active |
| `denial_eliminated_from_output` | No belief interval overlaps any denial interval |
| `empty_group_empty_output` | Edge case |
| `single_affirm_produces_one_active` | Edge case |

### 2. Property Tests for Resolution Decisions (8 tests)

**File:** `crates/oxibrain-core/tests/resolution_property.rs` (NEW)

AGENTS.md requires "Property tests for ... resolution decisions." The resolution
module had 5 example-based tests but zero `proptest` coverage. Added:

| Property | What it verifies |
|---|---|
| `score_in_bounds` | Score always clamped to [0, 1] |
| `type_mismatch_zero_score` | Hard type gate → score 0 |
| `exact_match_always_links` | Exact normalized match always reaches tau_high |
| `resolve_picks_highest_score` | Best candidate is always selected |
| `normalize_idempotent` | `normalize(normalize(x)) == normalize(x)` |
| `normalize_lowercase_collapsed` | Output is lowercase, whitespace-collapsed |
| `no_candidates_is_new` | Edge case |
| `resolve_deterministic_same_input` | Determinism |

### 3. Migration Chain Tests (expanded from 1 → 5 tests)

**File:** `crates/oxibrain-store/tests/migration_chain.rs` (MODIFIED)

AGENTS.md requires "Migration chain test from every historical schema version."
The existing test only covered empty-to-current. Added:

- `migrates_from_v1_with_data`: v1 fixture with episode → v3; verifies data
  integrity, predicate seeding (v2), FTS5/TF-IDF/salience/compaction columns (v3)
- `migrates_from_v2_with_data`: v2 fixture → v3; verifies data preserved,
  predicates not re-seeded, v3 structures present
- `migration_idempotent`: re-running `migration::run()` is a no-op
- `newer_db_is_hard_error`: future schema version is a hard `BrainError::Migration`

### 4. §13.2 Budget Measurement

**File:** `doc/DESIGN.md` §13.2 (MODIFIED)

M2 deferred budget measurement — the bench suite compiled but actual numbers were
never recorded. First measurement (Apple M4, release build, criterion 30-sample
median, 200-entity / 500-statement fixture):

| Operation | Budget | Measured | Status |
|---|---|---|---|
| declaration write | < 5 ms | **0.42 ms** | ✅ |
| hybrid query (top 20) | < 80 ms | **1.44 ms** | ✅ |
| traversal, depth 3, ≤256 nodes | < 100 ms | **0.32 ms** | ✅ |
| `get_entity` | < 10 ms | not benchmarked | — |
| `assemble_context` (3K tokens) | < 150 ms | not benchmarked | — |
| reproject from cache | < 5 min | not benchmarked | — |
| cold start (index load) | < 2 s | not benchmarked | — |

All three measured operations are well within budget at fixture scale (≤8%
utilization). The four unmeasured operations need larger fixtures and are deferred.

---

## Impact on M3

**Zero file overlap.** All changes are in:
- `crates/oxibrain-core/tests/` (new files — not touched by M3)
- `crates/oxibrain-store/tests/migration_chain.rs` (M3 adds no new migrations)
- `doc/DESIGN.md` §13.2 (M3 touches §5.3, §12.3, §14, §17 — different sections)

The M3 session's uncommitted working tree was preserved through the merge.

**For M3d:** The budget measurement is partially complete. When M3d runs the eval
suite and benchmarks, the §13.2 table already has the first measurement for 3 of 7
operations. M3d should add benchmarks for the remaining 4 (`get_entity`,
`assemble_context`, `reproject`, `cold start`) and ideally re-run at a larger scale.

---

## Test Count

Workspace total: **103 tests** (was 78 at M2 exit + M3 additions + this session's 20).
0 failures. Clippy `-D warnings` clean. `cargo fmt --check` clean.

---

End of note.
