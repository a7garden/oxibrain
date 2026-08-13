import type { ReactNode } from "react";

/// Render oxibrain's brief markdown, turning `[surface](entity://id)` links into
/// clickable navigation targets and `**bold**`/`` `code` `` into styled spans.
/// Minimal by design — the brief format is fixed (see `oxibrain-views`).
export function BriefMarkdown({
  markdown,
  onNavigate,
}: {
  markdown: string;
  onNavigate: (entityId: string) => void;
}) {
  const lines = markdown.split("\n");
  return (
    <div className="space-y-1 font-mono text-sm leading-relaxed text-text">
      {lines.map((line, i) => {
        if (line.startsWith("# ")) {
          return (
            <h1 key={i} className="pt-2 text-lg font-semibold text-text">
              {inline(line.slice(2), onNavigate)}
            </h1>
          );
        }
        if (line.startsWith("## ")) {
          return (
            <h2
              key={i}
              className="pt-4 text-xs font-semibold uppercase tracking-wider text-text-faint"
            >
              {inline(line.slice(3), onNavigate)}
            </h2>
          );
        }
        if (line.startsWith("- ")) {
          return (
            <div key={i} className="pl-2">
              <span className="mr-1 text-text-faint">·</span>
              {inline(line.slice(2), onNavigate)}
            </div>
          );
        }
        if (line.trim() === "") {
          return <div key={i} className="h-2" />;
        }
        return <div key={i}>{inline(line, onNavigate)}</div>;
      })}
    </div>
  );
}

const ENTITY_LINK = /\[([^\]]+)\]\(entity:\/\/([^)]+)\)/g;
const BOLD = /\*\*([^*]+)\*\*/g;
const CODE = /`([^`]+)`/g;

function inline(text: string, onNavigate: (entityId: string) => void): ReactNode {
  // Split on entity links first, then bold/code inside each segment.
  const segments: ReactNode[] = [];
  let last = 0;
  let key = 0;
  for (const m of text.matchAll(ENTITY_LINK)) {
    if (m.index !== undefined && m.index > last) {
      segments.push(...plain(text.slice(last, m.index), key++));
    }
    const surface = m[1];
    const entityId = m[2];
    segments.push(
      <button
        key={key++}
        onClick={() => onNavigate(entityId)}
        className="text-amber underline decoration-amber/40 underline-offset-2 hover:text-amber-bright"
        title={`Open ${entityId}`}
      >
        {surface}
      </button>,
    );
    if (m.index !== undefined) last = m.index + m[0].length;
  }
  if (last < text.length) {
    segments.push(...plain(text.slice(last), key++));
  }
  return segments;
}

function plain(text: string, base: number): ReactNode[] {
  // Render **bold** and `code` within a plain-text segment.
  const out: ReactNode[] = [];
  const parts: Array<{ kind: "text" | "bold" | "code"; value: string }> = [];
  let rest = text;
  let key = base;
  let matched = true;
  while (matched) {
    matched = false;
    for (const [re, kind] of [
      [BOLD, "bold"],
      [CODE, "code"],
    ] as const) {
      re.lastIndex = 0;
      const m = re.exec(rest);
      if (m) {
        if (m.index > 0) parts.push({ kind: "text", value: rest.slice(0, m.index) });
        parts.push({ kind, value: m[1] });
        rest = rest.slice(m.index + m[0].length);
        matched = true;
        break;
      }
    }
  }
  if (rest.length > 0) parts.push({ kind: "text", value: rest });

  for (const p of parts) {
    if (p.kind === "bold") {
      out.push(
        <strong key={key++} className="font-semibold text-text">
          {p.value}
        </strong>,
      );
    } else if (p.kind === "code") {
      out.push(
        <code key={key++} className="text-text-faint">
          {p.value}
        </code>,
      );
    } else {
      out.push(<span key={key++}>{p.value}</span>);
    }
  }
  return out;
}
