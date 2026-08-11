# oxibrain M4 — Surfaces & Security Design Spec

> **Date:** 2026-08-12
> **Authority:** `doc/DESIGN.md` v1.0 (§§11, 12, 13.1, 14.3, 15, 17). This spec
> scopes and concretizes M4. Where this spec and DESIGN.md disagree, DESIGN.md wins
> unless this spec explicitly records a deviation (§12).
> **Predecessor:** M3 Extraction & Evaluation (complete — see
> `docs/superpowers/specs/2026-08-11-oxibrain-m3-extraction-eval-design.md`).
> **Status:** Design. Drives the M4 implementation plan.

---

## 1. Goal

The surfaces and security milestone: oxibrain leaves embedded-only mode and
becomes a networked service with authentication, authorization, and the full
product surface set. M4 delivers:

- **Security core** — spaces enforcement, scopes/capabilities, token issuance
  and verification, audit log, trust tier enforcement, redaction (the only true
  delete), and the injection test suite (§14.3).
- **MCP server** — `oxibrain-mcp` crate on `rmcp` 3.x, exposing the full tool
  set (§12.2) with token authentication and client-sampling `LlmPort`.
- **Daemon + transports** — `oxibrain serve --stdio|--socket|--http|--daemon`;
  one writer per store via advisory lock (P8).
- **oxibrain-client** — thin async client for consuming apps to talk to a
  running daemon.
- **Full CLI** — every subcommand from §12.4: `ask`, `entity`, `timeline`,
  `why`, `contradictions`, `review`, `reextract`, `reproject`, `redact`,
  `export`, `import`, `serve`, `token`, `eval`, `predicate`.
- **Export/import** — full-fidelity JSONL round-trip (§12.5).
- **Markdown vault connector** — `oxibrain-connectors` crate; reads a directory
  of `.md` files into episodes.

The exit condition is a brain that Claude Desktop connects to over a scoped
token, two apps share one brain through the daemon, redaction is verified end to
end, and a first ecosystem app integrates read-only.

## 2. M4 Exit Criteria (DESIGN §17)

1. Claude Desktop uses oxibrain as memory over a scoped token.
2. Two apps share one brain through the daemon.
3. Redaction closures verified — `redact --dry-run` prints the closure;
   `redact` executes it; `doctor --check-orphans` finds nothing.
4. A first ecosystem app integrates read-only (oxibrain-client, Read capability).
5. Export → import → reproject yields a byte-identical projection.
6. Injection suite passes (§14.3) — instruction-shaped text never escapes the
   validator; trust weighting holds.
7. M1 + M2 + M3 exit criteria still hold.

---

## 3. Scope

### 3.1 In M4

| Capability | Detail |
|---|---|
| Scope / Capability types | `Capability` enum (Read, Write, Ingest, Sample, Admin, Redact); `Scope` struct (spaces, caps, filters, expiry) — DESIGN §11.2 |
| Token issuance | `issue_token(scope) -> TokenId`; random opaque token; stored in `tokens` table |
| Token verification | `verify_token(token_hash) -> Option<Scope>`; bcrypt or SHA-256 hash at rest |
| Token revocation | `revoke_token(id)`; `list_tokens()` |
| Audit log | Write every write/redact/merge/token-issue/scope-grant/sampling-auth/config-change to `audit_log` (table exists from M0) |
| Scope enforcement | Middleware wrapper: every Brain write method checks the caller's Scope before proceeding; `BrainError::Scope{required}` |
| Redaction | Resolve closure → audit → tombstone → delete mentions/assertions/statements → re-fold → rebuild indexes → verify no orphans (§11.5) |
| Injection suite | Property + example tests: instruction-shaped episode text; assert validator blocks, trust weighting holds |
| MCP server | `oxibrain-mcp` crate on `rmcp`; tool set from §12.2; token auth; sampling `LlmPort` adapter |
| Daemon | `oxibrain serve --stdio\|--socket\|--http\|--daemon`; transports; one writer |
| oxibrain-client | Thin async client; `Brain` trait unified embedded vs daemon (P6) |
| Full CLI | All §12.4 subcommands |
| Export/import | JSONL of ledger + cache + audit; round-trip tested |
| Markdown connector | `oxibrain-connectors` crate; directory of `.md` → episodes |

