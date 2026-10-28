/* The only app module that imports Tauri; the bridge's commands go through
   the client package. */
import { invoke } from "@tauri-apps/api/core";
import { homeDir } from "@tauri-apps/api/path";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { ZoomAction } from "../ui/lib/zoom";

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

export async function confirmPluginMessage(content: string): Promise<boolean> {
  const message = `Send this message to the selected chat?\n\n${content}`;
  if (!inNativeShell()) return window.confirm(message);
  const { confirm } = await import("@tauri-apps/plugin-dialog");
  return confirm(message, {
    title: "Plugin message",
    okLabel: "Send message",
    cancelLabel: "Cancel",
  });
}

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

export async function homeFolder(): Promise<string | null> {
  if (!inNativeShell()) return null;
  return homeDir();
}

export async function revealInFolder(path: string): Promise<void> {
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

export async function openWithOpener(url: string): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

let currentZoom = 1;

export function setWebviewZoom(level: number) {
  currentZoom = level;
  void getCurrentWebview()
    .setZoom(level)
    .catch((error) => console.warn("could not zoom the window", error));
}

/** Calls `handle` with each step the shell's View menu asks of this window. */
export function onZoomMenu(handle: (action: ZoomAction) => void) {
  const stop = getCurrentWebviewWindow().listen<string>("zoom", (event) => {
    if (event.payload === "in" || event.payload === "out" || event.payload === "reset")
      handle(event.payload);
  });
  return () => void stop.then((unlisten) => unlisten());
}

export async function pickFiles(): Promise<string[]> {
  if (!inNativeShell()) return [];
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({
    multiple: true,
    directory: false,
    title: "Attach files",
  });
  return typeof selected === "string" ? [selected] : (selected ?? []);
}

export async function listenFileDrop(
  handler: (event: { type: string; paths?: string[]; x: number; y: number }) => void,
): Promise<() => void> {
  if (!inNativeShell()) return () => {};
  let sequence = 0;
  return getCurrentWebview().onDragDropEvent(({ payload }) => {
    const observed = ++sequence;
    if (payload.type === "leave") {
      handler({ type: "leave", x: 0, y: 0 });
      return;
    }
    void getCurrentWindow()
      .scaleFactor()
      .then((scale) => {
        if (payload.type !== "drop" && observed !== sequence) return;
        handler({
          type: payload.type,
          paths: "paths" in payload ? payload.paths : undefined,
          x: payload.position.x / scale / currentZoom,
          y: payload.position.y / scale / currentZoom,
        });
      })
      .catch(() => {
        if (observed === sequence) handler({ type: "leave", x: 0, y: 0 });
      });
  });
}
