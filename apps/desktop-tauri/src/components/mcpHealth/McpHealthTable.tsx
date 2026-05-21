import type { KeyboardEvent } from "react";

import type { MCPServiceHealthView } from "../../lib/types";
import {
  backoffRemaining,
  formatRelative,
  projectStatus,
  statusLabel,
  visualState,
  type VisualState,
} from "./mcpHealthModel";

export function McpHealthTable({
  services,
  expandedId,
  probingServiceId,
  onToggle,
  onRowKeyDown,
  onProbe,
}: {
  services: MCPServiceHealthView[];
  expandedId: string | null;
  probingServiceId: string | null;
  onToggle: (serviceId: string) => void;
  onRowKeyDown: (event: KeyboardEvent<HTMLTableRowElement>, serviceId: string) => void;
  onProbe: (serviceId: string) => void;
}) {
  return (
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
          {services.map((service) => {
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
                onToggle={() => onToggle(service.serviceId)}
                onKeyDown={(event) => onRowKeyDown(event, service.serviceId)}
                onProbe={() => onProbe(service.serviceId)}
              />
            );
          })}
        </tbody>
      </table>
    </div>
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
