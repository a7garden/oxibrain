import { useState, useEffect } from "react";
import { api, type TimelineEntry, type SpaceOverview } from "../api";

export function Timeline() {
  const [entityId, setEntityId] = useState("");
  const [entries, setEntries] = useState<TimelineEntry[]>([]);
  const [recent, setRecent] = useState<SpaceOverview["recent_entities"]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.spaceOverview("personal").then((o) => {
      setRecent(o.recent_entities);
      if (o.recent_entities[0] && !entityId) {
        setEntityId(o.recent_entities[0].id);
      }
    });
  }, []);

  useEffect(() => {
    if (!entityId) return;
    setLoading(true);
    setError(null);
    api
      .timeline(entityId, "personal")
      .then(setEntries)
      .catch((e) => setError(e instanceof Error ? e.message : "Failed"))
      .finally(() => setLoading(false));
  }, [entityId]);

  // Compute date range
  const dates = entries.flatMap((e) =>
    [e.valid_from, e.valid_to].filter(Boolean) as string[],
  );
  const minDate = dates.length ? dates.reduce((a, b) => (a < b ? a : b)) : "";
  const maxDate = dates.length
    ? dates.reduce((a, b) => (a > b ? a : b))
    : "";

  const dateToPercent = (date: string | null) => {
    if (!minDate || !maxDate) return 100;
    if (!date) return 100; // open-ended → extends to "now"
    const min = new Date(minDate).getTime();
    const max = new Date(maxDate).getTime();
    const val = new Date(date).getTime();
    if (max === min) return 50;
    return ((val - min) / (max - min)) * 100;
  };

  // Group by predicate
  const predicates = [...new Set(entries.map((e) => e.predicate))];
  const palette = ["var(--color-amber)", "var(--color-sage)", "var(--color-violet)", "var(--color-rose)"];

  return (
    <div className="flex h-full flex-col p-8">
      {/* Entity selector */}
      <div className="mb-6 flex items-center gap-3">
        <input
          value={entityId}
          onChange={(e) => setEntityId(e.target.value)}
          placeholder="entity id…"
          className="w-64 rounded-lg border border-line bg-surface px-3 py-2 font-mono text-sm text-text placeholder:text-text-faint focus:border-amber focus:outline-none"
        />
        {recent.length > 0 && (
          <div className="flex gap-1.5">
            {recent.slice(0, 5).map((e) => (
              <button
                key={e.id}
                onClick={() => setEntityId(e.id)}
                className={`rounded-md border px-2 py-1 font-mono text-xs transition-colors ${
                  entityId === e.id
                    ? "border-amber text-amber"
                    : "border-line text-text-faint hover:border-line-bright hover:text-text-dim"
                }`}
              >
                {truncate(e.surface, 16)}
              </button>
            ))}
          </div>
        )}
      </div>

      {loading && (
        <div className="flex flex-1 items-center justify-center">
          <div className="animate-pulse-amber text-xl text-amber">◐</div>
        </div>
      )}

      {error && (
        <div className="font-mono text-sm text-rose">{error}</div>
      )}

      {!loading && !error && entries.length === 0 && (
        <div className="flex flex-1 items-center justify-center">
          <p className="font-display text-lg text-text-faint">
            No timeline data for this entity.
          </p>
        </div>
      )}

      {!loading && !error && entries.length > 0 && (
        <>
          {/* Time axis */}
          <div className="mb-2 ml-44 flex items-center justify-between font-mono text-xs text-text-faint">
            <span>{minDate?.slice(0, 10)}</span>
            <span>{maxDate?.slice(0, 10)}</span>
          </div>

          {/* Timeline rows */}
          <div className="flex-1 space-y-3 overflow-auto">
            {predicates.map((pred, pi) => {
              const predEntries = entries.filter((e) => e.predicate === pred);
              const color = palette[pi % palette.length];
              return (
                <div key={pred} className="flex items-center gap-4">
                  <div className="w-40 shrink-0 text-right">
                    <span className="font-mono text-xs text-text-dim">
                      {pred}
                    </span>
                  </div>
                  <div className="relative h-8 flex-1 rounded-md bg-surface/40">
                    {/* Axis line */}
                    <div className="absolute left-0 top-1/2 h-px w-full bg-line" />
                    {predEntries.map((e, ei) => {
                      const left = dateToPercent(e.valid_from);
                      const right = 100 - dateToPercent(e.valid_to);
                      return (
                        <div
                          key={ei}
                          className="group absolute top-1 h-6 rounded-md transition-all hover:h-7"
                          style={{
                            left: `${left}%`,
                            right: `${right}%`,
                            minWidth: 4,
                            backgroundColor: color,
                            opacity: 0.7 + e.confidence * 0.3,
                          }}
                          title={`${e.object_surface}\n${e.valid_from?.slice(0, 10)} → ${e.valid_to?.slice(0, 10) ?? "now"}\nconf ${(e.confidence * 100).toFixed(0)}%`}
                        >
                          <span className="absolute left-1.5 top-0.5 truncate font-mono text-[10px] text-ink">
                            {truncate(e.object_surface, 20)}
                          </span>
                        </div>
                      );
                    })}
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}
