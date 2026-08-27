import { useState } from "react";

import type { SessionHydrationView } from "@source-inc/gents-desktop-client";

import {
  sessionHydrationLabel,
  sessionHydrationNeedsRetry,
  visibleSessionHydration,
} from "../lib/sessionHydration";

export type SessionHydrationStatusProps = {
  hydration?: SessionHydrationView | null;
  sessionId: string | null;
  agentDid?: string | null;
  onRetry?: () => void | Promise<unknown>;
};

export function SessionHydrationStatus({
  hydration,
  sessionId,
  agentDid,
  onRetry,
}: SessionHydrationStatusProps) {
  const visible = visibleSessionHydration(hydration, sessionId, agentDid);
  const [retrying, setRetrying] = useState(false);
  if (!visible) return null;

  const label = sessionHydrationLabel(visible);
  const failed = sessionHydrationNeedsRetry(visible);
  const inFlight = visible.phase === "requested" || visible.phase === "serving";

  async function retry() {
    if (!onRetry || retrying) return;
    setRetrying(true);
    try {
      await onRetry();
    } finally {
      setRetrying(false);
    }
  }

  return (
    <div
      aria-live={failed ? "assertive" : "polite"}
      className={`session-hydration-status is-${visible.phase}`}
      data-hydration-phase={visible.phase}
      data-testid="session-hydration-status"
      role={failed ? "alert" : "status"}
    >
      <span className="session-hydration-copy">
        {inFlight ? (
          <span aria-hidden="true" className="session-hydration-pulse" />
        ) : null}
        {label}
      </span>
      {failed && onRetry ? (
        <button
          className="chip-button"
          data-testid="session-hydration-retry"
          disabled={retrying}
          onClick={() => void retry()}
          type="button"
        >
          {retrying ? "Retrying…" : "Retry"}
        </button>
      ) : null}
    </div>
  );
}
