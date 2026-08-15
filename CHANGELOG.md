# Changelog

All notable changes to oxibrain are documented here. Conventional commits;
squash-merged.

## [0.2.0] — 2026-08-16

### Features

- **Local GGUF extraction wired into the CLI** — extraction works with no API
  key: `OXIBRAIN_LLM_PROVIDER=local` (the default when no key is set) opens the
  GGUF from the model manifest, grammar-constrained (§7.4).
- **Lazy model pull on first extraction use** (ADR-005) — `oxibrain init` stays
  instant and offline; the extract model downloads automatically on the first
  `extract`/`reextract`, resumable, digest-verified. `OXIBRAIN_MODELS_DIR`
  points at a pre-pulled directory for air-gapped installs. `init` prints a
  one-line hint.
- **`oxibrain sync`** — idempotent vault sync (mtime-anchored) from a directory
  of markdown notes.
- **Registry: multi-type entity objects** — `ObjectKind::Entity` type set;
  relaxed subject types for containment/alias predicates.
- **`ANTHROPIC_BASE_URL` override** for the HTTP provider.

### Fixes

- reextract surfaces and records per-episode failures (invalid LLM output goes
  to `extraction_failures`, never silently dropped); CLI extraction max_tokens
  2048 → 8192.
- `oxibrain-llm-local`: decode-bounds and batch fixes for long prompts.
- `import-oxios` passes the resolved `space_id` to ingest.

### Documentation

- `doc/ARCHITECTURE.md` v2.3: §1.3/§8.4 rewritten around lazy pull.
- ADR-005 accepted and implemented.

## [0.1.0] — 2026-08-15

Initial release: episode ledger + knowledge projection, CLI, MCP server, local
LLM/embedding.
