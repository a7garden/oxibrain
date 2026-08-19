# Plan C: Curation Parity (P4)

> Complete CLI merge/split/alias/retract/declare/predicate/source-policy operations.
> Every correction emits an auditable Declaration; reprojection remains deterministic.

## Status

- [x] Analysis complete
- [ ] Implementation

## Context

**Blueprint:** `doc/spec/ecosystem-v2-memory-kernel.md` §9 P4, §3.2, §6.3
**Exit condition:** Every correction emits an auditable Declaration; reprojection remains deterministic.

**Current state:**
- Engine: `Declaration::Merge`, `Retract`, `RegisterSource`, `SetSourcePolicy` exist.
  Missing: `Split`, `Alias`, `RegisterPredicate`.
- MCP: `declare`, `retract`, `merge_entities`, `review_merges` — 15-tool cap reached.
  New curation ops go through existing `declare` tool (JSON payload). No new MCP tools.
- Facade: `Brain::declare(space, decl)` handles any Declaration variant. No new facade methods needed.
- CLI: Only `entity show` and `predicate list` exist. Everything else missing.

## Design Decisions

1. **Split = undo merge.** `Declaration::Split { entity: EntityRef }` finds the active
   merge where `entity` is the loser, sets `undone_at`, clears `merged_into`.
   If no active merge exists → `BrainError::Invalid("no active merge for entity")`.
   Deterministic: `undone_at` = declaration episode's `ingested_at` (same as all projections).

2. **Alias = add UserDeclared key.** `Declaration::Alias { entity: EntityRef, surface: String }`
   resolves the entity (following merge chain), inserts an `EntityKey` with
   `origin = UserDeclared`. Idempotent via INSERT OR IGNORE.

3. **RegisterPredicate = truth-half write.** `Declaration::RegisterPredicate { name: String, def_json: String }`
   inserts into `predicates` table. Must be deterministic for reprojection.
   `major_version`/`minor_version` extracted from `def_json` (fields `major_version`, `minor_version`).

4. **CLI retract uses statement_id-first path** (mirrors MCP `tool_retract`):
   `oxibrain entity retract <statement_id>` calls `retract_parts` then `declare`.

5. **CLI declare accepts raw JSON:** `oxibrain declare '<json>' --space s`.
   This is the power-user path; structured subcommands cover common cases.

6. **Source policy CLI:** `oxibrain source policy <source_name> --trust <tier> [--effective-from <ms>] [--effective-to <ms>] --space s`.

7. **Predicate add CLI:** `oxibrain predicate add <json> --space s` where JSON is a full PredicateDef.

## Task Breakdown

### Task 1: Core — Split, Alias, RegisterPredicate declaration variants + projection

**Files:**
- Modify: `crates/oxibrain-store/src/project.rs` — add variants to `Declaration` enum, add projection arms
- Modify: `crates/oxibrain-store/src/knowledge.rs` (kcrud) — add `undo_merge`, `clear_merged_into` helpers

**Interfaces:**
- Consumes: existing `Declaration` enum, `project_declaration`, `kcrud` module.
- Produces: three new Declaration variants that project deterministically.

**Steps:**

1. Add variants to `Declaration` enum in `project.rs`:
```rust
    Split {
        entity: EntityRef,
    },
    Alias {
        entity: EntityRef,
        surface: String,
    },
    RegisterPredicate {
        name: String,
        def_json: String,
    },
```

2. Add projection arms in `project_declaration` match:

**Important:** Add `use rusqlite::OptionalExtension;` to the imports at the top of project.rs
(needed for `.optional()`). Use `rusqlite::params!` (fully qualified, matching existing style).

