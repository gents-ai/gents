import type { OperationsRailTabDescriptor } from "./operationsRailContext.js";

export type OperationsRailTabPanelProps = {
  tab: OperationsRailTabDescriptor;
};

export function OperationsRailTabPanel({ tab }: OperationsRailTabPanelProps) {
  return (
    <div
      role="tabpanel"
      id={`operations-rail-panel-${tab.id}`}
      aria-labelledby={`operations-rail-tab-${tab.id}`}
      className="operations-rail-tab-panel"
    >
      {tab.render()}
    </div>
  );
}
