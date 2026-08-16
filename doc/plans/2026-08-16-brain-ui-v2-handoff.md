# brain-ui v2 Handoff — Session Break at Task 9

> **Status:** Handoff for the brain-ui v2 redesign (spec `doc/spec/brain-ui-v2.md`,
> plan `doc/plans/2026-08-16-brain-ui-v2.md`). Execution method: subagent-driven
> development (fresh implementer + reviewer per task, fix loops, ledger).
> **Branch:** `feat/brain-ui-v2` (base `main` @ `e6c9d04`)
> **Last verified commit:** `7749d81` — tree clean
> **Fine-grained record:** `.superpowers/sdd/brain-ui-v2/progress.md` (ledger; git-ignored)
> **Remaining work:** Tasks 9–13 of the plan (T9 was IN FLIGHT at break time — see §1)

## 1. Resume Procedure (read this first)

1. `cd /Volumes/MERCURY/PROJECTS/oxibrain && git status --porcelain && git log --oneline -8`
   — working tree must be clean; you are on `feat/brain-ui-v2`.
2. **Determine whether Task 9 landed after the break:** check for a commit
   `feat(brain-ui): ask view with per-belief provenance` and the file
   `.superpowers/sdd/brain-ui-v2/task-9-report.md`.
   - If present: treat T9 as implemented but UNREVIEWED — generate the review
     package (§4 commands) over `7749d81..HEAD` and dispatch the task reviewer
     before continuing.
   - If absent: re-dispatch the T9 implementer per its brief
     (`.superpowers/sdd/brain-ui-v2/task-9-brief.md`) with the dispatch context
     from §5.
3. Continue the task loop T9 → T10 → T11 → T12 → T13, then the final
   whole-branch review, per `.superpowers/sdd/brain-ui-v2/progress.md` (ledger)
   and the plan. Never re-dispatch a task the ledger marks complete.

## 2. Shipped This Session (14 commits, all review-gated)

| Commit | Task | What |
|--------|------|------|
| `f678107` + `8871081` | T1 | Store fetch `contradiction_details` (surfaces + per-value episodes) + rustfmt |
| `9086070` | T2 | `contradictions` tool serves retract-ready DTO + contract test |
| `f1377d5` | T3 | `space://` overview contract fix (surfaces, exact keys) — fixed 2 live UI bugs |
| `8cd929e` + `0b36a0f` | T4 | `timeline://` resource + predicate assertion |
| `e4ebcfb` + `faa9a8b` | T5 | oxi token layer (OKLCH 3-tier), SUIT/SUITE fonts, light/dark + font-var emission fix |
| `5bdc8c0` + `5381b4a` | T6 | TanStack Router (hash) + Query shell, contract-accurate api.ts, Overview; fix: scannable hue classes, callTool text fallback, text-2xs |
| `cbb3543` | T7 | EntityPage: Brief \| Timeline tabs, deep-linkable `?tab=` |
| `6a6232b` + `4f907f4` + `7749d81` | T8 | sigma.js graph explorer; fix rounds: oklch→rgba canvas boundary, `multi:true`, StrictMode/theme lifecycle, raw font read |

**Backend contracts now pinned by tests in `crates/oxibrain-mcp/src/server.rs`:**
`space://` → `{space, space_id, entity_count, episode_count, contradiction_count, recent_entities:[{id,surface,type}]}`;
`contradictions` → 9-key ContradictionDetail array; `timeline://{id}` → 8-key TimelineEntry array (epoch-ms).
TS types in `apps/brain-ui/src/api.ts` mirror these — the tests are the source of truth.

## 3. Cross-Task Decisions Later Tasks Must Honor

1. **Text-returning tools**: `remember`, `merge_entities`, `retract`, `brief`,
   `navigate` return raw server text (callTool falls back from JSON.parse).
   Views render the string + rely on query invalidation for state changes.
   T10/T11 briefs that mention object results are superseded by this.
2. **Hue classes**: never construct `bg-hue-*`/`text-hue-*` dynamically —
   Tailwind's scanner must see literals. Use `HUE_DOT` / `HUE_CHIP` maps from
   `src/lib/hue.ts`.
3. **Canvas boundary**: sigma cannot parse `oklch()` — all colors fed to sigma
   go through the 1×1-canvas rgba readback (`toRgba` in `hue.ts` and
   `useSigmaGraph.ts`); font stacks are read RAW (no color conversion).
4. **`entity://` beliefs** carry the `statement` id needed for `why` (T9 uses this).
5. **Vite dev CORS**: browser fetch from :5173 → daemon :18080 is blocked;
   production `--ui-dir` serving is same-origin (fine). T13 must add a vite
   dev proxy (or serve_http CORS headers) for the live smoke.

## 4. SDD Loop Mechanics (this harness has no skill scripts — manual equivalents)

```bash
WS=.superpowers/sdd/brain-ui-v2
BASE=<commit before dispatch>   # record before every implementer dispatch
# review package:
{ echo "== commits =="; git log --oneline $BASE..HEAD; echo; echo "== stat =="; git diff --stat $BASE..HEAD; echo; echo "== diff =="; git diff -U10 $BASE..HEAD; } > $WS/task-N-review-pkg.txt
```