### 3.2 Deferred to M5+

| Deferred | Milestone | Why |
|---|---|---|
| Cross-space `shared` resolution | post-v1 | DESIGN §11.1 explicitly states "not implemented in v1" |
| SQLCipher at-rest encryption | M5+ | Feature flag; complicates backup; documented as off-by-default (§11.6) |
| Sync / ledger log shipping | post-v1 | DESIGN §11.6: "Sync is post-v1" |
| Subscriptions (push) | M5+ | Transport-neutral subscriptions are an enhancement; polling is sufficient for v1 |
| Dense GGUF embeddings | M5 | Heavy native dependency; TF-IDF remains default |

### 3.3 The security model in one paragraph

Every operation is scoped. A **token** carries a **Scope** (spaces +
capabilities + filters + expiry). Tokens are issued by an Admin-capable caller,
stored as SHA-256 hashes (never plaintext after issuance), and verified on every
request. The daemon authenticates incoming connections by token; the embedded
library skips authentication (same process = trusted). Every write — manual
(`declare`, `merge`, `retract`, `redact`) or derived (extraction) — writes an
audit entry before acting. Spaces are hard boundaries: no query, traversal, or
write crosses one, enforced at the store layer.

---

## 4. Architecture

### 4.1 Dependency DAG

M3 established: `ports ← core ← index ← store ← oxibrain` (facade) +
`oxibrain-llm-http` (adapter). M4 adds two adapter crates and one connector:

```
ports          ← +Scope, +Capability, +BrainError::Scope
  ↑
core           ← +security types (pure: Capability, Scope, TokenInfo,
                  RedactionClosure, RedactionResult)
  ↑
index          ← unchanged
  ↑
store          ← +security.rs (token CRUD, audit write, scope check),
                  +redaction.rs (closure resolution + execution),
                  +export.rs (JSONL export/import)
  ↑
oxibrain       ← facade: Brain gains security methods (issue_token,
                  verify_token, revoke_token, list_tokens, redact,
                  export, import) + Scope-aware variants
  ↑
oxibrain-cli   ← full §12.4 surface
  ↑
oxibrain-mcp       ← NEW adapter: rmcp server + tool set + sampling LlmPort.
                      Depends on oxibrain (facade) + rmcp. Feature-gated.
oxibrain-client    ← NEW: thin async client. Depends on ports + reqwest.
oxibrain-connectors ← NEW: markdown vault connector. Depends on core + store.
```

**Dependency rules (DESIGN §15, enforced):**
- `oxibrain-mcp` depends on `oxibrain` (facade) + `rmcp`. Never on store, core, or index directly.
- `oxibrain-client` depends on `oxibrain-ports` + `reqwest`. Never on store.
- `oxibrain-connectors` depends on `oxibrain-core` + `oxibrain-store`. It writes episodes, which requires the store.
- Default features pull **zero** oxi-ecosystem crates. `oxibrain-mcp` and `oxibrain-client` are feature-gated.
- The standalone guarantee: `cargo build -p oxibrain --no-default-features --features http-llm` still produces a working brain without MCP/client/connectors.

### 4.2 Scope enforcement flow

```
Client (MCP / CLI / embedded)
  │
  ├─ Present token (MCP: header; CLI: config; embedded: bypassed)
  │
  ├─ Daemon resolves token → Scope (or rejects)
  │
  ├─ Scope check:
  │    Read cap?     → search, recall, get_entity, traverse, timeline, why
  │    Write cap?    → declare, retract, merge_entities
  │    Ingest cap?   → ingest, remember
  │    Sample cap?   → sampling LlmPort authorized
  │    Redact cap?   → redact
  │    Admin cap?    → token issue/revoke, predicate add, config change
  │
  ├─ Space check: operation's space ∈ scope.spaces?
  │
  ├─ Predicate filter: operation's predicate ∈ scope.predicate_filter?
  │
  └─ Execute → Brain facade method → store → audit
```

