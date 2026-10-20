import { useCallback, useSyncExternalStore } from "react";
import { registry } from "../registry";
import type { Contribution } from "../types";

/* the resolved contributions for one area; re-renders only when that area
   mutates */
export function useContributions(area: string): readonly Contribution[] {
  const subscribe = useCallback(
    (onChange: () => void) => registry.subscribeArea(area, onChange),
    [area],
  );
  const getSnapshot = useCallback(() => registry.getArea(area), [area]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
