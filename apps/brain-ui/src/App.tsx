import { useState, useEffect, type ReactNode } from "react";
import { api, type SpaceOverview } from "./api";
import { GraphExplorer } from "./views/GraphExplorer";
import { Timeline } from "./views/Timeline";
import { AskProvenance } from "./views/AskProvenance";
import { ContradictionInbox } from "./views/ContradictionInbox";
import { QuickCapture } from "./views/QuickCapture";

type ViewId = "graph" | "timeline" | "ask" | "contradictions" | "capture";

const NAV_ITEMS: Array<{
  id: ViewId;
  label: string;
  icon: string;
  desc: string;
}> = [
  { id: "graph", label: "Graph", icon: "✦", desc: "Constellation of entities" },
  { id: "timeline", label: "Timeline", icon: "◐", desc: "Beliefs over time" },
  { id: "ask", label: "Ask", icon: "⌕", desc: "Query with provenance" },
  { id: "contradictions", label: "Conflicts", icon: "⚡", desc: "Contradictions inbox" },
  { id: "capture", label: "Capture", icon: "✎", desc: "Quick note" },
];

export default function App() {
  const [view, setView] = useState<ViewId>("graph");
  const [connected, setConnected] = useState(false);
  const [overview, setOverview] = useState<SpaceOverview | null>(null);

  // Poll connection + overview
  useEffect(() => {
    let active = true;
    const check = async () => {
      try {
        const o = await api.spaceOverview("personal");
        if (active) {
          setConnected(true);
          setOverview(o);
        }
      } catch {
        if (active) setConnected(false);
      }
    };
    check();
    const timer = setInterval(check, 5000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  const currentNav = NAV_ITEMS.find((n) => n.id === view);

  return (
    <div className="flex h-full font-sans">
      {/* Sidebar */}
      <aside className="flex w-56 flex-col border-r border-line bg-ink-2">
        <div className="flex items-center gap-2.5 px-5 py-5">
          <span className="text-xl text-amber">◐</span>
          <h1 className="font-display text-lg font-semibold tracking-tight text-text">
            oxibrain
          </h1>
        </div>

        <nav className="flex flex-col gap-0.5 px-3">
          {NAV_ITEMS.map((item) => (
            <button
              key={item.id}
              onClick={() => setView(item.id)}
              className={`group flex items-center gap-3 rounded-lg px-3 py-2 text-left transition-colors ${
                view === item.id
                  ? "bg-surface text-amber"
                  : "text-text-dim hover:bg-surface/50 hover:text-text"
              }`}
            >
              <span className="w-5 text-center text-sm">{item.icon}</span>
              <span className="text-sm font-medium">{item.label}</span>
            </button>
          ))}
        </nav>

        <div className="mt-auto px-5 py-4">
          {overview ? (
            <div className="space-y-1 font-mono text-xs text-text-faint">
              <div>{overview.entity_count} entities</div>
              <div>{overview.episode_count} episodes</div>
              {overview.contradictions > 0 && (
                <div className="text-rose">{overview.contradictions} conflicts</div>
              )}
            </div>
          ) : (
            <div className="font-mono text-xs text-text-faint">—</div>
          )}
          <div className="mt-3 flex items-center gap-2">
            <span
              className={`h-1.5 w-1.5 rounded-full ${
                connected
                  ? "bg-sage animate-pulse-amber"
                  : "bg-rose"
              }`}
            />
            <span className="font-mono text-xs text-text-faint">
              {connected ? "connected" : "offline"}
            </span>
          </div>
        </div>
      </aside>

      {/* Main content */}
      <main className="flex flex-1 flex-col overflow-hidden">
        {/* Header */}
        <header className="flex items-center justify-between border-b border-line px-8 py-4">
          <div>
            <h2 className="font-display text-2xl font-light tracking-tight text-text">
              {currentNav?.label}
            </h2>
            <p className="font-mono text-xs text-text-faint">{currentNav?.desc}</p>
          </div>
        </header>

        {/* View */}
        <div className="flex-1 overflow-auto">
          {connected ? (
            <div key={view} className="animate-fade-in h-full">
              {renderView(view)}
            </div>
          ) : (
            <DisconnectedView />
          )}
        </div>
      </main>
    </div>
  );
}

function renderView(view: ViewId): ReactNode {
  switch (view) {
    case "graph": return <GraphExplorer />;
    case "timeline": return <Timeline />;
    case "ask": return <AskProvenance />;
    case "contradictions": return <ContradictionInbox />;
    case "capture": return <QuickCapture />;
  }
}

function DisconnectedView() {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4">
      <div className="text-5xl text-text-faint opacity-30">◐</div>
      <div className="text-center">
        <p className="font-display text-xl text-text-dim">No brain found</p>
        <p className="mt-2 font-mono text-sm text-text-faint">
          Start the daemon:
        </p>
        <code className="mt-2 block rounded-lg bg-surface px-4 py-2 font-mono text-xs text-amber">
          oxibrain serve --http 127.0.0.1:18080
        </code>
      </div>
    </div>
  );
}
