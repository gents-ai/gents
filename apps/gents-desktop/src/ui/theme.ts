/* Theme is one attribute on <html>. The kit reads nothing else. The first
   run takes the OS preference; after that the choice is the person's. */
export type ThemePreference = "light" | "dark";

const KEY = "gents-theme";

export function themePreference(): ThemePreference {
  try {
    const stored = localStorage.getItem(KEY);
    if (stored === "light" || stored === "dark") return stored;
  } catch {
    /* storage unavailable */
  }
  return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function applyTheme(preference: ThemePreference) {
  if (preference === "dark") document.documentElement.dataset.theme = "dark";
  else delete document.documentElement.dataset.theme;
  try {
    localStorage.setItem(KEY, preference);
  } catch {
    /* storage unavailable */
  }
}

/* paint before first render */
export function initTheme() {
  applyTheme(themePreference());
}
