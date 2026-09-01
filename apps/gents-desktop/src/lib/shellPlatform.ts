export function isMacTauriShell(): boolean {
  return (
    "__TAURI_INTERNALS__" in window && navigator.platform.toUpperCase().includes("MAC")
  );
}

export function isMobileTauriShell(): boolean {
  return (
    "__TAURI_INTERNALS__" in window &&
    /Android|iPhone|iPad|iPod/i.test(navigator.userAgent)
  );
}

/// Overlay titlebar: the webview owns the top strip, so the shell needs to
/// reserve drag + traffic-light space — but only in the real macOS app, never
/// in the browser harness.
export function applyShellPlatform(root: HTMLElement = document.documentElement) {
  if (isMacTauriShell()) {
    root.dataset.shell = "mac";
  }
}
