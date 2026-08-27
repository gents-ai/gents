import type { DeploymentView, SyncHealthView } from "@source-inc/gents-desktop-client";

import {
  syncHealthDiagnostics,
  syncHealthLabel,
  syncHealthState,
} from "../lib/syncHealth";

export type SyncHealthIndicatorProps = {
  deployments?: DeploymentView[];
  syncHealth?: SyncHealthView | null;
};

export function SyncHealthIndicator({
  deployments = [],
  syncHealth = null,
}: SyncHealthIndicatorProps) {
  const state = syncHealthState(syncHealth);
  if (!state) return null;
  const label = syncHealthLabel(syncHealth);
  const diagnostics = syncHealthDiagnostics(syncHealth, deployments);
  const hydration = diagnostics.hydration;

  return (
    <details
      className={`sync-health-indicator is-${state}`}
      data-sync-state={state}
      data-testid="sync-health-indicator"
    >
      <summary
        aria-label={`${label}. Show sync diagnostics.`}
        data-testid="sync-health-summary"
      >
        <span aria-hidden="true" className="sync-health-dot" />
        <span>{label}</span>
      </summary>
      <div
        className="sync-health-details"
        data-testid="sync-health-details"
        role="region"
        aria-label="Sync diagnostics"
      >
        <dl>
          <div>
            <dt>State</dt>
            <dd>{diagnostics.state}</dd>
          </div>
          <div>
            <dt>Since</dt>
            <dd>{diagnostics.since ?? "—"}</dd>
          </div>
          <div>
            <dt>Offline since</dt>
            <dd>{diagnostics.offlineSince ?? "—"}</dd>
          </div>
          <div>
            <dt>Stalled since</dt>
            <dd>{diagnostics.stalledSince ?? "—"}</dd>
          </div>
          <div>
            <dt>Error class</dt>
            <dd>{diagnostics.lastErrorClass ?? "—"}</dd>
          </div>
          <div>
            <dt>Last error</dt>
            <dd>{diagnostics.lastError ?? "—"}</dd>
          </div>
          <div>
            <dt>Pairing retries</dt>
            <dd>{diagnostics.pairingRetryCount}</dd>
          </div>
          <div>
            <dt>Route retries</dt>
            <dd>{diagnostics.routeRetryCount}</dd>
          </div>
          <div>
            <dt>Connected peers</dt>
            <dd>{diagnostics.connectedPeerCount}</dd>
          </div>
          <div>
            <dt>Hydration</dt>
            <dd>
              {hydration
                ? `${hydration.phase} · ${hydration.mergedCount}${
                    hydration.servedCount == null ? "" : ` of ${hydration.servedCount}`
                  }${hydration.sessionId ? ` · ${hydration.sessionId}` : ""}`
                : "—"}
            </dd>
          </div>
        </dl>
        {diagnostics.peers.length > 0 ? (
          <ul className="sync-health-peers">
            {diagnostics.peers.map((peer) => (
              <li key={peer.agentDid}>
                <strong>{peer.label}</strong>
                <span>
                  {peer.dialSucceeded ? "connected" : "not connected"}
                  {peer.lastError ? ` · ${peer.lastError}` : ""}
                </span>
                {peer.pairing.map((pairing) => (
                  <span key={`${peer.agentDid}:${pairing.collectionId}`}>
                    {pairing.collectionId}: retries {pairing.pairingRetryCount}
                    {pairing.lastRetryErrorClass
                      ? ` · ${pairing.lastRetryErrorClass}`
                      : ""}
                    {pairing.stuckSince ? ` · stuck since ${pairing.stuckSince}` : ""}
                  </span>
                ))}
                {peer.routes.map((route) => (
                  <span key={route.routeId}>
                    {route.direction}: retries {route.retryCount}
                    {route.lastRetryErrorClass ? ` · ${route.lastRetryErrorClass}` : ""}
                    {route.lastError ? ` · ${route.lastError}` : ""}
                  </span>
                ))}
              </li>
            ))}
          </ul>
        ) : null}
      </div>
    </details>
  );
}
