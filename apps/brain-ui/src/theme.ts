export type Theme = "light" | "dark";

export function applyTheme(theme: Theme): void {
  document.documentElement.classList.toggle("dark", theme === "dark");
}

export function currentTheme(): Theme {
  return document.documentElement.classList.contains("dark") ? "dark" : "light";
}

export function toggleTheme(): Theme {
  const next: Theme = currentTheme() === "dark" ? "light" : "dark";
  localStorage.setItem("oxi-theme", next);
  applyTheme(next);
  return next;
}

export function initTheme(): void {
  const saved = localStorage.getItem("oxi-theme");
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  applyTheme(saved === "dark" || (!saved && prefersDark) ? "dark" : "light");
}

export function watchSystemTheme(cb: (t: Theme) => void): () => void {
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const listener = () => {
    if (localStorage.getItem("oxi-theme") === null) {
      const t: Theme = mq.matches ? "dark" : "light";
      applyTheme(t);
      cb(t);
    }
  };
  mq.addEventListener("change", listener);
  return () => mq.removeEventListener("change", listener);
}