Embedded mode skips token resolution (the caller is in-process) but still
respects space boundaries (always enforced at the store layer).

### 4.3 Redaction flow (§11.5)

```
redact(target, reason)
  │
  ├─ 1. Resolve closure [reader]:
  │     target → {episodes, extractions, summaries, mentions,
  │                assertions, statements (left unsupported)}
  │
  ├─ 2. Write audit entry with reason [WriteOp] — BEFORE acting
  │
  ├─ 3. Tombstone [WriteOp]:
  │     episodes.content → "[redacted]"
  │     extractions.raw_response → "[redacted]"
  │     summaries.text → "[redacted]"
  │     Keep: row, id, hashes, timestamps, redacted_at = now
  │
  ├─ 4. Delete + re-fold [WriteOp]:
  │     DELETE mentions WHERE assertion_id IN closure
  │     DELETE assertions WHERE episode_id IN closure
  │     DELETE statements WHERE id NOT IN (remaining assertions)
  │     Re-fold affected belief groups
  │
  ├─ 5. Rebuild affected indexes [WriteOp]:
  │     FTS5, TF-IDF, communities for affected space
  │
  └─ 6. Verify no orphans:
  │     doctor --check-orphans → must report zero
  │
  └─ return RedactionResult { episodes, assertions, statements, mentions }
```

Redaction is idempotent: calling it twice on the same target is a no-op (second
call finds empty closure). `redact --dry-run` resolves and returns the closure
without acting.

### 4.4 Module map

```
oxibrain-core/src/
  security.rs          # NEW — Capability, Scope, TokenInfo, RedactionClosure,
                       #         RedactionResult, RedactTarget (pure types)

oxibrain-ports/src/
  error.rs             # EXTEND — BrainError::Scope { required: String },
                       #   BrainError::Unauthorized, BrainError::Conflict

oxibrain-store/src/
  security.rs          # NEW — token CRUD (issue, verify, revoke, list),
                       #         audit_write, audit_list, check_scope
  redaction.rs         # NEW — resolve_closure, execute_redaction, dry_run
  export.rs            # NEW — export_jsonl, import_jsonl
  migration.rs         # EXTEND — v4 migration (tokens table)
  migrations/v4.sql    # NEW — CREATE TABLE tokens + indexes

oxibrain/src/
  lib.rs               # EXTEND — Brain gains:
                       #   issue_token, verify_token, revoke_token, list_tokens,
                       #   redact, redact_dry_run, audit_log
                       #   export_jsonl, import_jsonl

oxibrain-cli/src/
  cli.rs               # EXTEND — full §12.4 subcommands
  cmd/
    ask.rs             # NEW
    entity.rs          # NEW — show, merge, alias
    timeline.rs        # NEW
    why.rs             # NEW
    contradictions.rs  # NEW
    reextract.rs       # NEW (deferred from M3)
    eval.rs            # NEW (deferred from M3)
    redact.rs          # NEW
    export.rs          # NEW
    import.rs          # NEW
    serve.rs           # NEW — daemon entry point
    token.rs           # NEW — issue, list, revoke
    extract.rs         # NEW (deferred from M3)

oxibrain-mcp/          # NEW crate (M4d)
  Cargo.toml
  src/
    lib.rs             # re-exports
    server.rs          # rmcp server impl: tool dispatch, token auth
    tools.rs           # tool definitions (§12.2 table)
    sampling.rs        # SamplingLlmPort — LlmPort backed by client sampling

oxibrain-client/       # NEW crate (M4e)
  Cargo.toml
  src/
    lib.rs             # BrainClient: async HTTP/Unix-socket client

oxibrain-connectors/   # NEW crate (M4b-connector)
  Cargo.toml
  src/
    lib.rs             # re-exports
    markdown.rs        # scan directory → Vec<Episode>
```

