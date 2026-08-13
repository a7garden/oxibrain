import { useCallback, useEffect, useState } from "react";
import { api, type SpaceOverview } from "../api";
import { BriefMarkdown } from "../markdown";

/// Render an entity page (brief) and let the user follow `entity://` links —
/// the agent-native navigation surface (§14.1).
export function Brief() {
  const [entityId, setEntityId] = useState("");
  const [markdown, setMarkdown] = useState<string | null>(null);
  const [recent, setRecent] = useState<SpaceOverview["recent_entities"]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .spaceOverview("personal")
      .then((o) => setRecent(o.recent_entities ?? []))
      .catch(() => {});
  }, []);

  const open = useCallback((id: string) => {
    setEntityId(id);
    setLoading(true);
    setError(null);
    api
      .brief(id, "personal")
      .then(setMarkdown)
      .catch((e) => setError(e instanceof Error ? e.message : "Failed"))
      .finally(() => setLoading(false));
  }, []);

  const handleNavigate = useCallback(
    (id: string) => open(id),
    [open],
  );

  return (
    <div className="flex h-full flex-col p-8">
      <div className="mb-6 flex gap-2">
        <div className="relative flex-1">
          <span className="pointer-events-none absolute left-4 top-1/2 -translate-y-1/2 text-lg text-text-faint">
            ▤
          </span>
          <input
            value={entityId}
            onChange={(e) => setEntityId(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && entityId && open(entityId)}
            placeholder="Entity id…"
            className="w-full rounded-xl border border-line bg-surface py-3 pl-12 pr-4 font-mono text-sm text-text placeholder:text-text-faint focus:border-amber focus:outline-none"
          />
        </div>
        <button
          onClick={() => entityId && open(entityId)}
          disabled={!entityId}
          className="rounded-xl bg-amber px-5 text-sm font-medium text-ink hover:bg-amber-bright disabled:opacity-40"
        >
          Open
        </button>
      </div>

      {recent.length > 0 && !markdown && (
        <div className="mb-6 flex flex-wrap gap-2">
          {recent.map((r) => (
            <button
              key={r.id}
              onClick={() => open(r.id)}
              className="rounded-full border border-line bg-surface px-3 py-1 font-mono text-xs text-text-faint hover:border-amber hover:text-text"
            >
              {r.surface}
            </button>
          ))}
        </div>
      )}

      {loading && <div className="text-sm text-text-faint">Loading…</div>}
      {error && <div className="text-sm text-rose">Error: {error}</div>}

      {markdown && (
        <div className="flex-1 overflow-y-auto rounded-xl border border-line bg-surface p-6">
          <BriefMarkdown markdown={markdown} onNavigate={handleNavigate} />
        </div>
      )}
    </div>
  );
}
