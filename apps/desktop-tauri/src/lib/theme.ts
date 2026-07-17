export type Theme = "dark" | "light";

const THEME_KEY = "defra-desktop-theme";

/// Dark is the design default; "light" is the only stored override so an
/// absent or corrupt value always resolves to a working theme.
export function loadTheme(): Theme {
  try {
    return window.localStorage.getItem(THEME_KEY) === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
  try {
    window.localStorage.setItem(THEME_KEY, theme);
  } catch {
    // Persistence is best-effort; the stamped attribute still themes the run.
  }
}
