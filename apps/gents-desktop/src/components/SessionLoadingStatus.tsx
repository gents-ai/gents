import { useState } from "react";

import type {
  SessionLoadingAction,
  SessionLoadingStatus as SessionLoadingStatusView,
} from "../lib/loadingStatus";

export type SessionLoadingStatusProps = {
  status: SessionLoadingStatusView | null;
  onRetryLocal?: () => void | Promise<unknown>;
  onRetryHydration?: () => void | Promise<unknown>;
  onReconnect?: () => void | Promise<unknown>;
  onConfigureInference?: () => void | Promise<unknown>;
};

export function SessionLoadingStatus({
  status,
  onRetryLocal,
  onRetryHydration,
  onReconnect,
  onConfigureInference,
}: SessionLoadingStatusProps) {
  const [busyAction, setBusyAction] = useState<SessionLoadingAction | null>(null);
  if (!status) return null;

  const handler =
    status.action === "retryLocal"
      ? onRetryLocal
      : status.action === "retryHydration"
        ? onRetryHydration
        : status.action === "reconnect"
          ? onReconnect
          : status.action === "configureInference"
            ? onConfigureInference
            : undefined;

  async function act() {
    if (!status?.action || !handler || busyAction) return;
    setBusyAction(status.action);
    try {
      await handler();
    } catch {
      // Recovery owners publish the resulting error/status; keep the click
      // promise from becoming an unhandled UI rejection.
    } finally {
      setBusyAction(null);
    }
  }

  const active = status.phase === "loading";
  const failed = status.phase === "failed";
  return (
    <div
      aria-live={failed ? "assertive" : "polite"}
      className={`session-loading-status is-${status.phase}`}
      data-loading-layer={status.layer}
      data-loading-phase={status.phase}
      data-testid="session-loading-status"
      role={failed ? "alert" : "status"}
    >
      <span className="session-loading-copy">
        {active ? <span aria-hidden="true" className="session-loading-pulse" /> : null}
        <span>
          <strong>{status.title}</strong>
          <span>{status.detail}</span>
        </span>
      </span>
      {status.action && handler ? (
        <button
          className="chip-button"
          data-testid={`session-loading-${status.action}`}
          disabled={busyAction !== null}
          onClick={() => void act()}
          type="button"
        >
          {busyAction === status.action
            ? busyLabel(status.action)
            : actionLabel(status.action)}
        </button>
      ) : null}
    </div>
  );
}

function actionLabel(action: SessionLoadingAction): string {
  switch (action) {
    case "retryLocal":
    case "retryHydration":
      return "Try again";
    case "reconnect":
      return "Reconnect";
    case "configureInference":
      return "Configure inference";
  }
}

function busyLabel(action: SessionLoadingAction): string {
  switch (action) {
    case "retryLocal":
    case "retryHydration":
      return "Retrying…";
    case "reconnect":
      return "Reconnecting…";
    case "configureInference":
      return "Opening…";
  }
}
