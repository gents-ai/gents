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

  const refresh = useCallback(async (background = false) => {
    if (!background) {
      setLoading(true);
    }
    setError(null);
    try {
      const rows = await listBackendsWithHealth();
      setBackends(rows);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (!background) {
        setLoading(false);
      }
    }
  }, []);

  // Poll like the MCP panel (10 s) so statuses and relative ages stay live
  // instead of freezing at mount; background refreshes skip the loading flip.
  useEffect(() => {
    void refresh();
    const handle = window.setInterval(() => {
      void refresh(true);
    }, 10_000);
    return () => window.clearInterval(handle);
  }, [refresh]);

  return { backends, loading, error, refresh };
}
