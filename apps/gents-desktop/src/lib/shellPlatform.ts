import { getCurrentWindow } from "@tauri-apps/api/window";

const inTauri = () => "__TAURI_INTERNALS__" in window;

// On macOS main stays alive while a runtime or sibling view still needs it.
// It is the sole automatic startup/recovery owner for the shared backend.
export function ownsAutomaticRecovery(): boolean {
  return !isMacTauriShell() || getCurrentWindow().label === "main";
}

/// iPadOS also reports MacIntel, so a touch screen rules macOS out.
export function isMacTauriShell(): boolean {
  return (
    inTauri() &&
    navigator.platform.toUpperCase().includes("MAC") &&
    navigator.maxTouchPoints === 0
  );
}

export function isWindowsTauriShell(): boolean {
  return inTauri() && navigator.platform.toUpperCase().startsWith("WIN");
}

export function isLinuxTauriShell(): boolean {
  return (
    inTauri() &&
    navigator.platform.toUpperCase().includes("LINUX") &&
    !isMobileTauriShell()
  );
}

export function isMobileTauriShell(): boolean {
  return inTauri() && /Android|iPhone|iPad|iPod/i.test(navigator.userAgent);
}

/// Only Windows uses a web-rendered window bar. macOS reserves native title
/// and tab chrome outside the webview; Linux keeps its window-manager chrome.
export function headerIsWindowBar(): boolean {
  return isWindowsTauriShell();
}

export function applyShellPlatform(root: HTMLElement = document.documentElement) {
  if (isMacTauriShell()) {
    root.dataset.shell = "mac";
  } else if (isWindowsTauriShell()) {
    root.dataset.shell = "windows";
  } else if (isLinuxTauriShell()) {
    root.dataset.shell = "linux";
  }
}
