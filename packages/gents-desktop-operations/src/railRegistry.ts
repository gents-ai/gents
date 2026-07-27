import type { ReactNode } from "react";

export type OperationsRailTab = {
  id: string;
  label: string;
  /** Hosts and package panels register through the same shape. */
  render: () => ReactNode;
};

/**
 * Host-extensible operations rail tab registry (design § Headless state vs presentation).
 */
export function createOperationsRailRegistry(initial: OperationsRailTab[] = []) {
  const tabs = new Map<string, OperationsRailTab>(initial.map((t) => [t.id, t]));
  return {
    register(tab: OperationsRailTab) {
      tabs.set(tab.id, tab);
    },
    unregister(id: string) {
      tabs.delete(id);
    },
    list(): OperationsRailTab[] {
      return Array.from(tabs.values());
    },
  };
}
