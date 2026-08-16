# brain-ui v2 — Daily-Driver Redesign

Status: approved design (2026-08-16). Implementation spec for the `apps/brain-ui`
desktop surface. Not an architecture change: engine, MCP tool semantics, and the
fifteen-tool cap are untouched.

## 1. Context

`apps/brain-ui` (M6 + M9 brief view) is a Vite + React 19 + Tailwind v4 app with
five views (graph, brief, ask, conflicts, capture), a bespoke warm-dark
"observatory" palette (hex tokens, Fraunces serif display), and no routing,
caching, or keyboard layer. Three gaps drive this redesign:

1. **M6 promises missing from the UI**: no timeline view, no merge-review view.
   `api.listMerges` / `mergeEntities` are already wrapped but unused. The
   conflicts-inbox Retract action is a stub that prints a CLI hint
   (`ContradictionInbox.tsx:27`).
2. **Contract mismatches (live bugs)**: the `space://` resource returns
   `contradiction_count` and `recent_entities[].canonical_key`, while the UI
   reads `contradictions` and `.surface` — the sidebar conflict count never
   renders and the Brief recent-entity chips render empty labels.
3. **Design-system divergence**: hex palette, serif identity, dark-only — all
   forbidden by the oxi design system, which is the brand authority for oxi
   projects (see `~/.omp/agent/managed-skills/oxi-design-system/DESIGN.md`,
   mirrored at `project-oxi/.github/DESIGN.md`).

## 2. Goals

- **Functional completion**: timeline (as an entity-page tab), merge review,
  working Retract, `why`-powered provenance in Ask.