```rust
        Declaration::Split { entity } => {
            let (entity_id, _) = resolve_or_create(conn, space, entity, &ep_id, 0, now, &[], cache)?;
            // Find active merge where this entity is the loser.
            let merge: Option<(String, String)> = conn
                .query_row(
                    "SELECT id, winner_id FROM entity_merges
                     WHERE loser_id = ?1 AND undone_at IS NULL
                     ORDER BY decided_at DESC LIMIT 1",
                    rusqlite::params![entity_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql_err)?;
            match merge {
                Some((merge_id, _winner)) => {
                    conn.execute(
                        "UPDATE entity_merges SET undone_at = ?1 WHERE id = ?2",
                        rusqlite::params![now.millis(), merge_id],
                    )
                    .map_err(sql_err)?;
                    conn.execute(
                        "UPDATE entities SET merged_into = NULL WHERE id = ?1",
                        rusqlite::params![entity_id],
                    )
                    .map_err(sql_err)?;
                    touched.push(entity_id);
                }
                None => {
                    return Err(BrainError::Invalid(format!(
                        "split: no active merge for entity {entity_id}"
                    )));
                }
            }
        }
        Declaration::Alias { entity, surface } => {
            let (entity_id, _) = resolve_or_create(conn, space, entity, &ep_id, 0, now, &[], cache)?;
            let normalized = resolution::normalize(&surface, &entity.ty);
            let key_id = entity_key_id(&entity_id, &normalized, &entity.ty);
            let key = EntityKey {
                id: key_id,
                space: space.to_string(),
                entity: entity_id.clone(),
                ty: entity.ty.clone(),
                normalized,
                surface: surface.clone(),
                origin: KeyOrigin::UserDeclared,
            };
            kcrud::insert_entity_key(conn, &key)?;
            cache.insert_key(space, &entity.ty, &key);
            touched.push(entity_id);
        }
        Declaration::RegisterPredicate { name, def_json } => {
            let v: serde_json::Value = serde_json::from_str(def_json)
                .map_err(|e| BrainError::Invalid(format!("predicate def_json: {e}")))?;
            let major = v.get("major_version").and_then(|x| x.as_i64()).unwrap_or(1) as i64;
            let minor = v.get("minor_version").and_then(|x| x.as_i64()).unwrap_or(0) as i64;
            conn.execute(
                "INSERT OR REPLACE INTO predicates (name, major_version, minor_version, def_json)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![name, major, minor, def_json],
            )
            .map_err(sql_err)?;
        }
```

**Note on imports:** `entity_key_id` is already imported at line 13. `EntityKey` and
`KeyOrigin` are already imported at lines 14-16. No new imports needed beyond
`use rusqlite::OptionalExtension;`.

3. Add `undo_merge` helper to kcrud (knowledge.rs) for testability:
```rust
/// Mark a merge as undone (split). Sets undone_at on the merge record.
pub fn undo_merge(conn: &Connection, merge_id: &str, undone_at: Timestamp) -> Result<(), BrainError> {
    conn.execute(
        "UPDATE entity_merges SET undone_at = ?1 WHERE id = ?2",
        params![undone_at.millis(), merge_id],
    )
    .map_err(sql_err)
}
```

4. Tests (in `crates/oxibrain-store/tests/` — new file `curation_declarations.rs`):
   - `split_undoes_active_merge`: declare merge, then split → entity is independent again.
   - `split_without_merge_fails`: split on unmerged entity → BrainError::Invalid.
   - `alias_adds_user_declared_key`: declare alias → entity key exists with UserDeclared origin.
   - `alias_is_idempotent`: declare same alias twice → no error, one key.
   - `register_predicate_inserts_row`: declare RegisterPredicate → predicates table has row.
   - `reproject_reproduces_split`: full reproject after split → same state.

**Acceptance:**
- All 6 tests pass.
- `cargo test -p oxibrain-store` — no regressions.
- Reprojection determinism test still passes.

---

### Task 2: CLI — entity merge/split/alias/retract + declare + predicate add + source policy

**Files:**
- Modify: `crates/oxibrain-cli/src/cli.rs` — add subcommands
- Modify: `crates/oxibrain-cli/src/main.rs` — wire dispatch
- Create: `crates/oxibrain-cli/src/cmd/declare.rs`
- Create: `crates/oxibrain-cli/src/cmd/source_policy.rs`
- Modify: `crates/oxibrain-cli/src/cmd/mod.rs` — add modules
- Modify: `crates/oxibrain-cli/src/cmd/entity_show.rs` — no change (existing)
- Modify: `crates/oxibrain-cli/src/cmd/predicate.rs` — add `add` subcommand

**Interfaces:**
- Consumes: `Brain::declare`, `Brain::retract_parts`, `Brain::ensure_space`, `Declaration` enum.
- Produces: complete CLI curation surface.

**Steps:**

1. Extend `EntityCmd` in `cli.rs`:
```rust
#[derive(Subcommand, Debug)]
pub enum EntityCmd {
    /// Show entity beliefs.
    Show {
        id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Merge two entities (loser → winner).
    Merge {
        /// Loser entity surface form.
        loser: String,
        /// Loser entity type.
        loser_type: String,
        /// Winner entity surface form.
        winner: String,
        /// Winner entity type.
        winner_type: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Split: undo the most recent merge for an entity.
    Split {
        /// Entity surface form.
        surface: String,
        /// Entity type.
        ty: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Add an alias to an entity.
    Alias {
        /// Entity surface form.
        surface: String,
        /// Entity type.
        ty: String,
        /// Alias surface form to add.
        alias: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Retract a statement by ID.
    Retract {
        /// Statement ID to retract.
        statement_id: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
}
```

