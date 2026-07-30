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

export function useReportConfigDirty(dirty: boolean) {
  const { reportDirty } = useConfigNavigationGuard();

  useLayoutEffect(() => {
    reportDirty(dirty);
    return () => reportDirty(false);
  }, [dirty, reportDirty]);
}
