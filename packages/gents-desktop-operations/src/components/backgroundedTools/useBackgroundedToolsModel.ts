import { useCallback, useMemo, useState } from "react";

import type {
  BackgroundedToolView,
  DesktopOperationsSnapshot,
} from "@source-inc/gents-desktop-client";
import {
  correlateProcess,
  derivedState,
  type DerivedState,
} from "./derivedState.js";

export type BackgroundedToolsSortKey =
  | "toolName"
  | "ageMs"
  | "requestId"
  | "awaitMode"
  | "derivedState"
  | "processLabel";
export type BackgroundedToolsSortDir = "ascending" | "descending";

export type ProjectedBackgroundedTool = BackgroundedToolView & {
  ageMs: number;
  derivedState: DerivedState;
  processLabel: string;
  processTooltip: string;
};

export function useBackgroundedToolsModel(
  snapshot: DesktopOperationsSnapshot | null,
) {
  const [stateFilters, setStateFilters] = useState<Set<DerivedState>>(
    new Set(),
  );
  const [awaitFilters, setAwaitFilters] = useState<Set<string>>(new Set());
  const [parentFilter, setParentFilter] = useState("all");
  const [hideHealthy, setHideHealthy] = useState(false);
  const [sortKey, setSortKey] = useState<BackgroundedToolsSortKey>("ageMs");
  const [sortDir, setSortDir] =
    useState<BackgroundedToolsSortDir>("descending");

  const projected = useMemo<ProjectedBackgroundedTool[]>(() => {
    if (!snapshot) return [];
    const now = Date.now();
    const executors = snapshot.liveness?.activeNativeExecutors ?? [];
    return snapshot.backgroundedTools.map((row) => {
      const process = correlateProcess(row, executors);
      return {
        ...row,
        ageMs: row.ageMs ?? 0,
        derivedState: derivedState(row, now),
        processLabel: process.label,
        processTooltip: process.tooltip,
      };
    });
  }, [snapshot]);

  const filtered = useMemo(() => {
    const rows = projected.filter((row) => {
      if (parentFilter !== "all" && row.requestId !== parentFilter)
        return false;
      if (stateFilters.size > 0 && !stateFilters.has(row.derivedState))
        return false;
      if (
        awaitFilters.size > 0 &&
        (row.awaitMode == null || !awaitFilters.has(row.awaitMode))
      )
        return false;
      if (
        hideHealthy &&
        !["stuck", "cancelPending", "deadline+"].includes(row.derivedState)
      )
        return false;
      return true;
    });
    const direction = sortDir === "ascending" ? 1 : -1;
    return [...rows].sort((left, right) => {
      const leftValue = left[sortKey];
      const rightValue = right[sortKey];
      if (leftValue == null && rightValue == null) return 0;
      if (leftValue == null) return 1;
      if (rightValue == null) return -1;
      if (typeof leftValue === "number" && typeof rightValue === "number") {
        return (leftValue - rightValue) * direction;
      }
      return String(leftValue).localeCompare(String(rightValue)) * direction;
    });
  }, [
    awaitFilters,
    hideHealthy,
    parentFilter,
    projected,
    sortDir,
    sortKey,
    stateFilters,
  ]);

  const parents = useMemo(
    () => Array.from(new Set(projected.map((row) => row.requestId))),
    [projected],
  );
  const stateOptions = useMemo(
    () =>
      Array.from(
        new Set([...projected.map((row) => row.derivedState), ...stateFilters]),
      ).sort(),
    [projected, stateFilters],
  );
  const awaitOptions = useMemo(
    () =>
      Array.from(
        new Set([
          ...projected
            .map((row) => row.awaitMode)
            .filter((mode): mode is string => mode != null),
          ...awaitFilters,
        ]),
      ).sort(),
    [projected, awaitFilters],
  );

  const onSort = useCallback(
    (key: BackgroundedToolsSortKey) => {
      if (sortKey === key) {
        setSortDir((current) =>
          current === "ascending" ? "descending" : "ascending",
        );
        return;
      }
      setSortKey(key);
      setSortDir("ascending");
    },
    [sortKey],
  );
  const toggleStateFilter = useCallback((state: DerivedState) => {
    setStateFilters((current) => {
      const next = new Set(current);
      next.has(state) ? next.delete(state) : next.add(state);
      return next;
    });
  }, []);
  const toggleAwaitFilter = useCallback((mode: string) => {
    setAwaitFilters((current) => {
      const next = new Set(current);
      next.has(mode) ? next.delete(mode) : next.add(mode);
      return next;
    });
  }, []);

  return {
    awaitFilters,
    awaitOptions,
    filtered,
    hideHealthy,
    parentFilter,
    parents,
    projected,
    sortDir,
    sortKey,
    stateFilters,
    stateOptions,
    onSort,
    setHideHealthy,
    setParentFilter,
    toggleAwaitFilter,
    toggleStateFilter,
  };
}
