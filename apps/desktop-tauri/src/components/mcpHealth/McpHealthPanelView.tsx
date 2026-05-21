import { useMemo, useState, type KeyboardEvent } from "react";

import type { MCPServiceHealthView } from "../../lib/types";

export type FilterId = "all" | "unhealthy" | "reconnecting";

export type McpHealthPanelViewProps = {
  services: MCPServiceHealthView[];
  loading: boolean;
  error: string | null;
  lastFetchedAt: string | null;
  probingServiceId: string | null;
  onProbe: (serviceId: string) => void;
  onRefresh: () => void;
};

type VisualState =
  | "healthy"
  | "degraded"
  | "evicted"
  | "reconnecting"
  | "stuck"
  | "unknown";

function visualState(service: MCPServiceHealthView): VisualState {
  // Accept both the persisted vocabulary ("degraded") and the public
  // HealthStatus name ("stale") so the panel keeps working if any older
  // row was written before the schema/vocab alignment landed.
  const raw = (service.status ?? "").toLowerCase();
  const status = raw === "stale" ? "degraded" : raw;
  const failureCount = service.failureCount ?? 0;
  const kMax = service.kMax ?? 1;
  // Derived "stuck" badge — failure_count >= 2K or last_seen older than 5 min.
  // The runtime does not model `Stuck`; the panel surfaces it visually so
  // operators have a clear "investigate now" signal without changing the
  // K-model state machine.
  const stuck =
    (status === "evicted" || status === "reconnecting") &&
    (failureCount >= 2 * Math.max(1, kMax) || isLastSeenOlderThan(service, 5 * 60_000));
  if (stuck) return "stuck";
  switch (status) {
    case "healthy":
      return "healthy";
    case "degraded":
      return "degraded";
    case "evicted":
      return "evicted";
    case "reconnecting":
      return "reconnecting";
    default:
      return "unknown";
  }
}

function isLastSeenOlderThan(service: MCPServiceHealthView, ms: number): boolean {
  if (!service.lastSeen) return false;
  const ts = Date.parse(service.lastSeen);
  if (Number.isNaN(ts)) return false;
  return Date.now() - ts > ms;
}

function projectStatus(
  visual: VisualState,
): "healthy" | "stale" | "unreachable" | "unknown" {
  if (visual === "healthy") return "healthy";
  if (visual === "degraded") return "stale";
  if (visual === "evicted" || visual === "reconnecting" || visual === "stuck") {
    return "unreachable";
  }
  return "unknown";
}

function statusLabel(visual: VisualState): string {
  switch (visual) {
    case "healthy":
      return "healthy";
    case "degraded":
      return "degraded";
    case "evicted":
      return "evicted (backoff)";
    case "reconnecting":
      return "reconnecting";
    case "stuck":
      return "stuck";
    default:
      return "unknown";
  }
}

function matchesFilter(service: MCPServiceHealthView, filter: FilterId): boolean {
  if (filter === "all") return true;
  const visual = visualState(service);
  if (filter === "unhealthy") return projectStatus(visual) !== "healthy";
  if (filter === "reconnecting") return visual === "reconnecting";
  return true;
}

