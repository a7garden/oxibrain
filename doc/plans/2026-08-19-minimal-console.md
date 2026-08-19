# Plan D: Minimal Console (P5)

> Deliver embedded repair/operations console and delete product-scope routes.
> Exit: `cargo install oxibrain` can perform visual review without Node;
> no chat, capture, authoring, or general graph route.

## Status

- [x] Analysis complete
- [ ] Implementation

## Context

**Blueprint:** `doc/spec/ecosystem-v2-memory-kernel.md` §6.4, §9 P5
**ADR:** `doc/adr/ADR-008-console-technology.md` — keep React+Vite, commit dist/, embed with include_dir!
**Exit condition:** `cargo install oxibrain` can perform visual review without Node.

**Current state:**
- `apps/brain-ui` exists with 8 routes: Overview, Graph, GraphEntity, Entity, Ask, Conflicts, Merges, Capture.
- `serve_http` in oxibrain-mcp serves static files from `--ui-dir` (user must build themselves).
- `dist/` is gitignored; no embedding; no CI gate.
- ADR-008 locks: commit dist/, embed with `include_dir!`, CI gate (bun build + diff + size ≤ 400KB).

**Console scope (§6.4):**
- IN: merge review, contradiction/retraction review, extraction-failure inspection, provenance/source-policy inspection, entity/statement detail, space/source/health/model/reproject/backup/restore operations.
- OUT: ask/chat, capture/authoring, general knowledge homepage, exploratory force graph, host task/note/session management.

## Design Decisions

1. **Embed with `include_dir!`:** Add `include_dir = "0.7"` to oxibrain-mcp. When `ui_dir` is None,
   serve from the embedded `apps/brain-ui/dist`. When `ui_dir` is Some, serve from filesystem (dev override).

2. **Remove out-of-scope routes:** Delete AskView, CaptureView, GraphView from router and nav.
   Keep: Overview, Entity, Conflicts, Merges.

3. **Add console-only views:**
   - `FailuresView` — extraction failure inspection (list + raw response + errors).
   - `SourcesView` — source registry + policy inspection.
   - `OperationsView` — reproject, backup, restore, doctor status.

4. **Commit dist/:** Remove `dist/` from `.gitignore`, build, commit.

5. **CI gate:** Add a `console` job to ci.yml: `bun install && bun run build`, then
   `git diff --exit-code apps/brain-ui/dist`, then assert gzip ≤ 400KB.

6. **MCP tools for new views:** The existing `stats` tool covers Overview.
   New data needs:
   - Extraction failures: add MCP tool `failures` (read, Capability::Read).
   - Sources: add MCP tool `sources` (read, Capability::Read).
   - Operations: `reproject` already exists as CLI; add MCP tool `reproject` (write, Capability::Write).
   - Backup/restore: CLI-only (not MCP — too destructive for agent access).
   
   Tool count: currently 15. Adding 3 would exceed cap. Solution: combine into one
   `console` tool with a `section` parameter (failures|sources|operations).
   Actually, re-reading the cap: "fifteen tools is the cap". Current count from tool_list:
   search, recall, brief, navigate, why, contradictions, traverse, review_merges, stats,
   ingest, remember, declare, retract, merge_entities, redact = 15.
   
   **Decision:** Replace `review_merges` with a broader `console` tool that returns
   merges OR failures OR sources based on a `section` parameter. This keeps the count at 15.
   Actually, `review_merges` is used by existing tests. Better: add a `section` param to
   `review_merges` making it a general console data tool, rename description.
   
   **Final decision:** Add `section` param to existing `review_merges` tool:
   - `section: "merges"` (default) → current behavior.
   - `section: "failures"` → extraction failures.
   - `section: "sources"` → source list + policies.
   This keeps tool count at 15 and is backward-compatible.

