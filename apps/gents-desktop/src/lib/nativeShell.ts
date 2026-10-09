/* The desktop shell's native side: this window, the folder dialog, the
   opener and the shell's own commands. It is the only app module that
   imports Tauri; the bridge's commands go through the client package. */
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Whether the app runs in the desktop shell rather than a plain browser. */
export const inNativeShell = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** This window's label; the shell's first window is "main". */
export const windowLabel = () => getCurrentWindow().label;

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

export async function showAndFocusWindow() {
  const appWindow = getCurrentWindow();
  await appWindow.show();
  await appWindow.setFocus();
}

export function setWindowTitle(title: string) {
  void getCurrentWindow().setTitle(title);
}

/** The window's own chrome in the page's theme; where the OS cannot, the
    page's theme still holds. */
export function setWindowTheme(theme: "light" | "dark") {
  void getCurrentWindow()
    .setTheme(theme)
    .catch(() => {});
}

/** Hears an event the shell sends this window; resolves to stop. */
export function listenToWindow<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  return getCurrentWindow().listen<T>(event, ({ payload }) => handler(payload));
}

/** Tells the shell whether the content under the pointer can still scroll
    sideways, so a history swipe arms only at its edge. */
export function reportSwipeScrollEdges(edges: { left: boolean; right: boolean }) {
  return invoke<void>("swipe_scroll_edges", edges);
}

/** Tells the shell this window finished setup; the shell latches it. */
export function announceWindowSetupComplete() {
  return invoke<void>("desktop_window_setup_complete");
}

/* whether a folder picker exists here at all: the button is shown only then */
export const canPickDirectory = inNativeShell;

/** The OS folder picker; null without the shell, where the caller falls back
    to the typed path. The import is a literal so Vite bundles the plugin:
    the packaged webview cannot resolve a bare module specifier at runtime. */
export async function pickDirectory(options: {
  defaultPath?: string | null;
  title?: string;
}): Promise<string | null> {
  if (!canPickDirectory()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    defaultPath: options.defaultPath ?? undefined,
    title: options.title,
  });
  return typeof picked === "string" ? picked : null;
}

export async function revealInFolder(path: string): Promise<void> {
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

/** Opens a URL with the OS's opener, which leaves the shell's environment
    to the browser it starts. */
export async function openWithOpener(url: string): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}
