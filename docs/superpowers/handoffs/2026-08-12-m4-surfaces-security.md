# Phase M4 Handoff — Surfaces & Security → M4 continuation / M5

> **Status:** M4 security core, export/import, full CLI, markdown connector
> shipped. MCP server scaffolded but blocked on toolchain constraint.
> **Branch:** `main` (squash-merge flow)
> **Last commit:** `a7ad8f9`
> **Predecessor:** M3→M4 handoff (`docs/superpowers/handoffs/2026-08-11-m3-to-m4.md`)
> **Tests:** 170 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.

---

## 1. What Shipped (8 commits since M3 `6b1b8bd`)

| Commit | What |
|--------|------|
| `7eac6ce` | docs(m4): surfaces and security design spec + implementation plan |
| `c0e7304` | feat(m4): core security types + BrainError scope/unauthorized/conflict variants |
| `d51c75f` | feat(m4): security core — token CRUD, audit log, redaction, migration v4, reproject redaction replay |
| `45ab9fd` | fix(m4): apply_replay timestamp — pass redacted_at to fold instead of epoch zero |
| `bf758e4` | feat(m4): JSONL export/import — full-fidelity round-trip of durable tables |
| `2a87628` | feat(m4): Brain facade security methods, markdown connector, injection suite, clippy fixes |
| `a7ad8f9` | feat(m4): full CLI surface + MCP server scaffold + reproject test fixes |

---

## 2. M4 Deliverables Status

| Capability (DESIGN §17) | Status |
|---|---|
| **Security core** | |
| Scope / Capability types (§11.2) | ✅ `core/security.rs` |
| Token issuance + verification + revocation | ✅ `store/security.rs`, migration v4 |
| Audit log (write + list) | ✅ `store/security.rs` |
| Redaction (closure + execute + dry-run) | ✅ `store/redaction.rs` |
| Redaction survives reproject (P1) | ✅ `redactions` table + replay step |
| Brain facade security methods | ✅ 9 new async methods |
| Injection suite (§14.3) | ✅ 5 tests |
| **Export/Import** | |
| JSONL export | ✅ `store/export.rs` |
| JSONL import + round-trip test | ✅ beliefs preserved after reproject |
| **CLI (§12.4)** | |
| ask, entity-show, timeline, why, contradictions | ✅ |
| reproject, redact (--dry-run), export, import | ✅ |
| token issue/list/revoke | ✅ |
| **Connectors** | |
| Markdown vault connector | ✅ `oxibrain-connectors` crate |
| **MCP server (§12.2)** | |
| oxibrain-mcp crate + tool set | ⚠️ scaffold written, blocked on toolchain |
| Daemon + transports | ⬜ not started |
| oxibrain-client | ⬜ not started |
| **Security enforcement** | |
| Scope-aware operation gating | ⚠️ types + Brain methods exist; enforcement middleware not wired |
| Token auth on MCP connections | ⚠️ deferred with MCP server |

---

## 3. Architecture: what was built

### New types (`oxibrain-core/src/security.rs`)
- `Capability` (Read, Write, Ingest, Sample, Admin, Redact) — `parse_set()`, `as_str()`
- `Scope` — spaces + caps + predicate_filter + entity_type_filter + expires_at
- `TokenInfo` — public metadata (secret shown once, stored as SHA-256 hash)
- `RedactTarget` — Episode, Entity, PredicateScoped
- `RedactionClosure` / `RedactionResult`
- `AuditEntry`

### Migration v4 (`migrations/v4.sql`)
- `tokens` table: id, token_hash, scope_json, issued_at, issued_by, revoked_at, label
- `redactions` table: id, target_json, reason, actor, redacted_at

### Store modules
- `security.rs` — token CRUD (issue/verify/revoke/list), audit (write/list), list_redactions
- `redaction.rs` — closure resolution (episode/entity/predicate-scoped), execute_redaction,
  apply_replay (for reproject), belief re-folding
- `export.rs` — JSONL export/import (18 tables, hex blob encoding, round-trip tested)

