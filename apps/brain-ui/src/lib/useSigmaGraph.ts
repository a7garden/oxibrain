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
 *  - Unmount kills the instance and clears `lastNodeKeyRef` so React
 *    StrictMode dev remounts rebuild the canvas.
 *
 *  Per ambiguity-resolution #2 (deterministic layout):
 *    - seed = mulberry32 over a sorted-id hash
 *    - initial positions on a circle
 *    - FA2 synchronous `assign` (≤100 iter, scalingRatio 10, gravity 0.5)
 *    - skip FA2 entirely for graphs with < 2 nodes
 *
 *  Canvas-boundary color contract: sigma's `parseColor` only understands
 *  `#hex`, `rgb()`, `rgba()`, and CSS named colors. Our design tokens
 *  are `oklch(...)` strings, which silently fall through to opaque black
 *  on the canvas. `hueColor()` (in `./hue`) and `resolveToken()` (below)
 *  both run the value through a 1×1 canvas readback before handing it
 *  across the boundary.
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

let _parseCanvas: HTMLCanvasElement | null = null;
/** Convert any modern CSS color string (oklch, lab, color(), etc.) to a
 *  hex string via a 1x1 canvas readback. Sigma's `parseColor` only
 *  understands hex / rgb() / rgba() / named — `oklch()` resolves to
 *  opaque black otherwise. */
function toRgba(cssColor: string): string {
  if (typeof document === "undefined") return "#000000";
  if (!_parseCanvas) {
    _parseCanvas = document.createElement("canvas");
    _parseCanvas.width = 1;
    _parseCanvas.height = 1;
  }
  const ctx = _parseCanvas.getContext("2d");
  if (!ctx) return "#000000";
  ctx.clearRect(0, 0, 1, 1);
  ctx.fillStyle = "rgba(0,0,0,0)";
  ctx.fillStyle = cssColor;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return `rgba(${r}, ${g}, ${b}, ${(a ?? 255) / 255})`;
}

/** Read `--token` from `<html>` and convert to an rgba() string suitable
 *  for sigma's canvas boundary. */
function resolveToken(name: string): string {
  return toRgba(
    getComputedStyle(document.documentElement)
      .getPropertyValue(name)
      .trim(),
  );
}

/** Resolve `--font-sans` and pick a single concrete family name — the
 *  token value is a comma-separated stack like
 *  `"SUIT Variable", "SUIT", system-ui, …`. Sigma accepts one family.
 *  Read the stack RAW (no rgba conversion — a font stack isn't a
 *  color, and `resolveToken`'s canvas readback would return garbage). */
