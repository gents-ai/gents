import { useCallback, useEffect, useState } from "react";

import type {
  BackendHealth,
  DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";

export type BackendHealthState = {
  backends: BackendHealth[] | null;
  loading: boolean;
  error: string | null;
  now: Date;
  refresh: () => Promise<void>;
};

export function useBackendHealth(
  explicitApi?: DesktopApiAdapter,
): BackendHealthState {
  const api = useOperationsApi(explicitApi);
  const [backends, setBackends] = useState<BackendHealth[] | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => new Date());

  const refresh = useCallback(
    async (background = false) => {
      setNow(new Date());
      if (!background) {
        setLoading(true);
      }
      setError(null);
      try {
        const rows = await api.listBackendsWithHealth();
        setBackends(rows);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (!background) {
          setLoading(false);
        }
      }
    },
    [api],
  );

  useEffect(() => {
    void refresh();
    const handle = window.setInterval(() => {
      void refresh(true);
    }, 10_000);
    return () => window.clearInterval(handle);
  }, [refresh]);

  return { backends, loading, error, now, refresh };
}
