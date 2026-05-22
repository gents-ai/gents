import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

/**
 * Live conformance witness for EventDelivery D1/D2.
 *
 * Lean source: crates/defra-agent/proofs/Proofs/EventDelivery/Properties.lean
 *   - D1_delivery_convergence at :62
 *   - D2_fair_delivery_latency at :119
 *
 * Each `it()` block is annotated with the Lean theorem it witnesses.
 * If a theorem's statement changes, this file MUST be updated to match —
 * see CLAUDE.md "Development Flow": spec changes are authoritative.
 *
 * P2P replication itself (defradb.rs add_replicator + iroh gossip) is
 * not Lean-modeled in this repo — the persisted-on-A → persisted-on-B
 * chain is a conformance witness, not a theorem witness.
 */
describeLive("Tauri app live replication (EventDelivery witnesses)", () => {
  // Witness: D1_delivery_convergence + D2_fair_delivery_latency.
  // A persisted doc on the remote node reaches the consumer's snapshot
  // view on the desktop node via the subscription path (not the rescan
  // tick) inside the 5-second window that proves the subscription-path
  // latency bound.
  it("D1/D2: a persisted behavior mutation converges on the peer within the subscription window", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const behavior =
        deployment.behaviors.find((b) => b.isDefault) ?? deployment.behaviors[0];
      expect(behavior).toBeDefined();

      const sentinel = `repl-d1-${Date.now()}`;
      await runner.adapter.saveBehaviorConfig({
        agentDid: runner.agentDid,
        behaviorId: behavior!.behaviorId,
        displayName: behavior!.displayName,
        systemPrompt: `${behavior!.systemPrompt ?? ""} ${sentinel}`,
        backendId: behavior!.backendId ?? null,
        toolSelectionId: behavior!.toolSelectionId ?? null,
        inferenceProfileId: behavior!.inferenceProfileId ?? null,
      });

      // D2 says the subscription path is 2-action: enqueue + handle. In
      // operational terms that's "noticeably under one rescan cadence."
      // A 5-second deadline ensures we never satisfy the assertion via
      // a rescan-tick fallback.
      await waitFor(
        async () => {
          const snapshot = await runner.fetchSnapshot();
          const replicated = snapshot.client?.deployments[0]?.behaviors.find(
            (b) => b.behaviorId === behavior!.behaviorId,
          );
          expect(replicated?.systemPrompt).toContain(sentinel);
        },
        { timeout: 5_000, interval: 200 },
      );
      logTurn(`D1/D2 witnessed behaviorId=${behavior!.behaviorId} sentinel=${sentinel}`);
    });
  }, 180_000);
});
