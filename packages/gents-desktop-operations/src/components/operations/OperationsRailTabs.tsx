import type { OperationsRailContextValue } from "./operationsRailContext.js";

export type OperationsRailTabsProps = Pick<
  OperationsRailContextValue,
  "tabs" | "activeTabId" | "setActiveTab"
>;

export function OperationsRailTabs({
  tabs,
  activeTabId,
  setActiveTab,
}: OperationsRailTabsProps) {
  if (tabs.length === 0) {
    return null;
  }
  return (
    <div
      role="tablist"
      aria-label="Operations"
      className="operations-rail-tabs"
    >
      {tabs.map((tab) => {
        const selected = tab.id === activeTabId;
        return (
          <button
            key={tab.id}
            type="button"
            role="tab"
            aria-selected={selected}
            aria-controls={`operations-rail-panel-${tab.id}`}
            id={`operations-rail-tab-${tab.id}`}
            className={selected ? "is-active" : undefined}
            onClick={() => setActiveTab(tab.id)}
          >
            <span className="operations-rail-tab-label">{tab.label}</span>
            {tab.badge ? (
              <span className="operations-rail-tab-badge">{tab.badge}</span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}
