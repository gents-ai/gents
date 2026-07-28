import { useCallback, useEffect, useRef, useState } from "react";

import { listToolCallHolds } from "@source-inc/gents-desktop-client";
import type { HeldToolCallView } from "@source-inc/gents-desktop-client";

export type ToolCallHoldsState = {
  holds: HeldToolCallView[] | null;
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
};

/// Held tool calls for one agent, polled every 10 s (the shared ops-panel
/// cadence) so a hold raised mid-turn surfaces without a manual refresh.
/// Background refreshes skip the loading flip to avoid flicker. A generation
/// counter drops out-of-order responses so a slow fetch for a previous agent
/// (or an older poll tick) can never overwrite fresher rows.
export function useToolCallHolds(agentDid: string | null): ToolCallHoldsState {
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
        const rows = await listToolCallHolds(agentDid);
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
    [agentDid],
  );

  useEffect(() => {
    // Reset stale rows immediately on agent switch: the previous agent's
    // holds must not linger while the new fetch is in flight.
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
