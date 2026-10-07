/* Theme is one attribute on <html>. The kit reads nothing else. The first
   run takes the OS preference; after that the choice is the person's, kept
   with their other preferences. */
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isMacTauriShell } from "../lib/shellPlatform";
export type ThemePreference = "light" | "dark";

/** The OS's preference, for a first run. */
export const systemTheme = (): ThemePreference =>
  matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";

/** Shows a theme: on the page and, on macOS, on the window's own chrome. */
export function applyTheme(preference: ThemePreference) {
  if (preference === "dark") document.documentElement.dataset.theme = "dark";
  else delete document.documentElement.dataset.theme;
  if (isMacTauriShell()) {
    void getCurrentWindow()
      .setTheme(preference)
      .catch(() => {
        // Keep the web theme usable if native appearance is unavailable.
      });
  }
}
