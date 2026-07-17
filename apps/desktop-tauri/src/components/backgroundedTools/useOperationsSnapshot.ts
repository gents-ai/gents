import { useCallback, useEffect, useMemo, useRef, useState } from "react";

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

export type OperationsSnapshotOptions = {
  enabled?: boolean;
};

const REFRESH_INTERVAL_MS = 2_000;

export function useOperationsSnapshot(
  request: DesktopOperationsSnapshotRequest,
  options: OperationsSnapshotOptions = {},
): OperationsSnapshotState {
  const enabled = options.enabled ?? true;
  const requestKey = useMemo(
    () =>
      JSON.stringify([
        request.agentDid ?? null,
        request.rootRequestId ?? null,
        request.includeTerminal ?? null,
      ]),
    [request.agentDid, request.includeTerminal, request.rootRequestId],
  );
  const stableRequest = useMemo<DesktopOperationsSnapshotRequest>(
    () => ({
      agentDid: request.agentDid ?? null,
      rootRequestId: request.rootRequestId ?? null,
      includeTerminal: request.includeTerminal,
    }),
    [request.agentDid, request.includeTerminal, request.rootRequestId],
  );
  const currentRequestKey = useRef(requestKey);
  currentRequestKey.current = requestKey;
  const [snapshotState, setSnapshotState] = useState<{
    requestKey: string;
    value: DesktopOperationsSnapshot;
  } | null>(null);
  const [errorState, setErrorState] = useState<{
    requestKey: string;
    value: string;
  } | null>(null);
  const [loadingRequestKey, setLoadingRequestKey] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoadingRequestKey(requestKey);
    try {
      const next = await fetchOperationsSnapshot(stableRequest);
      if (currentRequestKey.current !== requestKey) return;
      setSnapshotState({ requestKey, value: next });
      setErrorState(null);
    } catch (e) {
      if (currentRequestKey.current !== requestKey) return;
      setErrorState({
        requestKey,
        value: e instanceof Error ? e.message : String(e),
      });
    } finally {
      setLoadingRequestKey((current) => (current === requestKey ? null : current));
    }
  }, [requestKey, stableRequest]);

  useEffect(() => {
    if (!enabled) return;

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
  }, [enabled, refresh]);

  const snapshot =
    snapshotState?.requestKey === requestKey ? snapshotState.value : null;
  const error = errorState?.requestKey === requestKey ? errorState.value : null;
  const isLoading =
    loadingRequestKey === requestKey || (snapshot === null && error === null);

  return { snapshot, error, isLoading, refresh };
}