2. Add `Declare` and `Source` top-level commands:
```rust
    /// Declare a statement from raw JSON (power-user path).
    Declare {
        /// Canonical declaration JSON.
        json: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Source management.
    Source {
        #[command(subcommand)]
        command: SourceCmd,
    },
```

3. Add `SourceCmd`:
```rust
#[derive(Subcommand, Debug)]
pub enum SourceCmd {
    /// Set trust policy for a source.
    Policy {
        /// Source name (as registered).
        name: String,
        /// Trust tier: trusted | untrusted.
        #[arg(long)]
        trust: String,
        /// Effective from (epoch ms). Defaults to now.
        #[arg(long)]
        effective_from: Option<i64>,
        /// Effective to (epoch ms). Open-ended if omitted.
        #[arg(long)]
        effective_to: Option<i64>,
        #[arg(long, default_value = "personal")]
        space: String,
    },
}
```

4. Extend `PredicateCmd`:
```rust
#[derive(Subcommand, Debug)]
pub enum PredicateCmd {
    /// List predicates in the core/v1 registry.
    List,
    /// Register a custom predicate from JSON.
    Add {
        /// Full PredicateDef JSON.
        json: String,
        #[arg(long, default_value = "personal")]
        space: String,
    },
}
```

5. Create `crates/oxibrain-cli/src/cmd/declare.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::Declaration;
use std::path::Path;

pub async fn run(dir: &Path, json: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl: Declaration =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse declaration: {e}"))?;
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("declared as episode: {ep_id}");
    Ok(())
}
```

6. Create `crates/oxibrain-cli/src/cmd/source_policy.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::Declaration;
use std::path::Path;

pub async fn run(
    dir: &Path,
    name: &str,
    trust: &str,
    effective_from: Option<i64>,
    effective_to: Option<i64>,
    space: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let now = brain.clock_now();
    let decl = Declaration::SetSourcePolicy {
        source_name: name.to_string(),
        trust: trust.to_string(),
        effective_from: effective_from.unwrap_or(now.millis()),
        effective_to,
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("policy set as episode: {ep_id}");
    Ok(())
}
```

7. Wire dispatch in `main.rs`:
```rust
        Command::Entity { command } => match command {
            cli::EntityCmd::Show { id, space } => cmd::entity_show::run(&dir, &id, &space).await,
            cli::EntityCmd::Merge { loser, loser_type, winner, winner_type, space } => {
                cmd::entity_merge::run(&dir, &loser, &loser_type, &winner, &winner_type, &space).await
            }
            cli::EntityCmd::Split { surface, ty, space } => {
                cmd::entity_split::run(&dir, &surface, &ty, &space).await
            }
            cli::EntityCmd::Alias { surface, ty, alias, space } => {
                cmd::entity_alias::run(&dir, &surface, &ty, &alias, &space).await
            }
            cli::EntityCmd::Retract { statement_id, space } => {
                cmd::entity_retract::run(&dir, &statement_id, &space).await
            }
        },
        Command::Declare { json, space } => cmd::declare::run(&dir, &json, &space).await,
        Command::Source { command } => match command {
            cli::SourceCmd::Policy { name, trust, effective_from, effective_to, space } => {
                cmd::source_policy::run(&dir, &name, &trust, effective_from, effective_to, &space).await
            }
        },
        Command::Predicate { command } => match command {
            cli::PredicateCmd::List => cmd::predicate::run(),
            cli::PredicateCmd::Add { json, space } => cmd::predicate::run_add(&dir, &json, &space).await,
        },
```

8. Create `crates/oxibrain-cli/src/cmd/entity_merge.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{Declaration, EntityRef};
use std::path::Path;

pub async fn run(
    dir: &Path,
    loser: &str,
    loser_type: &str,
    winner: &str,
    winner_type: &str,
    space: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl = Declaration::Merge {
        loser: EntityRef { surface: loser.to_string(), ty: loser_type.to_string() },
        winner: EntityRef { surface: winner.to_string(), ty: winner_type.to_string() },
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("merged as episode: {ep_id}");
    Ok(())
}
```

9. Create `crates/oxibrain-cli/src/cmd/entity_split.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{Declaration, EntityRef};
use std::path::Path;

pub async fn run(dir: &Path, surface: &str, ty: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl = Declaration::Split {
        entity: EntityRef { surface: surface.to_string(), ty: ty.to_string() },
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("split as episode: {ep_id}");
    Ok(())
}
```

