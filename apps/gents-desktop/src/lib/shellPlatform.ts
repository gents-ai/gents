const inTauri = () => "__TAURI_INTERNALS__" in window;

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

/// The header is the window bar where the app draws the top of the window:
/// macOS (overlay titlebar) and Windows (no native title bar).
export function headerIsWindowBar(): boolean {
  return isMacTauriShell() || isWindowsTauriShell();
}

export function applyShellPlatform(root: HTMLElement = document.documentElement) {
  if (isMacTauriShell()) {
    root.dataset.shell = "mac";
    void trackFullscreen(root);
  } else if (isWindowsTauriShell()) {
    root.dataset.shell = "windows";
  } else if (isLinuxTauriShell()) {
    root.dataset.shell = "linux";
  }
}

/// In fullscreen macOS hides the traffic lights, so the header drops the
/// inset that clears them.
async function trackFullscreen(root: HTMLElement) {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const appWindow = getCurrentWindow();
    const sync = async () => {
      root.dataset.windowFullscreen = String(await appWindow.isFullscreen());
    };
    await sync();
    await appWindow.onResized(() => void sync());
  } catch {
    // Not a Tauri window (browser harness): keep the inset.
  }
}
