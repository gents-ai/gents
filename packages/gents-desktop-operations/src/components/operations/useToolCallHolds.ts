import { useCallback, useEffect, useRef, useState } from "react";

import type {
  DesktopApiAdapter,
  HeldToolCallView,
} from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";

export type ToolCallHoldsState = {
  holds: HeldToolCallView[] | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
};

export function useToolCallHolds(
  agentDid: string | null,
  explicitApi?: DesktopApiAdapter,
): ToolCallHoldsState {
  const api = useOperationsApi(explicitApi);
  const [holds, setHolds] = useState<HeldToolCallView[] | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  const refresh = useCallback(
    async (background = false) => {
      const generation = ++generationRef.current;
      if (!agentDid) {
        setHolds([]);
        setLoading(false);
        return;
      }
      if (!background) {
        setLoading(true);
      }
      setError(null);
      try {
        const rows = await api.listToolCallHolds(agentDid);
        if (generationRef.current === generation) {
          setHolds(rows);
        }
      } catch (err) {
        if (generationRef.current === generation) {
          setError(err instanceof Error ? err.message : String(err));
        }
      } finally {
        if (!background && generationRef.current === generation) {
          setLoading(false);
        }
      }
    },
    [agentDid, api],
  );

  useEffect(() => {
    setHolds(null);
    setLoading(true);
    void refresh();
    const handle = window.setInterval(() => {
      void refresh(true);
    }, 10_000);
    return () => window.clearInterval(handle);
  }, [refresh]);

  return { holds, loading, error, refresh };
}
