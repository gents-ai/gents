import { useCallback, useEffect, useRef, useState } from "react";

import { listMcpServicesWithHealth, probeMcpService } from "../../lib/desktop-api";
import type { MCPServiceHealthView } from "../../lib/types";
import { McpHealthPanelView } from "./McpHealthPanelView";

/// Poll the persisted health collection every 10 s. The agent rewrites
/// every cycle (default 30 s) so 10 s polling gives a 10-40 s update
/// window without burning Tauri-bridge calls. Stops on unmount via
/// AbortController + a generation guard against stale fetches.
const POLL_INTERVAL_MS = 10_000;

export function McpHealthPanel() {
  const [services, setServices] = useState<MCPServiceHealthView[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [lastFetchedAt, setLastFetchedAt] = useState<string | null>(null);
  const [probingServiceId, setProbingServiceId] = useState<string | null>(null);
  const generationRef = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++generationRef.current;
    setLoading(true);
    try {
      const next = await listMcpServicesWithHealth();
      if (generation !== generationRef.current) return;
      setServices(next);
      setError(null);
      setLastFetchedAt(new Date().toISOString());
    } catch (caught) {
      if (generation !== generationRef.current) return;
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      if (generation === generationRef.current) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
    const handle = window.setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);
    return () => {
      window.clearInterval(handle);
      // Bumping generation forces any in-flight fetch result to be ignored.
      generationRef.current += 1;
    };
  }, [refresh]);

  const probe = useCallback(
    async (serviceId: string) => {
      setProbingServiceId(serviceId);
      try {
        await probeMcpService(serviceId);
        await refresh();
      } catch (caught) {
        setError(caught instanceof Error ? caught.message : String(caught));
      } finally {
        setProbingServiceId(null);
      }
    },
    [refresh],
  );

  return (
    <McpHealthPanelView
      services={services}
      loading={loading}
      error={error}
      lastFetchedAt={lastFetchedAt}
      probingServiceId={probingServiceId}
      onProbe={(serviceId) => {
        void probe(serviceId);
      }}
      onRefresh={() => {
        void refresh();
      }}
    />
  );
}