7. **Operations (reproject/backup/restore):** These stay CLI-only. The console OperationsView
   shows status (last reproject time, store size) via `stats` tool, and provides buttons
   that call the JSON-RPC endpoint directly for reproject. Backup/restore remain CLI-only
   (file operations).

## Task Breakdown

### Task 1: Remove out-of-scope routes from brain-ui

**Files:**
- Modify: `apps/brain-ui/src/router.tsx` — remove graph, ask, capture routes
- Modify: `apps/brain-ui/src/App.tsx` — remove nav items for graph, ask, capture
- Delete: `apps/brain-ui/src/views/AskView.tsx`
- Delete: `apps/brain-ui/src/views/CaptureView.tsx`
- Delete: `apps/brain-ui/src/views/GraphView.tsx`
- Delete: `apps/brain-ui/src/lib/useSigmaGraph.ts`
- Modify: `apps/brain-ui/package.json` — remove sigma, graphology, graphology-layout-forceatlas2

**Steps:**
1. Remove graph/ask/capture imports and routes from router.tsx.
2. Remove nav items from App.tsx (keep Overview, Conflicts, Merges, Entity).
3. Delete the view files and useSigmaGraph.
4. Remove sigma/graphology from package.json dependencies.
5. Verify `bun run build:ts` passes (type check).

**Acceptance:** TypeScript compiles. No references to deleted views.

---

### Task 2: Add console views (Failures, Sources, Operations)

**Files:**
- Create: `apps/brain-ui/src/views/FailuresView.tsx`
- Create: `apps/brain-ui/src/views/SourcesView.tsx`
- Create: `apps/brain-ui/src/views/OperationsView.tsx`
- Modify: `apps/brain-ui/src/router.tsx` — add routes
- Modify: `apps/brain-ui/src/App.tsx` — add nav items
- Modify: `apps/brain-ui/src/api.ts` — add API wrappers
- Modify: `apps/brain-ui/src/queries.ts` — add query keys/fetchers

**Steps:**
1. Add `console` API wrapper in api.ts:
```typescript
console_data: (section: "merges" | "failures" | "sources", space?: string) =>
  callTool("review_merges", { section, space }),
```

2. Create FailuresView: table of extraction failures (episode_id, extractor_id, created_at, errors).
   Expandable row shows raw_response.

3. Create SourcesView: table of sources (name, kind, mode) + policy info.

4. Create OperationsView: shows stats (entity count, episode count, store size),
   reproject button (calls JSON-RPC `reproject` method directly).

5. Add routes and nav items.

**Acceptance:** TypeScript compiles. Views render with mock data structure.

---

### Task 3: MCP — extend review_merges with section param

**Files:**
- Modify: `crates/oxibrain-mcp/src/server.rs` — extend tool_review_merges

**Steps:**
1. In `tool_review_merges`, read optional `section` arg (default "merges").
2. Match on section:
   - "merges" → current behavior (list_merges).
   - "failures" → call quarantine::list_failures, return JSON.
   - "sources" → call ledger::list_sources (or equivalent), return JSON.
3. Update tool description in tool_list().
4. Add tests for failures and sources sections.

**Acceptance:** All existing tests pass. New sections return correct data.

---

### Task 4: Embed console in binary with include_dir

**Files:**
- Modify: `crates/oxibrain-mcp/Cargo.toml` — add include_dir dependency
- Modify: `crates/oxibrain-mcp/src/server.rs` — embed dist, fallback logic
- Modify: `apps/brain-ui/.gitignore` — remove dist/ line

**Steps:**
1. Add to oxibrain-mcp Cargo.toml:
```toml
include_dir = "0.7"
```

2. In server.rs, add at module level:
```rust
use include_dir::{include_dir, Dir};
static CONSOLE_DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../apps/brain-ui/dist");
```

