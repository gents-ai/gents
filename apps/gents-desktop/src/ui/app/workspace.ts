/* The workspace: where things sit on screen. Three regions — the gents
   chrome on the left (rail and nav panel), the main pane, and a dock on
   the right — and one store for their state, so no screen owns layout.
   The dock holds the surfaces a person opened, as tabs, one of them
   showing; nothing is there until asked for. Which tabs it holds outlives
   the visit; that it was open does not. The dock's width is the resize
   hook's, in the shell. */
import { useSyncExternalStore } from "react";
import { navPreference, saveNavPreference, type NavMode } from "@/nav";

export type DockState = {
  open: boolean;
  /** registered surface ids, in the order they were opened */
  tabs: string[];
  /** the tab showing; null when none */
  active: string | null;
};

export type Workspace = {
  nav: NavMode;
  dock: DockState;
};

const DOCK_KEY = "gents-prototype-dock";
const EMPTY: DockState = { open: false, tabs: [], active: null };

function loadDock(): DockState {
  try {
    const raw = localStorage.getItem(DOCK_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<DockState> & {
        surface?: string | null;
      };
      const tabs = Array.isArray(parsed.tabs)
        ? parsed.tabs.filter((t): t is string => typeof t === "string")
        : typeof parsed.surface === "string"
          ? [parsed.surface]
          : [];
      const active =
        typeof parsed.active === "string" && tabs.includes(parsed.active)
          ? parsed.active
          : (tabs[0] ?? null);
      /* the tabs are remembered, not that the dock was open: it opens when asked */
      return { open: false, tabs, active };
    }
  } catch {
    /* storage unavailable */
  }
  return EMPTY;
}

function saveDock(dock: DockState) {
  try {
    localStorage.setItem(DOCK_KEY, JSON.stringify(dock));
  } catch {
    /* storage unavailable */
  }
}

let state: Workspace = { nav: navPreference(), dock: loadDock() };
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());
const set = (next: Workspace) => {
  state = next;
  saveDock(next.dock);
  emit();
};
const setDock = (dock: DockState) => set({ ...state, dock });

export const workspace = {
  get: () => state,
  subscribe: (l: () => void) => {
    listeners.add(l);
    return () => listeners.delete(l);
  },
  setNav(nav: NavMode) {
    saveNavPreference(nav);
    set({ ...state, nav });
  },
  /** show a surface: added as a tab if it is not one, made the one showing */
  openSurface(id: string) {
    const { tabs } = state.dock;
    setDock({ open: true, tabs: tabs.includes(id) ? tabs : [...tabs, id], active: id });
  },
  /** bring an open tab forward */
  activate(id: string) {
    if (!state.dock.tabs.includes(id)) return;
    setDock({ ...state.dock, open: true, active: id });
  },
  /** take a tab away; the neighbour shows, or the dock closes with the last one */
  closeTab(id: string) {
    const { tabs, active } = state.dock;
    const i = tabs.indexOf(id);
    if (i < 0) return;
    const next = tabs.filter((t) => t !== id);
    const nextActive =
      active === id ? (next[Math.min(i, next.length - 1)] ?? null) : active;
    setDock({
      open: next.length > 0 && state.dock.open,
      tabs: next,
      active: nextActive,
    });
  },
  /** put a tab at another position; the tabs keep their order otherwise */
  moveTab(id: string, to: number) {
    const { tabs } = state.dock;
    const from = tabs.indexOf(id);
    if (from < 0) return;
    const at = Math.max(0, Math.min(tabs.length - 1, to));
    if (at === from) return;
    const next = tabs.filter((t) => t !== id);
    next.splice(at, 0, id);
    setDock({ ...state.dock, tabs: next });
  },
  closeDock() {
    setDock({ ...state.dock, open: false });
  },
  /** the dock as it was, with the tabs it had */
  reopenDock() {
    if (state.dock.tabs.length > 0) setDock({ ...state.dock, open: true });
  },
  /** tests only */
  reset() {
    set({ nav: navPreference(), dock: EMPTY });
  },
};

export function useWorkspace(): Workspace {
  return useSyncExternalStore(workspace.subscribe, workspace.get, workspace.get);
}
