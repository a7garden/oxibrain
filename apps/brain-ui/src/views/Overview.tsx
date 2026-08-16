import { Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { ErrorState } from "../components/ErrorState";
import { HUE_DOT, hueForType } from "../lib/hue";
import { fetchers, qk } from "../queries";

/** Landing page: stat cards + recent-entity chips + conflicts banner. */
export function Overview() {
  const { data, isPending, isError, error, refetch } = useQuery({
    queryKey: qk.space,
    queryFn: fetchers.space,
    refetchInterval: 30_000,
  });

  if (isPending) {
    return (
      <div className="p-8">
        <div className="skeleton h-8 w-48" />
        <div className="mt-6 grid grid-cols-3 gap-4">
          <div className="skeleton h-24" />
          <div className="skeleton h-24" />
          <div className="skeleton h-24" />
        </div>
        <div className="mt-8 grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4">
          {Array.from({ length: 8 }).map((_, i) => (
            <div key={i} className="skeleton h-10" />
          ))}
        </div>
      </div>
    );
  }

  if (isError || !data) {
    return (
      <ErrorState
        message={error instanceof Error ? error.message : "Failed to load space"}
        onRetry={() => refetch()}
      />
    );
  }

  const { entity_count, episode_count, contradiction_count, recent_entities } =
    data;

  return (
    <div className="p-8">
      <header className="mb-8">
        <h1 className="font-display text-3xl font-light tracking-tight text-text">
          Overview
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          space · {data.space}
        </p>
      </header>

      {/* Stat cards */}
      <section className="grid grid-cols-1 gap-4 sm:grid-cols-3">
        <StatCard label="Entities" value={entity_count} />
        <StatCard label="Episodes" value={episode_count} />
        <StatCard
          label="Conflicts"
          value={contradiction_count}
          tone={contradiction_count > 0 ? "error" : "neutral"}
        />
      </section>

      {/* Conflicts banner */}
      {contradiction_count > 0 && (
        <Link
          to="/conflicts"
          className="bg-status-error-subtle text-status-error-on-subtle mt-6 flex items-center justify-between gap-3 rounded-[var(--card-radius)] border border-line px-5 py-4 transition-colors hover:bg-status-error-subtle/80"
        >
          <span className="font-display text-sm font-medium">
            {contradiction_count} contradiction
            {contradiction_count === 1 ? "" : "s"} need review
          </span>
          <span className="font-mono text-xs">open conflicts →</span>
        </Link>
      )}

      {/* Recent entities */}
      <section className="mt-10">
        <h2 className="font-display text-sm font-medium tracking-wider uppercase text-text-subtle">
          Recent entities
        </h2>
        {recent_entities.length === 0 ? (
          <p className="mt-3 font-mono text-sm text-text-subtle">
            No entities yet. Capture a note to begin.
          </p>
        ) : (
          <ul className="mt-4 flex flex-wrap gap-2">
            {recent_entities.map((e) => (
              <li key={e.id}>
                <Link
                  to="/entity/$entityId"
                  params={{ entityId: e.id }}
                  search={{ tab: "brief" }}
                  className="bg-surface-muted hover:bg-surface-sunken flex items-center gap-2 rounded-full px-3 py-1.5 font-mono text-xs text-text transition-colors"
                >
                  <span
                    aria-hidden
                    className={`h-1.5 w-1.5 rounded-full ${HUE_DOT[hueForType(e.type)]}`}
                  />
                  <span>{e.surface}</span>
                  <span className="text-text-subtle">· {e.type}</span>
                </Link>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}

function StatCard({
  label,
  value,
  tone,
}: {
  label: string;
  value: number;
  tone?: "neutral" | "error";
}) {
  return (
    <div className="rounded-[var(--card-radius)] border border-line bg-surface-raised p-5">
      <p className="font-mono text-xs font-medium tracking-wider uppercase text-text-subtle">
        {label}
      </p>
      <p
        className={
          "mt-2 font-display text-3xl font-light " +
          (tone === "error" ? "text-status-error" : "text-text")
        }
      >
        {value}
      </p>
    </div>
  );
}