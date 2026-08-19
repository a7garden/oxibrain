import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ErrorState } from "../components/ErrorState";
import { fetchers, qk } from "../queries";
import type { SourceRow } from "../api";

/** Source kinds the registry accepts (DESIGN §7.1). The DB column is
 *  unconstrained so unknown kinds land here as `other` — display the
 *  raw value rather than masking it. */
type SourceKind = "note" | "chat" | "import" | "capture" | "derived" | "other";
type SourceMode = "live" | "snapshot" | "shadow" | "other";

function kindTone(kind: string): { label: string; classes: string } {
  switch (kind as SourceKind) {
    case "note":
      return {
        label: "note",
        classes: "bg-hue-blue-subtle text-hue-blue-on-subtle",
      };
    case "chat":
      return {
        label: "chat",
        classes: "bg-hue-amber-subtle text-hue-amber-on-subtle",
      };
    case "import":
      return {
        label: "import",
        classes: "bg-hue-violet-subtle text-hue-violet-on-subtle",
      };
    case "capture":
      return {
        label: "capture",
        classes: "bg-hue-green-subtle text-hue-green-on-subtle",
      };
    case "derived":
      return {
        label: "derived",
        classes: "bg-surface-muted text-text-muted",
      };
    default:
      return {
        label: kind,
        classes: "bg-surface-muted text-text-muted",
      };
  }
}

function modeTone(mode: string): { label: string; classes: string } {
  switch (mode as SourceMode) {
    case "live":
      return {
        label: "live",
        classes: "bg-status-success-subtle text-status-success-on-subtle",
      };
    case "snapshot":
      return {
        label: "snapshot",
        classes: "bg-surface-muted text-text-muted",
      };
    case "shadow":
      return {
        label: "shadow",
        classes: "bg-status-warning-subtle text-status-warning-on-subtle",
      };
    default:
      return {
        label: mode,
        classes: "bg-surface-muted text-text-muted",
      };
  }
}

/** Pretty-print the `claims_json` blob. It's a JSON object of policy
 *  key/value pairs (DESIGN §7.1) — show verbatim under the expandable
 *  row so the user sees exactly what the registry stored. */
function policyPreview(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

export function SourcesView() {
  const query = useQuery({
    queryKey: qk.sources,
    queryFn: fetchers.sources,
    refetchInterval: 30_000,
  });

  return (
    <div className="mx-auto max-w-4xl p-8">
      <header className="mb-8">
        <h1 className="font-display text-2xl font-semibold text-text">
          Sources
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          registered ingestion sources and their policies
        </p>
      </header>

      {query.isPending ? (
        <div className="space-y-3">
          <div className="skeleton h-10 w-full" />
          <div className="skeleton h-10 w-full" />
          <div className="skeleton h-10 w-full" />
        </div>
      ) : query.isError ? (
        <ErrorState
          message={
            query.error instanceof Error
              ? query.error.message
              : String(query.error)
          }
          onRetry={() => query.refetch()}
        />
      ) : query.data.length === 0 ? (
        <p className="text-sm text-text-subtle">
          No sources registered yet — ingest something to populate the
          registry.
        </p>
      ) : (
        <SourcesTable rows={query.data} />
      )}
    </div>
  );
}

function SourcesTable({ rows }: { rows: SourceRow[] }) {
  // Alphabetical by name (server already sorts that way, but re-sort
  // client-side so a future server change doesn't reshuffle the UI).
  const sorted = [...rows].sort((a, b) => a.name.localeCompare(b.name));
  return (
    <div className="overflow-hidden rounded-[var(--card-radius)] border border-line bg-surface-raised">
      <table className="w-full table-fixed text-left">
        <thead>
          <tr className="border-b border-line/50 bg-surface-muted/40 font-mono text-2xs uppercase tracking-wider text-text-subtle">
            <th className="w-[32%] px-4 py-2.5">Name</th>
            <th className="w-[16%] px-4 py-2.5">Kind</th>
            <th className="w-[14%] px-4 py-2.5">Mode</th>
            <th className="w-[14%] px-4 py-2.5">Created</th>
            <th className="px-4 py-2.5">Policy</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-line/50">
          {sorted.map((row) => (
            <SourceRowItem key={row.id} row={row} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SourceRowItem({ row }: { row: SourceRow }) {
  // Row-local expansion: lets users compare policies side-by-side without
  // a second query round-trip.
  const [open, setOpen] = useState(false);
  const kind = kindTone(row.kind);
  const mode = modeTone(row.mode);
  return (
    <>
      <tr
        className="cursor-pointer text-sm transition-colors hover:bg-surface-muted/40"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <td className="px-4 py-3">
          <div className="flex items-center gap-2">
            <span
              className="font-mono text-xs text-text-subtle"
              aria-hidden
            >
              {open ? "▾" : "▸"}
            </span>
            <span className="truncate text-text" title={row.name}>
              {row.name}
            </span>
          </div>
        </td>
        <td className="px-4 py-3">
          <span
            className={`inline-block rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${kind.classes}`}
          >
            {kind.label}
          </span>
        </td>
        <td className="px-4 py-3">
          <span
            className={`inline-block rounded-full px-2 py-0.5 font-mono text-2xs font-medium tracking-wider uppercase ${mode.classes}`}
          >
            {mode.label}
          </span>
        </td>
        <td className="px-4 py-3 font-mono text-xs text-text-subtle">
          {new Date(row.created_at).toISOString().slice(0, 10)}
        </td>
        <td className="px-4 py-3">
          <span className="line-clamp-1 font-mono text-xs text-text-subtle">
            {policyPreview(row.claims_json).split("\n")[0] || "—"}
          </span>
        </td>
      </tr>
      {open && (
        <tr className="bg-surface-sunken/60">
          <td colSpan={5} className="px-4 py-4">
            <div>
              <p className="mb-1.5 font-mono text-2xs uppercase tracking-wider text-text-subtle">
                claims_json
              </p>
              <pre className="max-h-72 overflow-auto rounded-[var(--card-radius)] border border-line bg-surface px-3 py-2.5 font-mono text-xs text-text">
                {policyPreview(row.claims_json)}
              </pre>
            </div>
          </td>
        </tr>
      )}
    </>
  );
}