# Recall release readiness

Date: 2026-10-24. Status: implementation and validation in progress.

The owner requested research-backed improvement of the remaining recall and
reader-quality problems, the included agent skill, and a deployed release, using
subagents. This authorizes the release workflow after its gates pass. It does not
change the immutable ledger, Full Reader PDC boundary, or sixteen-tool cap.

## Work streams

1. Diagnose candidate loss using current captures and implementation. Fix general
   query/candidate behavior, with script-independent algorithms and regression
   tests beyond the known corpus.
2. Preserve source bundles and make consumer citation identities easy to use.
   Any compact representation is versioned, retains the complete out-of-band
   coordinate/support map, and counts actual canonical context tokens.
3. Improve the shipped agent skill and the reference reader workflow together.
   Evaluate actual conditional/clarification content, supported quotations and
   provenance rather than crediting action labels alone.
4. Freeze a deterministic subset of a licensed public memory dataset before
   assessing the new implementation. Known follow-up cases are diagnostics;
   report them separately from the external subset. No case-specific keys or
   answer labels enter retrieval, prompts, registry semantics, or skills.
5. Validate and release a coherent version including all reviewed working-tree
   implementation. Preserve unrelated temporary/archival workspaces. Verify the
   published packages, release assets, and installed executable separately.

## Acceptance

Correctness requirements are unchanged: source/scope isolation, exact budget
accounting, complete truth groups, no historical leakage, byte-identical truth
reprojection, PDC conformance, crash/migration coverage and writing-system parity.
The full test/lint/advisory/standalone/fresh-consumer gates must pass.

For known retrieval regressions, all required source ranges must remain reachable
within the declared budget, including conditions and competing evidence. The
external subset target is at least 90% answer-turn retrieval at the pinned budget;
report full-turn and annotated-answer-turn reachability separately where exact
span annotations are unavailable. A dataset mismatch is reported, not relabelled.

The pinned local reference reader must return a parseable, source-bound answer
or an explicit limitation. Its checks must reject invalid citation IDs/ranges and
invented quotations. No-evidence controls must abstain. Independently inspect
conflict and applicability cases, keeping action labels and semantic judgments
separate. Do not manufacture a semantic-correctness score from containment.
If the model cannot satisfy a stronger generation contract, limit that contract
and fail safely rather than claiming an unsupported quality tier.

Release gates include the embedded skill's independent forward test, public
package dependency resolution, asset checksums/content, and a post-install smoke
check against an isolated store. Back up any real store before a required local
schema upgrade; never convert or rewrite user documents as part of deployment.

## Evidence

The previous baseline and measured failures are preserved in
[the follow-up report](../research/2026-10-24-post-m12-followup.md). New experimental
artifacts and final release identities will be recorded in a new dated report.