3. Modify `handle_http_get`: when `ui_dir` is None, serve from `CONSOLE_DIST`:
```rust
async fn handle_http_get(
    reader: &mut BufReader<tokio::net::TcpStream>,
    path: &str,
    ui_dir: Option<Arc<PathBuf>>,
) -> anyhow::Result<()> {
    let rel = path.split('?').next().unwrap_or("/");
    let rel = rel.strip_prefix('/').unwrap_or(rel);
    let rel = if rel.is_empty() { "index.html" } else { rel };

    // Filesystem override (dev mode).
    if let Some(ref dir) = ui_dir {
        // existing logic...
    }

    // Embedded console.
    let file_path = if rel.contains("..") { return 404; } else { rel };
    match CONSOLE_DIST.get_file(file_path) {
        Some(file) => {
            let data = file.contents();
            let ct = content_type_from_name(file_path);
            write_http_response_with_ct(reader, 200, "OK", ct, data).await
        }
        None => {
            // SPA fallback.
            match CONSOLE_DIST.get_file("index.html") {
                Some(f) => write_http_response_with_ct(reader, 200, "OK", "text/html", f.contents()).await,
                None => write_http_response(reader, 404, "Not Found", b"not found").await,
            }
        }
    }
}
```

4. Remove `dist/` from `apps/brain-ui/.gitignore`.

**Acceptance:** `cargo build -p oxibrain-mcp` compiles. serve_http without --ui-dir serves embedded console.

---

### Task 5: Build and commit dist/

**Files:**
- Build output: `apps/brain-ui/dist/`

**Steps:**
1. Run `cd apps/brain-ui && bun install && bun run build`.
2. Verify dist/ output exists and is reasonable size.
3. `git add apps/brain-ui/dist/`.

**Acceptance:** dist/ committed. `cargo build -p oxibrain-cli` succeeds (include_dir finds dist/).

---

### Task 6: CI gate

**Files:**
- Modify: `.github/workflows/ci.yml` — add console job

**Steps:**
1. Add a `console` job:
```yaml
  console:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: oven-sh/setup-bun@v2
      - name: install deps
        working-directory: apps/brain-ui
        run: bun install
      - name: build
        working-directory: apps/brain-ui
        run: bun run build
      - name: bundle matches source
        run: git diff --exit-code apps/brain-ui/dist
      - name: bundle size
        run: |
          SIZE=$(find apps/brain-ui/dist -type f -exec cat {} + | gzip -9 | wc -c)
          if [ "$SIZE" -gt 409600 ]; then
            echo "console bundle exceeds 400KB gzipped: $SIZE bytes"; exit 1
          fi
```

**Acceptance:** CI job defined correctly.

---

### Task 7: Documentation

**Files:**
- Modify: `doc/ARCHITECTURE.md`

**Steps:**
1. Update §16.4 or add §16.5 documenting the embedded console.
2. Note that `oxibrain serve --http 127.0.0.1:18080` now serves the console without --ui-dir.
3. Bump version header.

**Acceptance:** Documentation reflects embedded console delivery.

---

## Dependency Graph

```
Task 1 (remove routes) ──┐
                         ├── Task 5 (build dist)
Task 2 (add views) ──────┘         │
                                   ▼
Task 3 (MCP section) ────── Task 4 (embed) ── Task 6 (CI)
Task 7 (docs) — independent
```

Tasks 1, 2, 3, 7 can run in parallel.
Task 5 depends on Tasks 1+2 (needs final source to build).
Task 4 depends on Task 5 (needs dist/ to exist for include_dir).
Task 6 depends on Task 4.

## Risk Notes

- **include_dir path:** `$CARGO_MANIFEST_DIR/../../apps/brain-ui/dist` — must exist at compile time.
  Task 5 must complete before Task 4's code is compiled. In practice, we build dist first,
  then add the include_dir code.
- **Bundle size:** With sigma/graphology removed, the bundle should shrink significantly.
  React 19 + TanStack Router + Query alone is ~150KB gzipped.
- **review_merges backward compat:** Adding `section` param with default "merges" is fully
  backward-compatible. Existing callers that don't pass `section` get the same behavior.
