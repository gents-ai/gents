import { useContext, useMemo } from "react";

import type {
  DesktopOperationsSnapshotRequest,
  RuntimeView,
} from "@source-inc/gents-desktop-client";
import { OperationsRailContext } from "../operations/operationsRailContext.js";
import { BackgroundedToolsFilters } from "./BackgroundedToolsFilters.js";
import { BackgroundedToolsSummary } from "./BackgroundedToolsSummary.js";
import { BackgroundedToolsTable } from "./BackgroundedToolsTable.js";
import { StuckDiagnostics } from "./StuckDiagnostics.js";
import { useBackgroundedToolsModel } from "./useBackgroundedToolsModel.js";
import { useOperationsSnapshot } from "./useOperationsSnapshot.js";

export type BackgroundedToolsPanelProps = {
  agentDid?: string | null;
  rootRequestId?: string | null;
  /** Selected deployment's runtime doc — carries real executor slot data. */
  runtime?: RuntimeView | null;
  /** Focus the lineage view on this row's parent request. */
  onOpenLineage?: (requestId: string) => void;
  /** Begin the interrupt (preview + cascade dialog) flow for this row's parent request. */
  onInterruptParent?: (requestId: string) => void;
  /** Resubmit a stale/expired request as a fresh one. */
  onResendRequest?: (requestId: string) => void;
  /** Injectable hook for non-native hosts and deterministic component tests. */
  useSnapshot?: typeof useOperationsSnapshot;
};

export function BackgroundedToolsPanel({
  agentDid,
  rootRequestId,
  runtime,
  onOpenLineage,
  onInterruptParent,
  onResendRequest,
  useSnapshot = useOperationsSnapshot,
}: BackgroundedToolsPanelProps = {}) {
  // Nullable on purpose: the panel also renders outside the operations rail
  // (tests, future standalone surfaces), where tab switching is a no-op.
  const rail = useContext(OperationsRailContext);
  const request = useMemo<DesktopOperationsSnapshotRequest>(
    () => ({
      agentDid: agentDid ?? null,
      rootRequestId: rootRequestId ?? null,
    }),
    [agentDid, rootRequestId],
  );
  const { snapshot, error, isLoading } = useSnapshot(request);
  const model = useBackgroundedToolsModel(snapshot);

  // A failed poll only replaces the panel when there is no prior snapshot.
  if (error && !snapshot) {
    return (
      <section className="background-tools-panel" aria-label="Background tools">
        <div className="empty-state">
          <span className="glyph" aria-hidden="true">
            ○
          </span>
          Snapshot bridge unavailable: {error}
        </div>
      </section>
    );
  }

  return (
    <section className="background-tools-panel" aria-label="Background tools">
      {error ? (
        <div className="muted small" data-testid="ops-stale-note" role="status">
          Live updates interrupted — showing the last snapshot. ({error})
        </div>
      ) : null}
      <StuckDiagnostics
        diagnostics={snapshot?.stuckDiagnostics ?? []}
        onResendRequest={onResendRequest}
      />
      <BackgroundedToolsFilters
        awaitFilters={model.awaitFilters}
        awaitOptions={model.awaitOptions}
        hideHealthy={model.hideHealthy}
        parentFilter={model.parentFilter}
        parents={model.parents}
        stateFilters={model.stateFilters}
        stateOptions={model.stateOptions}
        onAwaitFilterToggle={model.toggleAwaitFilter}
        onHideHealthyChange={model.setHideHealthy}
        onParentFilterChange={model.setParentFilter}
        onStateFilterToggle={model.toggleStateFilter}
      />
      <BackgroundedToolsSummary
        filteredCount={model.filtered.length}
        projectedCount={model.projected.length}
        runtime={runtime}
      />
      <BackgroundedToolsTable
        isLoading={isLoading}
        rows={model.filtered}
        sortDir={model.sortDir}
        sortKey={model.sortKey}
        onActivateLineage={
          rail ? () => rail.setActiveTab("lineage") : undefined
        }
        onInterruptParent={onInterruptParent}
        onOpenLineage={onOpenLineage}
        onSort={model.onSort}
      />
    </section>
  );
}
