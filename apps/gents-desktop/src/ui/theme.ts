import { setWindowTheme } from "../lib/nativeShell";
import { isMacTauriShell } from "../lib/shellPlatform";

/** What the person chose: follow the system's appearance, or a fixed one. */
export type ThemePreference = "system" | "light" | "dark";

/** What is drawn. */
export type Theme = "light" | "dark";

export const systemTheme = (): Theme =>
  matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";

export const resolveTheme = (preference: ThemePreference): Theme =>
  preference === "system" ? systemTheme() : preference;

export function applyTheme(preference: ThemePreference) {
  if (resolveTheme(preference) === "dark")
    document.documentElement.dataset.theme = "dark";
  else delete document.documentElement.dataset.theme;
  /* a window given no theme follows the system's, and so does what the page
     reads as the system's appearance */
  if (isMacTauriShell()) setWindowTheme(preference === "system" ? null : preference);
}
