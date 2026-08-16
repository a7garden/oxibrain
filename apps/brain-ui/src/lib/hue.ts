/** Stable, deterministic hue assignment for an entity type.
 * The same type always renders with the same hue, across sessions and pages. */

export type Hue = "red" | "amber" | "green" | "teal" | "blue" | "purple";

const HUES: readonly Hue[] = ["red", "amber", "green", "teal", "blue", "purple"];

export function hueForType(type: string): Hue {
  let h = 0;
  for (let i = 0; i < type.length; i++) {
    h = (h * 31 + type.charCodeAt(i)) >>> 0;
  }
  return HUES[h % HUES.length];
}

/** Resolved CSS color string for the hue token — read live so dark-mode flips
 *  propagate without re-mounting components. */
export function hueColor(type: string): string {
  return getComputedStyle(document.documentElement)
    .getPropertyValue(`--color-hue-${hueForType(type)}`)
    .trim();
}