10. Create `crates/oxibrain-cli/src/cmd/entity_alias.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{Declaration, EntityRef};
use std::path::Path;

pub async fn run(
    dir: &Path,
    surface: &str,
    ty: &str,
    alias: &str,
    space: &str,
) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let decl = Declaration::Alias {
        entity: EntityRef { surface: surface.to_string(), ty: ty.to_string() },
        surface: alias.to_string(),
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("alias added as episode: {ep_id}");
    Ok(())
}
```

11. Create `crates/oxibrain-cli/src/cmd/entity_retract.rs`:
```rust
use oxibrain::Brain;
use oxibrain::BrainConfig;
use oxibrain_store::project::{DeclObject, Declaration};
use std::path::Path;

pub async fn run(dir: &Path, statement_id: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let (subject, predicate, object) = brain.retract_parts(&space_id, statement_id).await?;
    // Find the originating episode for audit context.
    let episode = String::new(); // retract_parts doesn't return episode; use empty.
    let decl = Declaration::Retract {
        subject,
        predicate,
        object,
        episode,
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("retracted as episode: {ep_id}");
    Ok(())
}
```

12. Extend `crates/oxibrain-cli/src/cmd/predicate.rs` with `run_add`:
```rust
pub async fn run_add(dir: &Path, json: &str, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    // Parse to extract name for the declaration.
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("parse predicate def: {e}"))?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("predicate def must have 'name' field"))?
        .to_string();
    let decl = Declaration::RegisterPredicate {
        name,
        def_json: json.to_string(),
    };
    let ep_id = brain.declare(&space_id, decl).await?;
    println!("predicate registered as episode: {ep_id}");
    Ok(())
}
```

13. Update `crates/oxibrain-cli/src/cmd/mod.rs` to add new modules:
```rust
pub mod declare;
pub mod entity_alias;
pub mod entity_merge;
pub mod entity_retract;
pub mod entity_split;
pub mod source_policy;
```

**Acceptance:**
- `cargo check -p oxibrain-cli` passes.
- `cargo test -p oxibrain-cli` — existing tests still pass.
- All new subcommands appear in `--help`.

---

### Task 3: E2E tests — curation round-trips

**Files:**
- Create: `crates/oxibrain-cli/tests/curation.rs`

**Interfaces:**
- Consumes: Task 1 + Task 2 outputs.
- Produces: end-to-end proof of curation parity.

**Steps:**

1. Write integration tests:
   - `merge_then_split_round_trip`: declare two entities, merge, verify merged, split, verify independent.
   - `alias_makes_entity_findable_by_alias`: declare entity, add alias, verify entity key exists.
   - `retract_by_statement_id`: declare statement, retract by ID, verify belief status = Retracted.
   - `declare_raw_json`: declare via raw JSON, verify episode created.
   - `source_policy_changes_trust`: register source, set policy, verify trust tier.
   - `predicate_add_and_list`: add predicate, verify it appears in predicates table.
   - `reproject_preserves_curation`: perform merge+split+alias, reproject, verify state unchanged.

**Acceptance:**
- All 7 tests pass.
- Reprojection determinism holds.

---

### Task 4: Documentation

**Files:**
- Modify: `doc/ARCHITECTURE.md` — add P4 curation parity documentation

**Steps:**
1. Add §4.2.2 "Curation operations" documenting:
   - All curation ops are Declarations (append-only, auditable).
   - Split undoes a merge (sets undone_at, clears merged_into).
   - Alias adds a UserDeclared EntityKey.
   - RegisterPredicate is truth-half (deterministic reprojection).
   - CLI surface: entity merge/split/alias/retract, declare, predicate add, source policy.
2. Update §16.4 CLI section with new commands.
3. Add decision D34: "Split is the inverse of Merge; it sets undone_at rather than deleting the merge record."
4. Bump version header.

**Acceptance:**
- Documentation reflects curation parity.
- No Rust files modified.

---

## Dependency Graph

```
Task 1 (core variants) ──┐
                         ├── Task 3 (E2E tests)
Task 2 (CLI commands) ───┘
Task 4 (docs) — independent
```

Task 1 and Task 2 can run in parallel (Task 2 compiles against Task 1's types but doesn't need them at check time if we use the right imports).
Task 3 depends on both Task 1 and Task 2.
Task 4 is independent.

## Risk Notes

- **Split on reproject:** If a Split declaration is replayed before its corresponding Merge (impossible in seq order, but defensive), the split will fail with "no active merge". This is correct behavior — it means the data is inconsistent.
- **RegisterPredicate versioning:** Using INSERT OR REPLACE means re-registering the same predicate name overwrites it. This is intentional (latest registration wins in replay order).
- **retract_parts episode field:** The current `retract_parts` returns `(EntityRef, String, DeclObject)` but not the originating episode ID. The CLI passes an empty string. This is acceptable — the episode field in Retract is audit context, not a FK.
