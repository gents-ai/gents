import { describe, expect, it } from "vitest";

import type { PairingCollectionStatusView, SyncHealthView } from "./index.js";

describe("sync health contract types", () => {
  it("exports separate pairing and sync views", () => {
    const pairing: PairingCollectionStatusView = {
      collectionId: "AgentSession",
      pairingRetryCount: 2,
      lastRetryAt: "2026-08-25T12:00:00Z",
      lastRetryErrorClass: "RpcTimeout",
      stuckSince: null,
    };
    const health: SyncHealthView = {
      state: "syncing",
      lastError: null,
      connectedPeerCount: 1,
      pendingDagCount: 1,
      persistedPendingDagCount: 1,
      pushRetryMarkerCount: 0,
      exhaustedFetchCount: 2,
      quarantinedDagCount: 0,
    };
    expect(health.state).toBe("syncing");
    expect(pairing.pairingRetryCount).toBe(2);
  });
});
