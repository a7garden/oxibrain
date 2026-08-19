# ADR-005: Lazy model pull on first extraction use

**Date:** 2026-08-15 (decision) · 2026-08-16 (review & completion)
**Status:** Accepted, implemented, verified
**Supersedes:** Part of §8.4 "Model artifacts are Cache-zone" in `doc/ARCHITECTURE.md` — the
original promise that `oxibrain init` fetches weights. The promise moved to first
extraction use. `doc/ARCHITECTURE.md` v2.3 reflects this.
**Related:** omp's `tiny` model subsystem (`packages/coding-agent/src/tiny/models.ts`,
"lazy first load, ~500 MB cached") — the reference pattern for LLM artifact
handling in the oxi ecosystem; ADR-003 (local inference engine).

## Context

§8.4 of `doc/ARCHITECTURE.md` (pre-v2.3) said:

> Weights are fetched by `oxibrain init` (or lazily on first `ingest`) into
> `~/.oxi/models/`, pinned by digest.

The `cmd/init.rs` (8 lines) only provisions the store and a default space —
it does not fetch weights. The CLI's LLM-using path (`extract`, `reextract`) calls
`cmd::llm::from_env()` → `local_from_manifest()`, which errored with *"no extract-role
model in the manifest — run `oxibrain model pull`"* when the manifest was empty.

The C2 promise ("works with no API key, in any language") was therefore broken at
the first new-user touch: install completes without the model, the first extraction
fails with a manual-fetch instruction, and the user runs two commands instead of
one.

The standalone guarantee and the §8.4 promise are both shippable; the question is
where in the user journey the download lives.

## Decision

**Lazy pull on first extraction use.** `oxibrain init` stays fast and dependency-free;
the local extract model is fetched the first time an LLM-using command needs it
(`extract`, `reextract`, or any future path that calls `cmd::llm::from_env`). The
download reuses `models::pull_entry` with its existing progress reporter
`cli_progress`. Once pulled, the artifact is verified by digest on every subsequent
use.

The air-gapped escape hatch is `OXIBRAIN_MODELS_DIR`: offline users point it at a
pre-pulled model directory, and the lazy pull becomes a `verify_entry` no-op.

The decision follows omp's pattern for its `tiny` model: no weights in the install,
lazy first load, cached afterwards.

### Why not auto-pull in `init`?

| Option                                       | First install | First extract | Doc/code match | Offline |
| -------------------------------------------- | ------------- | ------------- | -------------- | ------- |
| `init` auto-pulls (former §8.4 promise)      | Slow (1.5 GB) | Instant       | Was the doc    | Fails   |
| Lazy pull on first use (this decision)       | Instant       | Slow once     | Doc rewritten (v2.3) | OK via `OXIBRAIN_MODELS_DIR` |
| Pre-built `init` flag + lazy fallback        | Configurable  | Slow once     | Best of both   | OK with flag |

Auto-pull front-loads a 1.5 GB download on the empty-install user who may want to
explore the binary first (CLI help, `stats`, read-only `search`, MCP `brief`/
`navigate`). That exploration path already works without the model. Lazy-pull keeps
the empty install instant and defers the cost to the moment extraction is actually
needed — which is when the user has already committed to the workflow.

The pre-built `init` flag (third option) is the best of both — but it is a UX
feature layered on top of lazy-pull. Lazy-pull makes it a one-flag additive change
later instead of a refactor.

## Implementation

Final state after review (2026-08-16). All paths relative to repo root.

- **`crates/oxibrain/src/pull_plan.rs`** — pure decision module.
  `plan_extract_pull(manifest, dir, defaults) -> ExtractPullPlan` with three
  outcomes: `NoOp`, `NeedsPullFromManifest(entry)`, `NeedsBootstrap(entry)`.
  Reads `manifest`/`defaults` as values, inspects `dir.join(&entry.file).exists()`,
  calls `models::verify_entry` — no network, no writes. P9: pure decision, side
  effects live with the caller. Four unit tests in-file (empty manifest →
  bootstrap; missing file → pull; present+verified → noop; corrupt digest → pull).
- **`crates/oxibrain-cli/src/cmd/llm.rs`** — `from_env()` and
  `local_from_manifest()` are `async`; the `Provider::Local` arm awaits
  `local_from_manifest()`, which first calls `ensure_local_model_present()`:
  load manifest → `plan_extract_pull` → on `NeedsBootstrap` persist the default
  entry into the manifest before pulling → `models::pull_entry` with
  `cli_progress`. A **malformed manifest is a loud error**, never a silent reset
  (the bootstrap must not overwrite entries whose parse failure the user never
  saw). Landed in `ccff474`; the extract.rs caller fix landed in the follow-up
  commit (see Review outcome).
- **`crates/oxibrain-cli/src/cmd/extract.rs` / `reextract.rs`** —
  `llm::from_env().await?`.
- **`crates/oxibrain/src/models.rs`** — `ModelEntry` gained `PartialEq, Eq`;
  `model_dir()` honors `OXIBRAIN_MODELS_DIR` (tested via the pure
  `model_dir_with` helper).
- **`crates/oxibrain-cli/src/cmd/init.rs`** — one-line hint so the lazy pull is
  never a surprise: *"model weights pull automatically on first extract —
  pre-fetch with `oxibrain model pull`"*.
- **`doc/ARCHITECTURE.md` v2.3** — §1.3 and §8.4 rewritten to match the decision.

