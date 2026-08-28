import { useState } from "react";

import type {
  DesktopApiAdapter,
  HeldToolCallView,
} from "@source-inc/gents-desktop-client";
import { useOperationsApi } from "../../apiContext.js";
import { useToolCallHolds } from "./useToolCallHolds.js";

function normalizedArgs(args: string | null) {
  if (!args) {
    return null;
  }
  const trimmed = args.trim();
  if (!trimmed || trimmed === "{}") {
    return null;
  }
  return trimmed;
}

function argsPreview(args: string) {
  return args.length > 120 ? `${args.slice(0, 120)}…` : args;
}

function deadlineLabel(deadlineAt: string | null) {
  if (!deadlineAt) {
    return null;
  }
  const deadline = new Date(deadlineAt);
  if (Number.isNaN(deadline.getTime())) {
    return null;
  }
  const remainingMs = deadline.getTime() - Date.now();
  if (remainingMs <= 0) {
    return "deadline passed";
  }
  const minutes = Math.floor(remainingMs / 60_000);
  if (minutes >= 1) {
    return `${minutes}m left`;
  }
  return `${Math.max(1, Math.floor(remainingMs / 1000))}s left`;
}

export type HoldsPanelProps = {
  agentDid: string | null;
  api?: DesktopApiAdapter;
  hideWhenIdle?: boolean;
};

export function HoldsPanel({
  agentDid,
  api: explicitApi,
  hideWhenIdle = false,
}: HoldsPanelProps) {
  const api = useOperationsApi(explicitApi);
  const { holds, loading, error, refresh } = useToolCallHolds(agentDid, api);
  const [busyCallId, setBusyCallId] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [denyingCallId, setDenyingCallId] = useState<string | null>(null);
  const [denyReason, setDenyReason] = useState("");

  if (hideWhenIdle && !error && (holds == null || holds.length === 0)) {
    return null;
  }

  const resolve = async (
    hold: HeldToolCallView,
    approve: boolean,
    reason?: string,
  ) => {
    if (!agentDid) {
      return;
    }
    setBusyCallId(hold.toolCallId);
    setActionError(null);
    try {
      await api.resolveToolCallHold({
        agentDid,
        toolCallId: hold.toolCallId,
        approve,
        reason: reason?.trim() ? reason.trim() : null,
      });
      setDenyingCallId(null);
      setDenyReason("");
      await refresh();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyCallId(null);
    }
  };

  return (
    <section className="holds-panel" data-testid="holds-panel">
      <div className="holds-panel-header">
        <span className="holds-panel-title">Holds</span>
        <button
          className="ghost-button"
          data-testid="holds-refresh"
          onClick={() => {
            void refresh();
          }}
          type="button"
        >
          Refresh
        </button>
      </div>
      {error ? (
        <p className="holds-panel-error" data-testid="holds-error">
          {error}
        </p>
      ) : null}
      {actionError ? (
        <p className="holds-panel-error" data-testid="holds-action-error">
          {actionError}
        </p>
      ) : null}
      {loading && holds == null ? (
        <p className="holds-panel-empty">Loading…</p>
      ) : null}
      {holds != null && holds.length === 0 && !error ? (
        <p className="holds-panel-empty" data-testid="holds-empty">
          No tool calls awaiting approval.
        </p>
      ) : null}
      <ul className="holds-panel-list" data-scroll-owner="holds">
        {(holds ?? []).map((hold) => {
          const fullArgs = normalizedArgs(hold.args);
          const preview = fullArgs ? argsPreview(fullArgs) : null;
          const deadline = deadlineLabel(hold.deadlineAt);
          const busy = busyCallId === hold.toolCallId;
          const denying = denyingCallId === hold.toolCallId;
          return (
            <li
              className="holds-panel-row"
              data-testid={`hold-row-${hold.toolCallId}`}
              key={hold.toolCallId}
            >
              <div className="holds-panel-row-main">
                <span className="holds-panel-tool">
                  {hold.toolName ?? hold.toolCallId}
                </span>
                {preview ? (
                  <code
                    className="holds-panel-args"
                    data-testid={`hold-args-preview-${hold.toolCallId}`}
                  >
                    {preview}
                  </code>
                ) : null}
                {deadline ? (
                  <span className="holds-panel-deadline">{deadline}</span>
                ) : null}
              </div>
              {hold.requestId ? (
                <div className="holds-panel-row-meta">
                  request {hold.requestId}
                </div>
              ) : null}
              {fullArgs ? (
                <details
                  className="holds-panel-args-details"
                  data-testid={`hold-args-details-${hold.toolCallId}`}
                >
                  <summary data-testid={`hold-args-toggle-${hold.toolCallId}`}>
                    View full arguments
                  </summary>
                  <pre
                    className="holds-panel-args-full"
                    data-testid={`hold-args-full-${hold.toolCallId}`}
                  >
                    {fullArgs}
                  </pre>
                </details>
              ) : null}
              <div className="holds-panel-row-actions">
                <button
                  className="ghost-button holds-approve"
                  data-testid={`hold-approve-${hold.toolCallId}`}
                  disabled={busy}
                  onClick={() => {
                    void resolve(hold, true);
                  }}
                  type="button"
                >
                  Approve
                </button>
                {denying ? (
                  <>
                    <input
                      className="holds-deny-reason"
                      data-testid={`hold-deny-reason-${hold.toolCallId}`}
                      onChange={(event) => setDenyReason(event.target.value)}
                      placeholder="Reason (optional)"
                      value={denyReason}
                    />
                    <button
                      className="ghost-button holds-deny"
                      data-testid={`hold-deny-confirm-${hold.toolCallId}`}
                      disabled={busy}
                      onClick={() => {
                        void resolve(hold, false, denyReason);
                      }}
                      type="button"
                    >
                      Confirm deny
                    </button>
                    <button
                      className="ghost-button"
                      data-testid={`hold-deny-cancel-${hold.toolCallId}`}
                      onClick={() => {
                        setDenyingCallId(null);
                        setDenyReason("");
                      }}
                      type="button"
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    className="ghost-button holds-deny"
                    data-testid={`hold-deny-${hold.toolCallId}`}
                    disabled={busy}
                    onClick={() => {
                      setDenyingCallId(hold.toolCallId);
                      setDenyReason("");
                    }}
                    type="button"
                  >
                    Deny
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
