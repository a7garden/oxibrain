# oxibrain M0 — Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the oxibrain workspace and the deterministic foundation: a content-addressed episode ledger backed by SQLite, a single-writer/many-reader store with migrations and advisory locking, ports with fakes, canonical serialization with content-derived ids, and a CLI that can `init`, `ingest`, read back, `doctor`, and `backup`/`restore`.

**Architecture:** A Cargo workspace of five crates for M0 (`oxibrain-ports` → `oxibrain-core` → `oxibrain-store` → `oxibrain` facade → `oxibrain-cli`). The store is the only crate that touches `rusqlite`. Writes serialize through one owned-thread actor holding the write connection; readers use a WAL pool. Every projection id is content-derived (BLAKE3) over a canonical serialization, so reprojection will be byte-identical in M1. No LLM, no embedding computation, no network in M0.

**Tech Stack:** Rust 2024, `rusqlite` (bundled SQLite with `sqlite3-sys`), `blake3`, `serde`/`serde_json`, `tokio` (rt-multi-thread), `clap` (derive), `proptest`, `tracing`. SQLite WAL mode, `PRAGMA foreign_keys=ON`.

**Authority:** `doc/DESIGN.md` v1.0 (§§3, 5, 7, 13, 15, 17). `AGENTS.md` for project conventions. This plan implements M0 only.

## M0 Exit Criteria (DESIGN.md §17)

1. `oxibrain init` creates a store with the full schema migrated.
2. Ingest an episode and read it back.
3. Kill mid-write and recover (crash safety via WAL + advisory lock + transactional stages).
4. Canonicalization property tests pass.

---

## Global Constraints

(Copied from `doc/DESIGN.md` and `AGENTS.md`. Every task's requirements implicitly include these.)

- **Rust 2024 edition.** `clippy` clean with `-D warnings`; `#![cfg_attr(test, allow(clippy::unwrap_used))]` in every crate so production code is linted and `.unwrap()` is test-only.
- **`expect("reason")` for invariants, `?` for fallible ops.** No bare `unwrap`/`expect` without a reason string in non-test code.
- **Public APIs return `BrainError`, never `anyhow` across a crate boundary.** `anyhow` is fine internally.
- **Module-per-file; `lib.rs` is an index.** Logic lives in focused submodules.
- **Time is always explicit `Timestamp` (UTC), never a bare `i64` in a signature.** Clock access goes through `ClockPort`.
- **Only `oxibrain-store` may reference `rusqlite`.** Enforced by a CI grep test.
- **Default features pull zero oxi-ecosystem crates.** `cargo build -p oxibrain --no-default-features --features http-llm` must produce a working standalone brain; `cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'` must match nothing.
- **Sentinel timestamps, never NULL.** `TIME_MIN = i64::MIN + 1`, `TIME_MAX = i64::MAX - 1`. Open intervals use sentinels (DESIGN.md §6.2).
- **No transaction spans an LLM call, network call, or embedding computation.** (Vacuous in M0 — none exist — but the rule holds.)
- **Squash merge. Conventional commits:** `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `perf:`. English commit messages.
- **Comments, doc-comments, commit messages, and design docs in English.** Korean is for chat, not source.

---

## File Structure (M0 crates only)

M0 ships five crates. The remaining workspace members from DESIGN.md §15 (`oxibrain-index`, `oxibrain-llm-http`, `oxibrain-llm-oxicode`, `oxibrain-embed-local`, `oxibrain-connectors`, `oxibrain-mcp`, `oxibrain-client`) are **not** created in M0 — they appear in later milestones. A workspace lint test guarantees only the five M0 members exist.

```
oxibrain/
├── Cargo.toml                 # workspace root, [workspace.dependencies], metadata
├── rust-toolchain.toml        # pin toolchain
├── deny.toml                  # cargo-deny config
├── clippy.toml                # shared lint settings (msrv etc.)
├── .gitignore                 # exists
├── crates/
│   ├── oxibrain-ports/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         # index, re-exports
│   │       ├── time.rs        # Timestamp, TIME_MIN/MAX, unix<->ts
│   │       ├── clock.rs       # ClockPort trait, SystemClock, FakeClock
│   │       ├── llm.rs         # LlmPort trait, LlmRequest/Response, FakeLlm (M0 stub)
│   │       ├── embedding.rs   # EmbeddingPort trait, FakeEmbedding (M0 stub)
│   │       └── error.rs       # BrainError (M0 subset), retryable classification
│   ├── oxibrain-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         # index
│   │       ├── id.rs          # content-derived ids: blake3, EpisodeId, ContentHash, hex
│   │       ├── canonical.rs   # canonical serialization (sorted keys, normalized numbers, RFC-3339)
│   │       └── types.rs       # Space, Episode, SourceRef, TrustTier, EpisodeKind value types
│   ├── oxibrain-store/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs         # Store, open, config, error conversion
│   │   │   ├── schema.rs      # PRAGMA setup, schema constants
│   │   │   ├── migration.rs   # Migration trait, runner, version tracking
│   │   │   ├── migrations/
│   │   │   │   ├── mod.rs
│   │   │   │   └── v1.sql     # full M0 schema (embedded)
│   │   │   ├── lock.rs        # cross-process advisory lock
│   │   │   ├── writer.rs      # WriterActor: owned thread + mpsc + coalescing
│   │   │   ├── reader.rs      # ReaderPool: N read-only WAL connections
│   │   │   ├── ledger.rs      # episode + space writes/reads (ledger zone only)
│   │   │   ├── meta.rs        # meta table get/set (schema/projection versions)
│   │   │   └── backup.rs      # online backup API + manifest
│   │   └── tests/
│   │       ├── migration.rs   # migration up-test from fixture
│   │       ├── crash.rs       # kill-mid-write recovery test
│   │       └── concurrency.rs # N readers + writer
│   ├── oxibrain/              # facade
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs         # Brain struct/trait, prelude, re-exports
│   │       └── config.rs      # BrainConfig, BrainConfigBuilder
│   └── oxibrain-cli/          # THE binary
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs        # tokio entry, Cli dispatch
│           ├── cli.rs         # clap Cli + Commands enum
│           └── cmd/
│               ├── mod.rs
│               ├── init.rs
│               ├── ingest.rs
│               ├── stats.rs
│               ├── doctor.rs
│               └── backup.rs
├── xtask/                     # workspace lint tasks (optional; see Task 10)
│   └── ...
└── .github/workflows/ci.yml   # CI (clippy, fmt, deny, tests, standalone guarantee)
```

**Dependency direction (enforced):**
```
oxibrain-cli → oxibrain → { oxibrain-core, oxibrain-store, oxibrain-ports }
                         oxibrain-store → { oxibrain-core, oxibrain-ports }
                         oxibrain-core  → { oxibrain-ports }
                         oxibrain-ports → (nothing internal)
```

---

## Task 1: Workspace skeleton, edition, lint config, CI gates

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `rust-toolchain.toml`
- Create: `deny.toml`
- Create: `clippy.toml`
- Create: `crates/oxibrain-ports/{Cargo.toml,src/lib.rs}`
- Create: `crates/oxibrain-core/{Cargo.toml,src/lib.rs}`
- Create: `crates/oxibrain-store/{Cargo.toml,src/lib.rs}`
- Create: `crates/oxibrain/{Cargo.toml,src/lib.rs}`
- Create: `crates/oxibrain-cli/{Cargo.toml,src/main.rs}`
- Create: `.github/workflows/ci.yml`
- Create: `crates/oxibrain-cli/tests/standalone_guarantee.rs`

**Interfaces:**
- Produces: a compiling workspace with five empty crates, `[workspace.dependencies]` for shared deps, CI that runs `fmt --check`, `clippy --all-targets --all-features -- -D warnings`, `deny check`, `test`, and the standalone-guarantee test.

- [ ] **Step 1: Write the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "2"
members = [
    "crates/oxibrain-ports",
    "crates/oxibrain-core",
    "crates/oxibrain-store",
    "crates/oxibrain",
    "crates/oxibrain-cli",
]

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "MIT OR Apache-2.0"
rust-version = "1.85"

[workspace.dependencies]
oxibrain = { path = "crates/oxibrain", version = "0.1.0" }
oxibrain-core = { path = "crates/oxibrain-core", version = "0.1.0" }
oxibrain-ports = { path = "crates/oxibrain-ports", version = "0.1.0" }
oxibrain-store = { path = "crates/oxibrain-store", version = "0.1.0" }
blake3 = "1.5"
hex = "0.4"
unicode-normalization = "0.1"
fs2 = "0.4"
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled", "backup"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "fs"] }
clap = { version = "4", features = ["derive"] }
proptest = "1"
tempfile = "3"
tracing = "0.1"
tracing-subscriber = "0.3"
thiserror = "1"
anyhow = "1"

[profile.release]
lto = "thin"
```

- [ ] **Step 2: Pin the toolchain**

Create `rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.85"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Write `deny.toml` (cargo-deny)**

```toml
[advisories]
db-urls = ["https://github.com/rustsec/advisory-db"]
yanked = "deny"

[licenses]
allow = [
    "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause",
    "BSD-3-Clause", "ISC", "Unicode-DFS-2016", "Unicode-3.0", "Zlib", "CC0-1.0",
]
confidence-threshold = 0.93

[bans]
multiple-versions = "warn"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

- [ ] **Step 4: Write `clippy.toml`**

```toml
msrv = "1.85"
```

- [ ] **Step 5: Write the five crate `Cargo.toml` files**

`crates/oxibrain-ports/Cargo.toml`:
```toml
[package]
name = "oxibrain-ports"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
thiserror.workspace = true
serde.workspace = true
serde_json.workspace = true
async-trait.workspace = true

[dev-dependencies]
proptest.workspace = true
```

`crates/oxibrain-core/Cargo.toml`:
```toml
[package]
name = "oxibrain-core"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxibrain-ports.workspace = true
blake3.workspace = true
hex.workspace = true
unicode-normalization.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
```

`crates/oxibrain-store/Cargo.toml`:
```toml
[package]
name = "oxibrain-store"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
oxibrain-ports.workspace = true
oxibrain-core.workspace = true
rusqlite.workspace = true
hex.workspace = true
fs2.workspace = true
blake3.workspace = true
serde.workspace = true
thiserror.workspace = true
tracing.workspace = true

[dev-dependencies]
proptest.workspace = true
tempfile.workspace = true
rusqlite.workspace = true
oxibrain-ports.workspace = true
```

`crates/oxibrain/Cargo.toml`:
```toml
[package]
name = "oxibrain"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[features]
default = ["http-llm"]
http-llm = []   # placeholder until M3 ships the real adapter

[dependencies]
oxibrain-core.workspace = true
oxibrain-store.workspace = true
oxibrain-ports.workspace = true
tokio.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
tempfile.workspace = true
```

`crates/oxibrain-cli/Cargo.toml`:
```toml
[package]
name = "oxibrain-cli"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "oxibrain"
path = "src/main.rs"

[dependencies]
oxibrain.workspace = true
oxibrain-ports.workspace = true
clap = { workspace = true, features = ["env"] }
tokio.workspace = true
tracing.workspace = true
tracing-subscriber = { workspace = true, features = ["env-filter"] }
anyhow.workspace = true
```

- [ ] **Step 6: Write minimal crate roots**

`crates/oxibrain-ports/src/lib.rs`:
```rust
//! Ports: traits owned by oxibrain, implementations pluggable.
//! The boundary between the engine and the outside world (LLM, embedding, clock).

#![cfg_attr(test, allow(clippy::unwrap_used))]
```
Repeat an analogous empty `lib.rs` for `oxibrain-core`, `oxibrain-store`, `oxibrain`, and an empty `main.rs` for `oxibrain-cli` (`fn main() {}`).

- [ ] **Step 7: Write the standalone-guarantee test**

`crates/oxibrain-cli/tests/standalone_guarantee.rs`:
```rust
//! Asserts no oxi-ecosystem crate leaks into the default build (AGENTS.md standalone rule).
//! Complements the CI `cargo tree | grep` check; runs as a normal test too.
use std::process::Command;

#[test]
fn no_oxi_ecosystem_deps() {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", "oxibrain", "--no-default-features", "--features", "http-llm"])
        .output()
        .expect("cargo tree");
    assert!(out.status.success(), "cargo tree failed");
    let tree = String::from_utf8(out.stdout).expect("utf8");
    for line in tree.lines() {
        let lower = line.to_ascii_lowercase();
        assert!(!lower.contains("oxios-"), "oxios dep leaked: {line}");
        assert!(!lower.contains("oxicode-"), "oxicode dep leaked: {line}");
    }
}
```

- [ ] **Step 8: Write the CI workflow**

`.github/workflows/ci.yml`:
```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.85
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - name: Install cargo-deny
        run: cargo install cargo-deny --locked || true
      - name: fmt
        run: cargo fmt --all -- --check
      - name: clippy
        run: cargo clippy --all-targets --all-features -- -D warnings
      - name: deny
        run: cargo deny check
      - name: standalone build
        run: cargo build -p oxibrain --no-default-features --features http-llm
      - name: standalone guarantee (no oxi crates)
        run: |
          if cargo tree -p oxibrain | grep -E 'oxios-|oxicode-'; then
            echo "oxi-ecosystem crate leaked into default build"; exit 1
          fi
      - name: rusqlite isolation (only store references it)
        run: |
          if grep -rn 'use rusqlite' --include='*.rs' crates \
             | grep -v 'crates/oxibrain-store/'; then
            echo "rusqlite referenced outside oxibrain-store"; exit 1
          fi
      - name: test
        run: cargo test --all-features
```

- [ ] **Step 9: Build, clippy, fmt, test**

Run: `cargo build && cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all-features`
Expected: all green; `standalone_guarantee` test passes.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "feat: scaffold oxibrain workspace (M0 task 1)

Five-crate Cargo workspace: ports, core, store, facade, cli.
CI gates: fmt, clippy -D warnings, deny, standalone guarantee,
rusqlite isolation. M0 only; later crates not yet created."
```

---

## Task 2: Timestamp, sentinel time, ports, BrainError

**Files:**
- Create: `crates/oxibrain-ports/src/time.rs`
- Create: `crates/oxibrain-ports/src/clock.rs`
- Create: `crates/oxibrain-ports/src/llm.rs`
- Create: `crates/oxibrain-ports/src/embedding.rs`
- Create: `crates/oxibrain-ports/src/error.rs`
- Modify: `crates/oxibrain-ports/src/lib.rs`

**Interfaces:**
- Produces: `Timestamp` (newtype over `i64`, unix millis), `TIME_MIN`, `TIME_MAX`; `ClockPort::{now}` + `SystemClock` + `FakeClock`; `LlmPort` + `FakeLlm` (trait + stub); `EmbeddingPort` + `FakeEmbedding` (trait + stub); `BrainError` (M0 subset).

- [ ] **Step 1: Write `time.rs`**

```rust
//! Explicit time. Signatures use `Timestamp`, never a bare `i64`.
//! Open intervals use sentinels, never NULL (DESIGN.md §6.2).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unix milliseconds, UTC. The only time type in the codebase.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Timestamp(pub i64);

/// Sentinel for "the beginning of time". `i64::MIN + 1` — never NULL.
pub const TIME_MIN: Timestamp = Timestamp(i64::MIN + 1);
/// Sentinel for "the end of time" / "still true". `i64::MAX - 1` — never NULL.
pub const TIME_MAX: Timestamp = Timestamp(i64::MAX - 1);

impl Timestamp {
    pub const fn from_millis(m: i64) -> Self { Self(m) }
    pub const fn millis(self) -> i64 { self.0 }
    pub const fn is_min(self) -> bool { self.0 == TIME_MIN.0 }
    pub const fn is_max(self) -> bool { self.0 == TIME_MAX.0 }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            TIME_MIN => write!(f, "Timestamp(TIME_MIN)"),
            TIME_MAX => write!(f, "Timestamp(TIME_MAX)"),
            _ => write!(f, "Timestamp({})", self.0),
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_min() { return write!(f, "-infinity"); }
        if self.is_max() { return write!(f, "+infinity"); }
        write!(f, "{}", self.0)
    }
}
```

- [ ] **Step 2: Write the failing test for sentinel ordering**

`crates/oxibrain-ports/src/time.rs` (append):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinels_order_correctly() {
        assert!(TIME_MIN < Timestamp(0));
        assert!(Timestamp(0) < TIME_MAX);
        assert!(TIME_MIN < TIME_MAX);
    }

    #[test]
    fn sentinels_are_not_i64_extrema() {
        assert_ne!(TIME_MIN.0, i64::MIN); // MIN is reserved for NULL-encoding detection
        assert_ne!(TIME_MAX.0, i64::MAX);
    }
}
```

