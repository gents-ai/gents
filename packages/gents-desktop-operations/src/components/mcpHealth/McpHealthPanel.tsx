import { useCallback, useEffect, useRef, useState } from "react";

import type {
  DesktopApiAdapter,
  MCPServiceHealthView,
} from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";
import { McpHealthPanelView } from "./McpHealthPanelView.js";
import type { McpProbeOutcome } from "./mcpHealthModel.js";

const POLL_INTERVAL_MS = 10_000;

export function McpHealthPanel({
  api: explicitApi,
}: {
  api?: DesktopApiAdapter;
} = {}) {
  const api = useOperationsApi(explicitApi);
  const [services, setServices] = useState<MCPServiceHealthView[]>([]);
  const [loading, setLoading] = useState<boolean>(true);
  const [error, setError] = useState<string | null>(null);
  const [lastFetchedAt, setLastFetchedAt] = useState<string | null>(null);
  const [probingServiceId, setProbingServiceId] = useState<string | null>(null);
  const [probeOutcomes, setProbeOutcomes] = useState<
    Record<string, McpProbeOutcome>
  >({});
  const generationRef = useRef(0);

  const refresh = useCallback(async () => {
    const generation = ++generationRef.current;
    setLoading(true);
    try {
      const next = await api.listMcpServicesWithHealth();
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
  }, [api]);

  useEffect(() => {
    void refresh();
    const handle = window.setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);
    return () => {
      window.clearInterval(handle);
      generationRef.current += 1;
    };
  }, [refresh]);

  const probe = useCallback(
    async (serviceId: string) => {
      setProbingServiceId(serviceId);
      try {
        const result = await api.probeMcpService(serviceId);
        setProbeOutcomes((prev) => ({
          ...prev,
          [serviceId]: {
            at: new Date().toISOString(),
            status: result.status,
            latencyMs: result.latencyMs,
            lastError: result.lastError ?? null,
          },
        }));
        await refresh();
      } catch (caught) {
        setProbeOutcomes((prev) => ({
          ...prev,
          [serviceId]: {
            at: new Date().toISOString(),
            error: caught instanceof Error ? caught.message : String(caught),
          },
        }));
      } finally {
        setProbingServiceId(null);
      }
    },
    [api, refresh],
  );

  return (
    <McpHealthPanelView
      services={services}
      loading={loading}
      error={error}
      lastFetchedAt={lastFetchedAt}
      probingServiceId={probingServiceId}
      probeOutcomes={probeOutcomes}
      onProbe={(serviceId) => {
        void probe(serviceId);
      }}
      onRefresh={() => {
        void refresh();
      }}
    />
  );
}
