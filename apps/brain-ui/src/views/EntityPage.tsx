import { useQuery } from "@tanstack/react-query";
import {
  Link,
  useNavigate,
  useParams,
  useSearch,
} from "@tanstack/react-router";
import { ErrorState } from "../components/ErrorState";
import { HUE_CHIP, hueForType } from "../lib/hue";
import { BriefMarkdown } from "../markdown";
import { fetchers, qk } from "../queries";

/** Entity detail page — header + Brief/Timeline tabs.
 *
 * Brief and Timeline live on independent query caches (different keys, refetch
 * intervals). Tab state is stored in `?tab=…` search param via the router's
 * validateSearch so deep-links restore the tab and Back/Forward work. */
export function EntityPage() {
  const { entityId } = useParams({ from: "/entity/$entityId" });
  const { tab } = useSearch({ from: "/entity/$entityId" });
  const navigate = useNavigate();

  const setTab = (next: "brief" | "timeline") => {
    if (next === tab) return;
    // push (not replace) so Back returns to the previous tab/entity.
    navigate({
      to: "/entity/$entityId",
      params: { entityId },
      search: { tab: next },
    });
  };

  const onNavigate = (targetId: string) => {
    navigate({
      to: "/entity/$entityId",
      params: { entityId: targetId },
      search: { tab: "brief" },
    });
  };

  // Header surface/type — look up the recent entity card by id so we have the
  // surface + type for the header badge. If not present (entity opened via a
  // direct entity:// link rather than from Overview), fall back to the entity
  // id alone — no badge, no type chip. No extra API call needed.
  const spaceQuery = useQuery({
    queryKey: qk.space,
    queryFn: fetchers.space,
  });
  const card = spaceQuery.data?.recent_entities.find((e) => e.id === entityId);

  return (
    <div className="p-8">
      <header className="mb-6">
        <div className="flex items-center gap-3">
          <h1 className="font-display text-3xl font-light tracking-tight text-text">
            {card?.surface ?? entityId}
          </h1>
          {card && (
            <span
              className={`rounded-full px-2.5 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${HUE_CHIP[hueForType(card.type)]}`}
            >
              {card.type}
            </span>
          )}
        </div>
        {card && (
          <p className="mt-1 font-mono text-xs text-text-subtle">{card.id}</p>
        )}
      </header>

      <div role="tablist" className="flex gap-1 border-b border-line">
        <TabButton
          label="Brief"
          active={tab === "brief"}
          onClick={() => setTab("brief")}
        />
        <TabButton
          label="Timeline"
          active={tab === "timeline"}
          onClick={() => setTab("timeline")}
        />
      </div>

      <div className="pt-4">
        {tab === "brief" ? (
          <BriefPanel entityId={entityId} onNavigate={onNavigate} />
        ) : (
          <TimelinePanel entityId={entityId} />
        )}
      </div>
    </div>
  );
}

function TabButton({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      role="tab"
      type="button"
      aria-selected={active}
      onClick={onClick}
      className={
        active
          ? "px-3 py-2 text-sm font-medium text-text border-b-2 border-interactive-primary -mb-px"
          : "px-3 py-2 text-sm text-text-muted hover:text-text"
      }
    >
      {label}
    </button>
  );
}

function BriefPanel({
  entityId,
  onNavigate,
}: {
  entityId: string;
  onNavigate: (entityId: string) => void;
}) {
  const query = useQuery({
    queryKey: qk.brief(entityId),
    queryFn: () => fetchers.brief(entityId),
  });

  if (query.isPending) {
    return (
      <div className="rounded-[var(--card-radius)] border border-line bg-surface-raised p-6">
        <div className="space-y-2">
          <div className="skeleton h-4 w-3/4" />
          <div className="skeleton h-4 w-full" />
          <div className="skeleton h-4 w-5/6" />
          <div className="skeleton h-4 w-2/3" />
        </div>
      </div>
    );
  }

  if (query.isError) {
    return (
      <ErrorState
        message={query.error instanceof Error ? query.error.message : String(query.error)}
        onRetry={() => query.refetch()}
      />
    );
  }

  return (
    <article className="rounded-[var(--card-radius)] border border-line bg-surface-raised p-6">
      <BriefMarkdown markdown={query.data} onNavigate={onNavigate} />
    </article>
  );
}

function TimelinePanel({ entityId }: { entityId: string }) {
  const query = useQuery({
    queryKey: qk.timeline(entityId),
    queryFn: () => fetchers.timeline(entityId),
  });

  if (query.isPending) {
    return (
      <div className="rounded-[var(--card-radius)] border border-line bg-surface-raised p-6">
        <div className="space-y-3">
          {[0, 1, 2, 3].map((i) => (
            <div key={i} className="skeleton h-10 w-full" />
          ))}
        </div>
      </div>
    );
  }

  if (query.isError) {
    return (
      <ErrorState
        message={query.error instanceof Error ? query.error.message : String(query.error)}
        onRetry={() => query.refetch()}
      />
    );
  }

  const entries = query.data;
  if (entries.length === 0) {
    return (
      <div className="rounded-[var(--card-radius)] border border-line bg-surface-raised p-6 font-mono text-sm text-text-subtle">
        No timeline entries yet.
      </div>
    );
  }

  return (
    <ul className="rounded-[var(--card-radius)] border border-line bg-surface-raised divide-y divide-line/50">
      {entries.map((entry) => (
        <li key={entry.statement_id} className="px-5 py-3">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0 flex-1">
              <p className="font-mono text-sm text-text">
                <span className="text-text-muted">{entry.predicate}</span>
                {" · "}
                {entry.object_entity ? (
                  <Link
                    to="/entity/$entityId"
                    params={{ entityId: entry.object_entity }}
                    search={{ tab: "brief" }}
                    className="text-interactive-primary hover:underline"
                  >
                    {entry.object_repr}
                  </Link>
                ) : (
                  <span>{entry.object_repr}</span>
                )}
              </p>
              <p className="mt-1 font-mono text-xs text-text-subtle">
                {formatDate(entry.valid_from)} → {formatDate(entry.valid_to)}
              </p>
            </div>
            <StatusBadge status={entry.status} />
          </div>
        </li>
      ))}
    </ul>
  );
}

function StatusBadge({ status }: { status: string }) {
  let classes: string;
  let label: string;
  if (status === "contradicted") {
    classes =
      "bg-status-error-subtle text-status-error-on-subtle";
    label = "contradicted";
  } else if (status === "active") {
    classes =
      "bg-status-success-subtle text-status-success-on-subtle";
    label = "active";
  } else {
    classes = "bg-surface-muted text-text-muted";
    label = status || "superseded";
  }
  return (
    <span
      className={`shrink-0 rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${classes}`}
    >
      {label}
    </span>
  );
}

/** Max representable JS date (8.64e15 ms). The server's TIME_MAX sentinel
 * (Number.MAX_SAFE_INTEGER) exceeds it — `new Date(TIME_MAX)` is an Invalid
 * Date and `.toISOString()` would throw and blank the whole view. */
const MAX_EPOCH_MS = 8_640_000_000_000_000;

/** Render epoch-ms as `YYYY-MM-DD`; the far-future sentinel renders as
 * `present` (an open-ended statement has no real end date to show). */
function formatDate(ms: number): string {
  if (ms >= MAX_EPOCH_MS) return "present";
  return new Date(ms).toISOString().slice(0, 10);
}
