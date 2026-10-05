/* The workspace: where things sit on screen. Three regions — the gents
   chrome on the left (rail and nav panel), the main pane, and a dock on
   the right. The dock holds the surfaces a person opened, as tabs, one of
   them showing; nothing is there until asked for. Each session keeps its
   own dock, since what a session's dock shows can belong to that session;
   the screens outside a session share one, and the new-session screen has
   its own. Whose dock a screen shows follows from its route during render,
   and every change names the dock it changes, so nothing acts on another
   screen's dock. Which tabs a dock holds outlives the visit and the app;
   that it was open lasts the run. The dock's width is the resize hook's,
   in the shell. */
import { useMemo } from "react";
import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { useRoute } from "@/lib/router";
import type { Route } from "@gents/shell";

export type DockState = {
  open: boolean;
  /** registered surface ids, in the order they were opened */
  tabs: string[];
  /** the tab showing; null when none */
  active: string | null;
};

/** Whose dock a screen shows: a session's own, the new-session screen's, or
    the one the other screens share. */
export function dockScope(route: Route): string {
  return dockScopeOf(route.name, route.name === "session" ? route.sessionId : null);
}

/** The same, from a route's name and its session. */
export function dockScopeOf(routeName: string, sessionId: string | null): string {
  if (routeName !== "session") return APP_SCOPE;
  return sessionId ? `session:${sessionId}` : NEW_SESSION_SCOPE;
}

const APP_SCOPE = "app";
const NEW_SESSION_SCOPE = "session:new";
/* the sessions whose docks are remembered, most recent last */
const REMEMBERED_SCOPES = 200;
const EMPTY: DockState = { open: false, tabs: [], active: null };

type WorkspaceState = {
  docks: Record<string, DockState>;
  /** scopes by last visit, most recent last; bounds what is remembered */
  visited: string[];
};

/* the dock works without storage: unavailable or refused, it lasts the run */
const browserStorage = {
  getItem(key: string) {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  setItem(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* storage unavailable */
    }
  },
  removeItem(key: string) {
    try {
      localStorage.removeItem(key);
    } catch {
      /* storage unavailable */
    }
  },
};

/* the single dock the app kept before docks were per session */
browserStorage.removeItem("gents-prototype-dock");

const store = createStore<WorkspaceState>()(
  persist((): WorkspaceState => ({ docks: {}, visited: [] }), {
    name: "gents-dock-by-scope",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    partialize: ({ docks, visited }) => ({
      /* the tabs are remembered, not that the dock was open: it opens when asked */
      docks: Object.fromEntries(
        Object.entries(docks).map(([scope, dock]) => [scope, { ...dock, open: false }]),
      ),
      visited,
    }),
  }),
);

const dockIn = (state: WorkspaceState, scope: string) => state.docks[scope] ?? EMPTY;

function change(scope: string, next: (dock: DockState) => DockState | null) {
  store.setState((state) => {
    const dock = dockIn(state, scope);
    const updated = next(dock);
    return updated === null || updated === dock
      ? state
      : { docks: { ...state.docks, [scope]: updated } };
  });
}

export const workspace = {
  /** the dock of `scope` now; for handlers and tests */
  dock: (scope: string) => dockIn(store.getState(), scope),
  /** a screen was shown: its dock is remembered longest */
  visit(scope: string) {
    store.setState((state) => {
      if (state.visited[state.visited.length - 1] === scope) return state;
      const docks = { ...state.docks };
      const visited = [...state.visited.filter((s) => s !== scope), scope];
      for (const forgotten of visited.splice(0, visited.length - REMEMBERED_SCOPES)) {
        delete docks[forgotten];
      }
      return { docks, visited };
    });
  },
  /** a session just created from the new-session screen keeps the dock it
      was composed with; the new-session screen starts empty again */
  adoptNewSessionDock(sessionId: string) {
    store.setState((state) => {
      const composed = state.docks[NEW_SESSION_SCOPE];
      if (!composed) return state;
      const docks = { ...state.docks, [`session:${sessionId}`]: composed };
      delete docks[NEW_SESSION_SCOPE];
      return { docks };
    });
  },
  /** show a surface: added as a tab if it is not one, made the one showing */
  openSurface(scope: string, id: string) {
    change(scope, ({ tabs }) => ({
      open: true,
      tabs: tabs.includes(id) ? tabs : [...tabs, id],
      active: id,
    }));
  },
  /** bring an open tab forward */
  activate(scope: string, id: string) {
    change(scope, (dock) =>
      dock.tabs.includes(id) ? { ...dock, open: true, active: id } : null,
    );
  },
  /** take a tab away; the neighbour shows, or the dock closes with the last one */
  closeTab(scope: string, id: string) {
    change(scope, (dock) => {
      const i = dock.tabs.indexOf(id);
      if (i < 0) return null;
      const tabs = dock.tabs.filter((t) => t !== id);
      const active =
        dock.active === id ? (tabs[Math.min(i, tabs.length - 1)] ?? null) : dock.active;
      return { open: tabs.length > 0 && dock.open, tabs, active };
    });
  },
  /** put a tab at another position; the tabs keep their order otherwise */
  moveTab(scope: string, id: string, to: number) {
    change(scope, (dock) => {
      const from = dock.tabs.indexOf(id);
      if (from < 0) return null;
      const at = Math.max(0, Math.min(dock.tabs.length - 1, to));
      if (at === from) return null;
      const tabs = dock.tabs.filter((t) => t !== id);
      tabs.splice(at, 0, id);
      return { ...dock, tabs };
    });
  },
  closeDock(scope: string) {
    change(scope, (dock) => (dock.open ? { ...dock, open: false } : null));
  },
  /** the dock as it was, with the tabs it had */
  reopenDock(scope: string) {
    change(scope, (dock) => (dock.tabs.length > 0 ? { ...dock, open: true } : null));
  },
  /** tests only */
  reset() {
    store.setState({ docks: {}, visited: [] });
  },
};

export type DockHandle = {
  scope: string;
  dock: DockState;
  openSurface: (id: string) => void;
  activate: (id: string) => void;
  closeTab: (id: string) => void;
  moveTab: (id: string, to: number) => void;
  closeDock: () => void;
  reopenDock: () => void;
};

/** The dock of the screen being rendered, found from the current route. */
export function useDock(): DockHandle {
  return useDockFor(dockScope(useRoute()));
}

/** The dock of `scope`, with its changes bound to it. */
export function useDockFor(scope: string): DockHandle {
  const dock = useStore(store, (state) => dockIn(state, scope));
  return useMemo(
    () => ({
      scope,
      dock,
      openSurface: (id) => workspace.openSurface(scope, id),
      activate: (id) => workspace.activate(scope, id),
      closeTab: (id) => workspace.closeTab(scope, id),
      moveTab: (id, to) => workspace.moveTab(scope, id, to),
      closeDock: () => workspace.closeDock(scope),
      reopenDock: () => workspace.reopenDock(scope),
    }),
    [scope, dock],
  );
}
