/* The browser's tabs. Each opened tab is a native page beneath the app
   (browser.rs) that stays alive while another tab shows; the tabs outlive
   the panel, which unmounts whenever another panel tab is chosen. */
import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { inNativeShell } from "../../../lib/nativeShell";
import { keyEventOf, type HandedKey } from "./handedKeys";
import { nextSeq } from "./order";

export type Tab = {
  id: string;
  /** whether it holds a page; a new tab shows the start page */
  opened: boolean;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  /** why the last address couldn't open, such as a missing file */
  error?: string;
};

type Browser = { tabs: Tab[]; active: string };

let made = 0;
const blank = (): Tab => ({
  id: `tab-${Date.now().toString(36)}-${++made}`,
  opened: false,
  url: "",
  title: "",
  loading: false,
  canGoBack: false,
  canGoForward: false,
});

const first = blank();
let browser: Browser = { tabs: [first], active: first.id };
const listeners = new Set<() => void>();

function set(next: Browser) {
  browser = next;
  for (const notify of listeners) notify();
}

function update(id: string, change: Partial<Tab>) {
  set({
    ...browser,
    tabs: browser.tabs.map((tab) => (tab.id === id ? { ...tab, ...change } : tab)),
  });
}

const subscribe = (notify: () => void) => {
  listeners.add(notify);
  return () => {
    listeners.delete(notify);
  };
};

export const useBrowser = () => useSyncExternalStore(subscribe, () => browser);

export const activeTab = (state: Browser = browser) =>
  state.tabs.find((tab) => tab.id === state.active) ?? state.tabs[0]!;

const call = (command: string, args: Record<string, unknown>) =>
  invoke(command, { ...args, seq: nextSeq() });

/* the native side shows the active tab's page, or none for a new tab */
function showActive() {
  const tab = activeTab();
  void call("browser_select", { tab: tab.opened ? tab.id : null }).catch(() => {});
}

export function selectTab(id: string) {
  if (!browser.tabs.some((tab) => tab.id === id)) return;
  set({ ...browser, active: id });
  showActive();
}

/* the shown panel's address field */
let focusAddress: (() => void) | null = null;
export function onFocusAddress(focus: (() => void) | null) {
  focusAddress = focus;
}

export function newTab(url?: string) {
  const tab = blank();
  set({ tabs: [...browser.tabs, tab], active: tab.id });
  showActive();
  if (url) open(url);
  else focusAddress?.();
}

/** Opens an address, a search or a file in the active tab. */
export function open(url: string) {
  const id = browser.active;
  update(id, { opened: true, url, loading: true, error: undefined });
  invoke("browser_open", { tab: id, url })
    .then(showActive)
    .catch((error: unknown) =>
      update(id, { opened: false, loading: false, error: String(error) }),
    );
}

export function closeTab(id: string) {
  const at = browser.tabs.findIndex((tab) => tab.id === id);
  if (at < 0) return;
  const rest = browser.tabs.filter((tab) => tab.id !== id);
  const tabs = rest.length > 0 ? rest : [blank()];
  const active =
    browser.active === id ? tabs[Math.min(at, tabs.length - 1)]!.id : browser.active;
  set({ tabs, active });
  void call("browser_close_tab", { tab: id }).catch(() => {});
  showActive();
}

export function go(action: "back" | "forward" | "reload" | "stop") {
  void invoke("browser_go", { tab: browser.active, action }).catch(() => {});
}

/* ---- what the native side reports ---- */

const RECENTS_KEY = "gents-browser-recents";
export type Recent = { url: string; title: string };

export function readRecents(): Recent[] {
  try {
    return JSON.parse(localStorage.getItem(RECENTS_KEY) ?? "[]") as Recent[];
  } catch {
    return [];
  }
}

function remember(url: string, title: string) {
  try {
    const next = [
      { url, title: title || new URL(url).host },
      ...readRecents().filter((r) => r.url !== url),
    ].slice(0, 8);
    localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    /* recents are a convenience */
  }
}

/* A command key from the page: the browser's own (⌘L the address, ⌘T a
   new tab, ⌘R reload, ⌘[ and ⌘] back and forward), or else replayed to the
   app's shortcuts, which now have the keyboard back. */
function handOver(handed: HandedKey) {
  const init = keyEventOf(handed);
  const plain = !handed.shift && !handed.alt && !handed.ctrl;
  const own: Record<string, () => void> = {
    KeyL: () => focusAddress?.(),
    KeyT: () => newTab(),
    KeyR: () => go("reload"),
    BracketLeft: () => go("back"),
    BracketRight: () => go("forward"),
  };
  const mine = plain ? own[init.code ?? ""] : undefined;
  if (mine) return mine();
  document.body.dispatchEvent(new KeyboardEvent("keydown", init));
}

type PageState = Omit<Tab, "id" | "opened" | "error"> & { tab: string };

let following = false;
export function follow() {
  if (following || !inNativeShell()) return;
  following = true;
  void listen<PageState>("browser-state", ({ payload: { tab: id, ...page } }) => {
    const tab = browser.tabs.find((t) => t.id === id);
    if (!tab) return;
    if (tab.loading && !page.loading && page.url) remember(page.url, page.title);
    update(id, page);
  });
  void listen<{ url: string }>("browser-new-tab", ({ payload }) => newTab(payload.url));
  void listen<HandedKey>("browser-key", ({ payload }) => handOver(payload));
}

/** Drops pages a previous load of this window left behind: the native views
    outlive a reload of the app's page, which starts with one new tab. */
export function resetBrowser() {
  void call("browser_close", {}).catch(() => {});
  follow();
}
