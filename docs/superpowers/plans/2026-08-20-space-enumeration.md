# Space Enumeration & First-Party RPC — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `list_spaces` on every surface (store → facade → CLI → MCP resource + native RPC → client typed helper), scope-correct, plus a `resources/read` scope-bypass fix and ADR-009 doc alignment.

**Architecture:** additive read path following existing patterns (`list_sources`, `space://` resources, `reproject` raw RPC). No new tools — the fifteen-tool MCP cap is untouched. First-party enumeration rides the native JSON-RPC layer.

**Tech Stack:** Rust 2024, rusqlite (store only), tokio, clap, serde_json.

## Global Constraints

- `clippy` clean with `-D warnings`; no `.unwrap()` outside tests (`#![cfg_attr(test, allow(clippy::unwrap_used))]` is already set per crate).
- Public facade APIs return `BrainError`, never `anyhow` across crate boundary. Time in public signatures is `Timestamp`, never bare `i64`.
- No `rusqlite`/`tokio` names in `oxibrain-core`/`oxibrain-index`; no `rusqlite` in `oxibrain-views`. `rusqlite` stays inside `oxibrain-store`.
- Store fns are decision-free fetches (P9).
- Comments/doc-comments in English.
- MCP tool count stays fifteen. `spaces/list` is a raw RPC method like `reproject`, never a tool.
- Scoped sessions (`Scope` present) see only their spaces everywhere enumeration or resources touch space data.
- Conventional commits: `feat:`, `test:`, `docs:`, `fix:`.

---

### Task 1: Store — `list_spaces` + `SpaceRow`

**Files:**
- Modify: `crates/oxibrain-store/src/ledger.rs` (beside `get_space`, ~line 51)
- Test: `crates/oxibrain-store/src/ledger.rs` inline `#[cfg(test)] mod` if one exists there; otherwise create `crates/oxibrain-store/tests/spaces.rs`

**Interfaces:**
- Produces: `pub struct SpaceRow { pub id: String, pub name: String, pub created_at: i64, pub episode_count: i64, pub entity_count: i64 }` (derives `Debug, Clone, Serialize, Deserialize`) and `pub fn list_spaces(conn: &Connection) -> Result<Vec<SpaceRow>, BrainError>` in `oxibrain_store::ledger`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn list_spaces_orders_by_creation_and_counts() {
    // Open a store the same way other store tests do (StoreHandle or a raw
    // migrated Connection — follow the file's existing test setup pattern).
    // Create spaces in known order with distinct timestamps:
    //   create_space(conn, "personal", Timestamp::from_millis(100))
    //   create_space(conn, "work",     Timestamp::from_millis(200))
    //   create_space(conn, "alpha",    Timestamp::from_millis(100))  // same ts as personal
    let rows = list_spaces(conn).unwrap();
    assert_eq!(rows.len(), 3);
    // Order: (created_at, id) — alpha vs personal tie broken by id.
    assert_eq!(rows[0].name, "alpha");
    assert_eq!(rows[1].name, "personal");
    assert_eq!(rows[2].name, "work");
    assert_eq!(rows.iter().find(|r| r.name == "work").unwrap().episode_count, 0);
    assert_eq!(rows.iter().find(|r| r.name == "work").unwrap().entity_count, 0);
}
```

Also assert one space with an ingested episode reports `episode_count >= 1` (reuse the file's episode-insert test helper if present; otherwise `insert_episode` on a prepared `Episode`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oxibrain-store spaces`
Expected: FAIL — `list_spaces` / `SpaceRow` not found.

- [ ] **Step 3: Implement**

```rust
/// A space row with live counts. Decision-free fetch (P9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceRow {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub episode_count: i64,
    pub entity_count: i64,
}

/// List all spaces with episode/entity counts, ordered by (created_at, id).
/// Canonical order — same store, same rows, same order.
pub fn list_spaces(conn: &Connection) -> Result<Vec<SpaceRow>, BrainError> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.name, s.created_at,
                (SELECT COUNT(*) FROM episodes e WHERE e.space_id = s.id),
                (SELECT COUNT(*) FROM entities en WHERE en.space_id = s.id)
             FROM spaces s
             ORDER BY s.created_at, s.id",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![], |r| {
            Ok(SpaceRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                episode_count: r.get(3)?,
                entity_count: r.get(4)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(rows)
}
```

