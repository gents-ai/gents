import { useState } from "react";

import type {
  ConversationLoadingAction,
  ConversationLoadingStatus as ConversationLoadingStatusView,
} from "../lib/loadingStatus";

export type ConversationLoadingStatusProps = {
  status: ConversationLoadingStatusView | null;
  onRetryLocal?: () => void | Promise<unknown>;
  onRetryHydration?: () => void | Promise<unknown>;
  onReconnect?: () => void | Promise<unknown>;
  onConfigureInference?: () => void | Promise<unknown>;
};

export function ConversationLoadingStatus({
  status,
  onRetryLocal,
  onRetryHydration,
  onReconnect,
  onConfigureInference,
}: ConversationLoadingStatusProps) {
  const [busyAction, setBusyAction] = useState<ConversationLoadingAction | null>(null);
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
      className={`conversation-loading-status is-${status.phase}`}
      data-loading-layer={status.layer}
      data-loading-phase={status.phase}
      data-testid="conversation-loading-status"
      role={failed ? "alert" : "status"}
    >
      <span className="conversation-loading-copy">
        {active ? (
          <span aria-hidden="true" className="conversation-loading-pulse" />
        ) : null}
        <span>
          <strong>{status.title}</strong>
          <span>{status.detail}</span>
        </span>
      </span>
      {status.action && handler ? (
        <button
          className="chip-button"
          data-testid={`conversation-loading-${status.action}`}
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

function actionLabel(action: ConversationLoadingAction): string {
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

function busyLabel(action: ConversationLoadingAction): string {
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
