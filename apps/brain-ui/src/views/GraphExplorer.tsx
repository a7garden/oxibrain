import { useState, useEffect, useRef, useCallback } from "react";
import { api, type LegacyGraphNode, type LegacyGraphEdge } from "../api";
import { BriefMarkdown } from "../markdown";

interface SimNode extends LegacyGraphNode {
  x: number;
  y: number;
  vx: number;
  vy: number;
  fx?: number;
  fy?: number;
}

const WIDTH = 800;
const HEIGHT = 600;
const REPULSION = 8000;
const SPRING = 0.04;
  const LINK_DIST = 120;
const DAMPING = 0.85;

export function GraphExplorer() {
  const [nodes, setNodes] = useState<SimNode[]>([]);
  const [edges, setEdges] = useState<LegacyGraphEdge[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [beliefs, setBeliefs] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [dragging, setDragging] = useState<string | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const nodesRef = useRef<SimNode[]>([]);
  const rafRef = useRef<number>(0);
  const draggingRef = useRef<string | null>(null);

  // Keep refs in sync
  nodesRef.current = nodes;
  draggingRef.current = dragging;

  // Initial load — space overview → recent entities → traverse
  useEffect(() => {
    loadGraph();
  }, []);

  const loadGraph = async () => {
    setLoading(true);
    setError(null);
    try {
      const overview = await api.spaceOverview("personal");
      const seeds = overview.recent_entities.slice(0, 6).map((e) => e.id);
      if (seeds.length === 0) {
        setLoading(false);
        return;
      }
      const result = await api.traverse(seeds, "personal", 2);
      // Dead view slated for deletion in Task 8. Cast the new traversal DTO
      // to the legacy shape so the rest of this file compiles unchanged.
      const nodes = result.nodes as unknown as LegacyGraphNode[];
      const edges = result.edges as unknown as LegacyGraphEdge[];
      const simNodes: SimNode[] = nodes.map((n, i) => ({
        ...n,
        x: WIDTH / 2 + Math.cos((i / nodes.length) * Math.PI * 2) * 150,
        y: HEIGHT / 2 + Math.sin((i / nodes.length) * Math.PI * 2) * 150,
        vx: 0,
        vy: 0,
      }));
      setNodes(simNodes);
      setEdges(edges);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load graph");
    }
    setLoading(false);
  };

  // Force simulation loop
  useEffect(() => {
    if (nodes.length === 0) return;

    const tick = () => {
      const ns = nodesRef.current.map((n) => ({ ...n }));

      // Repulsion
      for (let i = 0; i < ns.length; i++) {
        for (let j = i + 1; j < ns.length; j++) {
          const dx = ns[j].x - ns[i].x;
          const dy = ns[j].y - ns[i].y;
          const dist = Math.sqrt(dx * dx + dy * dy) || 1;
          const force = REPULSION / (dist * dist);
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          if (draggingRef.current !== ns[i].id) {
            ns[i].vx -= fx;
            ns[i].vy -= fy;
          }
          if (draggingRef.current !== ns[j].id) {
            ns[j].vx += fx;
            ns[j].vy += fy;
          }
        }
      }

      // Spring (edges)
      const edgeMap = new Map<string, LegacyGraphEdge[]>();
      for (const e of edges) {
        const ka = `${e.from}`;
        const kb = `${e.to}`;
        if (!edgeMap.has(ka)) edgeMap.set(ka, []);
        if (!edgeMap.has(kb)) edgeMap.set(kb, []);
        edgeMap.get(ka)!.push(e);
        edgeMap.get(kb)!.push(e);
      }

      for (const e of edges) {
        const a = ns.find((n) => n.id === e.from);
        const b = ns.find((n) => n.id === e.to);
        if (!a || !b) continue;
        const dx = b.x - a.x;
        const dy = b.y - a.y;
        const dist = Math.sqrt(dx * dx + dy * dy) || 1;
        const force = (dist - LINK_DIST) * SPRING;
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        if (draggingRef.current !== a.id) {
          a.vx += fx;
          a.vy += fy;
        }
        if (draggingRef.current !== b.id) {
          b.vx -= fx;
          b.vy -= fy;
        }
      }

      // Center gravity + integrate
      for (const n of ns) {
        if (n.id === draggingRef.current) continue;
        n.vx += (WIDTH / 2 - n.x) * 0.002;
        n.vy += (HEIGHT / 2 - n.y) * 0.002;
        n.vx *= DAMPING;
        n.vy *= DAMPING;
        n.x += n.vx;
        n.y += n.vy;
        // Bounds
        n.x = Math.max(40, Math.min(WIDTH - 40, n.x));
        n.y = Math.max(40, Math.min(HEIGHT - 40, n.y));
      }

      setNodes(ns);
      rafRef.current = requestAnimationFrame(tick);
    };

    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, [nodes.length, edges]);

  // Selection → fetch beliefs
  useEffect(() => {
    if (!selected) {
      setBeliefs(null);
      return;
    }
    api.brief(selected).then(setBeliefs).catch(() => setBeliefs(null));
  }, [selected]);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent, nodeId: string) => {
      e.stopPropagation();
      setDragging(nodeId);
      setSelected(nodeId);
    },
    [],
  );

  const handleMouseMove = useCallback((e: React.MouseEvent) => {
    if (!draggingRef.current || !svgRef.current) return;
    const rect = svgRef.current.getBoundingClientRect();
    const scaleX = WIDTH / rect.width;
    const scaleY = HEIGHT / rect.height;
    const x = (e.clientX - rect.left) * scaleX;
    const y = (e.clientY - rect.top) * scaleY;
    const ns = nodesRef.current.map((n) =>
      n.id === draggingRef.current ? { ...n, x, y, vx: 0, vy: 0 } : n,
    );
    nodesRef.current = ns;
  }, []);

  const handleMouseUp = useCallback(() => {
    setDragging(null);
  }, []);

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="animate-pulse-amber text-2xl text-amber">◐</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-full items-center justify-center">
        <p className="font-mono text-sm text-rose">{error}</p>
      </div>
    );
  }

  if (nodes.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <p className="font-display text-lg text-text-faint">
          No entities yet. Capture a note to begin.
        </p>
      </div>
    );
  }

  const nodeSet = new Set(nodes.map((n) => n.id));

  return (
    <div className="flex h-full">
      {/* Graph canvas */}
      <div className="flex-1 relative">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          className="h-full w-full cursor-grab active:cursor-grabbing"
          onMouseMove={handleMouseMove}
          onMouseUp={handleMouseUp}
          onMouseLeave={handleMouseUp}
          onClick={() => setSelected(null)}
        >
          {/* Edges */}
          {edges
            .filter((e) => nodeSet.has(e.from) && nodeSet.has(e.to))
            .map((e, i) => {
              const a = nodes.find((n) => n.id === e.from);
              const b = nodes.find((n) => n.id === e.to);
              if (!a || !b) return null;
              const isHighlighted =
                selected && (e.from === selected || e.to === selected);
              return (
                <line
                  key={i}
                  x1={a.x}
                  y1={a.y}
                  x2={b.x}
                  y2={b.y}
                  stroke={isHighlighted ? "var(--color-amber-dim)" : "var(--color-line)" }
                  strokeWidth={isHighlighted ? 1.5 : 0.8}
                  opacity={selected && !isHighlighted ? 0.3 : 0.7}
                />
              );
            })}

          {/* Nodes */}
          {nodes.map((n) => {
            const isSelected = n.id === selected;
            const isConnected = edges.some(
              (e) =>
                (e.from === selected && e.to === n.id) ||
                (e.to === selected && e.from === n.id),
            );
            const dimmed = selected && !isSelected && !isConnected;
            const r = isSelected ? 10 : 7;
            return (
              <g
                key={n.id}
                transform={`translate(${n.x},${n.y})`}
                onMouseDown={(e) => handleMouseDown(e, n.id)}
                style={{ cursor: "pointer" }}
                opacity={dimmed ? 0.3 : 1}
              >
                {/* Glow */}
                {isSelected && (
                  <circle r={r + 8} fill="var(--color-amber-glow)" />
                )}
                <circle
                  r={r}
                  fill={isSelected ? "var(--color-amber)" : "var(--color-surface-2)"}
                  stroke={
                    isSelected
                      ? "var(--color-amber)"
                      : "var(--color-line-bright)"
                  }
                  strokeWidth={1.5}
                />
                <text
                  y={r + 14}
                  textAnchor="middle"
                  className="font-mono"
                  fontSize={10}
                  fill={isSelected ? "var(--color-text)" : "var(--color-text-dim)"}
                >
                  {truncate(n.surface, 18)}
                </text>
              </g>
            );
          })}
        </svg>

        {/* Controls overlay */}
        <div className="absolute bottom-4 right-4 flex gap-2">
          <button
            onClick={loadGraph}
            className="rounded-lg border border-line bg-surface/80 px-3 py-1.5 font-mono text-xs text-text-dim backdrop-blur transition-colors hover:border-amber hover:text-amber"
          >
            refresh
          </button>
        </div>
      </div>

      {/* Detail panel */}
      {selected && beliefs && (
        <aside className="w-96 border-l border-line bg-ink-2 p-5 overflow-auto animate-fade-in">
          <BriefMarkdown markdown={beliefs} onNavigate={(id) => setSelected(id)} />
        </aside>
      )}
    </div>
  );
}
function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}
