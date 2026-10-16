import { setWindowTheme } from "../lib/nativeShell";
import { isMacTauriShell } from "../lib/shellPlatform";
export type ThemePreference = "light" | "dark";

export const systemTheme = (): ThemePreference =>
  matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";

export function applyTheme(preference: ThemePreference) {
  if (preference === "dark") document.documentElement.dataset.theme = "dark";
  else delete document.documentElement.dataset.theme;
  if (isMacTauriShell()) setWindowTheme(preference);
}
