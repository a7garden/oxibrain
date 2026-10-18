# Experience Memory v1 — spec

> **Version:** 1.0 · **Date:** 2026-10-16 · **Status:** Approved design (owner decision,
> 2026-10-16). Implementation is **M11** (`doc/ROADMAP.md` §7), gated on M10 items
> 10.1 (Uncertainty), 10.2 (summaries-with-sources), 10.9 (negative predicate family).
> **Coordinates:** ADR-018 (structural decision). `doc/ARCHITECTURE.md`
> §5.5/§9/§13/§16.2 are amended at landing, not by this file.
> **Reference:** LongMemEval-V2 (arXiv:2605.12493) — five experience abilities and the
> AgentRunbook knowledge pools; Mem0's procedural-memory write-ups. Cited for the problem
> shape, not copied: oxibrain answers it with the ledger, not a new store.

## 0. Premise

Agents that work — coding agents, web agents, the oxi apps — produce trajectories: long,
noisy sequences of actions and observations. Fed to oxibrain today, those trajectories
yield person-and-project facts, because that is what the registry and the extraction
prompt ask for. The brain cannot answer the questions that make an agent an *experienced
colleague*: how is this task done here, what breaks here, which assumption that holds
elsewhere is wrong here.

LongMemEval-V2 names exactly these abilities (workflow knowledge, environment gotchas,
premise awareness) and shows that conversational-memory methods miss them on noisy
trajectories, while a three-pool structure — raw evidence slices, state-transition
records, strategy notes — recovers them. oxibrain already owns all three granularities:
episodes/chunks (raw slices), assertions (state transitions), derived summaries (strategy
notes). What is missing is (a) trajectory-shaped ingestion with stable identity, (b) an
experience predicate family, (c) task-clustered consolidation into runbooks. This spec
adds exactly those three and nothing else.

## 1. Scope: abilities mapped onto what already exists

| LME-V2 ability | oxibrain mechanism | Change |
|---|---|---|
| Static state recall | entity extraction over trajectory episodes | none |
| Dynamic state tracking | bi-temporal fold, `as_of` / `known_at` | none |
| Workflow knowledge | Derived runbook episodes (§4) + `Task` entities | new consolidation mode |
| Environment gotchas | `fails_when` + M10 `failed_because` assertions | new predicate |
| Premise awareness | M10 `assumed` + `premise_differs` | new predicate |

**Non-goals.** No new memory plane, store, or memory type; no new MCP tool; no authoring
surface (a runbook is derived, never user- or agent-edited — §13.2); no ordered-step
assertions (ADR-018); no public-benchmark harness — the in-house corpus of §5 is the gate,
and the 2026-08-13 research note deferring LongMemEval-V2-the-benchmark stands. The owner
decision of 2026-10-16 pulls the *capability* forward, not the public benchmark.

## 2. Trajectory ingestion

### 2.1 Source identity

- `SourceRef::AgentTrace` is promoted to `AgentTrace { agent: String }` — the v2.20
  `Session { session_id }` precedent. Serialization is `("agent_trace", Some(agent))`;
  the ref column already exists, so there is **no schema migration**, only a data
  migration backfilling legacy unit-variant rows. `import-oxios` is the only known legacy
  writer (verified against the tree at implementation time); its rows backfill with
  `agent = "import-oxios"`.
- Event identity (§5.6): source `("agent_trace", agent)`, occurrence the run id.
  Replaying the same run with identical bytes is an idempotent retry; the same occurrence
  with different bytes is a conflict (exit 6). Callers wanting finer replay granularity
  mint run ids with suffixes (`…#3`) — their choice, no schema impact.

### 2.2 Rendering contract (caller-side, deterministic)

```
[step 001 · 2026-10-16T02:11:04Z · tool: bash]
action: <verbatim bytes>
observation: <verbatim bytes, deterministically truncated>
```

- **The caller renders; oxibrain never parses tool-call schemas.** Rendering is documented
  in `oxibrain schema ingest` so any agent framework can comply, and none is privileged
  (the P11 discipline applied to frameworks, not scripts).
- Steps are separated by one blank line, so the recursive chunk ladder's early separators
  cut at step boundaries: chunks approximate LME-V2's raw-slice pool with **no new index**.
- Truncation (per-step observation; default 4,096 bytes, policy-configurable): first
  2,048 bytes + `…[truncated N bytes]…` + last 2,048 bytes. Deterministic, byte-counted.
