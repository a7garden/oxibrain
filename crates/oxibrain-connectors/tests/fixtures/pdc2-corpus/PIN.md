# Vendored PDC2 conformance corpus — DO NOT EDIT IN PLACE

This directory is a **vendored copy** of the Portable Document Contract 2
conformance corpus, read by `tests/pdc_corpus.rs` (v2 runner section). It is
deliberately copied into the tree so the corpus runner never depends on a
sibling checkout being present at test time.

## Pin

| Field | Value |
|---|---|
| Contract format | `pdc-document-conformance/2` |
| Corpus revision | 2 |
| Upstream commit | `0ee51ea` (`main`) |
| Upstream tag | `v2.0.0-draft.2` |
| Normative texts | `references/PDC-2.0.md`, `references/PDC-QUERY-1.0.md` |
| Related pin | `../pdc-corpus/` (frozen `pdc-document-conformance/1` revision 3) |

The corpus includes the v1 readability fixtures (`fixtures/valid/*.djot`,
`fixtures/valid/minimal.html`, `fixtures/valid/semantics.html`) required by
PDC 2 §6.3; those files are byte-identical to the frozen v1 corpus vendored
one level up.

## Update procedure

Copy `corpus.json` and `fixtures/` from the new pinned commit, update this
table, and update `PDC2_CORPUS_REVISION` (and the format pin) in
`crates/oxibrain-connectors/src/pdc/mod.rs` together. Never hand-edit the
vendored fixtures or `corpus.json` to make a test pass — a behavior-changing
fixture update belongs upstream and must accompany the normative change that
justifies it.
