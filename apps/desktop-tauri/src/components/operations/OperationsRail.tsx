import {
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import {
  OperationsRailContext,
  type OperationsRailContextValue,
  type OperationsRailTabDescriptor,
  type OperationsRailTabId,
} from "./operationsRailContext";
import { OperationsRailTabPanel } from "./OperationsRailTabPanel";
import { OperationsRailTabs } from "./OperationsRailTabs";

export type OperationsRailProviderProps = {
  tabs: OperationsRailTabDescriptor[];
  /** Initial active tab id. Defaults to the first registered tab. */
  initialActiveTabId?: OperationsRailTabId | null;
  children: ReactNode;
};

export type OperationsRailProps = {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
};

export function OperationsRailProvider({
  tabs,
  initialActiveTabId,
  children,
}: OperationsRailProviderProps) {
  const [activeTabId, setActiveTabId] = useState<OperationsRailTabId | null>(
    initialActiveTabId ?? tabs[0]?.id ?? null,
  );

  const setActiveTab = useCallback((id: OperationsRailTabId) => {
    setActiveTabId(id);
  }, []);

  const value: OperationsRailContextValue = useMemo(
    () => ({
      tabs,
      activeTabId:
        activeTabId !== null && tabs.some((tab) => tab.id === activeTabId)
          ? activeTabId
          : (tabs[0]?.id ?? null),
      setActiveTab,
    }),
    [tabs, activeTabId, setActiveTab],
  );

  return (
    <OperationsRailContext.Provider value={value}>
      {children}
    </OperationsRailContext.Provider>
  );
}

export function OperationsRail({
  open = true,
  onOpenChange,
}: OperationsRailProps = {}) {
  const value = useContext(OperationsRailContext);
  if (!value || value.tabs.length === 0) {
    // Either no provider (foundation default) or no registered tabs:
    // render nothing so the chat shell layout doesn't get a phantom column.
    return null;
  }
  const activeTab =
    value.tabs.find((tab) => tab.id === value.activeTabId) ?? value.tabs[0];
  const activeLabel = activeTab?.label ?? "Operations";

  if (!open) {
    return (
      <aside
        className="operations-rail is-collapsed"
        aria-label="Operations"
      >
        <button
          type="button"
          className="operations-rail-collapsed-button"
          aria-expanded="false"
          aria-label={`Open operations drawer, ${activeLabel} selected`}
          onClick={() => onOpenChange?.(true)}
        >
          <span aria-hidden="true">‹</span>
          <span>Ops</span>
        </button>
      </aside>
    );
  }

  return (
    <aside className="operations-rail is-open" aria-label="Operations">
      <header className="operations-rail-header">
        <div>
          <h2>Operations</h2>
          <p>{activeLabel}</p>
        </div>
        <button
          type="button"
          className="operations-rail-close"
          aria-expanded="true"
          aria-label="Close operations drawer"
          onClick={() => onOpenChange?.(false)}
        >
          Close
        </button>
      </header>
      <OperationsRailTabs
        tabs={value.tabs}
        activeTabId={value.activeTabId}
        setActiveTab={value.setActiveTab}
      />
      <OperationsRailTabPanel tab={activeTab} />
    </aside>
  );
}
