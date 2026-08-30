# Oxi home fixtures — cross-app migration matrix

Canonical fixture matrix for the unified Oxi home work
(`docs/superpowers/plans/2026-08-30-oxi-home-layout-remaining-work.md`, P1-5).
Each repository owns the fixtures it can exercise; the matrix below is the
shared vocabulary so the four apps test the same states the same way.
Fixtures are built **in code** with hermetic temp homes (`OXI_HOME` / `HOME`
pointed at a tempdir) — never committed binary trees, never the real
`~/.oxi`.

| # | Fixture | State | Covered by |
|---|---------|-------|------------|
| 1 | Clean install, one space | `<home>/.oxi` with only `brain/` scaffold (or nothing) and `spaces/personal/vault/` | oxibrain: `migrate::tests::fresh_install_has_nothing_to_do`; oximemo: `migrate_spaces` fresh tests; oxios/oxicode resolver tests |
| 2 | Legacy install per app | populated `~/.oxi/models` (oxibrain), `~/.oxios` (oxios), `~/.oxicode` (oxicode), app-support vault (oximemo), flat `~/.oxi/vault` (oximemo) | oxibrain: `ready_counts_pending_work`, `migrate_copies_tree_and_marks_journal_complete`; per-repo migrate tests |
| 3 | Partial migration at every journal boundary | journal `in_progress`; destination holds a subset of source files (crash simulation: pre-copy some files, run migrate) | oxibrain: `resume_after_partial_copy_completes_without_recopy`; oximemo: resume-after-partial tests |
| 4 | Old + new identical content | destination mirrors source file-for-file (already migrated) | oxibrain: `rerun_after_complete_is_already_migrated`, `source_missing_destination_populated_is_already_migrated` |
| 5 | Old + new conflicting content | one relative path with different bytes, or a destination-only file | oxibrain: `conflicting_content_refuses_and_touches_nothing`, `extra_destination_file_is_a_conflict`; oximemo: `MergeRequired` tests |
| 6 | Multiple spaces, one shared vault | `spaces/<a>/vault`, `spaces/<b>/vault` visible to oxios + oximemo; each an expanded root in `documents.toml` | oximemo spaces tests + oxios knowledge-root tests (`spaces/personal/vault` fallback) |
| 7 | No brain process available | registration deferred: pending record written, no brain files touched, replay succeeds when the binary appears | oxibrain: `client_round_trip::client_registers_document_root_over_stdio_boundary` (server side); oximemo: offline pending/flush tests + `register_live` cross-process test |
| 8 | `OXI_HOME` at a temp dir | every resolver honors the override; explicit app overrides (`OXIOS_HOME`, `OXICODE_HOME`, `OXIMEMO_VAULT`) still win | oxibrain: `paths::tests`, models `model_dir_*`; per-repo resolver tests |

## Assertions every migration fixture must make

- files preserved (content digest-equal after copy);
- permissions preserved where applicable (executable bits on copied trees);
- `.git` history preserved for vault migrations (oximemo owns these cases);
- derived indexes rebuildable / not migrated;
- the **source is never deleted** and never modified;
- a dry-run/preflight performs **zero** filesystem mutations.

## Engine contract (one shape, four repos)

`preflight → journal → idempotent copy (skip identical by size+digest) →
verify → journal complete`. Conflicts abort with both paths and touch
nothing. Journals live beside their destination (`brain/.models.migration-
journal.json` in oxibrain; `oximemo/migration-journal.json` in oximemo;
`<oxi_home>/<app>.migration-journal.json` in oxios/oxicode).

## Rollback (compatibility window)

Because migrations never delete or modify the source, rollback is always
possible and boring:

1. Stop the app that owns the subtree (no writer on the destination).
2. **Preserve both sides**: keep the destination subtree *and* its journal
   (`*.migration-journal.json`) for diagnosis, and keep the legacy source.
3. Remove only the destination subtree when reverting (the canonical
   location reverts to "absent"; resolvers fall back to the read-only
   legacy location during the compatibility window).
4. Restore legacy-first operation only after verification: re-run the
   app against the legacy path, confirm behavior, and only then re-attempt
   migration (the journal re-runs idempotently; a `Conflict` state must be
   resolved by hand before any retry).

The cutover release removes the read-only fallbacks — it never deletes
user data.