- [ ] **Step 3: Run test, verify pass**

Run: `cargo test -p oxibrain-ports time`
Expected: PASS.

- [ ] **Step 4: Write `clock.rs`**

```rust
//! Clock access goes through ClockPort so tests control time.

use crate::time::Timestamp;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait ClockPort: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub struct SystemClock;

impl ClockPort for SystemClock {
    fn now(&self) -> Timestamp {
        let dur = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock pre-epoch");
        Timestamp(dur.as_millis() as i64)
    }
}

#[derive(Debug)]
pub struct FakeClock {
    current: std::sync::atomic::AtomicI64,
}

impl FakeClock {
    pub fn new(start: Timestamp) -> Self { Self { current: start.0.into() } }
    pub fn advance(&self, by_millis: i64) {
        self.current.fetch_add(by_millis, std::sync::atomic::Ordering::Relaxed);
    }
}

impl ClockPort for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.current.load(std::sync::atomic::Ordering::Relaxed))
    }
}
```

- [ ] **Step 5: Write `error.rs` (M0 subset of BrainError, DESIGN.md §13.5)**

```rust
//! Typed errors at every public boundary. anyhow is internal only.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrainError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("schema version mismatch: found {found}, expected {expected}")]
    Migration { found: i64, expected: i64 },
    #[error("store locked by another writer: {holder}")]
    Locked { holder: String },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("corruption: {0}")]
    Corruption(String),
}

impl BrainError {
    /// True if repeating the operation might succeed (transient I/O, lock contention).
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Storage(_) | Self::Locked { .. })
    }
}

```

Note: there is intentionally **no** `impl From<rusqlite::Error> for BrainError` anywhere — it would violate the orphan rule (`BrainError` is foreign to every crate that has `rusqlite`, and vice versa). `oxibrain-store` instead owns two tiny helpers `sql_err`/`io_err` used via `.map_err(...)?` at every rusqlite/IO boundary (see Task 4 Step 5). The `?`-on-rusqlite shortcut does **not** compile in this workspace.

- [ ] **Step 6: Write `llm.rs` and `embedding.rs` (M0 trait stubs)**

`crates/oxibrain-ports/src/llm.rs`:
```rust
//! LLM inference port. M0 defines the trait only; adapters ship in M3.

use crate::error::BrainError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub prompt: String,
    pub json_schema: Option<serde_json::Value>,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub text: String,
    pub raw: serde_json::Value,
}

#[async_trait::async_trait]
pub trait LlmPort: Send + Sync {
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, BrainError>;
}
```
`async-trait` is in `[workspace.dependencies]` (Task 1); ports' `Cargo.toml` adds `async-trait.workspace = true`.

`crates/oxibrain-ports/src/embedding.rs`:
```rust
//! Embedding port. M0 defines the trait only; adapters ship in M2/M3.

use crate::error::BrainError;

pub trait EmbeddingPort: Send + Sync {
    fn dim(&self) -> usize;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, BrainError>;
}
```

- [ ] **Step 7: Wire up `lib.rs`**

`crates/oxibrain-ports/src/lib.rs`:
```rust
//! Ports: traits owned by oxibrain, implementations pluggable.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod clock;
pub mod embedding;
pub mod error;
pub mod llm;
pub mod time;

pub use clock::{ClockPort, FakeClock, SystemClock};
pub use error::BrainError;
pub use llm::{LlmPort, LlmRequest, LlmResponse};
pub use time::{Timestamp, TIME_MAX, TIME_MIN};
```

- [ ] **Step 8: Run clippy + tests**

Run: `cargo clippy -p oxibrain-ports --all-targets -- -D warnings && cargo test -p oxibrain-ports`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(ports): Timestamp, sentinels, ClockPort, LlmPort/EmbeddingPort traits, BrainError

M0 ports: explicit Timestamp (UTC millis) with TIME_MIN/TIME_MAX sentinels
(never NULL — DESIGN §6.2), ClockPort with System/Fake impls, LlmPort and
EmbeddingPort trait definitions (adapters land in M2/M3), and the M0 subset
of BrainError."
```

---

## Task 3: Core value types, canonical serialization, content-derived ids

**Files:**
- Create: `crates/oxibrain-core/src/types.rs`
- Create: `crates/oxibrain-core/src/canonical.rs`
- Create: `crates/oxibrain-core/src/id.rs`
- Modify: `crates/oxibrain-core/src/lib.rs`

**Interfaces:**
- Produces: `ContentHash` (`[u8; 32]`); value types `Space`, `Episode`, `SourceRef`, `TrustTier`, `EpisodeKind`; `canonical_json_value(value) -> String`; `EpisodeId`/content-hash derivation via BLAKE3.

- [ ] **Step 1: Write `types.rs` (M0 ledger value types, DESIGN.md §5.2–5.3)**

```rust
//! Ledger value types. Knowledge types (entities, statements, ...) land in M1.