(Adapt `sql_err`/`params!` usage to the file's imports — both are already used in this file.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oxibrain-store`
Expected: PASS (existing tests still green).

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-store/src/ledger.rs
git commit -m "feat(store): list_spaces with counts, canonical (created_at, id) order"
```

---

### Task 2: Facade — `SpaceInfo` + `Brain::list_spaces`

**Files:**
- Modify: `crates/oxibrain/src/models.rs` (add `SpaceInfo`)
- Modify: `crates/oxibrain/src/lib.rs` (add method beside `ensure_space`, ~line 135)
- Modify: `crates/oxibrain/src/compat.rs` (add to `_check_methods`)
- Test: inline `#[cfg(test)]` in `lib.rs` near the new method (follow existing facade test placement)

**Interfaces:**
- Consumes: `oxibrain_store::ledger::{list_spaces, SpaceRow}` from Task 1.
- Produces: `pub struct SpaceInfo { pub id: String, pub name: String, pub created_at: Timestamp, pub episode_count: i64, pub entity_count: i64 }` (derives `Debug, Clone, PartialEq, Serialize, Deserialize`) in `oxibrain::models`, re-exported at crate root; `impl Brain { pub async fn list_spaces(&self) -> Result<Vec<SpaceInfo>, BrainError> }`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn list_spaces_returns_created_spaces() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("work").await.unwrap();
    let _ = brain.ensure_space("personal").await.unwrap();
    let spaces = brain.list_spaces().await.unwrap();
    let names: Vec<&str> = spaces.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"work"));
    assert!(names.contains(&"personal"));
    assert!(spaces.iter().all(|s| s.created_at.millis() > 0));
}
```

(Check the crate's existing tests for the canonical tempdir/Brain setup — `tempfile` is already a dev-dependency of crates that test `Brain`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oxibrain list_spaces`
Expected: FAIL — no method `list_spaces` on `Brain`.

- [ ] **Step 3: Implement**

`models.rs`:

```rust
/// A space as seen by callers: identity plus live counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub id: String,
    pub name: String,
    pub created_at: Timestamp,
    pub episode_count: i64,
    pub entity_count: i64,
}
```