### 4.5 New workspace dependencies

```toml
# Added to [workspace.dependencies]:
rmcp = { version = "3", features = ["server", "transport-io", "transport-sse"] }
sha2 = "0.10"          # token hashing
walkdir = "2"           # markdown connector directory traversal
glob = "0.3"            # markdown connector glob patterns
```

---

## 5. Security data types (core/security.rs)

### 5.1 Capability and Scope

```rust
use serde::{Deserialize, Serialize};

/// What a token holder may do. DESIGN §11.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Read,     // search, recall, get_entity, traverse, timeline, why
    Write,    // declare, retract, merge_entities, review_merges
    Ingest,   // ingest, remember
    Sample,   // sampling LlmPort — off by default (§12.3)
    Admin,    // token issue/revoke, predicate add, config change
    Redact,   // redact — separate capability on purpose (§12.2)
}

/// A bit set of capabilities.
pub type CapabilitySet = std::collections::BTreeSet<Capability>;

/// Authorization scope carried by a token. DESIGN §11.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub spaces: Vec<String>,
    pub caps: CapabilitySet,
    pub predicate_filter: Option<Vec<String>>,
    pub entity_type_filter: Option<Vec<String>>,
    pub expires_at: Option<Timestamp>,
}

impl Scope {
    /// Check if a capability is granted and not expired.
    pub fn permits(&self, cap: Capability, space: &str) -> bool {
        self.caps.contains(&cap)
            && self.spaces.iter().any(|s| s == space)
            && self.expires_at.map_or(true, |exp| now < exp)
    }
}
```

### 5.2 Token info (public-facing; the secret is never stored)

```rust
/// Public metadata for a token. The secret itself is shown once at issuance
/// and stored only as a SHA-256 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: String,            // content-derived from (scope, issued_at, nonce)
    pub scope: Scope,
    pub issued_at: Timestamp,
    pub issued_by: String,     // actor (admin token id or "cli")
    pub revoked_at: Option<Timestamp>,
    pub label: Option<String>, // human-readable hint
}
```

### 5.3 Redaction types

```rust
/// What to redact.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RedactTarget {
    Episode { id: String },
    Entity { space: String, entity_id: String },
    PredicateScoped { space: String, entity_id: String, predicate: String },
}

/// The set of objects that will be affected by a redaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionClosure {
    pub episodes: Vec<String>,
    pub assertions: Vec<String>,
    pub statements: Vec<String>,
    pub mentions: Vec<String>,
    pub extractions: Vec<String>,
    pub summaries: Vec<String>,
}

/// What a redaction actually did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionResult {
    pub closure: RedactionClosure,
    pub beliefs_refolded: usize,
}
```

---

## 6. Token storage and verification (store/security.rs)

### 6.1 Schema (migration v4)

```sql
CREATE TABLE IF NOT EXISTS tokens (
    id           TEXT PRIMARY KEY,
    token_hash   TEXT NOT NULL UNIQUE,   -- SHA-256 hex of the secret
    scope_json   TEXT NOT NULL,           -- serialized Scope
    issued_at    INTEGER NOT NULL,
    issued_by    TEXT NOT NULL,
    revoked_at   INTEGER,
    label        TEXT
);
CREATE INDEX IF NOT EXISTS idx_tokens_hash ON tokens(token_hash);
CREATE INDEX IF NOT EXISTS idx_tokens_revoked ON tokens(revoked_at);
```

`LEDGER_SCHEMA_VERSION` bumps to 4. Migration has an up-test from v3.

### 6.2 Token lifecycle