## Review outcome (2026-08-16)

The review found five defects in the pre-review state; all fixed in the follow-up
commit:

1. **Broken HEAD.** `ccff474` made `from_env` async and updated `reextract.rs`
   but omitted the `extract.rs` caller — the committed tree did not compile.
   The `.await` fix was sitting uncommitted in the working tree.
2. **Silent manifest overwrite.** `ensure_local_model_present` used
   `load_manifest().unwrap_or_default()`, swallowing parse errors; the bootstrap
   path would then overwrite a user's hand-edited manifest. Now a parse error
   propagates loudly (`load_manifest_at` still maps *absent* → empty, so the
   fresh-install bootstrap path is unchanged).
3. **Escape hatch was not real.** The decision claimed offline users could
   "point at a pre-pulled model dir", but `model_dir()` was hard-wired to
   `$HOME/.oxi/models`. Implemented `OXIBRAIN_MODELS_DIR`; §8.4 now names it.
4. **Stale ADR snapshot.** The pre-review ADR described "8 files not committed"
   including three "pre-existing WIP" lines and deferred the network smoke to
   "the reviewer with internet". Both resolved: the WIP lines were the earlier
   `f9ab60f`/`ccff474` work (already committed), and the network smoke ran
   (below).
5. **Inaccurate C2 wording.** The draft said "a first-time `oxibrain ask` …
   works (modulo the one-time download)". `ask` never calls the LLM — the
   lazy-pull triggers on `extract`/`reextract`. Corrected throughout.

Also verified: **MCP stdio safety.** The MCP server's extraction uses
`SamplingLlmPort` (the client's model, §8.5 tier 1′), never `cmd::llm::from_env`,
so the lazy-pull progress prints cannot corrupt JSON-RPC framing. If a future MCP
path adopts the local model, its pull messages must move to stderr or logging.

## Verification (2026-08-16, this machine)

| Gate | Result |
| ---- | ------ |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo test --workspace` | pass (0 failures) |
| Standalone guarantee (`-p oxibrain --no-default-features --features http-llm` + tree scan for `oxios-`/`oxicode-`) | pass, no match |
| Smoke: NoOp path — fresh store, cached model in `~/.oxi/models`; `reextract` verifies the digest, opens the model, runs extraction without downloading | pass |
| Smoke: bootstrap path — fresh store, empty `OXIBRAIN_MODELS_DIR`; `reextract` pulls 1.1 GB, persists the manifest, verifies the digest, opens and extracts | pass |

Both smokes ran the extraction end-to-end; the validator quarantined the output
(recorded in `extraction_failures`, per the no-silent-drop rule). That is the
known extraction-quality workstream, not a lazy-pull defect — the artifact path
(plan → pull → persist → verify → open) is what this ADR owns, and it held.

## Consequences

### Positive

- **Empty install is instant.** `cargo install oxibrain-cli && oxibrain init` completes
  in well under a second. Exploration commands (`help`, `stats`, `page --kind space`,
  MCP read tools) work immediately without a download.
- **Lazy fetch is the only fetch.** The user sees a download progress line exactly
  once per machine, on the first command that needs the model.
- **C2 is real.** A first-time `oxibrain extract`/`reextract` after `init` works
  with no API key (modulo the one-time download).
- **omp-validated UX pattern.** No install bundle, lazy load, cached. We follow
  rather than reinvent.
- **The bootstrap path persists the manifest.** Subsequent `from_env` calls
  short-circuit on the persisted manifest+file — no re-planning, no re-download.

### Negative / trade-offs

- **`init && ingest && extract` is no longer one-shot.** The first LLM-using
  command blocks for the model download. The §8.4 rewrite (v2.3) and the `init`
  hint line set that expectation honestly.
- **Daemon mode (MCP serve) uses client sampling, not the local model**, so its
  cold start is unaffected. A future daemon-side local tier will lazy-load on
  first use — same behavior as the CLI, acceptable.
- **`--no-pull` flag not implemented.** `OXIBRAIN_MODELS_DIR` covers the
  air-gapped case; a per-run `--no-pull` remains a small additive change if ever
  needed.

## Decisions on the pre-review open questions

1. **`init` output wording** — yes: one hint line (implemented).
2. **MCP serve startup messaging** — no. Serve never triggers the pull (sampling
   path); noise without value.
3. **Already-pulled installs** — `plan_extract_pull` returns `NoOp`; the
   "pulling…" line fires only on the truly-first path. Confirmed by the NoOp smoke.
4. **`oxibrain model pull` deprecation** — no. It remains the pre-fetch utility
   for metered/offline setups, and shares `pull_entry` with the lazy path.
5. **Doc rewrite scope** — done: §1.3, §8.4 (v2.3). §16.4 needs no change (it
   does not discuss model fetch). The `company/` README reference from the draft
   review no longer applies (no such line in that project).
6. **Integration test with a stubbed pull** — not added. The decision logic is
   covered by the four `pull_plan` unit tests; the glue is 30 lines exercised by
   two end-to-end smokes (NoOp and bootstrap). A stub-seam would exist only for
   CI hermeticity; revisit if CI ever needs to cover the pull path.

## What this ADR does *not* touch

- The `memory_edit`-style supersession work (`update | forget | invalidate`).
  Still pending; separate ADR.
- Local embedder wiring into CLI read paths. Not done.
- Extraction quality on Korean prose (tier-0 risk). Not done.
