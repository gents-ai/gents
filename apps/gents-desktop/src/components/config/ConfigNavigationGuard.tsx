import { createContext, useContext, useLayoutEffect, type ReactNode } from "react";

type ConfigNavigationGuardValue = {
  reportDirty: (dirty: boolean) => void;
  requestNavigation: (navigate: () => void) => void;
};

const unguardedNavigation: ConfigNavigationGuardValue = {
  reportDirty: () => undefined,
  requestNavigation: (navigate) => navigate(),
};

const ConfigNavigationGuardContext =
  createContext<ConfigNavigationGuardValue>(unguardedNavigation);

export function ConfigNavigationGuardProvider({
  children,
  value,
}: {
  children: ReactNode;
  value: ConfigNavigationGuardValue;
}) {
  return (
    <ConfigNavigationGuardContext.Provider value={value}>
      {children}
    </ConfigNavigationGuardContext.Provider>
  );
}

export function useConfigNavigationGuard() {
  return useContext(ConfigNavigationGuardContext);
}

/**
 * Every config editor already computes its exact persisted-document dirty
 * state for the status chip. Reuse that single source of truth for navigation
 * protection instead of maintaining a second, event-based approximation.
 */
export function useReportConfigDirty(dirty: boolean) {
  const { reportDirty } = useConfigNavigationGuard();

  // Navigation can happen on the first pointer event after the status chip
  // changes. Publish before paint so a just-saved clean form cannot leave one
  // stale guarded click behind.
  useLayoutEffect(() => {
    reportDirty(dirty);
    return () => reportDirty(false);
  }, [dirty, reportDirty]);
}
