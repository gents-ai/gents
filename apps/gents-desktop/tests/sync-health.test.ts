import { describe, expect, it } from "vitest";

import type { SyncHealthView } from "@source-inc/gents-desktop-client";
import {
  formatElapsedSince,
  syncHealthDiagnostics,
  syncHealthLabel,
  syncHealthState,
} from "../src/lib/syncHealth";

function health(overrides: Partial<SyncHealthView> = {}): SyncHealthView {
  return {
    state: "healthy",
    since: null,
    offlineSince: null,
    stalledSince: null,
    lastErrorClass: null,
    lastError: null,
    pairingRetryCount: 0,
    routeRetryCount: 0,
    connectedPeerCount: 1,
    ...overrides,
  };
}

describe("syncHealthLabel", () => {
  it("names healthy, syncing, stalled, offline-since, and failed", () => {
    const now = Date.parse("2026-08-27T12:00:00Z");
    expect(syncHealthLabel(health(), now)).toBe("Sync healthy");
    expect(syncHealthLabel(health({ state: "syncing" }), now)).toBe("Syncing");
    expect(
      syncHealthLabel(
        health({
          state: "stalled",
          stalledSince: "2026-08-27T11:50:00Z",
        }),
        now,
      ),
    ).toBe("Sync stalled for 10m");
    expect(
      syncHealthLabel(
        health({
          state: "offline",
          offlineSince: "2026-08-27T10:00:00Z",
        }),
        now,
      ),
    ).toBe("Offline for 2h");
    expect(syncHealthLabel(health({ state: "failed" }), now)).toBe("Sync failed");
    expect(syncHealthState(null)).toBeNull();
    expect(syncHealthState(health({ state: "future-state" }))).toBeNull();
    expect(formatElapsedSince("2026-08-27T11:59:20Z", now)).toBe("40s");
  });
});

describe("syncHealthDiagnostics", () => {
  it("exposes global pairing, route, error-class, and stuck-since counters", () => {
    const diagnostics = syncHealthDiagnostics(
      health({
        state: "stalled",
        lastErrorClass: "RpcTimeout",
        lastError: "timeout",
        pairingRetryCount: 6,
        stalledSince: "2026-08-27T11:00:00Z",
      }),
      [
        {
          label: "Studio",
          agentDid: "did:test:agent",
          dialSucceeded: true,
          lastError: null,
          pairing: [
            {
              collectionId: "AgentSession",
              pairingRetryCount: 6,
              lastRetryAt: "2026-08-27T11:00:00Z",
              lastRetryErrorClass: "RpcTimeout",
              stuckSince: "2026-08-27T11:00:00Z",
            },
          ],
          routes: [
            {
              routeId: "r1",
              direction: "client-to-runtime",
              directoryId: "peer-1",
              transportPeerId: null,
              address: null,
              template: "machine",
              desired: true,
              applied: false,
              liveMatch: false,
              filterSummary: "machine",
              lastError: "timeout",
              retryCount: 2,
              lastRetryAt: "2026-08-27T11:00:00Z",
              lastRetryErrorClass: "RpcTimeout",
            },
          ],
        } as never,
      ],
    );
    expect(diagnostics.state).toBe("stalled");
    expect(diagnostics.lastErrorClass).toBe("RpcTimeout");
    expect(diagnostics.pairingRetryCount).toBe(6);
    expect(diagnostics.peers[0]?.pairing[0]?.stuckSince).toBe("2026-08-27T11:00:00Z");
    expect(diagnostics.peers[0]?.routes[0]?.retryCount).toBe(2);
  });
});
