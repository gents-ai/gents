import { useState } from "react";

import { STATE_GLYPH, STATE_LABEL } from "./displayState";
import type { BackendHealth, InferenceCallSummary } from "./types";

function ageString(iso: string | null, now: Date): string {
  if (!iso) return "—";
  const ms = now.getTime() - new Date(iso).getTime();
  if (Number.isNaN(ms)) return "—";
  if (ms < 0) return "in the future";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}

function probeTone(status: string): "success" | "warning" | "error" | null {
  switch (status) {
    case "healthy":
      return "success";
    case "unhealthy":
      return "error";
    case "stale":
    case "rate_limited":
    case "circuit_open":
      return "warning";
    default:
      return null;
  }
}

function lastCallEndedAt(c: InferenceCallSummary): string | null {
  return c.endedAt ?? c.startedAt ?? c.queuedAt;
}

function LastCallHint({
  backend,
  now,
}: {
  backend: BackendHealth;
  now: Date;
}) {
  const last = backend.recentCalls[0];
  if (!last) {
    return (
      <div className="backend-health__last-call-hint">no calls observed</div>
    );
  }
  if (last.failureReason) {
    return (
      <div className="backend-health__last-call-hint">
        last call failed:{" "}
        <span className="backend-health__last-call-reason">
          {last.failureReason}
        </span>{" "}
        · {ageString(lastCallEndedAt(last), now)}
      </div>
    );
  }
  return (
    <div className="backend-health__last-call-hint">
      last call{" "}
      <span
        className="backend-health__last-call-reason"
        data-tone="ok"
      >
        {last.callState}
      </span>{" "}
      · {ageString(lastCallEndedAt(last), now)}
    </div>
  );
}

function ConfigCell({
  label,
  value,
  tone,
  muted,
}: {
  label: string;
  value: string;
  tone?: "success" | "warning" | "error";
  muted?: boolean;
}) {
  return (
    <div className="backend-health__config-cell">
      <span className="backend-health__config-label">{label}</span>
      <span
        className={
          "backend-health__config-value" +
          (muted ? " backend-health__config-value--muted" : "")
        }
        data-tone={tone}
      >
        {value}
      </span>
    </div>
  );
}

function CallsTable({
  calls,
  now,
}: {
  calls: InferenceCallSummary[];
  now: Date;
}) {
  if (calls.length === 0) {
    return (
      <div className="backend-health__empty-calls">
        No InferenceCall records for this backend. Either no work has been
        routed here yet, or all records have aged out of the query window.
      </div>
    );
  }
  return (
    <table className="backend-health__calls-table">
      <thead>
        <tr>
          <th scope="col">seq</th>
          <th scope="col">kind</th>
          <th scope="col">state</th>
          <th scope="col">queue_depth</th>
          <th scope="col">failure_reason</th>
          <th scope="col">tokens (p / c)</th>
          <th scope="col">age</th>
        </tr>
      </thead>
      <tbody>
        {calls.map((c) => (
          <tr key={c.callId || `${c.callSeq}`}>
            <td>{c.callSeq}</td>
            <td>{c.callKind}</td>
            <td>
              <span
                className="backend-health__call-state"
                data-state={c.callState}
              >
                {c.callState}
              </span>
            </td>
            <td>{c.queueDepthAtEnqueue ?? "—"}</td>
            <td className="backend-health__call-reason">
              {c.failureReason ?? "—"}
            </td>
            <td>
              {c.promptTokens != null
                ? `${c.promptTokens} / ${c.completionTokens ?? "—"}`
                : "—"}
            </td>
            <td>{ageString(lastCallEndedAt(c), now)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export type BackendHealthRowProps = {
  backend: BackendHealth;
  now: Date;
};

export function BackendHealthRow({ backend, now }: BackendHealthRowProps) {
  const [expanded, setExpanded] = useState(false);
  const state = backend.displayState;
  const detailId = `backend-health-detail-${backend.backendId}`;

  return (
    <li
      className="backend-health__row"
      data-state={state}
      data-expanded={expanded ? "true" : "false"}
      data-backend-id={backend.backendId}
    >
      <button
        type="button"
        className="backend-health__summary"
        aria-expanded={expanded}
        aria-controls={detailId}
        onClick={() => setExpanded((v) => !v)}
      >
        <span
          className="backend-health__status-glyph"
          data-state={state}
          aria-hidden="true"
        >
          {STATE_GLYPH[state]}
        </span>
        <div className="backend-health__identity">
          <div className="backend-health__name">
            <span>{backend.name}</span>
            <span className="backend-health__kind-badge">
              {backend.providerKind}
            </span>
          </div>
          <div className="backend-health__endpoint">{backend.endpoint}</div>
          <LastCallHint backend={backend} now={now} />
        </div>
        <span className="backend-health__state-label" data-state={state}>
          {STATE_LABEL[state]}
        </span>
        <span className="backend-health__chevron" aria-hidden="true">
          ›
        </span>
      </button>
      {expanded ? (
        <div className="backend-health__detail" id={detailId}>
          <section className="backend-health__detail-section">
            <h3 className="backend-health__detail-heading">
              Admission policy &amp; probe
            </h3>
            <div className="backend-health__config-grid">
              <ConfigCell
                label="enabled"
                value={String(backend.enabled)}
                tone={backend.enabled ? "success" : "warning"}
              />
              <ConfigCell
                label="probe_status"
                value={backend.probeStatus}
                tone={probeTone(backend.probeStatus) ?? undefined}
              />
              <ConfigCell
                label="last_probe"
                value={backend.lastProbe ? ageString(backend.lastProbe, now) : "never"}
                muted={!backend.lastProbe}
              />
              <ConfigCell
                label="max_concurrent"
                value={String(backend.maxConcurrent)}
              />
              <ConfigCell
                label="max_queue_depth"
                value={String(backend.maxQueueDepth)}
              />
            </div>
          </section>
          <section className="backend-health__detail-section">
            <h3 className="backend-health__detail-heading">Models</h3>
            {backend.models.length === 0 ? (
              <span className="backend-health__config-value backend-health__config-value--muted">
                (no models declared)
              </span>
            ) : (
              <div className="backend-health__model-chips">
                {backend.models.map((m) => (
                  <span key={m} className="backend-health__model-chip">
                    {m}
                  </span>
                ))}
              </div>
            )}
          </section>
          <section className="backend-health__detail-section">
            <h3 className="backend-health__detail-heading">
              Recent calls (InferenceCall, last 10)
            </h3>
            <CallsTable calls={backend.recentCalls} now={now} />
          </section>
        </div>
      ) : null}
    </li>
  );
}
