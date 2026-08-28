# Agent-First CLI — Phase 1 (op registry) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the hand-written MCP tool catalogue into a new `oxibrain-ops` registry crate and generate `tools/list` from it **byte-identically** (golden-fixture guarded).

**Architecture:** One `OpSpec` per op in `oxibrain-ops`; `oxibrain-mcp`'s `tool_list()` delegates to the registry. Zero behavior change — this phase only removes the schema duplication that ADR-012 deletes. `space` stays optional, the tool set stays the current 15 (cutover is P3).

**Tech Stack:** Rust 2024, serde_json (workspace), no new dependencies beyond serde_json.

## Global Constraints

- `doc/ARCHITECTURE.md` v2.13 and `doc/spec/agent-first-cli-v1.md` are the contract; Phase 1 must not change any observable output of `tools/list` (byte-identical JSON, same key order).
- clippy clean with `-D warnings`; no `unwrap` outside tests (`#![cfg_attr(test, allow(clippy::unwrap_used))]` if needed).
- English comments/doc-comments. Conventional commits.
- The golden fixture is blessed from the **pre-refactor** code and never hand-edited afterwards.

---

### Task 1: Bless the golden fixture from current code

**Files:**
- Create: `crates/oxibrain-ops/tests/golden_tools_list.json` (blessed output)
- Temporary: `crates/oxibrain-mcp/src/server.rs` (a `#[cfg(test)]` bless test, removed after blessing)

**Interfaces:**
- Produces: the fixture file every later task asserts against.

- [ ] **Step 1: Create the ops crate skeleton with the fixture directory**

`crates/oxibrain-ops/Cargo.toml`:

```toml
[package]
name = "oxibrain-ops"
edition.workspace = true
version.workspace = true
license.workspace = true
description = "Agent-facing op registry — single source of truth for MCP tools, CLI dispatch, and schemas (ADR-012)"
repository.workspace = true
rust-version.workspace = true
readme = "../../README.md"
keywords = ["agent", "ops", "registry", "mcp"]
categories = ["development-tools"]

[dependencies]
serde_json.workspace = true
```

`crates/oxibrain-ops/src/lib.rs` (minimal for now):

```rust
//! The agent-facing op registry (ADR-012, `doc/spec/agent-first-cli-v1.md`).
//!
//! One `OpSpec` per operation. MCP `tools/list`, the CLI `oxibrain <op>`
//! dispatch, `oxibrain schema`, and the generated SKILL.md all derive from
//! this registry — there are no hand-maintained schema duplicates.
```

Add `"crates/oxibrain-ops"` to workspace members and
`oxibrain-ops = { path = "crates/oxibrain-ops", version = "0.9.0" }` to
`[workspace.dependencies]` in the root `Cargo.toml`.

- [ ] **Step 2: Add a temporary bless test to oxibrain-mcp**

In `crates/oxibrain-mcp/src/server.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
// TEMPORARY (Phase 1 blessing): writes the golden fixture from the current
// hand-written catalogue. Run once with BLESS=1, then remove this test.
#[test]
fn bless_golden_tools_list() {
    if std::env::var("BLESS").is_err() {
        return;
    }
    let fixture = serde_json::to_string_pretty(&tool_list()).unwrap();
    std::fs::create_dir_all("../../crates/oxibrain-ops/tests").unwrap();
    std::fs::write("../../crates/oxibrain-ops/tests/golden_tools_list.json", fixture + "\n")
        .unwrap();
}
```

- [ ] **Step 3: Bless and verify**

Run: `BLESS=1 cargo test -p oxibrain-mcp bless_golden_tools_list`
Expected: PASS; `crates/oxibrain-ops/tests/golden_tools_list.json` exists, starts with `{`, contains `"tools"` and 15 tool names.

- [ ] **Step 4: Remove the temporary bless test** from `server.rs`.

### Task 2: The OpSpec registry

**Files:**
- Modify: `crates/oxibrain-ops/src/lib.rs`
- Test: `crates/oxibrain-ops/tests/golden.rs`

**Interfaces:**
- Produces: `pub struct OpSpec { pub name, pub summary, pub caps, pub mutating }`, `pub fn ops() -> &'static [OpSpec]`, `pub fn tools_list() -> serde_json::Value`, `pub fn find(name: &str) -> Option<&'static OpSpec>`. Later phases consume these (CLI dispatch in P2, schema gen in P2, cap enforcement in P3).

- [ ] **Step 1: Write the failing golden test**

`crates/oxibrain-ops/tests/golden.rs`:

