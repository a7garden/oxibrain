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

/** Resolved CSS color string for the hue token — read live so dark-mode flips
 *  propagate without re-mounting components. */
export function hueColor(type: string): string {
  return getComputedStyle(document.documentElement)
    .getPropertyValue(`--color-hue-${hueForType(type)}`)
    .trim();
}