- **oxi design-system migration** (replace, don't merge): OKLCH 3-tier tokens,
  SUIT/SUITE + Geist Mono, light & dark themes, six label hues.
- **UX fundamentals**: URL routing with deep links, TanStack Query caching and
  mutation invalidation, ⌘K command palette, skeletons, error boundaries,
  offline state, keyboard shortcuts.

Usage scenario: **daily driver** — keyboard-first navigation, responsive with
thousands of entities (bounded subgraphs, not whole-space rendering).

## 3. Non-goals

- New MCP **tools** (cap stays 15; extensions are resources and output DTOs).
- Episode-ledger browser, subscriptions/live push (ADR-001 unchanged).
- Authoring/editing features (architecture boundary).
- Multi-space switcher UI (personal space only, as today).
- Mobile-specific layouts beyond not-breaking at narrow widths.

## 4. Stack

| Area | Choice | Note |
|---|---|---|
| Router | TanStack Router, **hash history** | `--ui-dir` serves static files; hash avoids server rewrites. Typed routes, search params (`/ask?q=`). |
| Server state | TanStack Query v5 | One query key family per resource/tool; polling for overview + conflicts; mutations invalidate. |
| Graph | sigma.js v3 + graphology + @react-sigma/core | WebGL rendering; ForceAtlas2 layout with fixed seed (deterministic positions). |
| Kept | Vite, React 19, Tailwind v4, bun | Unchanged. |

New deps: `@tanstack/react-router`, `@tanstack/react-query`, `sigma`,
`graphology`, `@react-sigma/core`, `graphology-layout-forceatlas2`,
`@fontsource/geist-mono`.

## 5. Design-system migration

- Token files land in `src/tokens/` per the oxi spec §2.2 (primitives →
  semantic → component tokens). Tailwind `@theme` maps semantic tokens to
  utilities (`bg-surface`, `text-text`, `border-line`, …). Components consume
  utilities only — never `var()`, never primitives, never `dark:` variants.
- Fonts: SUIT Variable (body) + SUITE Variable (display ≥20px) from jsDelivr;
  Geist Mono via Fontsource. Fraunces / Geist-sans Google-Fonts links removed
  from `index.html`.
- Themes: `.dark` class on `<html>`, inline FOUC script reading
  `localStorage['oxi-theme']` + `prefers-color-scheme` before CSS loads;
  default follows system preference; sidebar toggle; shortcut `t`.
- Entity types map to the six label hues (red/amber/green/teal/blue/purple) by
  stable hash of the type string. Graph node colors come from the same mapping
  (tokens are read at the edge of the canvas and passed as colors to sigma).
- Forbidden patterns enforced in review: serif identity, hex/rgb/hsl in
  component code, `dark:` outside token files, CSS borders on inputs
  (box-shadow instead), left accent rails on cards, `React.FC`.

## 6. Backend extensions (MCP server, `crates/oxibrain-mcp`)

### 6.1 New resource: `timeline://{entity_id}`

`?space=personal&from=<ms>&to=<ms>` (range optional). Returns the existing
`TimelineEntry` DTO from `oxibrain-store::timeline` (statement_id, predicate,
object_repr, status, valid_from, valid_to, recorded_at). Register in
`resources_list` templates. `Brain::timeline()` already exists — this is a
transport surface only.

### 6.2 `contradictions` tool output → UI DTO

Replace the raw `Vec<Statement>` serialization with a purpose-built DTO
carrying everything one-click Retract needs (the existing `retract` tool takes
subject `{surface,type}`, predicate, object `{kind,value}`, episode id):

```json
{
  "statement_id": "…",
  "subject": { "id": "…", "surface": "Alice", "type": "person" },
  "predicate": "born_in",
  "values": [
    { "kind": "entity", "value": "Seoul", "episode_ids": ["…"], "confidence": 0.9 },
    { "kind": "entity", "value": "Busan", "episode_ids": ["…"], "confidence": 0.8 }
  ]
}
```

Store-side data already exists (`ContradictionData.affirm_episodes`,
statement subject/object). Exact field sourcing is settled in the
implementation plan; the contract above is the requirement.

### 6.3 `space://` contract fix

Canonical shape (fixes the §1.2 bugs; `recent_entities` gains `surface` and a
plain `type` field — store joins the names table in `list_entities`):

```json
{
  "space": "personal",
  "entity_count": 12,
  "episode_count": 340,
  "contradiction_count": 2,
  "recent_entities": [{ "id": "…", "surface": "Alice", "type": "person" }]
}
```

**Contract tests** in `oxibrain-mcp` assert the exact JSON keys of
`space://`, `timeline://`, and the `contradictions` DTO. Frontend types are
written to mirror these tests — the tests are the source of truth.

## 7. Information architecture

Hash routes:

```
/            Overview — stat cards, recent entities, conflicts summary (home)
/graph/:id?  Explorer — sigma canvas, zoom/pan, node click → side panel
             (beliefs via entity:// + link to brief); :id sets the focus node
/entity/:id  Entity page — tabs: Brief (markdown + entity:// links) | Timeline
/ask?q=      Search + provenance — per-result expand runs why(), shows
             assertions (episode, extractor, mention) + link to brief
/conflicts   Inbox — per-value evidence episodes, one-click Retract behind a
             confirm dialog (calls retract with the chosen value + episode),
             open-brief link
/merges      Merge review — MergeRecord list + new-merge form fed by search
             pickers (loser/winner surface + type) → merge_entities
/capture     Quick capture — remember() with result feedback (episodes,
             extracted, quarantined)
```

Merges of Brief + Timeline into one entity page: the M6 "timeline view" is
stronger as a tab beside the brief of the same entity. The raw-entity-id input
in Brief is removed — ⌘K palette and `/ask` replace it.

**⌘K command palette** (global): fuzzy search over `search` tool results +
view navigation + actions (capture, theme toggle). `/` focuses it in ask mode;
`c` opens capture; `t` toggles theme.

## 8. UX details

- **Query keys**: `['space']`, `['search', q]`, `['brief', id]`,
  `['timeline', id]`, `['graph', id, depth]`, `['contradictions']`,
  `['merges']`, `['why', statementId]`. Overview + conflicts poll (30 s /
  15 s); everything else stale-while-revalidate. Mutations (`remember`,
  `retract`, `merge_entities`) invalidate `['space']` + affected entity keys +
  `['contradictions']` / `['merges']`.
- **States**: skeleton shimmer per card/list, per-view error boundary with
  retry, offline banner from Query's failure state (replaces the 5 s manual
  poll).
- **Graph performance**: bounded subgraph via `traverse` (`max_nodes` cap,
  default 256; depth 2), FA2 layout iteration budget, WebGL rendering. Focus
  entity is pinned/centered; neighbors colored by type hue.

## 9. Verification

1. `cargo test -p oxibrain-mcp` — new resource + DTO contract tests green.
2. `bun run build:ts` + `vite build` clean.
3. **Live smoke**: `oxibrain serve --http 127.0.0.1:18080 --ui-dir …/dist`
   against a temp store; drive every route in a real browser (light + dark):
   deep links, back/forward, ⌘K, retract round-trip (conflict count drops),
   merge round-trip, capture round-trip, graph zoom/pan/node-click.
4. Design-system review: forbidden-pattern grep over `src/` (hex literals,
   `dark:` in components, Fraunces remnants).

## 10. Risks

- **sigma.js + React 19**: @react-sigma/core peer versions may lag; if so,
  wrap sigma imperatively in a thin hook instead of the React binding.
- **Retract semantics**: one-click retract from the inbox must keep the audit
  trail (it writes a Declaration episode — by construction); confirm dialog is
  mandatory because it is destructive to beliefs.
- **Hash routing**: URLs are uglier (`/#/entity/…`); accepted tradeoff for
  zero server rewrites under `--ui-dir`.