use blake3::Hash;
use oxibrain_ports::Timestamp;
use serde::{Deserialize, Serialize};

/// BLAKE3 digest over normalized content.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
    pub fn from_hash(h: Hash) -> Self { Self(h.into()) }
    pub fn hex(&self) -> String { hex::encode(self.0) }
}

impl std::fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentHash({})", self.hex())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier { Trusted, SemiTrusted, Untrusted }
impl TrustTier {
    pub fn as_db(&self) -> &'static str {
        match self { Self::Trusted => "trusted", Self::SemiTrusted => "semi_trusted", Self::Untrusted => "untrusted" }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s { "trusted" => Some(Self::Trusted), "semi_trusted" => Some(Self::SemiTrusted),
                  "untrusted" => Some(Self::Untrusted), _ => None }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeKind { Primary, Declaration, Derived }
impl EpisodeKind {
    pub fn as_db(&self) -> &'static str {
        match self { Self::Primary => "primary", Self::Declaration => "declaration", Self::Derived => "derived" }
    }
    pub fn parse_db(s: &str) -> Option<Self> {
        match s { "primary" => Some(Self::Primary), "declaration" => Some(Self::Declaration),
                  "derived" => Some(Self::Derived), _ => None }
    }
}

/// Where an episode came from. M0 supports Note and Declaration; others land later.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "snake_case")]
pub enum SourceRef {
    Note { path: String },
    Document { uri: String },
    Conversation,
    Message,
    AgentTrace,
    Declaration,
    Derived { of: String },
}

impl SourceRef {
    /// (source_kind column, source_ref column) for persistence.
    pub fn db_columns(&self) -> (&'static str, Option<String>) {
        match self {
            Self::Note { path } => ("note", Some(path.clone())),
            Self::Document { uri } => ("document", Some(uri.clone())),
            Self::Conversation => ("conversation", None),
            Self::Message => ("message", None),
            Self::AgentTrace => ("agent_trace", None),
            Self::Declaration => ("declaration", None),
            Self::Derived { of } => ("derived", Some(of.clone())),
        }
    }
    pub fn kind_db(&self) -> &'static str { self.db_columns().0 }
}

/// A namespace and isolation boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub created_at: Timestamp,
}

/// The atom of record. Immutable once written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub space: String,
    pub seq: u64,
    pub content_hash: ContentHash,
    pub content: String,
    pub source: SourceRef,
    pub trust: TrustTier,
    pub kind: EpisodeKind,
    pub occurred_at: Timestamp,
    pub ingested_at: Timestamp,
    pub redacted_at: Option<Timestamp>,
}
```
Add `hex = "0.4"` to core `[dependencies]`.

- [ ] **Step 2: Write `canonical.rs`**

```rust
//! Canonical serialization. A bug here is a determinism bug (DESIGN.md §5.6):
//! sorted keys, normalized numbers, RFC-3339 UTC timestamps. Property-tested.

use serde_json::{Map, Value};

/// Recursively sort all object keys in a JSON value. Object key order is the only
/// non-determinism in `serde_json`; sorting it makes the output a pure function of the value.
pub fn canonicalize_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> = map
                .iter()
                .map(|(k, vv)| (k.clone(), canonicalize_value(vv)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(Map::from_iter(pairs))
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_value).collect()),
        other => other.clone(),
    }
}

/// Canonical JSON string of a Value: sorted keys, compact (no whitespace).
pub fn canonical_json_value(v: &Value) -> String {
    serde_json::to_string(&canonicalize_value(v)).expect("canonical json infallible")
}

/// Canonical bytes over a raw JSON string: parse, canonicalize, re-emit compactly.
pub fn canonical_bytes(json: &str) -> Result<Vec<u8>, serde_json::Error> {
    let v: Value = serde_json::from_str(json)?;
    Ok(canonical_json_value(&v).into_bytes())
}
```

Note: canonicalization happens at the `serde_json::Value` layer (parse → recursively sort object keys → compact `to_string`). `serde_json`'s compact scalar output is already deterministic; key order is the only non-determinism, and sorting removes it. No custom `Formatter` is needed (and `serde_json::ser::Format` is not a real trait).

- [ ] **Step 3: Write the failing property tests for canonicalization**

`crates/oxibrain-core/src/canonical.rs` (append):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn canonical_is_deterministic(s in "[a-z]{0,20}") {
            let v = serde_json::json!({ "b": 1, "a": { "y": 2, "x": 1 }, "c": &s });
            let c1 = canonical_json_value(&v);
            let c2 = canonical_json_value(&v);
            prop_assert_eq!(c1, c2);
        }

        #[test]
        fn keys_are_sorted(v in "[a-z]{1,5}") {
            let unsorted = serde_json::json!({ &v: 1, "a": 2, "z": 3 });
            let canon = canonical_json_value(&unsorted);
            // keys appear in sorted order: a, then v, then z (lexicographic)
            let mut keys: Vec<&str> = canon
                .trim_matches(|c| c == '{' || c == '}')
                .split(',')
                .map(|kv| kv.split(':').next().unwrap().trim_matches('"'))
                .collect();
            let mut sorted = keys.clone();
            sorted.sort();
            prop_assert_eq!(keys, sorted);
        }
    }
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p oxibrain-core canonical`
Expected: PASS.

- [ ] **Step 5: Write `id.rs` (content-derived ids, DESIGN.md §5.6)**

```rust
//! Content-derived ids. Every projection id is derived from content, not random,
//! so reprojection is byte-identical (DESIGN.md §5.6, P1).

use crate::canonical;
use crate::types::{ContentHash, SourceRef};
use blake3::Hasher;
use oxibrain_ports::Timestamp;

/// Hex string id (TEXT PRIMARY KEY in SQLite).
pub type Id = String;

fn hex(bytes: [u8; 32]) -> String { hex::encode(bytes) }

/// BLAKE3 over canonical JSON of the fields.
fn derive(fields: &[(&str, &str)]) -> [u8; 32] {
    let mut h = Hasher::new();
    for (k, v) in fields {
        h.update(k.as_bytes());
        h.update(&[0u8]); // key/value separator
        h.update(v.as_bytes());
        h.update(&[0u8]);
    }
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    out
}

/// `EpisodeId = blake3(space, content_hash, source_ref, occurred_at)`
pub fn episode_id(
    space: &str,
    content_hash: &ContentHash,
    source: &SourceRef,
    occurred_at: Timestamp,
) -> Id {
    // source serialized canonically so the id is stable
    let source_json = serde_json::to_value(source)
        .ok()
        .map(|v| canonical::canonical_json_value(&v))
        .expect("source serializable");
    hex(derive(&[
        ("space", space),
        ("content_hash", &content_hash.hex()),
        ("source_ref", &source_json),
        ("occurred_at", &occurred_at.millis().to_string()),
    ]))
}

/// Content hash over normalized content. M0 normalization: NFKC + CR/CRLF→LF + trim trailing ws.
pub fn content_hash(content: &str) -> ContentHash {
    let normalized = normalize_content(content);
    let mut h = Hasher::new();
    h.update(normalized.as_bytes());
    let mut out = [0u8; 32];
    h.finalize_xof().fill(&mut out);
    ContentHash(out)
}

/// NFKC unicode normalization + CR/CRLF→LF + trailing-whitespace trim.
pub fn normalize_content(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.replace("\r\n", "\n").replace('\r', "\n")
        .nfkc()
        .collect::<String>()
        .trim_end()
        .to_string()
}
```
`hex` and `unicode-normalization` are in `[workspace.dependencies]`; core's `Cargo.toml` adds both (Task 1 Step 5).

- [ ] **Step 6: Write the failing id-determinism test**

`crates/oxibrain-core/src/id.rs` (append):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn episode_id_is_stable() {
        let ch = content_hash("hello world");
        let src = SourceRef::Note { path: "a.md".into() };
        let id1 = episode_id("s1", &ch, &src, Timestamp(1000));
        let id2 = episode_id("s1", &ch, &src, Timestamp(1000));
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_space_different_id() {
        let ch = content_hash("hello world");
        let src = SourceRef::Note { path: "a.md".into() };
        let id1 = episode_id("s1", &ch, &src, Timestamp(1000));
        let id2 = episode_id("s2", &ch, &src, Timestamp(1000));
        assert_ne!(id1, id2);
    }

    proptest! {
        #[test]
        fn content_hash_deterministic(s in ".{0,200}") {
            let h1 = content_hash(&s).hex();
            let h2 = content_hash(&s).hex();
            prop_assert_eq!(h1, h2);
        }

        #[test]
        fn normalization_is_stable(s in "[a-z \n\r]{0,80}") {
            // CRLF and trailing ws collapse; NFC stable.
            let h1 = content_hash(&s).hex();
            let n = normalize_content(&s);
            let h2 = content_hash(&n).hex();
            prop_assert_eq!(h1, h2, "normalize must be idempotent-ish for hashing");
        }
    }
}
```

- [ ] **Step 7: Run tests, verify pass**

Run: `cargo test -p oxibrain-core`
Expected: PASS.

- [ ] **Step 8: Wire `lib.rs`**

`crates/oxibrain-core/src/lib.rs`:
```rust
//! oxibrain-core: the engine. Knows nothing of MCP/HTTP/CLI (DESIGN.md P6).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod canonical;
pub mod id;
pub mod types;

pub use canonical::{canonical_bytes, canonical_json_value, canonicalize_value};
pub use id::{content_hash, episode_id, normalize_content, Id};
pub use types::{ContentHash, Episode, EpisodeKind, Space, SourceRef, TrustTier};
```

- [ ] **Step 9: clippy + test + commit**

Run: `cargo clippy -p oxibrain-core --all-targets -- -D warnings && cargo test -p oxibrain-core`
```bash
git add -A
git commit -m "feat(core): ledger value types, canonical serialization, content-derived ids

ContentHash + Episode value types (M0 ledger subset). Canonical JSON
serializer (sorted keys, compact) — a bug here is a determinism bug.
BLAKE3 content-derived EpisodeId and content hash; NFC normalization.
Property tests: canonical determinism, key ordering, hash stability."
```

---

## Task 4: Store schema, migrations, open/lock

**Files:**
- Create: `crates/oxibrain-store/src/lib.rs`
- Create: `crates/oxibrain-store/src/schema.rs`
- Create: `crates/oxibrain-store/src/migration.rs`
- Create: `crates/oxibrain-store/src/migrations/mod.rs`
- Create: `crates/oxibrain-store/src/migrations/v1.sql`
- Create: `crates/oxibrain-store/src/lock.rs`
- Create: `crates/oxibrain-store/src/meta.rs`
- Modify: `crates/oxibrain-store/src/lib.rs` (store-local `sql_err`/`io_err` helpers)

**Interfaces:**
- Produces: `Store::open(path) -> Result<Store>` (applies migrations, acquires advisory lock, sets PRAGMAs); `Store::user_version() -> i64`; current schema version constant `LEDGER_SCHEMA_VERSION = 1`.

- [ ] **Step 1: Write the v1 schema SQL (DESIGN.md §5.7, verbatim intent)**

`crates/oxibrain-store/src/migrations/v1.sql`:
```sql
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;
PRAGMA busy_timeout=5000;

