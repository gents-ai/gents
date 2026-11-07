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

export function sendAppOpened() {
  if (inNativeShell() && windowLabel() === "main") {
    void invoke("product_telemetry_app_opened").catch(() => {});
  }
}

export type ProductTelemetryStatus = { configured: boolean; enabled: boolean };

export function productTelemetryStatus(): Promise<ProductTelemetryStatus> {
  if (!inNativeShell()) return Promise.resolve({ configured: false, enabled: false });
  return invoke("product_telemetry_status");
}

export function setProductTelemetry(enabled: boolean): Promise<ProductTelemetryStatus> {
  return invoke("product_telemetry_set_enabled", { enabled });
}

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

/** null leaves the window to follow the system's appearance */
export function setWindowTheme(theme: "light" | "dark" | null) {
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

/** Sent to the main view when the system opens the app with links; it
    takes them with `takeOpenedLinks`. */
export const OPEN_LINK_EVENT = "desktop://open-link";

/** The `gents:` links the system has opened the app with since the last
    take. Only the main view takes them. */
export function takeOpenedLinks(): Promise<string[]> {
  return invoke<string[]>("desktop_take_links");
}

export function reportSwipeScrollEdges(edges: { left: boolean; right: boolean }) {
  return invoke<void>("swipe_scroll_edges", edges);
}

export function announceWindowSetupComplete() {
  return invoke<void>("desktop_window_setup_complete");
}

/** a UX plugin's page in a window of its own, or that window brought
    forward when it is already open */
export function openPluginWindow(request: {
  pluginId: string;
  nodeDid: string | null;
  title: string;
}) {
  return invoke<void>("desktop_open_plugin_window", request);
}

export const canPickDirectory = inNativeShell;

export async function confirmPluginMessage(
  plugin: string,
  content: string,
): Promise<boolean> {
  const message = `${plugin} wants to send this message to the selected chat:\n\n${content}`;
  if (!inNativeShell()) return window.confirm(message);
  const { confirm } = await import("@tauri-apps/plugin-dialog");
  return confirm(message, {
    title: `Message from ${plugin}`,
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

/** Whether the desktop app may post notifications: the plugin answers
    only granted or not, so asking again is what tells a refusal from a
    question never asked. */
export async function notificationsGranted(): Promise<boolean> {
  return (await import("@tauri-apps/plugin-notification")).isPermissionGranted();
}

export async function requestNotifications() {
  return (await import("@tauri-apps/plugin-notification")).requestPermission();
}

/** Posts a notification, settling once the system took it or failing with
    why it didn't. The plugin's sendNotification neither waits for nor
    returns the native call, so a refusal would vanish; this is the call it
    makes. */
export async function postNotification(options: { title: string; body?: string }) {
  await invoke("plugin:notification|notify", { options });
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

/* ---- the browser panel's pages, native views beneath the app's webview
   (bridge/browser.rs). A command that changes what shows carries the
   window's increasing `seq`, so one arriving late is dropped. ---- */

export type BrowserRect = { x: number; y: number; width: number; height: number };
export type BrowserPlacement = {
  bounds: BrowserRect;
  open: BrowserRect;
  solids: BrowserRect[];
  radius: number;
};

/** Loads an address, a search result or a local file into a tab, making
    the tab if it's new; rejects with why it can't, such as a missing file. */
export function browserOpen(tab: string, url: string): Promise<void> {
  return invoke("browser_open", { tab, url });
}

export function browserSelect(tab: string | null, seq: number): Promise<void> {
  return invoke("browser_select", { tab, seq });
}

export function browserPlace(placement: BrowserPlacement, seq: number): Promise<void> {
  return invoke("browser_place", { placement, seq });
}

export function browserHide(seq: number): Promise<void> {
  return invoke("browser_hide", { seq });
}

export function browserGo(
  tab: string,
  action: "back" | "forward" | "reload" | "stop",
): Promise<void> {
  return invoke("browser_go", { tab, action });
}

export function browserCloseTab(tab: string, seq: number): Promise<void> {
  return invoke("browser_close_tab", { tab, seq });
}

export function browserClose(seq: number): Promise<void> {
  return invoke("browser_close", { seq });
}

/** One file to open, picked in the system's dialog. */
export async function pickFile(title: string): Promise<string | null> {
  if (!inNativeShell()) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ multiple: false, directory: false, title });
  return typeof picked === "string" ? picked : null;
}