`lib.rs` (read path — use the file's `read_op!` macro exactly as other read methods do; shown here conceptually):

```rust
/// List all spaces with counts, ordered by (created_at, id).
pub async fn list_spaces(&self) -> Result<Vec<SpaceInfo>, BrainError> {
    let h = self.handle.clone();
    tokio::task::spawn_blocking(move || {
        read_op!(h, |conn| {
            ledger::list_spaces(conn)?
                .into_iter()
                .map(|r| SpaceInfo {
                    id: r.id,
                    name: r.name,
                    created_at: Timestamp::from_millis(r.created_at),
                    episode_count: r.episode_count,
                    entity_count: r.entity_count,
                })
                .collect()
        })
    })
    .await
    .map_err(|e| BrainError::Storage(format!("join: {e}")))?
}
```

IMPORTANT: read the `read_op!` macro definition (lib.rs lines 17-23) and match its real invocation shape used by sibling read methods; do not invent a new pattern. Add `SpaceInfo` to the crate-root re-exports (`pub use models::{...}`) and to `compat.rs` `_check_methods` as `let _ = Brain::list_spaces;` plus a `_check_types` entry for `SpaceInfo`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oxibrain`
Expected: PASS, including `compat_surface_compiles`.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain/src/models.rs crates/oxibrain/src/lib.rs crates/oxibrain/src/compat.rs
git commit -m "feat(facade): Brain::list_spaces + SpaceInfo on the stable surface"
```

---

### Task 3: MCP — `spaces/list` RPC, `spaces://` resource, scope rules, resources bypass fix

**Files:**
- Modify: `crates/oxibrain-mcp/src/server.rs` — dispatch (`handle`, ~line 208), `resources_list` (~line 953), `resources_read` (~line 1000), plus a new helper near `enforce_scope` (~line 134)
- Test: same file's `#[cfg(test)] mod tests` (existing helpers: `fresh_server`, `fresh_scoped`, `msg`)

**Interfaces:**
- Consumes: `Brain::list_spaces` (Task 2).
- Produces: raw JSON-RPC method `spaces/list` → result `[{ "id", "name", "created_at", "episode_count", "entity_count" }]`; MCP static resource `spaces://` with identical JSON; scope filtering on both; `resources/read` now scope-gated for every scheme.

- [ ] **Step 1: Write the failing tests** (all in the existing tests module, following its style)

```rust
#[tokio::test]
async fn spaces_list_rpc_lists_all_when_unscoped() {
    let (dir, server) = fresh_server().await;
    let _ = server.brain.ensure_space("work").await.unwrap();
    let resp = server.handle(msg(1, "spaces/list", None)).await.unwrap();
    let arr = resp["result"]["spaces"].as_array().unwrap();
    assert!(arr.iter().any(|s| s["name"] == json!("work")));
    drop(dir);
}

#[tokio::test]
async fn spaces_list_rpc_scoped_filters_to_membership() {
    // fresh_scoped(&[Capability::Read], &["alpha"]) — pattern exists at ~line 2568
    let (dir, server) = fresh_scoped(&[Capability::Read], &["alpha"]).await;
    let _ = server.brain.ensure_space("beta").await.unwrap();
    let resp = server.handle(msg(1, "spaces/list", None)).await.unwrap();
    let names: Vec<&str> = resp["result"]["spaces"]
        .as_array().unwrap()
        .iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["alpha"]);
    drop(dir);
}

#[tokio::test]
async fn resources_read_denies_foreign_space_under_scope() {
    // Security regression: resources/read previously bypassed enforce_scope.
    let (dir, server) = fresh_scoped(&[Capability::Read], &["alpha"]).await;
    let _ = server.brain.ensure_space("beta").await.unwrap();
    let resp = server.handle(msg(1, "resources/read",
        Some(json!({ "uri": "space://beta" })))).await.unwrap();
    assert!(resp["error"].is_object(), "expected denial, got: {resp}");
    drop(dir);
}

#[tokio::test]
async fn spaces_resource_lists_scoped_spaces() {
    let (dir, server) = fresh_scoped(&[Capability::Read], &["alpha"]).await;
    let _ = server.brain.ensure_space("beta").await.unwrap();
    let resp = server.handle(msg(1, "resources/read",
        Some(json!({ "uri": "spaces://" })))).await.unwrap();
    let text = resp["result"]["contents"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(text).unwrap();
    let names: Vec<&str> = v["spaces"].as_array().unwrap()
        .iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["alpha"]);
    drop(dir);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p oxibrain-mcp spaces`
Expected: FAIL — unknown method / no `spaces` key / foreign read succeeds (the bypass).

- [ ] **Step 3: Implement**

(a) Helper (near `enforce_scope`):

```rust
/// Spaces the current session may see. `None` scope (trusted local
/// channel) sees all; a scoped session sees only its membership.
async fn visible_spaces(&self) -> Result<Vec<SpaceInfo>, (i64, String)> {
    let all = self.brain.list_spaces().await
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?;
    if let Some(scope) = &self.scope {
        return Ok(all.into_iter().filter(|s| scope.spaces.contains(&s.id)).collect());
    }
    Ok(all)
}

/// Scope gate for resource reads: spaces are hard boundaries (§15.1) and
/// resources are queries. Requires Read capability when a scope is present.
async fn enforce_scope_resource(&self, space_name: &str) -> Result<(), (i64, String)> {
    let Some(scope) = &self.scope else { return Ok(()) };
    if !scope.caps.contains(Capability::Read) {
        return Err((UNAUTHORIZED, "token lacks read capability".into()));
    }
    let id = self.brain.ensure_space(space_name).await
        .map_err(|e| (INTERNAL_ERROR, format!("ensure_space: {e}")))?;
    if !scope.spaces.contains(&id) {
        return Err((UNAUTHORIZED, format!("token not scoped to space '{space_name}'")));
    }
    Ok(())
}
```

(b) Dispatch arm beside `"reproject"`:

```rust
"spaces/list" => match msg.id {
    Some(id) => match self.visible_spaces().await {
        Ok(v) => Some(success(id, json!({ "spaces": v }))),
        Err((code, m)) => Some(error(id, code, m)),
    },
    None => None,
},
```

(c) `resources_list`: add to the static `resources` array:

```rust
{
    "uri": "spaces://",
    "name": "All spaces",
    "description": "Spaces this session may see: id, name, created_at, episode/entity counts.",
    "mimeType": "application/json"
}
```

(d) `resources_read`: at the top, after resolving `space_name` (the existing code already computes it: path for `space`, `?space=` otherwise — extend the scheme check so `spaces` maps to the default/personal and skip gating for `spaces://`, which self-filters), call `enforce_scope_resource(&space_name)?` for every non-`spaces` scheme, then add the arm:

```rust
"spaces" => {
    let list = self.visible_spaces().await
        .map_err(|(code, m)| (code, m))?;
    serde_json::to_string_pretty(&json!({ "spaces": list }))
        .unwrap_or_default()
}
```

Match the function's existing return/error conventions exactly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p oxibrain-mcp`
Expected: PASS — including the existing resource contract tests (`space_resource_contract_exact_keys` etc.) and scope tests.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-mcp/src/server.rs
git commit -m "feat(mcp): spaces/list native RPC + spaces:// resource; fix resources scope bypass"
```

---

### Task 4: Client — `call_rpc_json` + `SpaceSummary` + `list_spaces()`

**Files:**
- Modify: `crates/oxibrain-client/src/lib.rs` (helper beside `ping` ~line 376; DTO + method in the impl block)
- Test: beside the existing client↔server round-trip tests — locate them first (they live in the `oxibrain-mcp` crate per repo history; `glob crates/oxibrain-mcp/tests/*`) and follow that file's spawn-and-connect pattern

**Interfaces:**
- Consumes: server `spaces/list` RPC (Task 3).
- Produces: `pub struct SpaceSummary { pub id: String, pub name: String, pub created_at_ms: i64, pub episode_count: i64, pub entity_count: i64 }` (derives `Debug, Clone, Serialize, Deserialize`) in `oxibrain_client`; `impl BrainClient { pub async fn call_rpc_json(&mut self, method: &str, params: Value) -> Result<Value>; pub async fn list_spaces(&mut self) -> Result<Vec<SpaceSummary>> }`.

- [ ] **Step 1: Write the failing test** (in the round-trip test file found above)

```rust
#[tokio::test]
async fn client_lists_spaces_over_socket() {
    // Follow the file's existing pattern: tempdir store, Brain::open,
    // ensure_space("work"), spawn the server on a temp Unix socket,
    // BrainClient::connect, then:
    let spaces = client.list_spaces().await.unwrap();
    assert!(spaces.iter().any(|s| s.name == "work"));
    assert!(spaces.iter().all(|s| !s.id.is_empty()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oxibrain-mcp client_lists_spaces`
Expected: FAIL — no method `list_spaces` on `BrainClient`.

- [ ] **Step 3: Implement**

```rust
/// Send a raw JSON-RPC request (non-tool method, e.g. `spaces/list`) and
/// return the parsed `result`. Protocol errors map to `Err`.
pub async fn call_rpc_json(&mut self, method: &str, params: Value) -> Result<Value> {
    let id = self.alloc_id();
    let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    self.send(&req).await?;
    let resp = self.recv().await?;
    if let Some(err) = resp.get("error") {
        bail!("{}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error"));
    }
    resp.get("result").cloned().context("missing result in response")
}

/// Enumerate spaces the daemon exposes to this session (native RPC — not an
/// MCP tool). Millis on the wire; convert to your own time type.
pub async fn list_spaces(&mut self) -> Result<Vec<SpaceSummary>> {
    let v = self.call_rpc_json("spaces/list", json!({})).await?;
    serde_json::from_value(v.get("spaces").cloned().unwrap_or(json!([])))
        .context("parse spaces/list result")
}
```

`SpaceSummary` goes at module level with the doc comment `/// A space as enumerated by [`BrainClient::list_spaces`] — client-owned DTO, no engine types.`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p oxibrain-client && cargo test -p oxibrain-mcp client`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-client/src/lib.rs
git commit -m "feat(client): typed list_spaces over the native spaces/list RPC"
```

---

### Task 5: CLI — `oxibrain spaces`

**Files:**
- Modify: `crates/oxibrain-cli/src/cli.rs` (new `Spaces` variant — no args)
- Create: `crates/oxibrain-cli/src/cmd/spaces.rs`
- Modify: `crates/oxibrain-cli/src/cmd/mod.rs` (register module)
- Modify: `crates/oxibrain-cli/src/main.rs` (dispatch arm)

**Interfaces:**
- Consumes: `Brain::list_spaces` (Task 2).
- Produces: `pub async fn run(dir: &Path) -> anyhow::Result<()>` in `cmd::spaces`; CLI verb `oxibrain spaces`.

- [ ] **Step 1: Write the failing test** (inline `#[cfg(test)]` in `cmd/spaces.rs`; check sibling cmd modules for the tempdir convention)

```rust
#[tokio::test]
async fn spaces_prints_table() {
    let dir = tempfile::TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let _ = brain.ensure_space("work").await.unwrap();
    drop(brain);
    // run() prints; assert it does not error and (if captured) contains "work".
    cmd::spaces::run(dir.path()).await.unwrap();
}
```

(If sibling commands capture output via a helper, use it; otherwise asserting no-error plus a direct `Brain::list_spaces` precondition is acceptable.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p oxibrain-cli spaces`
Expected: FAIL — module `cmd::spaces` does not exist.

- [ ] **Step 3: Implement**

`cmd/spaces.rs`:

```rust
//! `oxibrain spaces` — list every space with live counts.
//!
//! Read-only: opens the store with `Brain::open_ro`, so it takes no
//! advisory lock and coexists with a running daemon (§16.1).

use anyhow::Result;
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> Result<()> {
    let brain = Brain::open_ro(BrainConfig::at(dir)).await?;
    let spaces = brain.list_spaces().await?;
    println!("{:<24} {:<16} {:<12} {:>9} {:>9}", "NAME", "ID", "CREATED", "EPISODES", "ENTITIES");
    for s in &spaces {
        println!(
            "{:<24} {:<16} {:<12} {:>9} {:>9}",
            s.name, &s.id[..s.id.len().min(16)],
            millis_to_iso(s.created_at.millis()), s.episode_count, s.entity_count
        );
    }
    Ok(())
}

/// Minimal UTC formatting without a chrono dependency (store-independent).
fn millis_to_iso(ms: i64) -> String {
    // days-since-epoch civil conversion (Howard Hinnant's algorithm)
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
```

`cli.rs` variant: `/// List all spaces with counts. Read-only; safe with a running daemon.` `Spaces,` — placed near `Stats`. `main.rs` arm: `Command::Spaces => cmd::spaces::run(&dir).await,`.

- [ ] **Step 4: Run test to verify it passes; smoke the binary**

Run: `cargo test -p oxibrain-cli && cargo run -q -p oxibrain-cli -- --dir /tmp/oxibrain-smoke spaces`
Expected: test PASS; binary prints the header (empty list on a fresh dir is fine — note: if `--dir` points at an uninitialized path, verify `open_ro` errors gracefully; if it hard-fails on a missing store, keep the error message clear and document that `init` is required first).

- [ ] **Step 5: Commit**

```bash
git add crates/oxibrain-cli/src/cli.rs crates/oxibrain-cli/src/cmd/spaces.rs crates/oxibrain-cli/src/cmd/mod.rs crates/oxibrain-cli/src/main.rs
git commit -m "feat(cli): oxibrain spaces — read-only space listing"
```

---

### Task 6: Docs + full gates

**Files:**
- Modify: `doc/ARCHITECTURE.md` — header (version v2.10, date 2026-08-20, v2.10 bullet), §15.1 (enumeration + resource gating sentence), §16.1 (two-surface statement per ADR-009), §16.2 (resources line + native-RPC paragraph), §16.4 (`oxibrain spaces` line)
- Modify: `doc/CONSUMPTION_CONTRACT.md` — `Brain::list_spaces() -> Result<Vec<SpaceInfo>>` beside `ensure_space`; `SpaceInfo` in the types list

**Interfaces:** none (documentation).

- [ ] **Step 1: Edit ARCHITECTURE.md**

Header block: bump `**Version:** v2.9` → `v2.10`, date → 2026-08-20, and insert above the v2.9 bullet:

```
> **v2.10 — Space enumeration and first-party RPC.** Spaces are enumerable
> (`Brain::list_spaces`, `oxibrain spaces`, `spaces://` resource, native
> `spaces/list` RPC) — scoped sessions see only their membership, and
> `resources/read` is now scope-gated like tools (the gap was found and fixed
> with tests). §16.1 is aligned with ADR-009: `Brain` is the embedded surface,
> `oxibrain-client` the remote surface; the one-trait unification is post-v1
> with a stated trigger. The fifteen-tool MCP cap is unchanged — first-party
> operations ride the native RPC layer.
```

§15.1, append to the hard-boundary paragraph: "Enumeration and resources obey the same boundary: a scoped session lists and reads only the spaces in its scope; resource reads are gated exactly like tool calls."

§16.1, replace the sentence `` `Brain` is one trait in both modes: a consumer changes topology by changing one line. `` with: "Two typed surfaces (ADR-009): `Brain` is the embedded surface — full API including port injection; `oxibrain-client::BrainClient` is the remote surface for daemon topology (ECOSYSTEM C6). Unification is post-v1, triggered by a consumer needing runtime topology switching; LLM-injecting methods cannot cross a process boundary by construction."

§16.2: extend the Resources line to `spaces://` (list), `space://`, `entity://{id}`, `episode://{id}`, `graph://{entity}?depth=n`; after the tools-table paragraph add: "Native JSON-RPC methods — `handshake`, `reproject`, `spaces/list` — are the first-party surface; they are not MCP tools and do not count against the cap."

§16.4: add a line `oxibrain spaces                         # list spaces with counts (read-only)` after `oxibrain stats`.

- [ ] **Step 2: Edit CONSUMPTION_CONTRACT.md**

Under the first stable-surface group (beside `ensure_space`): `- `Brain::list_spaces() -> Result<Vec<SpaceInfo>>``. In the Types list append `SpaceInfo`.

- [ ] **Step 3: Full gates**

Run each; all must pass before claiming done:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1 || true
```

- [ ] **Step 4: Commit**

```bash
git add doc/ARCHITECTURE.md doc/CONSUMPTION_CONTRACT.md doc/adr/ADR-009-brain-topology-deferred.md docs/superpowers/specs/2026-08-20-space-enumeration-design.md
git commit -m "docs: v2.10 space enumeration surfaces, ADR-009 topology deferral, resources scope rule"
```

---

## Self-review notes

- Task ordering encodes the dependency chain: 1 → 2 → 3 → (4, 5) → 6. Tasks 4 and 5 are independent of each other.
- The `read_op!` and round-trip-test integration points are named, not guessed — implementers must read the adjacent code and match shapes.
- No task touches the fifteen tools list, the predicate registry, or any write path of the ledger.
