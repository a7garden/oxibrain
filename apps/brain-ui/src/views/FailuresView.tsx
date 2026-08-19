import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ErrorState } from "../components/ErrorState";
import { fetchers, qk } from "../queries";
import type { ExtractionFailure } from "../api";

/** Parse the JSON-encoded `errors_json` blob from the server. The
 *  quarantine module stores a JSON array of validator messages; failures
 *  pre-dating that contract may store arbitrary JSON, so we tolerate any
 *  shape and fall back to the raw string. Returns the FIRST message as
 *  the row-level preview; the full payload stays in the expandable
 *  detail view. */
function firstErrorPreview(raw: string): string {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (Array.isArray(parsed) && parsed.length > 0) {
      const head = parsed[0];
      if (typeof head === "string") return head;
      if (head && typeof head === "object") return JSON.stringify(head);
    }
    if (typeof parsed === "string") return parsed;
  } catch {
    /* not JSON — fall through */
  }
  return raw;
}

/** Show full validator payload under the expandable row. Mirrors the
 *  pre-formatted `<pre>` style used by the MCP tool output: raw JSON in,
 *  raw JSON out, no client-side reformatting. */
function detailJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

export function FailuresView() {
  const query = useQuery({
    queryKey: qk.failures,
    queryFn: fetchers.failures,
    refetchInterval: 15_000,
  });

  return (
    <div className="mx-auto max-w-4xl p-8">
      <header className="mb-8">
        <h1 className="font-display text-2xl font-semibold text-text">
          Failures
        </h1>
        <p className="mt-1 font-mono text-xs text-text-subtle">
          extraction failures — claims that exhausted the repair loop
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
        <p className="text-sm text-status-success-on-subtle">
          No extraction failures recorded — every extractor succeeded.
        </p>
      ) : (
        <FailuresTable rows={query.data} />
      )}
    </div>
  );
}

function FailuresTable({ rows }: { rows: ExtractionFailure[] }) {
  // Newest first — created_at is the server's wall-clock stamp on the
  // quarantine row.
  const sorted = [...rows].sort((a, b) => b.created_at - a.created_at);
  return (
    <div className="overflow-hidden rounded-[var(--card-radius)] border border-line bg-surface-raised">
      <table className="w-full table-fixed text-left">
        <thead>
          <tr className="border-b border-line/50 bg-surface-muted/40 font-mono text-2xs uppercase tracking-wider text-text-subtle">
            <th className="w-[26%] px-4 py-2.5">Episode</th>
            <th className="w-[18%] px-4 py-2.5">Extractor</th>
            <th className="w-[14%] px-4 py-2.5">Recorded</th>
            <th className="px-4 py-2.5">First error</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-line/50">
          {sorted.map((row) => (
            <FailureRow key={row.id} row={row} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function FailureRow({ row }: { row: ExtractionFailure }) {
  // Expansion is row-local state — each row owns its own toggle so users
  // can leave one open while inspecting others.
  const [open, setOpen] = useState(false);
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
            <span className="truncate font-mono text-xs text-text" title={row.episode_id}>
              {row.episode_id}
            </span>
          </div>
        </td>
        <td className="px-4 py-3 font-mono text-xs text-text-muted">
          {row.extractor_id}
        </td>
        <td className="px-4 py-3 font-mono text-xs text-text-subtle">
          {new Date(row.created_at).toISOString().slice(0, 10)}
        </td>
        <td className="px-4 py-3 text-sm text-text">
          <span className="line-clamp-2">{firstErrorPreview(row.errors_json)}</span>
        </td>
      </tr>
      {open && (
        <tr className="bg-surface-sunken/60">
          <td colSpan={4} className="px-4 py-4">
            <div className="space-y-3">
              <DetailBlock label="errors_json" body={detailJson(row.errors_json)} />
              <DetailBlock label="raw_response" body={row.raw_response} />
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

function DetailBlock({ label, body }: { label: string; body: string }) {
  return (
    <div>
      <p className="mb-1.5 font-mono text-2xs uppercase tracking-wider text-text-subtle">
        {label}
      </p>
      <pre className="max-h-80 overflow-auto rounded-[var(--card-radius)] border border-line bg-surface px-3 py-2.5 font-mono text-xs text-text">
        {body}
      </pre>
    </div>
  );
}