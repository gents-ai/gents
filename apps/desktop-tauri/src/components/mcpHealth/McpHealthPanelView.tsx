import { useMemo, useState, type KeyboardEvent } from "react";

import type { MCPServiceHealthView } from "../../lib/types";
import { McpHealthFilters, McpHealthSummary } from "./McpHealthSummary";
import { McpHealthTable } from "./McpHealthTable";
import {
  formatRelative,
  matchesFilter,
  projectStatus,
  visualState,
  type FilterId,
} from "./mcpHealthModel";

export type McpHealthPanelViewProps = {
  services: MCPServiceHealthView[];
  loading: boolean;
  error: string | null;
  lastFetchedAt: string | null;
  probingServiceId: string | null;
  onProbe: (serviceId: string) => void;
  onRefresh: () => void;
};

export function McpHealthPanelView({
  services,
  loading,
  error,
  lastFetchedAt,
  probingServiceId,
  onProbe,
  onRefresh,
}: McpHealthPanelViewProps) {
  const [filter, setFilter] = useState<FilterId>("all");
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const summary = useMemo(() => {
    const buckets = {
      healthy: 0,
      degraded: 0,
      reconnecting: 0,
      evicted: 0,
      stuck: 0,
      unknown: 0,
    };
    for (const service of services) {
      const v = visualState(service);
      buckets[v as keyof typeof buckets] += 1;
    }
    return buckets;
  }, [services]);

  const filterCounts = useMemo(() => {
    const all = services.length;
    const unhealthy = services.filter(
      (s) => projectStatus(visualState(s)) !== "healthy",
    ).length;
    const reconnecting = services.filter(
      (s) => visualState(s) === "reconnecting",
    ).length;
    return { all, unhealthy, reconnecting };
  }, [services]);

  const visibleServices = useMemo(
    () => services.filter((service) => matchesFilter(service, filter)),
    [services, filter],
  );

  function toggleExpand(serviceId: string) {
    setExpandedId((current) => (current === serviceId ? null : serviceId));
  }

  function onRowKeyDown(event: KeyboardEvent<HTMLTableRowElement>, serviceId: string) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      toggleExpand(serviceId);
    } else if (event.key === "Escape" && expandedId === serviceId) {
      event.preventDefault();
      setExpandedId(null);
    }
  }

  return (
    <section className="mcp-health-panel" aria-labelledby="mcp-health-title">
      <header className="mcp-health-header">
        <div>
          <h2 id="mcp-health-title">MCP services / health</h2>
          <p className="mcp-health-subtitle">
            Registered <code>ToolServiceRegistry</code> entries with their most recently
            persisted health state.
          </p>
        </div>
        <button
          type="button"
          className="mcp-health-refresh"
          onClick={onRefresh}
          disabled={loading}
        >
          {loading ? "Refreshing…" : "Refresh"}
        </button>
      </header>

      <div className="mcp-health-meta" aria-live="polite">
        {error ? (
          <span className="mcp-health-error">{error}</span>
        ) : lastFetchedAt ? (
          <span>fetched {formatRelative(lastFetchedAt)}</span>
        ) : null}
      </div>

      <McpHealthSummary summary={summary} />

      <McpHealthFilters
        filter={filter}
        counts={filterCounts}
        onFilterChange={setFilter}
      />

      {services.length === 0 ? (
        <div className="mcp-health-empty" role="status">
          <div className="mcp-health-empty-emoji" aria-hidden>
            ∅
          </div>
          <div className="mcp-health-empty-title">
            {loading ? "Loading MCP service health…" : "No MCP services registered."}
          </div>
          {!loading && (
            <div>
              Services appear here once the agent writes to{" "}
              <code>ToolServiceHealthState</code>.
            </div>
          )}
        </div>
      ) : visibleServices.length === 0 ? (
        <div className="mcp-health-empty" role="status">
          No services match this filter.
        </div>
      ) : (
        <McpHealthTable
          services={visibleServices}
          expandedId={expandedId}
          probingServiceId={probingServiceId}
          onToggle={toggleExpand}
          onRowKeyDown={onRowKeyDown}
          onProbe={onProbe}
        />
      )}
    </section>
  );
}
