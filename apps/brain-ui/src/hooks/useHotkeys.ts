import { useEffect, useRef } from "react";

/** Map of hotkey -> handler. The key string is normalized:
 *  - `mod` matches `metaKey` OR `ctrlKey` (covers ⌘ on macOS, Ctrl on Win/Linux)
 *  - `shift+` `alt+` `meta+` `ctrl+` modifiers are honored literally
 *  - All other parts must match `event.key` (so `"/"` is a literal slash,
 *    `"c"` is a plain c, `"Enter"` matches Enter).
 *  - Single-key (no modifier) combos are suppressed when the event target is
 *    an `<input>` / `<textarea>` / `<select>` or is contentEditable — the
 *    user is typing and we must not eat the keystroke. Modifier combos
 *    (including `mod`) always fire. */
export type HotkeyMap = Record<string, (e: KeyboardEvent) => void>;

function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

/** Parse a hotkey spec into a predicate over KeyboardEvents. */
function matchSpec(spec: string): (e: KeyboardEvent) => boolean {
  const parts = spec.toLowerCase().split("+").map((p) => p.trim());
  const key = parts[parts.length - 1];
  const mods = new Set(parts.slice(0, -1));
  return (e) => {
    if (e.key.toLowerCase() !== key) return false;
    const wantMod = mods.has("mod");
    const wantShift = mods.has("shift");
    const wantAlt = mods.has("alt");
    const wantMeta = mods.has("meta");
    const wantCtrl = mods.has("ctrl");
    // `mod` = meta or ctrl; otherwise the literal modifier must match exactly.
    const modOk = wantMod
      ? e.metaKey || e.ctrlKey
      : wantMeta
        ? e.metaKey
        : wantCtrl
          ? e.ctrlKey
          : !e.metaKey && !e.ctrlKey;
    if (!modOk) return false;
    if (wantShift !== e.shiftKey) return false;
    if (wantAlt !== e.altKey) return false;
    return true;
  };
}

/** Register a window-level keydown listener. Combos are normalized so the
 *  caller doesn't have to write `(e.metaKey || e.ctrlKey)` themselves.
 *
 *  The map is stored in a ref so callers can pass an inline object without
 *  re-binding the listener on every render. The listener is removed on
 *  unmount and when the map identity changes.
 *
 *  Modifiers must be honored via `preventDefault` to avoid browser-default
 *  behaviors (e.g. `mod+k` opening Firefox's search bar, `mod+p` printing).
 *  We only `preventDefault` when a spec actually matched — keystrokes we
 *  don't handle fall through to the page. */
export function useHotkeys(map: HotkeyMap): void {
  const mapRef = useRef(map);
  mapRef.current = map;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const editable = isEditableTarget(e.target);
      const current = mapRef.current;
      // Iterate in insertion order — callers can rely on map order to
      // express precedence. First match wins.
      for (const spec of Object.keys(current)) {
        const hasMod = spec.toLowerCase().includes("mod") ||
          spec.toLowerCase().includes("shift+") ||
          spec.toLowerCase().includes("alt+") ||
          spec.toLowerCase().includes("meta+") ||
          spec.toLowerCase().includes("ctrl+");
        if (editable && !hasMod) continue;
        if (!matchSpec(spec)(e)) continue;
        e.preventDefault();
        current[spec](e);
        return;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