function resolveFontFamily(): string {
  const stack = getComputedStyle(document.documentElement)
    .getPropertyValue("--font-sans")
    .trim();
   if (stack.includes("SUIT Variable")) return "SUIT Variable";
   const quoted = stack.match(/"([^"]+)"/);
   if (quoted) return quoted[1]!;
   const first = stack.split(",")[0]?.trim().replace(/^["']|["']$/g, "");
   return first && first.length > 0 ? first : "sans-serif";
 }

/** Map a graphology node attribute object → sigma display data. */
function buildNodeReducer(nodeId: string | null, interactive: string) {
  return (id: string, data: Record<string, unknown>) => {
    const size = (data.size as number) ?? 6;
    const isSelected = nodeId !== null && id === nodeId;
    return {
      x: data.x as number,
      y: data.y as number,
      label: data.label as string | undefined,
      color: isSelected ? interactive : (data.color as string),
      size: isSelected ? Math.max(size + 4, 12) : size,
      labelColor: (data.labelColor as string | undefined) ?? "rgba(255,255,255,1)",
    };
  };
}

function buildEdgeReducer() {
  return (_id: string, data: Record<string, unknown>) => ({
    color: (data.color as string | undefined) ?? "rgba(128,128,128,1)",
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

// ── Spread normalization ────────────────────────────────────────────────

const MAX_BBOX_DIAG = 200;

/** Rescale node positions so the post-FA2 bounding-box diagonal fits
 *  within `MAX_BBOX_DIAG`. Preserves relative geometry (uniform scale
 *  around the centroid). No-op for graphs with 0–1 nodes. */
function normalizeSpread(graph: Graph): void {
  if (graph.order < 2) return;
  let minX = Infinity,
    minY = Infinity,
    maxX = -Infinity,
    maxY = -Infinity;
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

    if (sigmaRef.current && idSetChanged) {
      sigmaRef.current.kill();
      sigmaRef.current = null;
      graphRef.current = null;
    }

    if (idSetChanged) {
      const rand = mulberry32(fnv1a(nodeKey));
      // multi: true — two statements can share a (from,to) pair (e.g.
      // `alice employed_by acme` and `alice likes acme`). With
      // multi: false, the second addEdgeWithKey throws and whitescreens
      // the app (no error boundary above this hook). The keyed addEdge
      // + hasEdge() dedupe already handles exact duplicates.
      const graph = new Graph({ multi: true, type: "directed" });

      const interactive = resolveToken("--color-interactive-primary");
      const text = resolveToken("--color-text");
      const edgeColor = resolveToken("--color-border-strong");
      const font = resolveFontFamily();

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

      if (graph.order >= 2) {
        forceAtlas2.assign(graph, {
          iterations: 100,
          settings: { scalingRatio: 10, gravity: 0.5, slowDown: 10 },
        });
      }
      normalizeSpread(graph);

      // Sigma container must be empty before constructing Sigma.
      container.innerHTML = "";

      const sigma = new Sigma(graph, container, {
        renderLabels: true,
        labelFont: font,
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

      // Spec §8: the focus entity is centered. Position the camera on the
      // focused node once the graph is laid out (positions are final after
      // FA2 + normalizeSpread, so this runs after construction).
      const focus = selectedRef.current;
      if (focus && graph.hasNode(focus)) {
        const pos = graph.getNodeAttributes(focus);
        if (Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
          sigma.getCamera().setState({ x: pos.x, y: pos.y, ratio: 1 });
        }
      }

      relayoutRef.current = () => {
        const g = graphRef.current;
        if (!g) return;
        if (g.order < 2) return;
        placeOnCircle(g, mulberry32(fnv1a(lastNodeKeyRef.current)));
        forceAtlas2.assign(g, {
          iterations: 100,
          settings: { scalingRatio: 10, gravity: 0.5, slowDown: 10 },
        });
        normalizeSpread(g);
        sigmaRef.current?.refresh();
      };
    } else {
      // Same id-set: nothing to do.
    }

    return () => {
      // Tear down on unmount. Clear lastNodeKeyRef so a subsequent mount
      // (e.g. React StrictMode dev re-run, or a hot module reload) sees
      // idSetChanged=true and rebuilds instead of leaving the canvas blank.
      if (sigmaRef.current) {
        sigmaRef.current.kill();
        sigmaRef.current = null;
        graphRef.current = null;
      }
      lastNodeKeyRef.current = "";
    };
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

  // Effect: theme flip — re-resolve ALL token colors fed to sigma
  // (label color, per-node fill, per-edge stroke). The `defaultEdgeColor`
  // setting is a no-op when every edge has an explicit `color` attr, so
  // we explicitly write the new color onto each edge.
  useEffect(() => {
    const observer = new MutationObserver(() => {
      const sigma = sigmaRef.current;
      const graph = graphRef.current;
      if (!sigma || !graph) return;
      const interactive = resolveToken("--color-interactive-primary");
      const text = resolveToken("--color-text");
      const edgeColor = resolveToken("--color-border-strong");
      const nodeFill = hueColor("Entity");
      for (const id of graph.nodes()) {
        graph.setNodeAttribute(id, "color", nodeFill);
        graph.setNodeAttribute(id, "labelColor", text);
      }
      for (const e of graph.edges()) {
        graph.setEdgeAttribute(e, "color", edgeColor);
      }
      sigma.setSetting("labelColor", { color: text });
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