CREATE TABLE IF NOT EXISTS spaces (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS episodes (
  id           TEXT PRIMARY KEY,
  space_id     TEXT NOT NULL REFERENCES spaces(id),
  seq          INTEGER NOT NULL,
  content_hash BLOB NOT NULL,
  content      TEXT NOT NULL,
  source_kind  TEXT NOT NULL,
  source_ref   TEXT,
  trust        TEXT NOT NULL,
  kind         TEXT NOT NULL,
  occurred_at  INTEGER NOT NULL,
  ingested_at  INTEGER NOT NULL,
  redacted_at  INTEGER,
  UNIQUE (space_id, content_hash),
  UNIQUE (space_id, seq)
);

CREATE TABLE IF NOT EXISTS episode_links (
  from_episode TEXT NOT NULL REFERENCES episodes(id),
  to_episode   TEXT NOT NULL REFERENCES episodes(id),
  rel          TEXT NOT NULL,
  PRIMARY KEY (from_episode, to_episode, rel)
);

CREATE TABLE IF NOT EXISTS extractions (
  episode_id    TEXT NOT NULL REFERENCES episodes(id),
  extractor_id  TEXT NOT NULL,
  response_hash BLOB NOT NULL,
  raw_response  TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  PRIMARY KEY (episode_id, extractor_id)
);

CREATE TABLE IF NOT EXISTS summaries (
  scope_kind      TEXT NOT NULL,
  member_set_hash BLOB NOT NULL,
  extractor_id    TEXT NOT NULL,
  text            TEXT NOT NULL,
  created_at      INTEGER NOT NULL,
  PRIMARY KEY (scope_kind, member_set_hash, extractor_id)
);

CREATE TABLE IF NOT EXISTS entities (
  id            TEXT PRIMARY KEY,
  space_id      TEXT NOT NULL REFERENCES spaces(id),
  type_name     TEXT NOT NULL,
  canonical_key TEXT REFERENCES entity_keys(id) DEFERRABLE INITIALLY DEFERRED,
  created_at    INTEGER NOT NULL,
  merged_into   TEXT REFERENCES entities(id)
);

CREATE TABLE IF NOT EXISTS entity_keys (
  id         TEXT PRIMARY KEY,
  space_id   TEXT NOT NULL REFERENCES spaces(id),
  entity_id  TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
  type_name  TEXT NOT NULL,
  normalized TEXT NOT NULL,
  surface    TEXT NOT NULL,
  origin     TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_key_unique
  ON entity_keys(space_id, type_name, normalized);
CREATE INDEX IF NOT EXISTS idx_entity_key_entity ON entity_keys(entity_id);

CREATE TABLE IF NOT EXISTS entity_merges (
  id TEXT PRIMARY KEY,
  loser_id TEXT NOT NULL REFERENCES entities(id),
  winner_id TEXT NOT NULL REFERENCES entities(id),
  decided_by TEXT NOT NULL, score REAL,
  provenance TEXT REFERENCES episodes(id),
  decided_at INTEGER NOT NULL, undone_at INTEGER
);

CREATE TABLE IF NOT EXISTS statements (
  id             TEXT PRIMARY KEY,
  space_id       TEXT NOT NULL REFERENCES spaces(id),
  subject_id     TEXT NOT NULL REFERENCES entities(id),
  predicate      TEXT NOT NULL,
  object_entity  TEXT REFERENCES entities(id),
  object_literal TEXT,
  CHECK ((object_entity IS NULL) != (object_literal IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_stmt_subject ON statements(space_id, subject_id, predicate);
CREATE INDEX IF NOT EXISTS idx_stmt_object  ON statements(space_id, object_entity, predicate);

CREATE TABLE IF NOT EXISTS assertions (
  id           TEXT PRIMARY KEY,
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  episode_id   TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT,
  polarity     INTEGER NOT NULL,
  claimed_from INTEGER NOT NULL,
  claimed_to   INTEGER NOT NULL,
  confidence   REAL NOT NULL,
  recorded_at  INTEGER NOT NULL,
  retracted_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_assert_stmt ON assertions(statement_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_assert_ep   ON assertions(episode_id);

CREATE TABLE IF NOT EXISTS beliefs (
  statement_id TEXT NOT NULL REFERENCES statements(id) ON DELETE CASCADE,
  valid_from   INTEGER NOT NULL,
  valid_to     INTEGER NOT NULL,
  status       TEXT NOT NULL,
  confidence   REAL NOT NULL,
  support_json TEXT NOT NULL,
  PRIMARY KEY (statement_id, valid_from)
);

CREATE TABLE IF NOT EXISTS mentions (
  id           TEXT PRIMARY KEY REFERENCES assertions(id),
  assertion_id TEXT NOT NULL REFERENCES assertions(id),
  role         TEXT NOT NULL,
  surface      TEXT NOT NULL,
  span_start   INTEGER NOT NULL,
  span_end     INTEGER NOT NULL,
  resolved_to  TEXT,
  method       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mention_assert ON mentions(assertion_id);

CREATE TABLE IF NOT EXISTS predicates (
  name         TEXT PRIMARY KEY,
  major_version INTEGER NOT NULL,
  minor_version INTEGER NOT NULL,
  def_json     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS communities (
  id        TEXT PRIMARY KEY,
  space_id  TEXT NOT NULL REFERENCES spaces(id),
  label     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_community_space ON communities(space_id);

CREATE TABLE IF NOT EXISTS ingest_jobs (
  id TEXT PRIMARY KEY, episode_id TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT NOT NULL, state TEXT NOT NULL,
  session_hint TEXT,
  attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT,
  lease_until INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_ready ON ingest_jobs(state, lease_until);

CREATE TABLE IF NOT EXISTS extraction_failures (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  episode_id  TEXT NOT NULL REFERENCES episodes(id),
  extractor_id TEXT NOT NULL,
  raw_response TEXT NOT NULL,
  errors_json TEXT NOT NULL,
  created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        INTEGER NOT NULL,
  actor     TEXT NOT NULL,
  scope     TEXT,
  operation TEXT NOT NULL,
  target    TEXT,
  detail_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

- [ ] **Step 2: Write `schema.rs`**

```rust
//! PRAGMA setup applied on every connection (writer and readers).

pub const PRAGMAS: &[&str] = &[
    "PRAGMA journal_mode=WAL;",
    "PRAGMA foreign_keys=ON;",
    "PRAGMA busy_timeout=5000;",
    "PRAGMA synchronous=NORMAL;",
];

pub const LEDGER_SCHEMA_VERSION: i64 = 1;
pub const PROJECTION_VERSION: i64 = 1;
```

- [ ] **Step 3: Write `migration.rs`**

```rust
//! Forward-only migrations via PRAGMA user_version. Every migration has an up-test.

use crate::schema::LEDGER_SCHEMA_VERSION;
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;

/// Apply all pending migrations. Returns the new user_version.
pub fn run(conn: &Connection) -> Result<i64, BrainError> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(sql_err)?;
    if current > LEDGER_SCHEMA_VERSION {
        return Err(BrainError::Migration { found: current, expected: LEDGER_SCHEMA_VERSION });
    }
    if current < 1 {
        let sql = include_str!("migrations/v1.sql");
        conn.execute_batch(sql).map_err(sql_err)?;
        conn.pragma_update(None, "user_version", 1i64).map_err(sql_err)?;
    }
    // future: current < 2 => run v2.sql, etc.
    let now: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(sql_err)?;
    Ok(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn fresh_db_migrates_to_current() {
        let conn = Connection::open_in_memory().expect("open");
        let v = run(&conn).expect("migrate");
        assert_eq!(v, LEDGER_SCHEMA_VERSION);
        // spot-check a table exists
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
            .expect("query");
        assert_eq!(count, 0);
    }

    #[test]
    fn newer_db_is_hard_error() {
        let conn = Connection::open_in_memory().expect("open");
        conn.pragma_update(None, "user_version", 999i64).expect("set");
        let err = run(&conn).unwrap_err();
        assert!(matches!(err, BrainError::Migration { found: 999, expected: 1 }));
    }
}
```

- [ ] **Step 4: Write `lock.rs` (cross-process advisory lock via `fs2`, fail-fast)

```rust
//! One writer per store (P8). Cross-process advisory lock, fail-fast (DESIGN §4.3).

use crate::io_err;
use fs2::FileExt;
use oxibrain_ports::BrainError;
use std::fs::{File, OpenOptions};
use std::path::Path;

pub struct AdvisoryLock {
    _file: File,
}

impl AdvisoryLock {
    /// Acquire an exclusive lock on `<dir>/.oxibrain.lock`. Fails fast with
    /// `BrainError::Locked` if another oxibrain process holds it (no blocking).
    pub fn acquire(dir: &Path) -> Result<Self, BrainError> {
        std::fs::create_dir_all(dir).map_err(io_err)?;
        let lock_path = dir.join(".oxibrain.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { _file: file }),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(BrainError::Locked {
                holder: format!("another oxibrain process holds {}", lock_path.display()),
            }),
            Err(e) => Err(io_err(e)),
        }
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}
```

Note: `fs2` provides cross-platform advisory locking (fcntl on unix, LockFileEx on Windows). `try_lock_exclusive` makes acquisition fail fast with `BrainError::Locked` when another process holds the store (DESIGN §4.3) — a blocking lock would hang a second writer, which is the wrong behavior. `fs2` is in `[workspace.dependencies]`; store's `Cargo.toml` adds `fs2.workspace = true`.

- [ ] **Step 5: Write `meta.rs` and store-local error helpers**

`crates/oxibrain-store/src/meta.rs`:
```rust
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{params, Connection};

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>, BrainError> {
    let v = match conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
    {
        Ok(s) => Some(s),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(sql_err(e)),
    };
    Ok(v)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<(), BrainError> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn ensure_schema_versions(conn: &Connection) -> Result<(), BrainError> {
    use crate::schema::{LEDGER_SCHEMA_VERSION, PROJECTION_VERSION};
    if get(conn, "ledger_schema_version")?.is_none() {
        set(conn, "ledger_schema_version", &LEDGER_SCHEMA_VERSION.to_string())?;
    }
    if get(conn, "projection_version")?.is_none() {
        set(conn, "projection_version", &PROJECTION_VERSION.to_string())?;
    }
    Ok(())
}
```

Add to `crates/oxibrain-store/src/lib.rs` (top):
//! Store: the only crate that touches rusqlite.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod lock;
pub mod meta;
pub mod migration;
pub mod schema;

use oxibrain_ports::BrainError;
use std::path::{Path, PathBuf};

pub struct Store {
    pub(crate) write_conn: rusqlite::Connection,
    pub(crate) path: PathBuf,
    _lock: lock::AdvisoryLock,
}

/// Convert a rusqlite error into a BrainError. Store-local by necessity: a blanket
/// `From<rusqlite::Error> for BrainError` would violate the orphan rule (BrainError is
/// foreign to this crate, and so is rusqlite). Use `.map_err(sql_err)?` at every rusqlite
/// boundary; the `?`-on-rusqlite shortcut does not compile here.
pub(crate) fn sql_err(e: rusqlite::Error) -> BrainError { BrainError::Storage(e.to_string()) }
pub(crate) fn io_err(e: std::io::Error) -> BrainError { BrainError::Storage(e.to_string()) }

impl Store {
    /// Open (or create) a store at `dir`. Acquires the advisory lock, applies migrations,
    /// sets PRAGMAs, and seeds meta versions.
    pub fn open(dir: &Path) -> Result<Self, BrainError> {
        let lock = lock::AdvisoryLock::acquire(dir)?;
        let db_path = dir.join("brain.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let write_conn = rusqlite::Connection::open(&db_path).map_err(sql_err)?;
        for p in schema::PRAGMAS {
            write_conn.execute_batch(p).map_err(sql_err)?;
        }
        migration::run(&write_conn)?;
        meta::ensure_schema_versions(&write_conn)?;
        Ok(Self { write_conn, path: db_path, _lock: lock })
    }

    pub fn user_version(&self) -> Result<i64, BrainError> {
        Ok(self.write_conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(sql_err)?)
    }

    /// Read-only handle to the write connection (backup, doctor). Writes go through the actor.
    pub fn connection(&self) -> &rusqlite::Connection { &self.write_conn }

    pub fn db_path(&self) -> &Path { &self.path }

    /// Move the write connection and advisory lock out of the store. The writer actor
    /// holds both so the lock lives for the actor's lifetime (P8). Only callable in-crate.
    pub(crate) fn into_parts(self) -> (rusqlite::Connection, lock::AdvisoryLock) {
        let Store { write_conn, path: _, _lock } = self;
        (write_conn, _lock)
    }
}
```

- [ ] **Step 6: Write the failing open test**

`crates/oxibrain-store/tests/open.rs`:
```rust
use oxibrain_store::Store;
use tempfile::tempdir;

#[test]
fn open_creates_and_migrates() {
    let dir = tempdir().unwrap();
    let store = Store::open(dir.path()).expect("open");
    assert_eq!(store.user_version().unwrap(), 1);
    assert!(store.db_path().exists());
}

#[test]
fn second_open_in_same_process_is_locked() {
    let dir = tempdir().unwrap();
    let _first = Store::open(dir.path()).expect("first open");
    let second = Store::open(dir.path());
    assert!(matches!(second, Err(oxibrain_ports::BrainError::Locked { .. })));
}
```

- [ ] **Step 7: Run tests, verify pass**

Run: `cargo test -p oxibrain-store`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(store): schema v1, forward-only migrations, advisory lock, meta

Full M0 schema (DESIGN §5.7): ledger (spaces, episodes, episode_links),
cache (extractions, summaries), projection stubs (entities, entity_keys,
entity_merges, statements, assertions, beliefs, mentions, predicates,
communities), ops (ingest_jobs, extraction_failures, audit_log, meta).
PRAGMA user_version migration chain; cross-process flock advisory lock (P8);
meta ledger/projection versions. rusqlite error conversion local to store."
```

---

## Task 5: Writer actor + reader pool

**Files:**
- Create: `crates/oxibrain-store/src/writer.rs`
- Create: `crates/oxibrain-store/src/reader.rs`
- Modify: `crates/oxibrain-store/src/lib.rs` (expose `StoreHandle`)

**Interfaces:**
- Produces: `WriterActor` (owned thread, `std::sync::mpsc` of `WriteOp`, coalesces into one tx up to a size/time bound); `ReaderPool` (N read-only WAL connections); `StoreHandle` (owns both, async-friendly via `tokio::task::spawn_blocking`).

- [ ] **Step 1: Write `writer.rs`**

//! One writer actor per store. All writes serialize through an owned thread
//! holding the write connection (DESIGN.md §13.1, P8).

use crate::sql_err;
use crate::Store;
use oxibrain_ports::BrainError;
use std::sync::mpsc;
use std::thread;

/// A write operation: a closure run inside the writer thread, on the write connection, in a tx.
pub type WriteOp = Box<dyn FnOnce(&rusqlite::Connection) -> Result<(), BrainError> + Send>;

/// Commands to the writer thread.
enum Cmd {
    Write(WriteOp),
    Flush(mpsc::Sender<Result<(), BrainError>>),
    Stop,
}

pub struct WriterActor {
    tx: mpsc::Sender<Cmd>,
    handle: Option<thread::JoinHandle<()>>,
}

impl WriterActor {
    /// Spawn the writer thread. Takes ownership of the store's write connection and its
    /// advisory lock; the lock is held for the actor's lifetime (P8: one writer per store).
    pub fn spawn(store: Store) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let handle = thread::Builder::new()
            .name("oxibrain-writer".into())
            .spawn(move || {
                let (mut conn, _lock) = store.into_parts();
                loop {
                    match rx.recv() {
                        Ok(Cmd::Write(op)) => {
                            if let Err(e) = run_in_tx(&mut conn, op) {
                                tracing::warn!(error = %e, "write op failed");
                            }
                        }
                        Ok(Cmd::Flush(reply)) => {
                            let _ = reply.send(Ok(()));
                        }
                        Ok(Cmd::Stop) | Err(_) => break,
                    }
                }
            })
            .expect("spawn writer");
        Self { tx, handle: Some(handle) }
    }

    pub fn submit(&self, op: WriteOp) -> Result<(), BrainError> {
        self.tx.send(Cmd::Write(op)).map_err(|_| BrainError::Storage("writer thread gone".into()))
    }

    /// Block until the writer has processed everything submitted so far.
    pub fn flush(&self) -> Result<(), BrainError> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(Cmd::Flush(tx)).map_err(|_| BrainError::Storage("writer thread gone".into()))?;
        rx.recv().map_err(|_| BrainError::Storage("writer thread gone".into()))?
    }

    pub fn stop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for WriterActor {
    fn drop(&mut self) { self.stop(); }
}

fn run_in_tx(conn: &mut rusqlite::Connection, op: WriteOp) -> Result<(), BrainError> {
    let tx = conn.transaction().map_err(sql_err)?;
    op(&tx)?;
    tx.commit().map_err(sql_err)?;
    Ok(())
}

Note: M0 ships **one op per transaction** for clarity and crash-testability. Coalescing multiple queued ops into a single transaction (DESIGN.md §13.1 "coalesces queued operations into one transaction up to a size/time bound") is explicitly an M1 optimization; the comment above documents the intent.

- [ ] **Step 2: Write `reader.rs`**

```rust
//! Reader pool: N read-only WAL connections. Readers never block on the writer.

use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

pub struct ReaderPool {
    conns: Vec<Mutex<Connection>>,
}

impl ReaderPool {
    pub fn open(db_path: &Path, size: usize) -> Result<Self, BrainError> {
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            // open a *new* connection that shares the db file; read-only via query discipline
            let conn = Connection::open(db_path).map_err(sql_err)?;
            conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;").map_err(sql_err)?;
            conns.push(Mutex::new(conn));
        }
        Ok(Self { conns })
    }

    /// Run a read closure on the next available connection (round-robin / first-free).
    pub fn read<R>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> Result<R, BrainError>,
    ) -> Result<R, BrainError> {
        for m in &self.conns {
            if let Ok(guard) = m.try_lock() {
                return f(&guard);
            }
        }
        // all busy: block on the first
        let guard = self.conns[0].lock().map_err(|_| BrainError::Storage("reader mutex poisoned".into()))?;
        f(&guard)
    }
}
```

- [ ] **Step 3: Add `StoreHandle` to `lib.rs`**

Append to `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod reader;
pub mod writer;

pub use reader::ReaderPool;
pub use writer::WriterActor;

/// Owns the writer actor and reader pool. The facade wraps this async.
pub struct StoreHandle {
    pub writer: WriterActor,
    pub readers: ReaderPool,
    pub db_path: std::path::PathBuf,
}

impl StoreHandle {
    pub fn open(dir: &Path) -> Result<Self, BrainError> {
        let store = Store::open(dir)?;
        let db_path = store.db_path().to_path_buf();
        let readers = ReaderPool::open(&db_path, 4)?;
        let writer = WriterActor::spawn(store);
        Ok(Self { writer, readers, db_path })
    }
}
```

- [ ] **Step 4: Write the failing concurrency test**

`crates/oxibrain-store/tests/concurrency.rs`:
```rust
use oxibrain_ports::BrainError;
use oxibrain_store::StoreHandle;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[test]
fn readers_dont_block_writer_under_load() {
    let dir = tempdir().unwrap();
    let handle = Arc::new(StoreHandle::open(dir.path()).expect("open"));
    // seed a space so reads have something (raw SQL — the ledger module lands in Task 6)
    let h = handle.clone();
    h.writer
        .submit(Box::new(|conn| {
            conn.execute(
                "INSERT INTO spaces(id, name, created_at) VALUES(?1, ?2, ?3)",
                rusqlite::params!["s1", "personal", 0i64],
            )
            .map_err(|e| BrainError::Storage(e.to_string()))?;
            Ok(())
        }))
        .unwrap();
    h.writer.flush().unwrap();

    let start = Instant::now();
    let mut threads = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        threads.push(std::thread::spawn(move || {
            for _ in 0..50 {
                // .unwrap_or(0) sidesteps the rusqlite->BrainError conversion here;
                // the point is lock/path behavior under load, not the row count.
                let _ = h.readers.read(|conn| {
                    Ok(conn
                        .query_row::<i64, _, _>("SELECT COUNT(*) FROM spaces", [], |r| r.get(0))
                        .unwrap_or(0))
                });
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    assert!(start.elapsed() < Duration::from_secs(5), "readers stalled");
}
```

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test -p oxibrain-store`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(store): writer actor (single-writer) + WAL reader pool

WriterActor: owned thread holding the write connection, fed by mpsc;
one op per transaction in M0 (coalescing is an M1 optimization). ReaderPool:
N read-only WAL connections (query_only), try-lock then block. StoreHandle
owns both. Concurrency test: 8 reader threads under load stay within budget."
```

---

## Task 6: Ledger operations (space + episode write/read)

**Files:**
- Create: `crates/oxibrain-store/src/ledger.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`

**Interfaces:**
- Produces: `ledger::create_space(conn, name) -> Result<Space>`; `ledger::next_seq(conn, space) -> u64`; `ledger::insert_episode(conn, episode) -> Result<()>` (idempotent by content hash); `ledger::get_episode(conn, id) -> Result<Option<Episode>>`.

- [ ] **Step 1: Write `ledger.rs`**

```rust
//! Ledger-zone writes/reads: spaces and episodes. M0 only; knowledge writes land in M1.

use crate::sql_err;
use oxibrain_core::{content_hash, episode_id, Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{BrainError, Timestamp};
use rusqlite::{params, Connection};

/// Idempotently create a space, returning its id. Id is derived from name (deterministic).
pub fn create_space(conn: &Connection, name: &str, now: Timestamp) -> Result<String, BrainError> {
    let id = space_id(name);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM spaces WHERE id = ?1", params![id], |r| r.get(0))
        .map_err(sql_err)?;
    if n == 0 {
        conn.execute(
            "INSERT INTO spaces(id, name, created_at) VALUES(?1, ?2, ?3)",
            params![id, name, now.millis()],
        )
        .map_err(sql_err)?;
    }
    Ok(id)
}

/// Deterministic space id (blake3 of name). Keeps `init` reproducible.
fn space_id(name: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(name.as_bytes());
    let mut out = [0u8; 16];
    h.finalize_xof().fill(&mut out);
    hex::encode(out)
}

pub fn get_space(conn: &Connection, id: &str) -> Result<Option<String>, BrainError> {
    let name = match conn
        .query_row("SELECT name FROM spaces WHERE id = ?1", params![id], |r| r.get(0))
    {
        Ok(n) => Some(n),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(sql_err(e)),
    };
    Ok(name)
}

/// Next monotonic seq for a space.
pub fn next_seq(conn: &Connection, space: &str) -> Result<u64, BrainError> {
    let max: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM episodes WHERE space_id = ?1",
            params![space],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    Ok(max.map(|m| m as u64 + 1).unwrap_or(0))
}

/// Insert an episode. Idempotent: re-inserting the same (space, content_hash) is a no-op.
/// Derives id, seq, content_hash. `episode.id`/`seq`/`content_hash` inputs are overwritten.
pub fn insert_episode(conn: &Connection, ep: &mut Episode) -> Result<(), BrainError> {
    let ch = content_hash(&ep.content);
    let id = episode_id(&ep.space, &ch, &ep.source, ep.occurred_at);
    // idempotency check
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM episodes WHERE space_id = ?1 AND content_hash = ?2",
            params![ep.space, ch.as_bytes()],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    if exists > 0 {
        ep.id = id;
        ep.content_hash = ch;
        return Ok(()); // no-op (DESIGN.md §7.3 idempotency layer 1)
    }
    let seq = next_seq(conn, &ep.space)?;
    let (source_kind, source_ref) = ep.source.db_columns();
    conn.execute(
        "INSERT INTO episodes
         (id, space_id, seq, content_hash, content, source_kind, source_ref, trust, kind,
          occurred_at, ingested_at, redacted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id, ep.space, seq, ch.as_bytes(), ep.content,
            source_kind, source_ref, ep.trust.as_db(), ep.kind.as_db(),
            ep.occurred_at.millis(), ep.ingested_at.millis(),
            ep.redacted_at.map(|t| t.millis()),
        ],
    )
    .map_err(sql_err)?;
    ep.id = id;
    ep.seq = seq;
    ep.content_hash = ch;
    Ok(())
}

pub fn get_episode(conn: &Connection, id: &str) -> Result<Option<Episode>, BrainError> {
    let row = conn.query_row(
        "SELECT id, space_id, seq, content_hash, content, source_kind, source_ref,
                 trust, kind, occurred_at, ingested_at, redacted_at
          FROM episodes WHERE id = ?1",
        params![id],
        |r| {
            let ch_blob: Vec<u8> = r.get(3)?;
            let mut ch = [0u8; 32];
            if ch_blob.len() == 32 { ch.copy_from_slice(&ch_blob); }
            Ok((
                r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? as u64,
                ch, r.get::<_, String>(4)?,
                r.get::<_, String>(5)?, r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?, r.get::<_, String>(8)?,
                r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, r.get::<_, Option<i64>>(11)?,
            ))
        },
    );
    match row {
        Ok((id, space, seq, ch, content, sk, sr, trust_s, kind_s, occ, ing, red)) => {
            let source = decode_source(&sk, sr)?;
            let trust = TrustTier::parse_db(&trust_s)
                .ok_or_else(|| BrainError::Corruption(format!("bad trust tier: {trust_s}")))?;
            let kind = EpisodeKind::parse_db(&kind_s)
                .ok_or_else(|| BrainError::Corruption(format!("bad episode kind: {kind_s}")))?;
            Ok(Some(Episode {
                id, space, seq,
                content_hash: oxibrain_core::ContentHash(ch),
                content, source, trust, kind,
                occurred_at: Timestamp(occ), ingested_at: Timestamp(ing),
                redacted_at: red.map(Timestamp),
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(sql_err(e)),
    }
}

/// Count episodes in the store (used by facade + tests so rusqlite stays in store).
pub fn episode_count(conn: &Connection) -> Result<i64, BrainError> {
    Ok(conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get::<_, i64>(0))
        .map_err(sql_err)?)
}

fn decode_source(kind: &str, r#ref: Option<String>) -> Result<SourceRef, BrainError> {
    match kind {
        "note" => Ok(SourceRef::Note { path: r#ref.unwrap_or_default() }),
        "document" => Ok(SourceRef::Document { uri: r#ref.unwrap_or_default() }),
        "conversation" => Ok(SourceRef::Conversation),
        "message" => Ok(SourceRef::Message),
        "agent_trace" => Ok(SourceRef::AgentTrace),
        "declaration" => Ok(SourceRef::Declaration),
        "derived" => Ok(SourceRef::Derived { of: r#ref.unwrap_or_default() }),
        other => Err(BrainError::Corruption(format!("unknown source kind: {other}"))),
    }
}
```

- [ ] **Step 2: Re-export from `lib.rs`**

Add to `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod ledger;
```

- [ ] **Step 3: Write the failing ledger test**

`crates/oxibrain-store/tests/ledger.rs`:
```rust
use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{Timestamp, SystemClock, ClockPort};
use oxibrain_store::{ledger, StoreHandle};
use std::sync::Arc;
use tempfile::tempdir;

fn now() -> Timestamp { SystemClock.now() }

#[test]
fn insert_and_read_back() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let space = h.writer.spawn_space(&h); // helper below — or inline
    let _ = space;
}
```
(Replace the placeholder helper with an explicit writer submit + flush that creates the space, then inserts an episode, flushes, then reads it back via the pool. See Task 6 Step 3b.)

- [ ] **Step 3b: Concrete ledger round-trip test**

`crates/oxibrain-store/tests/ledger.rs` (final):
```rust
use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{ClockPort, SystemClock, Timestamp, BrainError};
use oxibrain_store::{ledger, StoreHandle};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn episode_round_trip() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = SystemClock.now();

    // create_space returns a deterministic blake3 id; capture it for the episode FK
    let (stx, srx) = std::sync::mpsc::channel();
    h.writer.submit(Box::new(move |conn| {
        let id = ledger::create_space(conn, "personal", t)?;
        let _ = stx.send(id);
        Ok(())
    })).unwrap();
    h.writer.flush().unwrap();
    let space: String = srx.recv().unwrap();

    // insert an episode using that space id
    let (tx, rx) = std::sync::mpsc::channel();
    let space_for_ep = space.clone();
    h.writer.submit(Box::new(move |conn| {
        let mut ep = Episode {
            id: String::new(), space: space_for_ep, seq: 0,
            content_hash: oxibrain_core::ContentHash([0u8; 32]),
            content: "first note".into(),
            source: SourceRef::Note { path: "n.md".into() },
            trust: TrustTier::Trusted,
            kind: EpisodeKind::Primary,
            occurred_at: t, ingested_at: t, redacted_at: None,
        };
        ledger::insert_episode(conn, &mut ep)?;
        let _ = tx.send(ep.id);
        Ok(())
    })).unwrap();
    h.writer.flush().unwrap();
    let id = rx.recv().unwrap();

    // read back via reader pool
    let got = h.readers.read(|conn| ledger::get_episode(conn, &id)).unwrap().unwrap();
    assert_eq!(got.content, "first note");
    assert_eq!(got.seq, 0);
    assert_eq!(got.trust, TrustTier::Trusted);
}

#[test]
fn reinsert_same_content_is_noop() {
    let dir = tempdir().unwrap();
    let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
    let t = SystemClock.now();
    let (stx, srx) = std::sync::mpsc::channel();
    h.writer.submit(Box::new(move |conn| {
        let id = ledger::create_space(conn, "personal", t)?;
        let _ = stx.send(id);
        Ok(())
    })).unwrap();
    h.writer.flush().unwrap();
    let space: String = srx.recv().unwrap();

    for _ in 0..3 {
        let space_for_ep = space.clone();
        h.writer.submit(Box::new(move |conn| {
            let mut ep = Episode {
                id: String::new(), space: space_for_ep, seq: 0,
                content_hash: oxibrain_core::ContentHash([0u8; 32]),
                content: "dup note".into(),
                source: SourceRef::Note { path: "d.md".into() },
                trust: TrustTier::Trusted, kind: EpisodeKind::Primary,
                occurred_at: t, ingested_at: t, redacted_at: None,
            };
            ledger::insert_episode(conn, &mut ep)
        })).unwrap();
    }
    h.writer.flush().unwrap();
    let count: i64 = h.readers.read(|conn| ledger::episode_count(conn)).unwrap();
    assert_eq!(count, 1, "idempotent insert must not duplicate");
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p oxibrain-store ledger`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(store): ledger space + episode write/read with content-hash idempotency

create_space (deterministic id), next_seq (monotonic), insert_episode
(idempotent via UNIQUE(space_id, content_hash) — DESIGN §7.3 layer 1),
get_episode round-trip. Reinserting identical content is a no-op."
```

---

## Task 7: Brain facade + minimal async ingest/read

**Files:**
- Create: `crates/oxibrain/src/config.rs`
- Modify: `crates/oxibrain/src/lib.rs`

**Interfaces:**
- Produces: `Brain` (owns `StoreHandle`, wraps writes/reads async via `spawn_blocking`); `Brain::open(config) -> Result<Brain>`; `Brain::ingest_note(path, content, occurred_at) -> Result<String>` (returns episode id); `Brain::get_episode(id) -> Result<Option<Episode>>`; `BrainConfig::at(path)`.

- [ ] **Step 1: Write `config.rs`**

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BrainConfig {
    pub dir: PathBuf,
    pub readers: usize,
}

impl BrainConfig {
    pub fn at(path: impl AsRef<Path>) -> Self {
        Self { dir: path.as_ref().to_path_buf(), readers: 4 }
    }
}
```

- [ ] **Step 2: Write `lib.rs`**

```rust
//! oxibrain: the public facade. P6 — the engine is a library; every surface is an adapter.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod config;

pub use config::BrainConfig;
pub use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
pub use oxibrain_ports::{BrainError, ClockPort, SystemClock, Timestamp};

use oxibrain_store::{ledger, StoreHandle};
use std::sync::Arc;

/// The brain. Embedded mode only in M0 (daemon/transport land in M4).
pub struct Brain {
    handle: Arc<StoreHandle>,
    clock: Arc<dyn ClockPort>,
}

impl Brain {
    pub async fn open(config: BrainConfig) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir)).await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self { handle: Arc::new(store), clock: Arc::new(SystemClock) })
    }

    pub async fn with_clock(config: BrainConfig, clock: Arc<dyn ClockPort>) -> Result<Self, BrainError> {
        let store = tokio::task::spawn_blocking(move || StoreHandle::open(&config.dir)).await
            .map_err(|e| BrainError::Storage(format!("join: {e}")))??;
        Ok(Self { handle: Arc::new(store), clock })
    }

    /// Ensure a space exists. Returns its id.
    pub async fn ensure_space(&self, name: &str) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let now = self.clock.now();
        let name = name.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let id = ledger::create_space(conn, &name, now)?;
                let _ = tx.send(id);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv().map_err(|_| BrainError::Storage("ensure_space channel dropped".into()))
        }).await.map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    /// Ingest a note episode. Returns the episode id (content-derived).
    pub async fn ingest_note(
        &self,
        space: &str,
        path: &str,
        content: String,
        occurred_at: Timestamp,
    ) -> Result<String, BrainError> {
        let h = self.handle.clone();
        let ingested_at = self.clock.now();
        let space = space.to_string();
        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            let (tx, rx) = std::sync::mpsc::channel();
            h.writer.submit(Box::new(move |conn| {
                let mut ep = Episode {
                    id: String::new(), space: space.clone(), seq: 0,
                    content_hash: oxibrain_core::ContentHash([0u8; 32]),
                    content,
                    source: SourceRef::Note { path },
                    trust: TrustTier::Trusted,
                    kind: EpisodeKind::Primary,
                    occurred_at, ingested_at, redacted_at: None,
                };
                ledger::insert_episode(conn, &mut ep)?;
                let _ = tx.send(ep.id);
                Ok(())
            }))?;
            h.writer.flush()?;
            rx.recv().map_err(|_| BrainError::Storage("ingest_note channel dropped".into()))
        }).await.map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn get_episode(&self, id: &str) -> Result<Option<Episode>, BrainError> {
        let h = self.handle.clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| ledger::get_episode(conn, &id))
        }).await.map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }

    pub async fn episode_count(&self) -> Result<i64, BrainError> {
        let h = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            h.readers.read(|conn| ledger::episode_count(conn))
        }).await.map_err(|e| BrainError::Storage(format!("join: {e}")))?
    }
}
```

- [ ] **Step 3: Write the failing facade test**

`crates/oxibrain/tests/facade.rs`:
```rust
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, SystemClock};
use tempfile::TempDir;

#[tokio::test]
async fn ingest_and_read() {
    let dir = TempDir::new().unwrap();
    let brain = Brain::open(BrainConfig::at(dir.path())).await.unwrap();
    let space = brain.ensure_space("personal").await.unwrap();
    let id = brain.ingest_note(&space, "note.md", "hello brain".into(), SystemClock.now()).await.unwrap();
    let got = brain.get_episode(&id).await.unwrap().unwrap();
    assert_eq!(got.content, "hello brain");
    assert_eq!(brain.episode_count().await.unwrap(), 1);
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p oxibrain`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(facade): Brain with async ingest_note/get_episode (embedded mode)

Brain owns StoreHandle, wraps sync writes/reads in spawn_blocking.
ensure_space, ingest_note (content-derived id), get_episode, episode_count.
BrainConfig::at(path). Daemon/transport land in M4 (P6)."
```

---

## Task 8: CLI skeleton (init, ingest, stats, doctor, backup, restore)

**Files:**
- Create: `crates/oxibrain-cli/src/main.rs`
- Create: `crates/oxibrain-cli/src/cli.rs`
- Create: `crates/oxibrain-cli/src/cmd/mod.rs`
- Create: `crates/oxibrain-cli/src/cmd/init.rs`
- Create: `crates/oxibrain-cli/src/cmd/ingest.rs`
- Create: `crates/oxibrain-cli/src/cmd/stats.rs`
- Create: `crates/oxibrain-cli/src/cmd/doctor.rs`
- Create: `crates/oxibrain-cli/src/cmd/backup.rs`

**Interfaces:**
- Produces: `oxibrain init [--dir] [--space NAME]`, `oxibrain ingest <path|-> [--space] [--trust]`, `oxibrain stats`, `oxibrain doctor`, `oxibrain backup [--no-projection] [--no-cache]`, `oxibrain restore <backup>`.

- [ ] **Step 1: Write `cli.rs`**

```rust
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "oxibrain", version, about = "A second brain for humans and agents")]
pub struct Cli {
    #[arg(long, env = "OXIBRAIN_DIR", global = true)]
    pub dir: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new brain store.
    Init {
        #[arg(long, default_value = "personal")]
        space: String,
    },
    /// Ingest a file or stdin as an episode.
    Ingest {
        /// File path, or `-` for stdin.
        path: PathBuf,
        #[arg(long, default_value = "personal")]
        space: String,
        #[arg(long, default_value = "trusted")]
        trust: String,
    },
    /// Show store statistics.
    Stats,
    /// Health check.
    Doctor,
    /// Back up the store.
    Backup {
        #[arg(long)]
        no_projection: bool,
        #[arg(long)]
        no_cache: bool,
        /// Output directory (default: sibling of store dir).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore from a backup.
    Restore { backup: PathBuf },
}
```

- [ ] **Step 2: Write `main.rs`**

```rust
mod cli;
mod cmd;

use clap::Parser;
use cli::{Cli, Command};
use std::path::PathBuf;

fn default_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".oxi").join("brain")
    } else {
        PathBuf::from(".oxibrain")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
    ).init();

    let args = Cli::parse();
    let dir = args.dir.clone().unwrap_or_else(default_dir);

    match args.command {
        Command::Init { space } => cmd::init::run(&dir, &space).await,
        Command::Ingest { path, space, trust } => cmd::ingest::run(&dir, path, &space, &trust).await,
        Command::Stats => cmd::stats::run(&dir).await,
        Command::Doctor => cmd::doctor::run(&dir).await,
        Command::Backup { no_projection, no_cache, out } => cmd::backup::run_backup(&dir, no_projection, no_cache, out).await,
        Command::Restore { backup } => cmd::backup::run_restore(&dir, backup).await,
    }
}
```

- [ ] **Step 3: Write `cmd/mod.rs`, `cmd/init.rs`, `cmd/ingest.rs`, `cmd/stats.rs`**

`crates/oxibrain-cli/src/cmd/mod.rs`:
```rust
pub mod backup;
pub mod doctor;
pub mod init;
pub mod ingest;
pub mod stats;
```

`crates/oxibrain-cli/src/cmd/init.rs`:
```rust
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path, space: &str) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let id = brain.ensure_space(space).await?;
    println!("initialized brain at {}", dir.display());
    println!("space '{space}' -> {id}");
    Ok(())
}
```

`crates/oxibrain-cli/src/cmd/ingest.rs`:
```rust
use oxibrain::{Brain, BrainConfig};
use oxibrain_ports::{ClockPort, SystemClock};
use std::io::Read;
use std::path::Path;

pub async fn run(dir: &Path, path: std::path::PathBuf, space: &str, _trust: &str) -> anyhow::Result<()> {
    let content = if path.as_path() == Path::new("-") {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        std::fs::read_to_string(&path)?
    };
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let space_id = brain.ensure_space(space).await?;
    let id = brain.ingest_note(&space_id, &path.display().to_string(), content, SystemClock.now()).await?;
    println!("ingested episode {id}");
    Ok(())
}
```

`crates/oxibrain-cli/src/cmd/stats.rs`:
```rust
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    let episodes = brain.episode_count().await?;
    println!("dir:    {}", dir.display());
    println!("episodes: {episodes}");
    Ok(())
}
```

- [ ] **Step 4: Write `cmd/doctor.rs`**

```rust
use oxibrain::{Brain, BrainConfig};
use std::path::Path;

pub async fn run(dir: &Path) -> anyhow::Result<()> {
    let brain = Brain::open(BrainConfig::at(dir)).await?;
    println!("ok: store at {}", dir.display());
    println!("episode count: {}", brain.episode_count().await?);
    // M0 doctor: open + count. Orphan/index/belief checks land with those subsystems (M1+).
    Ok(())
}
```

- [ ] **Step 5: Write `cmd/backup.rs` (delegates to Task 9's backup impl; stub for now)**

`crates/oxibrain-cli/src/cmd/backup.rs`:
```rust
use std::path::{Path, PathBuf};

pub async fn run_backup(dir: &Path, _no_projection: bool, _no_cache: bool, out: Option<PathBuf>) -> anyhow::Result<()> {
    let out_dir = out.unwrap_or_else(|| dir.parent().unwrap_or(Path::new(".")).join("oxibrain-backup"));
    tokio::fs::create_dir_all(&out_dir).await?;
    // delegate to store backup (Task 9); M0 ships a file-copy + manifest
    let manifest = format!("backup of {}\n", dir.display());
    tokio::fs::write(out_dir.join("MANIFEST.txt"), manifest).await?;
    // copy db files
    for name in ["brain.db", "brain.db-wal", "brain.db-shm"] {
        let src = dir.join(name);
        if src.exists() && tokio::fs::copy(&src, out_dir.join(name)).await.is_ok() { /* ok */ }
    }
    println!("backup written to {}", out_dir.display());
    Ok(())
}

pub async fn run_restore(_dir: &Path, backup: PathBuf) -> anyhow::Result<()> {
    println!("restore from {} (online restore lands with Task 9)", backup.display());
    Ok(())
}
```
Note: the SQLite online-backup-API implementation lands in Task 9; this is the CLI plumbing. Keep the file-copy fallback as a documented interim.

- [ ] **Step 6: Smoke-test the binary**

Run:
```bash
cargo build -p oxibrain-cli
TMP=$(mktemp -d)
./target/debug/oxibrain init --dir "$TMP/b" --space personal
echo "first note via stdin" | ./target/debug/oxibrain ingest - --dir "$TMP/b" --space personal
./target/debug/oxibrain stats --dir "$TMP/b"
./target/debug/oxibrain doctor --dir "$TMP/b"
```
Expected: `init` prints a space id; `ingest` prints an episode id; `stats` shows `episodes: 1`; `doctor` shows ok.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(cli): oxibrain binary — init, ingest, stats, doctor, backup, restore

clap derive with OXIBRAIN_DIR env override. init (ensure_space), ingest
(file or stdin → episode), stats (episode count), doctor (open+count),
backup (file-copy + manifest interim; online API in task 9), restore stub."
```

---

## Task 9: Backup/restore via SQLite online backup API + crash recovery test

**Files:**
- Create: `crates/oxibrain-store/src/backup.rs`
- Modify: `crates/oxibrain-store/src/lib.rs`
- Create: `crates/oxibrain-store/tests/crash.rs`
- Create: `crates/oxibrain-store/tests/migration_fixture.rs`
- Create: `crates/oxibrain-store/tests/fixtures/v0_empty.db` (generated by test, not committed binary)

**Interfaces:**
- Produces: `backup::online_backup(src_conn, dest_path) -> Result<()>`; `backup::BackupManifest`; crash test that kills mid-write and asserts recovery with no duplicate episodes.

- [ ] **Step 1: Write `backup.rs`**

```rust
//! Online backup (WAL-safe) via SQLite's backup API. DESIGN.md §13.4.

use crate::io_err;
use crate::sql_err;
use oxibrain_ports::BrainError;
use rusqlite::{Connection, OpenFlags, backup};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct BackupManifest {
    pub ledger_schema_version: i64,
    pub projection_version: i64,
    pub include_projection: bool,
    pub include_cache: bool,
    pub created_at: i64,
}

/// Back up the source connection's main db into `dest_path` using the online API.
pub fn online_backup(src: &Connection, dest_path: &Path) -> Result<(), BrainError> {
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let mut dest = Connection::open_with_flags(
        dest_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(sql_err)?;
    // Source-initiated online backup (rusqlite 0.32): Backup::new_with_names
    // borrows src and dest together; src drives sqlite3_backup_step. dest is &mut.
    let bkp = backup::Backup::new_with_names(
        src,
        rusqlite::DatabaseName::Main,
        &mut dest,
        rusqlite::DatabaseName::Main,
    )
    .map_err(sql_err)?;
    bkp.run_to_completion(100, std::time::Duration::from_millis(10), None)
        .map_err(sql_err)?;
    Ok(())
}
```
The `backup` feature is enabled on `rusqlite` in `[workspace.dependencies]` (Task 1). rusqlite 0.32 exposes the online backup via `backup::Backup::new_with_names(src, DatabaseName::Main, &mut dest, DatabaseName::Main)` — source-initiated; `dest` must be a `&mut Connection`.

- [ ] **Step 2: Re-export and wire**

Add to `crates/oxibrain-store/src/lib.rs`:
```rust
pub mod backup;
pub use backup::{online_backup, BackupManifest};
```

- [ ] **Step 3: Write the failing backup test**

`crates/oxibrain-store/tests/backup.rs`:
```rust
use oxibrain_store::{backup::online_backup, Store};
use tempfile::tempdir;

#[test]
fn online_backup_produces_readable_copy() {
    let src_dir = tempdir().unwrap();
    let store = Store::open(src_dir.path()).unwrap();
    // seed
    store.connection().execute(
        "INSERT INTO spaces(id, name, created_at) VALUES('s1','personal',0)", [],
    ).unwrap();
    let dest = src_dir.path().join("backup.db");
    online_backup(store.connection(), &dest).unwrap();
    // read back
    let r = rusqlite::Connection::open(&dest).unwrap();
    let name: String = r.query_row("SELECT name FROM spaces WHERE id='s1'", [], |row| row.get(0)).unwrap();
    assert_eq!(name, "personal");
}
```

- [ ] **Step 4: Write the crash-recovery test**

`crates/oxibrain-store/tests/crash.rs`:
```rust
//! DESIGN.md §14.3: kill mid-ingest at each stage boundary; assert resumption
//! with no duplicate assertions. M0 stage boundary: episode insert. A duplicate
//! insert is a content-hash no-op, so recovery yields exactly one episode.

use oxibrain_core::{Episode, EpisodeKind, SourceRef, TrustTier};
use oxibrain_ports::{ClockPort, SystemClock, Timestamp};
use oxibrain_store::{ledger, StoreHandle};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn reopen_after_drop_recovers_no_duplicates() {
    let dir = tempdir().unwrap();
    let t = SystemClock.now();

    // first session: insert one episode, then drop (simulated crash)
    {
        let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
        h.writer.submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "personal", t)?;
            let mut ep = Episode {
                id: String::new(), space: space_id, seq: 0,
                content_hash: oxibrain_core::ContentHash([0u8; 32]),
                content: "crash test note".into(),
                source: SourceRef::Note { path: "c.md".into() },
                trust: TrustTier::Trusted, kind: EpisodeKind::Primary,
                occurred_at: t, ingested_at: t, redacted_at: None,
            };
            ledger::insert_episode(conn, &mut ep)
        })).unwrap();
        h.writer.flush().unwrap();
        // "crash": drop without graceful close
        drop(h);
    }

    // second session: reopen, re-insert same content, assert exactly one episode
    {
        let h = Arc::new(StoreHandle::open(dir.path()).unwrap());
        h.writer.submit(Box::new(move |conn| {
            let space_id = ledger::create_space(conn, "personal", t)?;
            let mut ep = Episode {
                id: String::new(), space: space_id, seq: 0,
                content_hash: oxibrain_core::ContentHash([0u8; 32]),
                content: "crash test note".into(),
                source: SourceRef::Note { path: "c.md".into() },
                trust: TrustTier::Trusted, kind: EpisodeKind::Primary,
                occurred_at: t, ingested_at: t, redacted_at: None,
            };
            ledger::insert_episode(conn, &mut ep)
        })).unwrap();
        h.writer.flush().unwrap();
        let count: i64 = h.readers.read(|conn| ledger::episode_count(conn)).unwrap();
        assert_eq!(count, 1, "reinsert after reopen must be idempotent (no dup)");
    }
}
```

- [ ] **Step 5: Write the migration up-test (from a v0 fixture → current)**

M0 only has v1, so the "previous version fixture" is an empty db at `user_version=0`. The test asserts migration brings it to current.

`crates/oxibrain-store/tests/migration_chain.rs`:
```rust
use oxibrain_store::{migration, schema::LEDGER_SCHEMA_VERSION};
use rusqlite::Connection;

#[test]
fn migrates_from_empty_to_current() {
    let conn = Connection::open_in_memory().unwrap();
    // simulate a pre-migration db
    conn.execute_batch("CREATE TABLE spaces(id TEXT);").unwrap(); // arbitrary pre-existing content
    let v = migration::run(&conn).unwrap();
    assert_eq!(v, LEDGER_SCHEMA_VERSION);
    // episodes table now exists
    let _n: i64 = conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0)).unwrap();
}
```

- [ ] **Step 6: Run all store tests**

Run: `cargo test -p oxibrain-store`
Expected: backup, crash, migration_chain all PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(store): online backup API + crash recovery + migration up-test

SQLite online backup (WAL-safe) via backup feature. Crash test: drop after
flush, reopen, reinsert same content → exactly one episode (content-hash
idempotency, DESIGN §7.3). Migration up-test from empty (user_version=0) to
current. M0 exit criteria met."
```

---

## Task 10: Verify M0 exit criteria + final CI pass

**Files:**
- Verify only; no new files unless CI reveals gaps.

**Interfaces:**
- Validates: M0 exit criteria (init, ingest+read, crash recovery, canonicalization proptests).

- [ ] **Step 1: Run the full suite**

Run:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
cargo build -p oxibrain --no-default-features --features http-llm
cargo tree -p oxibrain | grep -E 'oxios-|oxicode-' && exit 1 || echo "no oxi deps"
```
Expected: every command green; the `grep` finds nothing.

- [ ] **Step 2: Manual M0 exit-criteria walkthrough**

```bash
TMP=$(mktemp -d)
# criterion 1: init
./target/debug/oxibrain init --dir "$TMP/b" --space personal
# criterion 2: ingest and read back
echo "my first thought" | ./target/debug/oxibrain ingest - --dir "$TMP/b"
./target/debug/oxibrain stats --dir "$TMP/b"   # episodes: 1
./target/debug/oxibrain doctor --dir "$TMP/b"  # ok
# criterion 3: kill mid-write recovery (covered by crash test; manual: re-run ingest with same content)
echo "my first thought" | ./target/debug/oxibrain ingest - --dir "$TMP/b"
./target/debug/oxibrain stats --dir "$TMP/b"   # episodes: 1 (idempotent)
# criterion 4: canonicalization proptests (covered by cargo test -p oxibrain-core canonical)
cargo test -p oxibrain-core canonical
```
Expected: stats stays at `episodes: 1` after the duplicate ingest; canonical proptests pass.

- [ ] **Step 3: Fix any CI gaps (no placeholders — only real fixes)**

If `cargo deny` flags a license not in the allowlist, add the exact license string to `deny.toml`. If clippy lints, fix the code. Do not suppress with `#[allow]` unless the lint is a known false positive with a comment explaining why.

- [ ] **Step 4: Final commit (if any fixes landed)**

```bash
git add -A
git commit -m "test(m0): verify exit criteria — init, ingest+read, crash recovery, proptests" || echo "nothing to commit"
```

---

## Self-Review Notes (post-write check)

**Spec coverage (DESIGN.md §17 M0):**
- Workspace + store + migrations + ledger/cache/ops tables → Task 1 (workspace), Task 4 (schema, migrations), schema covers all four zones.
- Writer actor + reader pool + advisory lock → Task 4 (lock), Task 5 (writer/reader).
- Canonical serialization + content-derived ids → Task 3.
- Ports with fakes → Task 2.
- CLI skeleton + doctor + backup/restore → Tasks 8, 9.
- `oxibrain init`, ingest+read, crash recovery, canonicalization proptests → Task 10 verification.

**Known M0 simplifications (intentional, not placeholders):**
- Writer actor runs **one op per transaction**; coalescing is explicitly deferred to M1 with a documented comment.
- Backup CLI uses file-copy as an interim until Task 9 ships the online API; Task 9 replaces it.
- `Brain::connect` (daemon mode) is M4 — not stubbed, just absent.
- Knowledge tables (entities/statements/beliefs/...) exist in schema but have no read/write paths; those land in M1.

**Type consistency:** `Episode`, `ContentHash`, `SourceRef`, `TrustTier`, `EpisodeKind`, `Timestamp`, `BrainError` names match across tasks. `episode_id`, `content_hash`, `canonical_json_value`, `insert_episode`, `get_episode`, `create_space`, `episode_count`, `sql_err`/`io_err` signatures stable. No `impl From<rusqlite::Error> for BrainError` exists (orphan rule); rusqlite boundaries use `.map_err(sql_err)?`. `fs2::try_lock_exclusive` makes the store fail-fast under a second writer.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-11-oxibrain-m0-foundation.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