function formatRelative(timestamp: string | null | undefined): string {
  if (!timestamp) return "never";
  const ts = Date.parse(timestamp);
  if (Number.isNaN(ts)) return timestamp;
  const ageMs = Date.now() - ts;
  if (ageMs < 0) {
    return formatRemaining(-ageMs).startsWith("imminent")
      ? "imminent"
      : `in ${formatRemaining(-ageMs)}`;
  }
  const s = Math.floor(ageMs / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function formatRemaining(ms: number): string {
  const totalSec = Math.max(0, Math.ceil(ms / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  if (totalSec < 5) return "imminent";
  return `${m}:${String(s).padStart(2, "0")}`;
}

function backoffRemaining(backoffUntil: string | null | undefined): {
  remainingMs: number;
  text: string;
} | null {
  if (!backoffUntil) return null;
  const target = Date.parse(backoffUntil);
  if (Number.isNaN(target)) return null;
  const remainingMs = target - Date.now();
  if (remainingMs <= 0) return { remainingMs: 0, text: "backoff expired" };
  return { remainingMs, text: `retry in ${formatRemaining(remainingMs)}` };
}

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

      <div className="mcp-health-summary" aria-label="Service health summary">
        <SummaryPill color="green" count={summary.healthy} label="healthy" />
        <SummaryPill color="yellow" count={summary.degraded} label="degraded" />
        <SummaryPill color="blue" count={summary.reconnecting} label="reconnecting" />
        <SummaryPill color="red" count={summary.evicted} label="evicted" />
        <SummaryPill color="red" count={summary.stuck} label="stuck" />
        {summary.unknown > 0 ? (
          <SummaryPill color="gray" count={summary.unknown} label="unknown" />
        ) : null}
      </div>

      <div role="group" aria-label="Filter" className="mcp-health-filters">
        <FilterChip
          label="All"
          count={filterCounts.all}
          active={filter === "all"}
          onClick={() => setFilter("all")}
        />
        <FilterChip
          label="Unhealthy"
          count={filterCounts.unhealthy}
          active={filter === "unhealthy"}
          onClick={() => setFilter("unhealthy")}
        />
        <FilterChip
          label="Reconnecting"
          count={filterCounts.reconnecting}
          active={filter === "reconnecting"}
          onClick={() => setFilter("reconnecting")}
        />
      </div>

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
        <div className="mcp-health-table-wrap">
          <table className="mcp-health-table" aria-describedby="mcp-health-title">
            <thead>
              <tr>
                <th scope="col">Service</th>
                <th scope="col">Status</th>
                <th scope="col">K model</th>
                <th scope="col">Last probe</th>
                <th scope="col">Last error</th>
                <th scope="col" aria-label="Actions"></th>
              </tr>
            </thead>
            <tbody>
              {visibleServices.map((service) => {
                const visual = visualState(service);
                const expanded = expandedId === service.serviceId;
                const busy = probingServiceId === service.serviceId;
                const backoff = backoffRemaining(service.backoffUntil);
                return (
                  <ServiceRows
                    key={service.serviceId}
                    service={service}
                    visual={visual}
                    expanded={expanded}
                    busy={busy}
                    backoff={backoff}
                    onToggle={() => toggleExpand(service.serviceId)}
                    onKeyDown={(event) => onRowKeyDown(event, service.serviceId)}
                    onProbe={() => onProbe(service.serviceId)}
                  />
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function SummaryPill({
  color,
  count,
  label,
}: {
  color: "green" | "yellow" | "red" | "blue" | "gray";
  count: number;
  label: string;
}) {
  return (
    <span className={`mcp-health-pill mcp-health-pill-${color}`}>
      <span className="mcp-health-pill-dot" aria-hidden />
      <span className="mcp-health-pill-count">{count}</span> {label}
    </span>
  );
}

function FilterChip({
  label,
  count,
  active,
  onClick,
}: {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={active ? "mcp-health-chip is-active" : "mcp-health-chip"}
      aria-pressed={active}
      onClick={onClick}
    >
      {label} <span className="mcp-health-chip-count">{count}</span>
    </button>
  );
}

function ServiceRows({
  service,
  visual,
  expanded,
  busy,
  backoff,
  onToggle,
  onKeyDown,
  onProbe,
}: {
  service: MCPServiceHealthView;
  visual: VisualState;
  expanded: boolean;
  busy: boolean;
  backoff: { remainingMs: number; text: string } | null;
  onToggle: () => void;
  onKeyDown: (event: KeyboardEvent<HTMLTableRowElement>) => void;
  onProbe: () => void;
}) {
  const dotClass =
    visual === "degraded"
      ? "yellow"
      : visual === "healthy"
        ? "green"
        : visual === "reconnecting"
          ? "blue"
          : "red";
  const k = service.kMax ?? 0;
  const fc = service.failureCount ?? 0;
  return (
    <>
      <tr
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        aria-controls={`mcp-health-detail-${service.serviceId}`}
        className={
          expanded ? "mcp-health-row mcp-health-row-expanded" : "mcp-health-row"
        }
        onClick={onToggle}
        onKeyDown={onKeyDown}
        data-testid={`mcp-health-row-${service.serviceId}`}
      >
        <td className="mcp-health-service-cell">
          <span className="mcp-health-caret" aria-hidden>
            ▸
          </span>
          <div>
            <div className="mcp-health-service-name">{service.serviceId}</div>
            {service.endpoint ? (
              <div className="mcp-health-service-endpoint">{service.endpoint}</div>
            ) : null}
          </div>
        </td>
        <td>
          <span
            className={`mcp-health-status mcp-health-status-${visual}`}
            data-testid={`mcp-health-status-${service.serviceId}`}
          >
            <span
              className={`mcp-health-dot mcp-health-dot-${dotClass} mcp-health-dot-${visual}`}
              aria-hidden
            />
            {statusLabel(visual)}
          </span>
        </td>
        <td>
          <KModelCell k={k} fc={fc} />
        </td>
        <td>
          <div className="mcp-health-last-probe">
            <span>{formatRelative(service.lastProbeAt ?? service.lastSeen)}</span>
            {backoff ? (
              <span
                className="mcp-health-backoff"
                data-testid={`mcp-health-backoff-${service.serviceId}`}
              >
                {backoff.text}
              </span>
            ) : null}
          </div>
        </td>
        <td>
          {service.lastErrorMessage ? (
            <span className="mcp-health-last-error" title={service.lastErrorMessage}>
              {service.lastErrorClass ?? "error"}: {service.lastErrorMessage}
            </span>
          ) : (
            <span className="mcp-health-last-error mcp-health-last-error-muted">—</span>
          )}
        </td>
        <td>
          <button
            type="button"
            className="mcp-health-probe-btn"
            onClick={(event) => {
              event.stopPropagation();
              onProbe();
            }}
            aria-busy={busy}
            aria-label={`Probe ${service.serviceId}`}
            data-testid={`mcp-health-probe-${service.serviceId}`}
          >
            {busy ? "Probing…" : "Probe"}
          </button>
        </td>
      </tr>
      {expanded ? (
        <tr className="mcp-health-detail-row">
          <td colSpan={6} id={`mcp-health-detail-${service.serviceId}`}>
            <div className="mcp-health-detail">
              <DetailKv
                rows={[
                  ["service_id", service.serviceId],
                  ["endpoint", service.endpoint ?? "—"],
                  ["status", `${service.status ?? "unknown"} (visual: ${visual})`],
                  ["projected status", projectStatus(visual)],
                  ["failure_count", `${fc} / ${k} (K)`],
                  ["backoff_until", service.backoffUntil ?? "—"],
                  ["last_probe_at", service.lastProbeAt ?? "—"],
                  ["last_seen", service.lastSeen ?? "—"],
                  ["last_error_class", service.lastErrorClass ?? "—"],
                  ["last_error_message", service.lastErrorMessage ?? "—"],
                  ["agent_did", service.agentDid ?? "—"],
                  ["updated_at", service.updatedAt ?? "—"],
                ]}
              />
            </div>
          </td>
        </tr>
      ) : null}
    </>
  );
}

function KModelCell({ k, fc }: { k: number; fc: number }) {
  if (k <= 1) {
    return (
      <div className="mcp-health-k-cell">
        <span className="mcp-health-k-badge mcp-health-k-badge-single">K=1</span>
        <span className="mcp-health-k-explainer">single-fail → evict</span>
      </div>
    );
  }
  const alarm = fc >= k;
  const warn = fc > 0 && fc < k;
  const badgeClass = alarm
    ? "mcp-health-k-badge mcp-health-k-badge-alarm"
    : warn
      ? "mcp-health-k-badge mcp-health-k-badge-warn"
      : "mcp-health-k-badge";
  const explainer =
    fc === 0
      ? "no failures"
      : fc < k
        ? `${k - fc} more fail${k - fc === 1 ? "" : "s"} until evict`
        : "evicted (back-off)";
  return (
    <div className="mcp-health-k-cell">
      <span className={badgeClass}>
        K={k} · {fc}/{k}
      </span>
      <span className="mcp-health-k-explainer">{explainer}</span>
    </div>
  );
}

function DetailKv({ rows }: { rows: Array<[string, string]> }) {
  return (
    <dl className="mcp-health-kv">
      {rows.map(([key, value]) => (
        <div key={key}>
          <dt>{key}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}
