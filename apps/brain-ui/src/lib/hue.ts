/** Stable, deterministic hue assignment for an entity type.
 *  The same type always renders with the same hue, across sessions and pages. */

export type Hue = "red" | "amber" | "green" | "teal" | "blue" | "purple";

const HUES: readonly Hue[] = ["red", "amber", "green", "teal", "blue", "purple"];

export function hueForType(type: string): Hue {
  let h = 0;
  for (let i = 0; i < type.length; i++) {
    h = (h * 31 + type.charCodeAt(i)) >>> 0;
  }
  return HUES[h % HUES.length];
}

/** Literal class map — Tailwind's scanner needs every literal class name in
 *  source to emit the corresponding utility. Dynamic template literals are
 *  not scanned. Tasks 7–11 reuse this for entity-page chips, graph nodes,
 *  conflict cards, etc. */
export const HUE_DOT: Record<Hue, string> = {
  red: "bg-hue-red",
  amber: "bg-hue-amber",
  green: "bg-hue-green",
  teal: "bg-hue-teal",
  blue: "bg-hue-blue",
  purple: "bg-hue-purple",
};

/** Filled-chip background + matching text — literal class map (Tailwind
 *  scanner can't infer `bg-hue-${name}/10`). Use for type badges, status
 *  chips, etc. */
export const HUE_CHIP: Record<Hue, string> = {
  red: "bg-hue-red/10 text-hue-red",
  amber: "bg-hue-amber/10 text-hue-amber",
  green: "bg-hue-green/10 text-hue-green",
  teal: "bg-hue-teal/10 text-hue-teal",
  blue: "bg-hue-blue/10 text-hue-blue",
  purple: "bg-hue-purple/10 text-hue-purple",
};

/** Convert any modern CSS color string (oklch, lab, color(), etc.) to a
 *  hex string via a 1x1 canvas readback. Sigma's `parseColor` only
 *  understands hex / rgb() / rgba() / named — `oklch()` resolves to
 *  opaque black otherwise. The canvas is created lazily and reused. */
let _parseCanvas: HTMLCanvasElement | null = null;
function toRgba(cssColor: string): string {
  if (typeof document === "undefined") return "#000000";
  if (!_parseCanvas) {
    _parseCanvas = document.createElement("canvas");
    _parseCanvas.width = 1;
    _parseCanvas.height = 1;
  }
  const ctx = _parseCanvas.getContext("2d");
  if (!ctx) return "#000000";
  // Some browsers reject invalid colors silently and leave the prior
  ctx.clearRect(0, 0, 1, 1);
  ctx.fillStyle = "rgba(0,0,0,0)";
  ctx.fillStyle = cssColor;
  ctx.fillRect(0, 0, 1, 1);
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
  return `rgba(${r}, ${g}, ${b}, ${a / 255})`;
}

/** Resolved CSS color string for the hue token — read live so dark-mode
 *  flips propagate without re-mounting components. Returned as `rgba()`
 *  so canvas consumers (sigma, graphology) accept it. */
export function hueColor(type: string): string {
  return toRgba(
    getComputedStyle(document.documentElement)
      .getPropertyValue(`--color-hue-${hueForType(type)}`)
      .trim(),
  );
}