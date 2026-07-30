import { createContext, useContext, type ReactNode } from "react";

export type OperationsRailTabId = string;

export type OperationsRailTabDescriptor = {
  id: OperationsRailTabId;
  label: string;
  badge?: string | null;
  render: () => ReactNode;
};

export type OperationsRailContextValue = {
  tabs: OperationsRailTabDescriptor[];
  activeTabId: OperationsRailTabId | null;
  setActiveTab: (id: OperationsRailTabId) => void;
};

export const OperationsRailContext =
  createContext<OperationsRailContextValue | null>(null);

export function useOperationsRail(): OperationsRailContextValue {
  const value = useContext(OperationsRailContext);
  if (!value) {
    throw new Error(
      "useOperationsRail must be used inside <OperationsRailProvider>",
    );
  }
  return value;
}
