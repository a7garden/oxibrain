/** Imperative sigma.js wrapper. Manages a single `Sigma` instance whose
 *  graph, layout, and per-node attributes come from the React side. We use
 *  the imperative API (no `@react-sigma/core`) so the dependency surface
 *  stays minimal — only `sigma`, `graphology`, and `graphology-layout-forceatlas2`.
 *
 *  Re-mount contract:
 *  - The Sigma instance is recreated when the SET of node ids changes
 *    (so node add/remove is a real layout re-run).
 *  - Selection flips write the `selected` attribute and call `refresh()` —
 *    no re-mount.
 *  - Theme flips (`.dark` toggling on `<html>`) re-resolve resolved colors
 *    via `getComputedStyle`, write new color attributes, and refresh.
 *  - Unmount kills the instance.
 *
 *  Per ambiguity-resolution #2 (deterministic layout):
 *    - seed = mulberry32 over a sorted-id hash
 *    - initial positions on a circle
 *    - FA2 synchronous `assign` (≤100 iter, scalingRatio 10, gravity 0.5)
 *    - skip FA2 entirely for graphs with < 2 nodes
 */

import Graph from "graphology";
import forceAtlas2 from "graphology-layout-forceatlas2";
import { useEffect, useRef } from "react";
import Sigma from "sigma";
import type { GraphEdge, GraphNode } from "../api";
import { hueColor } from "./hue";

export interface UseSigmaGraphArgs {
  containerRef: React.RefObject<HTMLDivElement | null>;
  nodes: GraphNode[];
  edges: GraphEdge[];
  selectedId: string | null;
  onNodeClick: (id: string) => void;
}

export interface UseSigmaGraphResult {
  relayout: () => void;
}

// ── Deterministic PRNG ──────────────────────────────────────────────────

/** FNV-1a hash of a string → 32-bit unsigned. Stable, well-distributed,
 *  cheap — used purely to seed the position PRNG from the node-id set. */