```rust
//! The registry's first output must be byte-identical to the pre-refactor
//! hand-written catalogue (ADR-012 migration guard). The fixture was blessed
//! from `oxibrain-mcp`'s hand-written `tool_list()` before the move.

#[test]
fn tools_list_matches_blessed_fixture() {
    let generated = serde_json::to_string_pretty(&oxibrain_ops::tools_list()).unwrap();
    let fixture = include_str!("golden_tools_list.json");
    assert_eq!(generated.trim(), fixture.trim());
}

#[test]
fn registry_has_fifteen_unique_ops_with_schemas() {
    let list = oxibrain_ops::tools_list();
    let tools = list["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15, "P1 keeps the current 15; the P3 cutover moves to 14");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names.len(), sorted.len(), "tool names must be unique");
    names.clear();
    for op in oxibrain_ops::ops() {
        assert!(tools.iter().any(|t| t["name"] == op.name), "{} missing from tools/list", op.name);
        assert_eq!(tools.iter().find(|t| t["name"] == op.name).unwrap()["inputSchema"]["type"], "object");
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p oxibrain-ops`
Expected: compile error — `tools_list`/`ops` not defined.

- [ ] **Step 3: Implement the registry**

Move the 15 tool entries **verbatim** from `crates/oxibrain-mcp/src/server.rs:1489-1665` (the `json!` bodies and description strings, unchanged). Shape:

```rust
use serde_json::{json, Value};

/// One agent-facing operation. The single source of truth for the MCP tool
/// catalogue, the CLI op dispatch, `oxibrain schema`, and the SKILL.md
/// generator (ADR-012). `schema` becomes registry-aware at P3 (live
/// predicate enums); P1 keeps the schemas static and byte-identical.
pub struct OpSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub schema: fn() -> Value,
    /// Declared capability names ("Read" | "Ingest" | "Write" | "Redact").
    /// Metadata through P5; enforcement (capability-filtered listings) is P6.
    pub caps: &'static [&'static str],
    /// Mutating ops participate in the dry-run/plan-token rails (P6).
    pub mutating: bool,
}

fn search_schema() -> Value { json!({ /* verbatim from server.rs:1493-1506 */ }) }
// … one schema fn per op, bodies verbatim from server.rs:1489-1665 …

pub const OPS: &[OpSpec] = &[
    OpSpec {
        name: "search",
        summary: "Search the brain across two planes and return the envelope {memory, documents, freshness}. …", // verbatim
        schema: search_schema,
        caps: &["Read"],
        mutating: false,
    },
    // … 14 more, in the exact tools/list order: recall, brief, navigate,
    // ingest, declare, why, contradictions, stats, traverse, review_merges,
    // remember, retract, merge_entities, redact …
];

pub fn ops() -> &'static [OpSpec] { OPS }

pub fn find(name: &str) -> Option<&'static OpSpec> {
    OPS.iter().find(|op| op.name == name)
}

pub fn tools_list() -> Value {
    json!({
        "tools": OPS.iter().map(|op| json!({
            "name": op.name,
            "description": op.summary,
            "inputSchema": (op.schema)(),
        })).collect::<Vec<_>>()
    })
}
```

Caps/mutating values per §16.2: `search/recall/brief/navigate/why/contradictions/stats/traverse/review_merges` → `Read`; `ingest` → `Ingest`; `remember/declare/retract/merge_entities` → `Write`, mutating; `redact` → `Redact`, mutating.

- [ ] **Step 4: Run tests**

Run: `cargo test -p oxibrain-ops`
Expected: both tests PASS (golden byte-identical).

### Task 3: oxibrain-mcp generates from the registry

**Files:**
- Modify: `crates/oxibrain-mcp/Cargo.toml` (add `oxibrain-ops.workspace = true`)
- Modify: `crates/oxibrain-mcp/src/server.rs:1487-1666` (delete the hand-written catalogue; `tool_list` delegates)

**Interfaces:**
- Consumes: `oxibrain_ops::tools_list()`.

- [ ] **Step 1: Add the dependency and delegate**

```rust
/// The advertised tool list, generated from the `oxibrain-ops` registry
/// (ADR-012). Byte-identical to the v2.12 hand-written catalogue — guarded
/// by `oxibrain-ops`' golden fixture test.
fn tool_list() -> Value {
    oxibrain_ops::tools_list()
}
```

Delete `tool()` helper and all 15 inline entries. `fn tool(...)` becomes dead — remove it. The `"tools/list" => success(id, tool_list())` call site at server.rs:343 is unchanged.

- [ ] **Step 2: Run the full gates**

Run: `cargo test -p oxibrain-mcp && cargo build && cargo test`
Expected: all green — including `tools_list_advertises_full_surface` (15 tools through the real session handler).

- [ ] **Step 3: Full workspace gates**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check && cargo build -p oxibrain --no-default-features --features http-llm && cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'; test $? -eq 1`
Expected: clippy/fmt clean; standalone guarantee holds (grep finds nothing).

- [ ] **Step 4: Commit**

```bash
git add crates/oxibrain-ops crates/oxibrain-mcp Cargo.toml docs/superpowers/plans/2026-08-28-agent-first-cli-p1.md doc/
git commit -m "feat: extract op registry (oxibrain-ops), generate tools/list byte-identically (ADR-012 P1)"
```
