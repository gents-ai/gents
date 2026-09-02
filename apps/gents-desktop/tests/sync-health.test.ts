import { describe, expect, it } from "vitest";

import {
  formatElapsedSince,
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
  syncHealthState,
  type SyncHealthView,
} from "@source-inc/gents-desktop-client";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    since: null,
    offlineSince: null,
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
  it("names healthy, syncing, stalled, offline-since, and failed", () => {
    const now = Date.parse("2026-08-27T12:00:00Z");
    expect(projectSyncOperationalStatus(health(), now)?.shortLabel).toBe(
      "Sync healthy",
    );
    expect(
      projectSyncOperationalStatus(health({ state: "syncing" }), now)?.shortLabel,
    ).toBe("Syncing");
    expect(
      projectSyncOperationalStatus(health({ state: "stalled" }), now)?.shortLabel,
    ).toBe("Sync stalled");
    expect(
      projectSyncOperationalStatus(
        health({
          state: "offline",
          offlineSince: "2026-08-27T10:00:00Z",
        }),
        now,
      )?.shortLabel,
    ).toBe("Offline for 2h");
    expect(
      projectSyncOperationalStatus(health({ state: "failed" }), now)?.shortLabel,
    ).toBe("Sync failed");
    expect(syncHealthState(null)).toBeNull();
    expect(syncHealthState(health({ state: "future-state" }))).toBeNull();
    expect(formatElapsedSince("2026-08-27T11:59:20Z", now)).toBe("40s");
  });
});

describe("syncHealthDiagnostics", () => {
  it("exposes the database gauges without rebuilding route retries", () => {
    const diagnostics = syncHealthDiagnostics(
      health({
        state: "stalled",
        lastError: "provider exhaustion",
        pendingDagCount: 2,
        persistedPendingDagCount: 3,
        pushRetryMarkerCount: 4,
        exhaustedFetchCount: 5,
      }),
    );
    expect(diagnostics.state).toBe("stalled");
    expect(diagnostics.pendingDagCount).toBe(2);
    expect(diagnostics.persistedPendingDagCount).toBe(3);
    expect(diagnostics.pushRetryMarkerCount).toBe(4);
    expect(diagnostics.exhaustedFetchCount).toBe(5);
  });
});
