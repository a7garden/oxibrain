# Vendored PDC conformance corpus — DO NOT EDIT IN PLACE

This directory is a **vendored copy** of the Portable Document Contract
conformance corpus, read by `tests/pdc_corpus.rs` via
`env!("CARGO_MANIFEST_DIR")`. It is deliberately copied into the tree so the
corpus runner never depends on a sibling checkout being present at test time
and never races against that checkout moving.

| Pin | Value |
|---|---|
| Corpus revision | 3 |
| Contract draft | 4 (`v1.0.0-draft.4`) |
| Pinned contract commit | `6481ef0` |
| Vendored | 2026-09-13 |
| Upstream path | `<contract repo>/conformance/` (`corpus.json` + `fixtures/`) |

The copy must be refreshed **deliberately** when the pin moves: re-copy
`corpus.json` and `fixtures/` from the new pinned commit, update this table,
and update `PDC_CORPUS_REVISION` in
`crates/oxibrain-connectors/src/pdc/mod.rs` together. Never hand-edit the
vendored fixtures or `corpus.json` to make a test pass — a behavior-changing
fixture update belongs upstream (contract repo) and must accompany the
normative change that justifies it.