function fnv1a(input: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ── Resolved-token helpers ──────────────────────────────────────────────

function resolveToken(name: string): string {
  return getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
}

/** Map a graphology node attribute object → sigma display data. */
function buildNodeReducer(
  nodeId: string | null,
  interactive: string,
) {
  return (id: string, data: Record<string, unknown>) => {
    const size = (data.size as number) ?? 6;
    const isSelected = nodeId !== null && id === nodeId;
    return {
      x: data.x as number,
      y: data.y as number,
      label: data.label as string | undefined,
      color: isSelected ? interactive : (data.color as string),
      size: isSelected ? Math.max(size + 4, 12) : size,
      labelColor: (data.labelColor as string | undefined) ?? "#ffffff",
    };
  };
}

function buildEdgeReducer() {
  return (_id: string, data: Record<string, unknown>) => ({
    color: (data.color as string | undefined) ?? "#666",
    size: (data.size as number | undefined) ?? 1,
  });
}

// ── Initial positions ───────────────────────────────────────────────────

const CIRCLE_RADIUS = 100;

function placeOnCircle(graph: Graph, rand: () => number): void {
  const ids = graph.nodes();
  const n = ids.length;
  if (n === 1) {
    graph.setNodeAttribute(ids[0]!, "x", 0);
    graph.setNodeAttribute(ids[0]!, "y", 0);
    return;
  }
  const jitter = (rand() - 0.5) * 0.4;
  ids.forEach((id, i) => {
    const theta = (2 * Math.PI * i) / n + jitter;
    graph.setNodeAttribute(id, "x", Math.cos(theta) * CIRCLE_RADIUS);
    graph.setNodeAttribute(id, "y", Math.sin(theta) * CIRCLE_RADIUS);
  });
}

// ── Hook ────────────────────────────────────────────────────────────────

const MAX_BBOX_DIAG = 200;

/** Rescale node positions so the post-FA2 bounding-box diagonal fits
 *  within `MAX_BBOX_DIAG`. Preserves relative geometry (uniform scale
 *  around the centroid). No-op for graphs with 0–1 nodes. */
function normalizeSpread(graph: Graph): void {
  if (graph.order < 2) return;
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const id of graph.nodes()) {
    const x = graph.getNodeAttribute(id, "x") as number;
    const y = graph.getNodeAttribute(id, "y") as number;
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  const diag = Math.hypot(maxX - minX, maxY - minY);
  if (diag <= MAX_BBOX_DIAG) return;
  const scale = MAX_BBOX_DIAG / diag;
  const cx = (minX + maxX) / 2;
  const cy = (minY + maxY) / 2;
  for (const id of graph.nodes()) {
    const x = graph.getNodeAttribute(id, "x") as number;
    const y = graph.getNodeAttribute(id, "y") as number;
    graph.setNodeAttribute(id, "x", (x - cx) * scale);
    graph.setNodeAttribute(id, "y", (y - cy) * scale);
  }
}

// ── Hook ────────────────────────────────────────────────────────────────

export function useSigmaGraph({
  containerRef,
  nodes,
  edges,
  selectedId,
  onNodeClick,
}: UseSigmaGraphArgs): UseSigmaGraphResult {
  // Latest-state refs so the (stable) effect closures see fresh values.
  const nodesRef = useRef(nodes);
  const edgesRef = useRef(edges);
  const selectedRef = useRef<string | null>(selectedId);
  const onClickRef = useRef(onNodeClick);
  nodesRef.current = nodes;
  edgesRef.current = edges;
  selectedRef.current = selectedId;
  onClickRef.current = onNodeClick;

  const sigmaRef = useRef<Sigma | null>(null);
  const graphRef = useRef<Graph | null>(null);
  const lastNodeKeyRef = useRef<string>("");
  const relayoutRef = useRef<(() => void) | null>(null);

  // Effect: (re)create the Sigma instance when the node-id set changes.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const sortedIds = [...nodes.map((n) => n.entity)].sort();
    const nodeKey = sortedIds.join("|");
    const idSetChanged = nodeKey !== lastNodeKeyRef.current;
    lastNodeKeyRef.current = nodeKey;

    // Tear down old instance on id-set change or unmount.
    if (sigmaRef.current && idSetChanged) {
      sigmaRef.current.kill();
      sigmaRef.current = null;
      graphRef.current = null;
    }

    // Build (or rebuild) the graphology graph on id-set change.
    if (idSetChanged) {
      const rand = mulberry32(fnv1a(nodeKey));
      const graph = new Graph({ multi: false, type: "directed" });

      const interactive = resolveToken("--color-interactive-primary");
      const text = resolveToken("--color-text");
      const edgeColor = resolveToken("--color-border-strong");

      for (const n of nodes) {
        if (graph.hasNode(n.entity)) continue;
        graph.addNode(n.entity, {
          label: n.entity,
          color: hueColor("Entity"),
          size: 6,
          labelColor: text,
        });
      }
      for (const e of edges) {
        if (!graph.hasNode(e.from) || !graph.hasNode(e.to)) continue;
        const key = `${e.from}->${e.to}:${e.predicate}:${e.statement_id}`;
        if (graph.hasEdge(key)) continue;
        graph.addEdgeWithKey(key, e.from, e.to, {
          color: edgeColor,
          size: 1,
          label: e.predicate,
        });
      }

      placeOnCircle(graph, rand);

      // Degree-aware sizing (clamped 4–12) BEFORE FA2 so the layout has
      // its final node sizes.
      const degree = new Map<string, number>();
      for (const id of graph.nodes()) {
        degree.set(id, graph.degree(id));
      }
      const minDeg = Math.min(...degree.values());
      const maxDeg = Math.max(...degree.values());
      const range = maxDeg - minDeg || 1;
      for (const id of graph.nodes()) {
        const d = degree.get(id) ?? 0;
        const t = (d - minDeg) / range;
        graph.setNodeAttribute(id, "size", 4 + t * 8);
      }

      // Skip FA2 for tiny graphs.
      if (graph.order >= 2) {
        forceAtlas2.assign(graph, {
          iterations: 100,
          settings: { scalingRatio: 10, gravity: 0.5, slowDown: 10 },
        });
      }
      // Normalize the spread so the camera auto-rescale produces a
      // legible view even when FA2 pushes disconnected components far
      // apart (scalingRatio 10 with a 3-node graph can spread thousands
      // of units). Find the post-layout bounding box and linearly scale
      // all positions so the diagonal fits within `MAX_BBOX_DIAG`. This
      // preserves the relative geometry FA2 produced.
      normalizeSpread(graph);

      // Sigma container must be empty before constructing Sigma (it
      // creates layered canvases that share its element).
      container.innerHTML = "";

      const sigma = new Sigma(graph, container, {
        renderLabels: true,
        labelWeight: "500",
        labelColor: { color: text },
        defaultEdgeColor: edgeColor,
        minCameraRatio: 0.2,
        maxCameraRatio: 4,
        nodeReducer: buildNodeReducer(selectedRef.current, interactive),
        edgeReducer: buildEdgeReducer(),
      });

      sigma.on("clickNode", ({ node }) => {
        onClickRef.current(node);
      });

      sigmaRef.current = sigma;
      graphRef.current = graph;

      relayoutRef.current = () => {
        const g = graphRef.current;
        if (!g) return;
        if (g.order < 2) return;
        // Reset to circle seed first so the relayout is deterministic for
        // the current node-id set.
        placeOnCircle(g, mulberry32(fnv1a(lastNodeKeyRef.current)));
        forceAtlas2.assign(g, {
          iterations: 100,
          settings: { scalingRatio: 10, gravity: 0.5, slowDown: 10 },
        });
        normalizeSpread(g);
        sigmaRef.current?.refresh();
      };
    } else {
      // Same id-set, same selection: nothing to do.
    }

    return () => {
      // Tear down on unmount or before the next recreate.
      if (sigmaRef.current) {
        sigmaRef.current.kill();
        sigmaRef.current = null;
        graphRef.current = null;
      }
    };
    // We intentionally key on `nodes.length` + their sorted identity so
    // re-renders with the same data don't churn. `selectedId` flips go
    // through a separate effect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [containerRef, nodes.map((n) => n.entity).sort().join("|")]);

  // Effect: selection highlight only (no re-mount).
  useEffect(() => {
    const sigma = sigmaRef.current;
    const graph = graphRef.current;
    if (!sigma || !graph) return;
    const interactive = resolveToken("--color-interactive-primary");
    sigma.setSetting(
      "nodeReducer",
      buildNodeReducer(selectedId, interactive),
    );
    sigma.refresh();
    // Also flag the node attribute so other reducers (and external
    // observers in tests) can read it directly.
    if (selectedId && graph.hasNode(selectedId)) {
      graph.setNodeAttribute(selectedId, "selected", true);
      for (const id of graph.nodes()) {
        if (id !== selectedId && graph.getNodeAttribute(id, "selected")) {
          graph.setNodeAttribute(id, "selected", false);
        }
      }
    } else {
      for (const id of graph.nodes()) {
        if (graph.getNodeAttribute(id, "selected")) {
          graph.setNodeAttribute(id, "selected", false);
        }
      }
    }
  }, [selectedId]);

  // Effect: theme flip — re-resolve token colors, push to attributes,
  // refresh.
  useEffect(() => {
    const observer = new MutationObserver(() => {
      const sigma = sigmaRef.current;
      const graph = graphRef.current;
      if (!sigma || !graph) return;
      const interactive = resolveToken("--color-interactive-primary");
      const text = resolveToken("--color-text");
      const edgeColor = resolveToken("--color-border-strong");
      for (const id of graph.nodes()) {
        graph.setNodeAttribute(id, "labelColor", text);
      }
      sigma.setSetting("labelColor", { color: text });
      sigma.setSetting("defaultEdgeColor", edgeColor);
      sigma.setSetting(
        "nodeReducer",
        buildNodeReducer(selectedRef.current, interactive),
      );
      sigma.refresh();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    });
    return () => observer.disconnect();
  }, []);

  return {
    relayout: () => relayoutRef.current?.(),
  };
}
