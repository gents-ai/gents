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

export function useView<T>(select: (view: ShellView) => T): T {
  return useStore(useApp().view, select);
}
