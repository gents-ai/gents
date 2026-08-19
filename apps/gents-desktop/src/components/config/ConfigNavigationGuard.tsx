import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

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

export function useConfigNavigationController() {
  const [dirty, reportDirty] = useState(false);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const pendingNavigation = useRef<(() => void) | null>(null);

  const requestNavigation = useCallback(
    (navigate: () => void) => {
      if (!dirty) {
        navigate();
        return;
      }
      pendingNavigation.current = navigate;
      setConfirmingDiscard(true);
    },
    [dirty],
  );

  const cancelDiscard = useCallback(() => {
    pendingNavigation.current = null;
    setConfirmingDiscard(false);
  }, []);

  const confirmDiscard = useCallback(() => {
    const navigate = pendingNavigation.current;
    pendingNavigation.current = null;
    setConfirmingDiscard(false);
    reportDirty(false);
    navigate?.();
  }, []);

  useEffect(() => {
    if (!dirty) return;
    const preventAccidentalClose = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", preventAccidentalClose);
    return () => window.removeEventListener("beforeunload", preventAccidentalClose);
  }, [dirty]);

  return {
    cancelDiscard,
    confirmDiscard,
    confirmingDiscard,
    reportDirty,
    requestNavigation,
  };
}

export function useReportConfigDirty(dirty: boolean) {
  const { reportDirty } = useConfigNavigationGuard();

  useLayoutEffect(() => {
    reportDirty(dirty);
    return () => reportDirty(false);
  }, [dirty, reportDirty]);
}
