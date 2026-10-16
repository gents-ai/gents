/* The desktop shell's native side: this window, the folder dialog, the
   opener and the shell's own commands. It is the only app module that
   imports Tauri; the bridge's commands go through the client package. */
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export const inNativeShell = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const windowLabel = () => getCurrentWindow().label;

export function minimizeWindow() {
  void getCurrentWindow().minimize();
}

export function toggleMaximizeWindow() {
  void getCurrentWindow().toggleMaximize();
}

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

export function setWindowTheme(theme: "light" | "dark") {
  void getCurrentWindow()
    .setTheme(theme)
    .catch(() => {});
}

export function listenToWindow<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  return getCurrentWindow().listen<T>(event, ({ payload }) => handler(payload));
}

export function reportSwipeScrollEdges(edges: { left: boolean; right: boolean }) {
  return invoke<void>("swipe_scroll_edges", edges);
}

export function announceWindowSetupComplete() {
  return invoke<void>("desktop_window_setup_complete");
}

export const canPickDirectory = inNativeShell;

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

export async function openWithOpener(url: string): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}
