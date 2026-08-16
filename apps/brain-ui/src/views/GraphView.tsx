/** `/graph` (focus-less) and `/graph/$entityId` (focused) explorer.
 *
 *  Data shape (server-side, real):
 *  - TraversalResult { nodes: TraversalNode[], edges: TraversalEdge[], truncated }
 *    with TraversalNode { entity: EntityId, depth: u8, salience: f64 }
 *  - Beliefs are fetched for the selected node and shown in the side panel.
 *
 *  Layout: the imperative `useSigmaGraph` hook owns the Sigma instance and
 *  the graphology graph. This view just feeds data + selection state in.
 */

import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import { ErrorState } from "../components/ErrorState";
import { useSigmaGraph } from "../lib/useSigmaGraph";
import { fetchers, qk } from "../queries";

export function GraphView() {
  const params = useParams({ strict: false }) as { entityId?: string };
  const focusId = params.entityId;

  // ── Space overview (for the no-focus path: recent 8 ids) ────────────
  const spaceQuery = useQuery({
    queryKey: qk.space,
    queryFn: fetchers.space,
    refetchInterval: 30_000,
  });
  const recentIds = useMemo(
    () =>
      (spaceQuery.data?.recent_entities ?? [])
        .slice(0, 8)
        .map((e) => e.id),
    [spaceQuery.data?.recent_entities],
  );
  const recentIdsReady = recentIds.length > 0;

  // ── Source data ──────────────────────────────────────────────────────
  const graphQuery = useQuery({
    queryKey: focusId ? qk.graph(focusId) : ["graph", "no-focus"],
    queryFn: () =>
      focusId
        ? fetchers.graph(focusId)
        : api.traverse(recentIds, "personal", 2),
    enabled: focusId !== undefined || recentIdsReady,
  });

  // ── Canvas host ──────────────────────────────────────────────────────
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(focusId ?? null);
  // Sync selection to route on navigation (but not on user clicks that
  // keep the same focusId). When the user navigates from /graph to
  // /graph/$id (or between focus routes), the selection should follow.
  useEffect(() => {
    setSelectedId(focusId ?? null);
  }, [focusId]);

  const graphNodes = useMemo(
    () => graphQuery.data?.nodes ?? [],
    [graphQuery.data?.nodes],
  );
  const graphEdges = useMemo(
    () => graphQuery.data?.edges ?? [],
    [graphQuery.data?.edges],
  );

  useSigmaGraph({
    containerRef,
    nodes: graphNodes,
    edges: graphEdges,
    selectedId,
    onNodeClick: (id) => setSelectedId(id),
  });

  // ── Side panel: beliefs of the selected node ────────────────────────
  const beliefsQuery = useQuery({
    queryKey: selectedId ? qk.beliefs(selectedId) : ["beliefs", "none"],
    queryFn: () => api.beliefs(selectedId!),
    enabled: selectedId !== null,
  });

  return (
    <div className="flex h-full flex-col">
      {/* ── Header strip (focus breadcrumb) ────────────────────────── */}
      <div className="flex items-center gap-3 border-b border-line bg-surface-raised px-6 py-3">
        <h2 className="font-display text-lg font-light tracking-tight text-text">
          Graph
        </h2>
        {focusId ? (
          <span className="font-mono text-xs text-text-muted">
            focused on{" "}
            <span className="text-text-subtle">{focusId}</span>
          </span>
        ) : (
          <span className="font-mono text-xs text-text-muted">
            {recentIds.length > 0
              ? `recent ${recentIds.length} entities (depth 2)`
              : "no recent entities"}
          </span>
        )}
      </div>

      <div className="flex min-h-0 flex-1">
        {/* ── Canvas area ────────────────────────────────────────── */}
        <div className="relative flex-1 bg-surface-sunken">
          {graphQuery.isError && (
            <ErrorState
              message="Could not load the graph. The daemon may be unreachable."
              onRetry={() => graphQuery.refetch()}
            />
          )}
          {!graphQuery.isError && graphQuery.isLoading && (
            <div className="flex h-full items-center justify-center">
              <div className="space-y-3 p-6">
                <div className="bg-surface-muted h-3 w-48 rounded" />
                <div className="bg-surface-muted h-3 w-64 rounded" />
                <div className="bg-surface-muted h-3 w-40 rounded" />
              </div>
            </div>
          )}
          {!graphQuery.isError &&
            !graphQuery.isLoading &&
            graphNodes.length === 0 && (
              <div className="flex h-full items-center justify-center px-6 text-center">
                <p className="font-mono text-sm text-text-subtle">
                  {focusId
                    ? "This entity has no edges yet. Declare a relation to see it here."
                    : "No entities captured yet. Use Capture to add your first."}
                </p>
              </div>
            )}
          <div ref={containerRef} className="absolute inset-0" />
        </div>

        {/* ── Right panel ────────────────────────────────────────── */}
        <aside className="bg-surface-raised flex w-80 flex-col border-l border-line">
          <div className="border-b border-line/60 px-5 py-4">
            <p className="text-2xs font-medium tracking-wider uppercase text-text-subtle">
              Selected node
            </p>
            {selectedId ? (
              <p className="mt-1 break-all font-mono text-sm text-text">
                {selectedId}
              </p>
            ) : (
              <p className="mt-1 font-mono text-sm text-text-muted">
                click a node to inspect
              </p>
            )}
            {selectedId && (
              <Link
                to="/entity/$entityId"
                params={{ entityId: selectedId }}
                search={{ tab: "brief" }}
                className="mt-3 inline-block font-mono text-xs text-interactive-primary hover:underline"
              >
                Open page →
              </Link>
            )}
          </div>

          <div className="flex-1 overflow-auto px-5 py-4">
            {!selectedId ? (
              <p className="font-mono text-xs text-text-subtle">
                No node selected.
              </p>
            ) : beliefsQuery.isLoading ? (
              <ul className="space-y-3">
                {Array.from({ length: 3 }).map((_, i) => (
                  <li key={i} className="space-y-1">
                    <div className="bg-surface-muted h-3 w-32 rounded" />
                    <div className="bg-surface-muted h-3 w-48 rounded" />
                  </li>
                ))}
              </ul>
            ) : beliefsQuery.isError ? (
              <ErrorState
                message="Could not load beliefs."
                onRetry={() => beliefsQuery.refetch()}
              />
            ) : (beliefsQuery.data ?? []).length === 0 ? (
              <p className="font-mono text-xs text-text-subtle">
                No beliefs recorded for this entity yet.
              </p>
            ) : (
              <ul className="divide-y divide-line/40">
                {(beliefsQuery.data ?? []).map((b, i) => (
                  <li key={i} className="py-3">
                    <p className="font-mono text-sm">
                      <span className="text-text-muted">{b.statement}</span>{" "}
                      <span className="text-text-subtle">·</span>{" "}
                      <span className="text-text">{confFmt(b.confidence)}</span>
                    </p>
                    <p className="mt-1 font-mono text-2xs text-text-subtle">
                      {supportFmt(b.support)}
                    </p>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </aside>
      </div>
    </div>
  );
}

// ── Formatters ──────────────────────────────────────────────────────────

function confFmt(c: number): string {
  return `${Math.round(c * 100)}%`;
}

function supportFmt(s: {
  affirm_count: number;
  deny_count: number;
  distinct_episodes: number;
}): string {
  return `${s.affirm_count} affirm · ${s.deny_count} deny · ${s.distinct_episodes} ep`;
}
