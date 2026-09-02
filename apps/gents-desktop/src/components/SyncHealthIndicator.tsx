import {
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
  syncHealthState,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";

export type SyncHealthIndicatorProps = {
  syncHealth?: SyncHealthView | null;
};

export function SyncHealthIndicator({ syncHealth = null }: SyncHealthIndicatorProps) {
  const status = projectSyncOperationalStatus(syncHealth);
  const state = syncHealthState(syncHealth) ?? "checking";
  const label = status.shortLabel;
  const diagnostics = syncHealthDiagnostics(syncHealth);

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
        className="sync-health-details mobile-viewport-popover"
        data-scroll-owner="popover"
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
            <dt>Last error</dt>
            <dd>{diagnostics.lastError ?? "—"}</dd>
          </div>
          <div>
            <dt>Connected peers</dt>
            <dd>{diagnostics.connectedPeerCount}</dd>
          </div>
          <div>
            <dt>Pending receive DAGs</dt>
            <dd>{diagnostics.pendingDagCount ?? "—"}</dd>
          </div>
          <div>
            <dt>Persisted pending DAGs</dt>
            <dd>{diagnostics.persistedPendingDagCount ?? "—"}</dd>
          </div>
          <div>
            <dt>Pending push retries</dt>
            <dd>{diagnostics.pushRetryMarkerCount ?? "—"}</dd>
          </div>
          <div>
            <dt>Exhausted fetches (total)</dt>
            <dd>{diagnostics.exhaustedFetchCount ?? "—"}</dd>
          </div>
          <div>
            <dt>Quarantined DAGs</dt>
            <dd>{diagnostics.quarantinedDagCount ?? "—"}</dd>
          </div>
        </dl>
      </div>
    </details>
  );
}
