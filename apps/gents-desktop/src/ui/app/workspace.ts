import { useEffect, useMemo } from "react";
import { useStore } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { createStore } from "zustand/vanilla";

import { useRoute } from "@/lib/router";
import { browserStorage } from "@/lib/storage";
import type { Route } from "@/lib/router";

export type DockState = {
  open: boolean;
  tabs: string[];
  active: string | null;
};

export function dockScope(route: Route): string {
  return dockScopeOf(route.name, route.name === "session" ? route.sessionId : null);
}

export function dockScopeOf(routeName: string, sessionId: string | null): string {
  if (routeName !== "session") return APP_SCOPE;
  return sessionId ? `session:${sessionId}` : NEW_SESSION_SCOPE;
}

const APP_SCOPE = "app";
const NEW_SESSION_SCOPE = "session:new";
const REMEMBERED_SCOPES = 200;
const EMPTY: DockState = { open: false, tabs: [], active: null };

type WorkspaceState = {
  docks: Record<string, DockState>;
  visited: string[];
};

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

export function restoreWorkspace(persisted: unknown): WorkspaceState {
  const saved = isRecord(persisted) ? persisted : {};
  const docks: Record<string, DockState> = {};
  for (const [scope, dock] of Object.entries(
    isRecord(saved.docks) ? saved.docks : {},
  )) {
    if (!isRecord(dock) || !Array.isArray(dock.tabs)) continue;
    const tabs = [
      ...new Set(dock.tabs.filter((t): t is string => typeof t === "string")),
    ];
    const active =
      typeof dock.active === "string" && tabs.includes(dock.active)
        ? dock.active
        : (tabs[0] ?? null);
    docks[scope] = { open: false, tabs, active };
  }
  const visited = Array.isArray(saved.visited)
    ? [...new Set(saved.visited.filter((s): s is string => typeof s === "string"))]
    : [];
  return { docks, visited };
}

function remembering(state: WorkspaceState, scope: string): WorkspaceState {
  const docks = { ...state.docks };
  const visited = [...state.visited.filter((s) => s !== scope), scope];
  for (const forgotten of visited.splice(0, visited.length - REMEMBERED_SCOPES)) {
    delete docks[forgotten];
  }
  return { docks, visited };
}

const store = createStore<WorkspaceState>()(
  persist((): WorkspaceState => ({ docks: {}, visited: [] }), {
    name: "gents-dock-by-scope",
    version: 1,
    storage: createJSONStorage(() => browserStorage),
    partialize: ({ docks, visited }) => ({
      docks: Object.fromEntries(
        Object.entries(docks).map(([scope, dock]) => [scope, { ...dock, open: false }]),
      ),
      visited,
    }),
    merge: (persisted, current) => ({ ...current, ...restoreWorkspace(persisted) }),
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
  dock: (scope: string) => dockIn(store.getState(), scope),
  visit(scope: string) {
    store.setState((state) => {
      if (state.visited[state.visited.length - 1] === scope) return state;
      return remembering(state, scope);
    });
  },
  adoptNewSessionDock(sessionId: string) {
    store.setState((state) => {
      const composed = state.docks[NEW_SESSION_SCOPE];
      if (!composed) return state;
      const docks = { ...state.docks, [`session:${sessionId}`]: composed };
      delete docks[NEW_SESSION_SCOPE];
      return remembering({ ...state, docks }, `session:${sessionId}`);
    });
  },
  openSurface(scope: string, id: string) {
    change(scope, ({ tabs }) => ({
      open: true,
      tabs: tabs.includes(id) ? tabs : [...tabs, id],
      active: id,
    }));
  },
  activate(scope: string, id: string) {
    change(scope, (dock) =>
      dock.tabs.includes(id) ? { ...dock, open: true, active: id } : null,
    );
  },
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
  reopenDock(scope: string) {
    change(scope, (dock) => (dock.tabs.length > 0 ? { ...dock, open: true } : null));
  },
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

export function useDock(): DockHandle {
  return useDockFor(dockScope(useRoute()));
}

export function useDockVisit(route: Route) {
  const scope = dockScope(route);
  useEffect(() => workspace.visit(scope), [scope]);
}

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