```rust
/// Issue a token. Returns (TokenInfo, secret). The secret is shown once.
pub fn issue_token(
    conn: &Connection,
    scope: &Scope,
    issued_by: &str,
    label: Option<&str>,
    now: Timestamp,
) -> Result<(TokenInfo, String), BrainError>;

/// Verify a token by its secret. Returns the scope if valid and not expired/revoked.
pub fn verify_token(
    conn: &Connection,
    secret: &str,
    now: Timestamp,
) -> Result<Option<Scope>, BrainError>;

/// Revoke a token by id.
pub fn revoke_token(conn: &Connection, id: &str, now: Timestamp) -> Result<(), BrainError>;

/// List all tokens (active and revoked).
pub fn list_tokens(conn: &Connection) -> Result<Vec<TokenInfo>, BrainError>;
```

Token secret format: 32 random bytes, hex-encoded (64 chars). Prefixed with
`obt_` (oxibrain token) for human recognition. Hashed with SHA-256 before
storage. **Tokens are operational state, not projection state** — they use
random nonces, not content-derived IDs. This does not violate P1 because tokens
are not part of the reprojection contract; they are ephemeral authorization
state that backs up with the ledger but is not rebuilt by `reproject()`.

### 6.3 Audit log (table exists from M0)

```rust
pub struct AuditEntry {
    pub id: i64,
    pub ts: Timestamp,
    pub actor: String,
    pub scope: Option<String>,
    pub operation: String,
    pub target: Option<String>,
    pub detail_json: Option<String>,
}

pub fn write_audit(
    conn: &Connection,
    actor: &str,
    scope: Option<&str>,
    operation: &str,
    target: Option<&str>,
    detail_json: Option<&str>,
    now: Timestamp,
) -> Result<(), BrainError>;

pub fn list_audit(
    conn: &Connection,
    limit: Option<i64>,
) -> Result<Vec<AuditEntry>, BrainError>;
```