- Dispatch: `task` agent, prompt = brief path (`$WS/task-N-brief.md`) +
  global constraints (`$WS/global-constraints.md`) + interfaces from earlier
  tasks + ambiguity resolutions + report path (`$WS/task-N-report.md`).
  One implementer at a time (never parallel).
- Review: `reviewer` agent with brief + report + review package + verbatim
  task-specific constraints. Fix loop ≤5 rounds (resume implementer via
  `hub send` while idle; if unreachable, fresh implementer with findings).
  Minors → ledger deferred lines. Every completion → ledger line.
- Gates: backend `cargo test -p oxibrain-mcp -p oxibrain-store -p oxibrain-client`,
  `cargo clippy --all-targets -p <crates> -- -D warnings`, `cargo fmt --all -- --check`;
  frontend `cd apps/brain-ui && bun run build:ts && bun run build`.
  fmt caught a miss in T1 — keep it in every backend gate.

## 5. Remaining Tasks (from the plan; briefs already extracted to `$WS/task-N-brief.md`)

| Task | Scope | Dispatch notes beyond the brief |
|------|-------|-------------------------------|
| **T9 AskView** | `/ask?q=` search + per-belief why provenance; DELETE `AskProvenance.tsx` | Interfaces: api.search/beliefs/why; beliefs carry `statement` id; house input pattern; one-expanded-at-a-time; debounce 300ms URL update (replace); input autofocus for T12's `/` hotkey. Smoke recipe in `task-7-report.md` |
| **T10 ConflictsView** | Inbox grouped by (subject, predicate); per-value episodes; one-click Retract behind confirm dialog; DELETE `ContradictionInbox.tsx` | `api.contradictionDetails()` (9-key DTO) + `api.retract(...)` returns TEXT string (see §3.1) — toast + invalidate `qk.contradictions/qk.space` + entity keys; 15s refetchInterval; dialog per §6.7 |
| **T11 MergesView + CaptureView** | Merge table + new-merge form with search pickers; capture textarea; DELETE `QuickCapture.tsx` | mergeEntities/remember return text (§3.1); merge pickers via debounced api.search; swap winner/loser; confirm dialog; invalidate qk.merges/qk.space/qk.graph/qk.brief |
| **T12 Palette + hotkeys** | ⌘K command palette (nav actions + debounced search→entity), `useHotkeys` (mod+k, `/`, `c`, `t`), mounted in App.tsx | Dialog per §6.7; focus trap; keyboard-only operable; `/` navigates /ask and focuses input |
| **T13 Final sweep** | Pattern scan (no hex/dark:/fraunces outside tokens), full gates, full live smoke both themes, vite dev proxy for CORS, ROADMAP M6 note, cleanup old views | Deferred-minor triage from ledger (incl. TIME_MAX "present" rendering, canvas z-order overlay) — final review decides fix vs park |

After T13: final whole-branch review (`review-package main..HEAD`, most-capable
reviewer, point at ledger deferred/parked lines), ONE fix wave if needed, then
`finishing-a-development-branch` (squash-merge to main per repo convention).

## 6. Key Code Anchors (read before dispatching T9+)

- `apps/brain-ui/src/api.ts` — contract types + wrappers; THE load-bearing file
- `apps/brain-ui/src/queries.ts` — `qk` keys + fetchers
- `apps/brain-ui/src/router.tsx` — hash route tree (8 routes, validateSearch)
- `apps/brain-ui/src/lib/hue.ts` — hueForType/HUE_DOT/HUE_CHIP/toRgba
- `apps/brain-ui/src/lib/useSigmaGraph.ts` — imperative sigma lifecycle
- `apps/brain-ui/src/App.tsx` — AppShell (sidebar §6.11, offline banner, theme toggle)
- `apps/brain-ui/src/tokens/` — oxi token layer (primitives/semantic/semantic-dark/components/theme)
- `crates/oxibrain-mcp/src/server.rs` — tool dispatch (~line 217), resources (~610+), contract tests (file end)

## 7. Risks & Gotchas

| Risk | Status | Follow-up |
|------|--------|-----------|
| Vite dev CORS blocks daemon fetch | known, worked around in smokes (API-layer bun scripts + one-off proxy) | T13 adds dev proxy; production unaffected |
| Reviewers found report-accuracy drift twice (claims not matching code) | pattern | treat report claims as unverified until reviewer confirms |
| Subagent shell redirects created stray artifacts (`useSigmaGraph.ts<` dir, phantom commit) | cleaned (7749d81 tree clean) | check `git status` after every implementer return |
| sigma labels/fonts/colors: three boundary lessons (oklch→rgba, raw font read, literal classes) | fixed | reuse the same boundary patterns in T12 palette rendering |
| Tailwind dynamic-class trap | fixed via literal maps | never template-literal utility names |

## 8. Deferred Minors (ledger; final review triages)

- T1: — (none open) · T2: add `-p oxibrain-client` to backend gates for tool-output changes
- T3: deterministic tie-break in surface fallback subquery
- T7: non-recent entity header falls back to raw id; TIME_MAX renders year 292278994 (render open intervals as "present" in T13)
- T8: canvas container z-order over empty-state overlay; cosmetic comment/indent in hue.ts/useSigmaGraph.ts

End of handoff. Read this + the ledger + the plan's Global Constraints, then resume at §1.
