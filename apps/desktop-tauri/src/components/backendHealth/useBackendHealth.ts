import { useCallback, useEffect, useState } from "react";

import { listBackendsWithHealth } from "../../lib/desktop-api";
import type { BackendHealth } from "./types";

export type BackendHealthState = {
  backends: BackendHealth[] | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
};

export function useBackendHealth(): BackendHealthState {
  const [backends, setBackends] = useState<BackendHealth[] | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const rows = await listBackendsWithHealth();
      setBackends(rows);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return { backends, loading, error, refresh };
}