Every write path (declare, ingest, extract, merge, redact, token issue/revoke)
calls `write_audit` **before** acting (§11.5: "Write the audit entry with the
reason — before acting").

---

## 7. Redaction (store/redaction.rs)

### 7.1 Closure resolution

Given a `RedactTarget`, resolve the full closure:

- **Episode** → the episode, its extractions, its summaries (if Derived), its
  assertions, their mentions, statements left unsupported.
- **Entity** → all episodes that mention the entity (via assertions), all
  assertions about the entity, all statements about the entity, mentions.
- **PredicateScoped** → subset of Entity filtered to one predicate.

Unsupported statements = statements with zero remaining assertions after the
closure's assertions are removed.

### 7.2 Execution

1. Write audit entry (actor, operation="redact", target, detail_json=reason).
2. Tombstone: `UPDATE episodes SET content='[redacted]', redacted_at=now WHERE id IN (...)`.
   Same for `extractions.raw_response` and `summaries.text`.
3. Delete mentions: `DELETE FROM mentions WHERE assertion_id IN (...)`.
4. Delete assertions: `DELETE FROM assertions WHERE episode_id IN (...)` or
   `WHERE id IN (...)` for entity-scoped.
5. Delete unsupported statements: `DELETE FROM statements WHERE id IN (...) AND
   NOT EXISTS (SELECT 1 FROM assertions WHERE statement_id = statements.id)`.
6. Re-fold affected belief groups: delete and rebuild beliefs for affected
   statements.
7. Rebuild indexes for the affected space.

**Idempotency:** a second call on the same target finds an empty closure
(episodes already redacted, assertions already deleted) and returns
`RedactionResult { closure: empty, beliefs_refolded: 0 }`.

---

## 8. Export / import (store/export.rs)

### 8.1 Format

Newline-delimited JSON (JSONL), one object per line. Each line has a `table`
field identifying the source table:

```jsonl
{"table":"spaces","row":{"id":"...","name":"personal","created_at":...}}
{"table":"episodes","row":{...}}
{"table":"extractions","row":{...}}
{"table":"summaries","row":{...}}
{"table":"entities","row":{...}}
{"table":"entity_keys","row":{...}}
{"table":"entity_merges","row":{...}}
{"table":"statements","row":{...}}
{"table":"assertions","row":{...}}
{"table":"mentions","row":{...}}
{"table":"episode_links","row":{...}}
{"table":"audit_log","row":{...}}
{"table":"meta","row":{...}}
```

Tables are exported in dependency order. Indexes and views (FTS5, TF-IDF,
communities, beliefs) are **not** exported — they are derived and rebuilt by
`reproject()`.

### 8.2 Round-trip contract

`export | import` into an empty store, then `reproject`, yields a byte-identical
projection to the original. This is tested by:
1. Build a store with known content (episodes, declarations, extractions).
2. Export to JSONL string.
3. Import into a fresh store.
4. `reproject()` on the fresh store.
5. Compare: episode_count, entity_count, statement_count, assertion_count,
   belief snapshot, index snapshot — all must match.

---

## 9. MCP server (oxibrain-mcp) — deferred detail

The MCP server wraps the Brain facade and adds token authentication. The tool
set maps directly to DESIGN §12.2:

| MCP tool | Brain facade method | Required cap |
|---|---|---|
| `search` | `query` | Read |
| `recall` | `assemble_context` | Read |
| `get_entity` | `beliefs` + `resolve_entity_id` | Read |
| `traverse` | `traverse` | Read |
| `timeline` | `timeline` | Read |
| `why` | `why` | Read |
| `ingest` | `ingest` + `extract_one` | Ingest |
| `remember` | `ingest` + `extract_one` (realtime) | Write |
| `declare` | `declare` | Write |
| `retract` | `declare` (denying) | Write |
| `merge_entities` | (new) `merge_entities` | Write |
| `review_merges` | (new) `pending_merges` | Read |
| `redact` | `redact` | Redact |

Sampling `LlmPort`: implements `LlmPort` by calling the MCP client's sampling
endpoint. Only authorized for tokens with `Sample` capability (§12.3).

**Detailed MCP design is deferred to the implementation plan (§14 sub-session
M4d).** This spec establishes the contract: every Brain facade method maps to an
MCP tool; token auth wraps every call; sampling is a separate capability.

---

## 10. CLI surface (§12.4)

The full CLI from DESIGN §12.4. New subcommands in M4:

```
oxibrain ask "<question>" [--as-of DATE] [--space s] [--explain]
oxibrain entity show <id> | merge <a> <b> | alias <id> <name>
oxibrain timeline <entity> [--from --to]
oxibrain why <statement-id>
oxibrain contradictions [--space s]
oxibrain extract [--space s]        # deferred from M3
oxibrain reextract [--extractor X]  # deferred from M3
oxibrain eval [--suite fast|full]   # deferred from M3
oxibrain redact <target> [--dry-run] --reason "..."
oxibrain export [--format jsonl] [--out FILE]
oxibrain import FILE
oxibrain serve [--stdio|--socket PATH|--http ADDR] [--daemon]
oxibrain token issue --space s --caps ... [--expires 30d] [--label "..."]
oxibrain token list | revoke <id>
oxibrain reproject
oxibrain predicate list
```

The CLI calls the Brain facade directly (embedded mode). `serve` starts the MCP
server.

---

## 11. Schema changes

**Migration v4** — `tokens` table (§6.1). `LEDGER_SCHEMA_VERSION` → 4.

The `audit_log` table already exists from v1. No new migration needed for it.

No other schema changes. All other M4 work (redaction, export/import, MCP) uses
existing tables.

---

## 12. Deviations from DESIGN.md

| # | Deviation | DESIGN says | M4 does | Reason |
|---|---|---|---|---|
| D1 | Sampling deferred to M4d implementation | §12.3: sampling LlmPort in M4 | Spec defines the contract; implementation may land in a later sub-session if rmcp sampling API proves unstable | rmcp 3.x sampling API needs validation. The extraction pipeline works with HTTP adapters. Sampling is an onboarding convenience, not a correctness requirement. |
| D2 | Daemon transports: stdio + Unix socket first | §12.4: `--stdio\|--socket\|--http` | stdio (for MCP clients like Claude Desktop) + Unix socket (for local daemon); HTTP deferred if time-constrained | HTTP adds TLS complexity (§11.6: non-loopback requires TLS). stdio + Unix socket cover both exit criteria (Claude Desktop + two apps sharing). |
| D3 | Subscriptions deferred | §12.2: "transport-neutral subscriptions" | Polling only in v1 | Subscriptions are an enhancement; polling is sufficient. Deferred to M5+. |
| D4 | Markdown connector is read-only | §1.4: "vaults are read through connectors" | Connector reads `.md` files and creates episodes; it never writes back | oxibrain never owns authoring (§1.4). The connector is a one-way ingest path. |
| D5 | Token uses SHA-256 hash, not bcrypt | §11.2: tokens presented by clients | SHA-256 hash at rest; token is 32 random bytes (high entropy) | bcrypt is for low-entropy passwords. A 256-bit random token does not need bcrypt's cost factor. SHA-256 is sufficient and avoids a native dependency. |

---

## 13. Open questions (M4 defaults)

1. **rmcp API stability.** rmcp 3.x is recent. *Default: pin to 3.x; if the API
   breaks, the fallback is a minimal in-house server over the same protocol types
   (DESIGN §18 risk row). Gate at M4d.*

2. **Token storage in the daemon vs. embedded.** *Default: tokens are stored in
   the SQLite store (same database). Embedded mode bypasses token checks
   (in-process = trusted). The daemon reads tokens from the store on each
   connection.*

3. **Scope enforcement granularity.** Should scope checks happen in the store or
   the facade? *Default: facade checks capability + space; store enforces space
   isolation (already does via space_id filtering). This keeps the store simple
   and puts authorization logic where the API surface is.*

4. **Export format: JSONL vs. SQLite backup.** *Default: JSONL is the portable
   format (§12.5). The existing `backup` command uses SQLite's online backup API
   (§13.4). Both coexist: backup is for same-version disaster recovery; JSONL
   export/import is for major-version migration and interoperability.*

5. **Redaction of Derived episodes.** A Derived episode summarizes sources.
   Redacting it should not redact the sources. *Default: redacting a Derived
   episode tombstones its text but does not cascade to its source episodes.
   Redacting a source episode cascades to Derived episodes that reference it
   (their text is tombstoned and will be regenerated on the next consolidation
   pass).*

---

## 14. Sub-session breakdown

| Sub-session | Scope | Est. commits | Difficulty |
|---|---|---|---|
| **M4a** | Security core: `Capability`/`Scope`/`TokenInfo` types; token CRUD + migration v4; audit log; `BrainError::Scope`; Brain facade security methods; injection test suite | 5–6 | 🔴 |
| **M4b** | Export/import: JSONL export + import + round-trip test; markdown connector crate | 3–4 | 🟡 |
| **M4c** | Full CLI: ask, entity, timeline, why, contradictions, redact, export, import, token, reproject, extract, reextract, eval subcommands | 4–5 | 🟡 |
| **M4d** | MCP server: `oxibrain-mcp` crate on rmcp; tool set; token auth; sampling LlmPort | 5–6 | 🔴 |
| **M4e** | Daemon + transports: `serve --stdio\|--socket`; `oxibrain-client` crate | 3–4 | 🟡 |

---

End of spec. Read this + `doc/DESIGN.md` §11 (security), §12 (surfaces), §13
(operations), §15 (workspace), §17 (M4) + the M3→M4 handoff
(`docs/superpowers/handoffs/2026-08-11-m3-to-m4.md`) — then proceed to the
implementation plan.