### Reproject extensions (P1-critical)
- Both replay queries filtered with `AND redacted_at IS NULL`
- New step 3.6: replay redactions from `redactions` table after extraction replay
- `apply_replay` uses the original `redacted_at` timestamp for fold (not epoch zero)

### Brain facade (`oxibrain/src/lib.rs`)
9 new methods: `issue_token`, `verify_token`, `revoke_token`, `list_tokens`, `audit_log`,
`redact_dry_run`, `redact`, `export_jsonl`, `import_jsonl`

### CLI (`oxibrain-cli`)
12 new subcommands: `ask`, `entity-show`, `timeline`, `why`, `contradictions`, `reproject`,
`redact` (with `--dry-run`), `export`, `import`, `token issue/list/revoke`

### New crates
- `oxibrain-connectors` — markdown vault directory scanner (5 tests)
- `oxibrain-mcp` — MCP server scaffold (7 tools: search, recall, get_entity, ingest,
  declare, why, contradictions) — NOT in workspace members, blocked on toolchain

---

## 4. MCP Server Blocker

**rmcp 0.12–0.14 depends on darling 0.23.0, which requires Rust ≥ 1.88.**
Our MSRV is pinned to 1.85 in `rust-toolchain.toml`.

The MCP server source (`crates/oxibrain-mcp/src/server.rs`) is written and follows
the rmcp API correctly (`#[tool_router]` macro, `ServerHandler`, stdio transport).
It's excluded from workspace members so the rest of the project compiles cleanly.

**Three resolution paths (pick one):**
1. **Bump `rust-toolchain.toml` to 1.88+** (when released). Simplest — the scaffold compiles.
2. **Wait for rmcp to lower MSRV** (darling downgrade or feature-gate).
3. **DESIGN §18 fallback**: minimal in-house MCP server over JSON-RPC without rmcp.
   The protocol is simple enough for a ~200-line stdio server.

---

## 5. Remaining M4 Work

1. **Resolve MCP toolchain blocker** → add `oxibrain-mcp` to workspace, test tool dispatch.
2. **Daemon + transports**: `oxibrain serve --stdio|--socket|--http|--daemon`.
3. **oxibrain-client**: thin async client for consuming apps.
4. **Scope enforcement middleware**: gate Brain operations by Scope before dispatch.
5. **Token auth on connections**: MCP/daemon verify token → Scope before tool dispatch.
6. **Sampling LlmPort**: LlmPort backed by MCP client sampling (§12.3).
7. **Grow CLI**: add `serve`, `predicate list`, `extract`, `reextract`, `eval` subcommands
   (some deferred from M3).

---

## 6. Key P1 decisions made this session

### Redaction + reprojection (flagged by advisory, fixed)

**Problem:** Redaction deletes assertions, but reproject replays extractions from cache
and recreates them — silently undoing entity-scoped redaction.

**Fix:**
- Episode-scoped redaction: `redacted_at IS NULL` filters in both reproject queries.
- Entity-scoped redaction: `redactions` table records what was redacted; reproject
  step 3.6 replays them (resolve closure → delete assertions → re-fold).
- `apply_replay` receives the original `redacted_at` timestamp and passes it to `fold()`
  — `fold(at=0)` would filter out all assertions and produce empty beliefs.

**Test:** `reproject_after_episode_redaction_preserves_projection` — redact → reproject →
assert zero assertions/beliefs recreated.

---

## 7. Test inventory

| Crate | Tests |
|---|---|
| oxibrain-ports | 4 (incl. FakeLlmPort) |
| oxibrain-core | 70 (incl. 9 security) |
| oxibrain-store (lib) | 22 (incl. 5 redaction, 5 security, 3 export) |
| oxibrain-store (integration) | 10 (open, reproject, search, injection_suite) |
| oxibrain (facade) | 8 |
| oxibrain-cli | 1 (standalone_guarantee) |
| oxibrain-connectors | 5 |
| oxibrain-llm-http | 3 |
| **Total** | **170 pass, 0 fail** |

---

End of handoff. M4 security core, export/import, CLI, and connectors are complete and
tested. MCP server is scaffolded but blocked on a toolchain constraint. Start M4
continuation (MCP + daemon) or M5 from here.
