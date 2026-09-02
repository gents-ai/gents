import { describe, expect, it } from "vitest";

import {
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
  syncHealthState,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    lastError: null,
    connectedPeerCount: 1,
    pendingDagCount: 0,
    persistedPendingDagCount: 0,
    pushRetryMarkerCount: 0,
    exhaustedFetchCount: 0,
    quarantinedDagCount: 0,
    ...overrides,
  };
}

describe("projectSyncOperationalStatus", () => {
  it("names unobserved, healthy, syncing, offline, and failed", () => {
    expect(projectSyncOperationalStatus(null).shortLabel).toBe("Checking sync");
    expect(projectSyncOperationalStatus(health())?.shortLabel).toBe("Sync healthy");
    expect(projectSyncOperationalStatus(health({ state: "syncing" }))?.shortLabel).toBe(
      "Syncing",
    );
    expect(projectSyncOperationalStatus(health({ state: "offline" }))?.shortLabel).toBe(
      "Offline",
    );
    expect(projectSyncOperationalStatus(health({ state: "failed" }))?.shortLabel).toBe(
      "Sync failed",
    );
    expect(syncHealthState(null)).toBeNull();
    expect(syncHealthState(health({ state: "future-state" }))).toBeNull();
  });
});

describe("syncHealthDiagnostics", () => {
  it("exposes the database gauges without rebuilding route retries", () => {
    const diagnostics = syncHealthDiagnostics(
      health({
        state: "syncing",
        lastError: "provider exhaustion",
        pendingDagCount: 2,
        persistedPendingDagCount: 3,
        pushRetryMarkerCount: 4,
        exhaustedFetchCount: 5,
      }),
    );
    expect(diagnostics.state).toBe("syncing");
    expect(diagnostics.pendingDagCount).toBe(2);
    expect(diagnostics.persistedPendingDagCount).toBe(3);
    expect(diagnostics.pushRetryMarkerCount).toBe(4);
    expect(diagnostics.exhaustedFetchCount).toBe(5);
  });
});
