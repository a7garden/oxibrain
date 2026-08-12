# Handoff — M5 Oxios Migration (oxibrain-side)

> **Status:** All oxibrain-side M5 deliverables shipped. The remaining M5 work
> is in the oxios repo: wiring `oxios-kernel` to depend on `oxibrain::*` and
> deleting `oxios-memory`.
> **Branch:** `main`
> **Predecessor:** `2026-08-12-m4-spec-gaps-closed.md`
> **Tests:** 226 pass, 0 fail. Clippy clean. Fmt clean. Standalone verified.

---

## 1. What shipped this session

### 1.1 Consumption Contract 1.0 (§16.4)

- **`doc/CONSUMPTION_CONTRACT.md`** — pins the stable public surface, stability
  tiers (`stable` / `unstable` / `internal`), versioning policy, and the full
  method list for every `Brain` method.
- **`crates/oxibrain/src/compat.rs`** — compile-time API surface test. Every
  stable `Brain` method and re-exported type is referenced by name; removing
  or changing a signature breaks compilation.

### 1.2 oxios-memory Importer (§16.3)

- **`crates/oxibrain-connectors/src/oxios.rs`** — reads an oxios-memory SQLite
  database directly (`read_oxios_memory`), no dependency on the `oxios-memory`
  crate. Returns `OxiosMemoryEntry` structs in chronological order.
- **CLI `import-oxios` subcommand** — `oxibrain import-oxios <db> --space <name>`
  ingests each entry as `SourceRef::AgentTrace`, `TrustTier::SemiTrusted`.
  Original creation date is prepended to the content for temporal extraction.

### 1.3 C1 Fallback Decision (§16.1, §20 item 6)

- **`doc/adr/ADR-002-c1-fallback-decision.md`** — decided: no local recall
  cache for v1. Agents degrade gracefully without memory during a brain
  outage. Rationale: embedded mode (the default) has no C1 risk; daemon mode
  is opt-in; the letter of the contract holds; reversible if data warrants.

### 1.4 oxibrain-connectors crate

Added to the workspace as a member. Previously existed on disk but was not in
`Cargo.toml` members. Now compiles and tests as part of the workspace.

---

## 2. What remains for M5 (oxios repo, not oxibrain)

These tasks are in `/Volumes/MERCURY/PROJECTS/oxios`, not here:

1. **Add `oxibrain` dependency to `oxios-kernel`.** Route all memory through
   `Brain::*`. Use `Brain::open(BrainConfig::at(dir))` for embedded mode.
2. **Replace `oxios-memory::MemoryStore` calls** with `Brain::ingest`,
   `Brain::query`, `Brain::assemble_context`, `Brain::beliefs`.
3. **Run the importer:** `oxibrain import-oxios ~/.oxios/workspace/memory.db
   --space personal`, then `oxibrain reextract --space personal`.
4. **Delete `oxios-memory`** from the workspace in the same PR that removes
   its last caller.
5. **Deprecate `oxios-memory` on crates.io** (not yank).

**Retirement trigger (§16.3):** the last `oxios_memory::` import removed from
the oxios workspace.

---

## 3. M5 exit criteria

| Criterion | Status |
|---|---|
| Consumption contract published | ✅ `doc/CONSUMPTION_CONTRACT.md` |
| Importer for existing stores | ✅ `oxibrain import-oxios` |
| C1 fallback decision made | ✅ ADR-002 |
| `oxios-kernel` on `Brain` | ❌ oxios repo |
| `oxios-memory` deleted | ❌ oxios repo |

**oxibrain-side M5 is complete.** The remaining two criteria are oxios-repo
work that doesn't require oxibrain changes.

---

## 4. Path to M6

M6 (Product / Desktop brain UI) is the final milestone. Per DESIGN §17:

> Desktop brain UI: graph explorer, timeline, ask-with-provenance, merge
> review, contradiction inbox, quick capture. Packaging, onboarding, docs site.

The frontend connects to the daemon's HTTP transport (`oxibrain serve --http`)
via JSON-RPC. Vite + React 19 + Tailwind v4 per the project stack.

The deferred M4 features (long-running tasks, subscriptions per ADR-001) ship
alongside M6 — the UI that consumes them is the natural forcing function.

---

End of handoff. oxibrain-side M5 is done. Start M6 (desktop UI) next.