- Trajectory observations frequently carry secrets (tokens, env vars, key material).
  Scrubbing before rendering is the **caller's duty**; the in-brain removal path is
  `redact()` — audited, never silent. oxibrain adds no scrubbing of its own (it cannot
  know what is secret in the caller's environment).
- Step timestamps are RFC-3339 UTC; a missing timestamp falls back to the run's
  `occurred_at`.
- The §9.3 chunk context prefix picks up the agent for free — `SourceRef` is already a
  prefix field.

### 2.3 `ingest` payload (additive)

```json
{ "content": "<rendered trajectory>", "space": "dev",
  "trace": { "agent": "claude-code", "run_id": "2026-10-16-001",
             "occurred_at": "2026-10-16T02:30:00Z" } }
```

- The presence of `trace` selects the trajectory path — one optional object with a fixed
  shape, **not** a discriminator union (§16.2 ban). `source_path` is unused on this path.
- **Trust:** trace episodes are recorded `Untrusted` — the v2.21 rule that agent-generated
  claims are not owner declarations, and a deliberate divergence from the note path's
  `trusted` default. The `trust` request field keeps its existing capability-gated rules;
  only the default differs. Elevation is a source-policy Declaration, which keys on a
  **registered** source (`source_policy_id = blake3(source_id, effective_from)`): the
  first trace ingest for an `agent` value ensures the source row `agent:<agent>` through
  the same idempotent `INSERT OR IGNORE` path sessions use (`ensure_source`; the
  `sources` table is not a reprojected projection), after which
  `oxibrain admin source policy agent:<agent> --trust <tier>` applies.
  `TrustedIngest` remains the sole capability bypass, unchanged.
- The MCP tool count stays **sixteen**; the schema change is additive, so v1.0 clients are
  unaffected (the §19.4 discipline).

## 3. Experience predicate family (core/v1, minor 4 → 5)

| field | `requires` | `fails_when` | `premise_differs` |
|---|---|---|---|
| subject types | `Task` | `Task, Code, Artifact, Project` | `Task, Concept, Code` |
| object kind | Entity `Concept, Artifact, Code, Document` | Literal `Text` | Literal `Text` |
| cardinality | MultiValued | MultiValued | MultiValued |
| temporality | Interval | Interval | Interval |
| invalidation | Coexist | Coexist | Coexist |
| confidence prior | 0.7 | 0.6 | 0.6 |
| `profile_relevant` | false | false | false |

Descriptions (registry data, fed verbatim into the extraction prompt):

- `requires` — "A prerequisite for performing this task in its environment: a tool,
  document, artifact, or concept it depends on."
- `fails_when` — "A recurring condition under which this task, code, artifact, or project
  fails in this environment; a gotcha. Assert only when the source shows repetition or
  states it explicitly."
- `premise_differs` — "An assumption that is valid in other environments but is wrong for
  this subject here."

Rules:

- **Consumes M10 10.9** (`failed_because` Point, `assumed`, `unknown_whether`); does not
  redefine it. Boundary: `failed_because` is one observed failure (Point, episode-bound);
  `fails_when` is the generalized recurring condition.
- **Generalization happens at extraction time, from Primary traces only.** Consolidation
  never writes assertions — `EpisodeKind::Derived` is terminal (§5.3) — so a runbook may
  *surface* a recurring failure in prose, but the queryable `fails_when` belief can only
  originate in an extractor pass over primary episodes.
- Minor registry bump ⇒ **no cache invalidation** (§5.5); the JSON Schema and the GBNF
  grammar regenerate from the registry (P4), so the new predicates are extraction-ready
  with zero pipeline code.
- Few-shot: trajectory examples in at least English and Korean join the golden corpus;
  selection stays trigram-Jaccard (language-independent, §9.6).
- **Environment scoping is by subject, not by predicate.** A gotcha or a broken premise
  is environment-relative, and the registry deliberately has no Environment dimension.
  The convention: experience subjects are environment-qualified surfaces
  ("deploy (oxibrain-prod)"), so resolution keeps per-environment entities separate and
  `fails_when` / `premise_differs` assertions from different environments never co-fold
  (all three are Coexist). When qualification is missed, `why` remains the discriminator
  — per-episode provenance shows which environment's trace made the claim.

## 4. Runbooks — the derived tier

- `Brain::consolidate(space, mode)` gains `mode = Task`: cluster Primary episodes (any
  source; predominantly `AgentTrace`) sharing a `Task` / `Code` / `Artifact` entity with
  ≥ N members (policy, default 3). The LLM summary is a **runbook**: ordered steps in
  prose, gotchas encountered, premises that broke — each traceable to member episodes
  through the P10 sources pairing.
- Cached under `summaries` keyed `(kind = "runbook", member_set_hash, extractor_id)`.
  New members ⇒ a **new** derived episode; the previous one stays (§13.2 unchanged). The
  `Uncertainty` block is mandatory (M10 10.1 dependency). Terminal: no assertion is ever
  extracted from one.
- **Retrieval adds no layer.** Runbooks ride the existing `Summaries` layer
  (summary-with-sources); member excerpts expand through `expand_top_k`. The
  `PackPolicy::experience()` constructor this spec originally promised is
  **deferred with evidence**: the measured gate (§5) shows the default policy
  plus the FTS-matched episode harvest meets every exit criterion (workflow
  r−b = +3 within the 2,000-token budget), and an unused public constructor
  with no consumer is API dead weight. It returns when a consumer needs a
  different expansion shape — the pure-decision seam (P9) it slots into
  already exists.
- CLI: `oxibrain admin consolidate [--mode topic|task]` — an admin verb, not an MCP tool;
  the cap is untouched.

## 5. Evaluation — as implemented

- Corpus `eval/experience/` (manifest + 22 trajectory episodes + 14 questions): 2
  environments sharing the task base name "deploy" (environment-qualified surfaces),
  English + Korean content (≥ 2 writing systems, §7.8 parity rule), questions per
  category (workflow, gotcha, premise — plus static/dynamic controls) with
  answer-bearing episode labels. The environment-scoping rule of §3 is exercised by
  construction: the two envs' identically-named tasks are distinct surfaces and so
  distinct entities.
- Suite: `oxibrain admin eval --suite experience` — the gate runner, deterministic and
  self-contained (no network, no LLM; the house discipline, so the fixed reader and
  LLM judge of the draft were replaced by word-substring scoring over rendered
  surfaces). Trajectories are ingested through the real M11 path
  (`record_trace`, Untrusted, event identity); gold statements are declared.
- Arms: **(a)** full-context ceiling (answer in the raw corpus), **(b)** lexical
  statements (the control), **(c)** hybrid search + entity expansion, **(r)** the
  full-stack context-gathering arm — `recall` within a 2,000-token budget over the
  assembled layers. Reported per category: a/b/c/r accuracy, (c)−(b), (r)−(b), and
  tokens/query. The exit criterion's (c) is arm r.
- **Measured 2026-10-16** (14 questions): ceiling a = 14/14; gotcha r−b = +2,
  workflow r−b = +3, premise r−b = +1; static/dynamic r−b = 0 (lexical-sufficient
  controls — honest zeros, not failures); r spends 1,139–1,290 tokens of the 2,000
  budget. Multi-hop questions (gotcha-2/4, premise-4) are the ones lexical cannot
  answer — that is the graph + raw-slice dividend, priced.
- What made workflow reachable: `build_context_input` now harvests **FTS-matched
  episodes first** (salience 0.9) and fills by recency — the raw-slice pool of §2.2
  was unreachable from `recall` before; statement-only arms cannot see workflow prose.
- M11's corpus **is** the gate for this capability — per the sequencing rule
  ("features added before the gate are unpriced"). It is independent of the
  conversational-corpus gate decision (ROADMAP §4 / D19): predicates and runbooks
  ride the truth half and the derived half, which are exactly what D19 keeps under
  either outcome.
- The public LongMemEval-V2 benchmark remains out of scope.

## 6. Landing sequence

| P | Delivers | Verified by |
|---|---|---|
| P1 | registry family (minor 5) + prompt/grammar/validator | `schema_contains_all_predicates` extended; corpus examples validate against the quote contract |
| P2 | `AgentTrace { agent }` + backfill migration | migration chain up-test from the prior version fixture |
| P3 | `trace` payload, event identity, conflict path | idempotent replay test; byte-mismatch → exit 6 |
| P4 | rendering-contract chunk behaviour | step-boundary chunk test; truncation determinism property test |
| P5 | few-shot corpus + trace extraction quality | `eval --suite fast` green; `fabricated_entity_rate` still 0.00 by measurement |
| P6 | `consolidate_runbooks` + runbook cache + admin verb | terminal-episode test covers runbook-derived episodes; P10 post-condition holds |
| P7 | experience corpus + three-arm reporting | M11 exit criteria (ROADMAP §7.2) |

## 7. Rejected alternatives

| Rejected | Why |
|---|---|
| Ordered-step assertions (`has_step` with ordinals) | the truth half stays atemporal; an ordinal is not foldable or invalidatable without positional semantics the registry does not have (ADR-018) |
| A separate procedural store (Mem0-style third memory type) | breaks P1 — a projection row no episode explains; runbooks are already Derived episodes with cached text |
| oxibrain-side trace parsing (tool-call schemas) | privileges one agent framework; the caller renders (P11 discipline applied to frameworks) |
| New MCP tools (`runbook`, `gotchas`) | the cap is sixteen; `recall` / `search` / `brief` already reach everything a runbook contains |
