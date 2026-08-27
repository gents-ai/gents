import { describe, expect, it } from "vitest";

import type {
  PairingCollectionStatusView,
  SessionHydrationView,
  SyncHealthView,
} from "./index.js";

describe("sync health contract types", () => {
  it("exports hydration, pairing, and derived sync views", () => {
    const hydration: SessionHydrationView = {
      sessionId: "session-1",
      agentDid: "did:test:agent",
      phase: "serving",
      mergedCount: 4,
      servedCount: 11,
    };
    const pairing: PairingCollectionStatusView = {
      collectionId: "AgentSession",
      pairingRetryCount: 2,
      lastRetryAt: "2026-08-25T12:00:00Z",
      lastRetryErrorClass: "RpcTimeout",
      stuckSince: null,
    };
    const health: SyncHealthView = {
      state: "stalled",
      since: pairing.stuckSince,
      offlineSince: null,
      stalledSince: "2026-08-25T12:00:00Z",
      lastErrorClass: "RpcTimeout",
      lastError: null,
      pairingRetryCount: 2,
      routeRetryCount: 0,
      connectedPeerCount: 1,
      hydration,
    };
    expect(health.state).toBe("stalled");
    expect(health.hydration.mergedCount).toBe(4);
    expect(pairing.pairingRetryCount).toBe(2);
  });
});
