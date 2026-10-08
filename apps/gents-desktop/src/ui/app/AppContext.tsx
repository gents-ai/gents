/* The desktop app for every screen: its stores to select from and its
   actions to call. Made once at the root, so the value never changes and
   providing it re-renders nothing. */
import { createContext, useContext } from "react";
import { useStore } from "zustand";

import type { DesktopApp } from "../../hooks/desktopApp";
import type { ShellView } from "../../hooks/shellView";

const AppContext = createContext<DesktopApp | null>(null);
export const AppProvider = AppContext.Provider;

export function useApp(): DesktopApp {
  const app = useContext(AppContext);
  if (!app) throw new Error("useApp: no AppProvider above");
  return app;
}

/** A value from what the shell decides; re-renders when it changes by identity. */
export function useView<T>(select: (view: ShellView) => T): T {
  return useStore(useApp().view, select);
}
