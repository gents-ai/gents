import { useCallback, useEffect, useRef, useState } from "react";

import { fetchOperationsSnapshot } from "../../lib/desktop-api";
import type {
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
} from "../../lib/types/operations";

export type OperationsSnapshotState = {
  snapshot: DesktopOperationsSnapshot | null;
  error: string | null;
  isLoading: boolean;
  refresh: () => Promise<void>;
};

const REFRESH_INTERVAL_MS = 2_000;

export function useOperationsSnapshot(
  request: DesktopOperationsSnapshotRequest,
): OperationsSnapshotState {
  const [snapshot, setSnapshot] = useState<DesktopOperationsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState<boolean>(true);
  const reqRef = useRef(request);
  reqRef.current = request;

  const refresh = useCallback(async () => {
    try {
      const next = await fetchOperationsSnapshot(reqRef.current);
      setSnapshot(next);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setIsLoading(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      if (cancelled) return;
      await refresh();
    };
    void tick();
    const id = setInterval(tick, REFRESH_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [refresh]);

  return { snapshot, error, isLoading, refresh };
}
