import { getCurrentWindow } from "@tauri-apps/api/window";

export function minimizeWindow() {
  void getCurrentWindow().minimize();
}

export function toggleMaximizeWindow() {
  void getCurrentWindow().toggleMaximize();
}

/// Goes through CloseRequested, so with the tray active it hides the window
/// like the native close button (bridge/mod.rs).
export function closeWindow() {
  void getCurrentWindow().close();
}

export function onMaximizedChange(handler: (maximized: boolean) => void): () => void {
  const appWindow = getCurrentWindow();
  let unlisten: (() => void) | undefined;
  let stopped = false;
  const sync = async () => handler(await appWindow.isMaximized());
  void sync();
  void appWindow
    .onResized(() => void sync())
    .then((stop) => (stopped ? stop() : (unlisten = stop)));
  return () => {
    stopped = true;
    unlisten?.();
  };
}
