import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";
import { createFixtureHelpers } from "./live-bridge-runner/adapter";

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
 *
 * IMPORTANT: The write is issued to the *remote* node (via the test-only
 * bridge endpoint /desktop/test-fixture/remote-save-behavior) and the read
 * is issued to the *desktop* node (via fetchSnapshot → desktop_core).
 * This write-on-A → visible-on-B chain is the actual D1/D2 cross-node
 * propagation witness.  A same-node roundtrip would not validate the P2P
 * subscription path at all.
 */
describeLive("Tauri app live replication (EventDelivery witnesses)", () => {
  // Witness: D1_delivery_convergence + D2_fair_delivery_latency.
  // Write a behavior document on the *remote* node (node A).  Read the
  // desktop snapshot from the *desktop* node (node B).  The mutation must
  // appear on B within the 5-second window, proving that iroh gossip +
  // the subscription path delivers the document without waiting for the
  // rescan tick.  If the test takes >5s the latency guarantee (D2) is
  // violated — DO NOT loosen this timeout.
  it("D1/D2: a remote write converges on the desktop node within the subscription window", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const behavior =
        deployment.behaviors.find((b) => b.isDefault) ?? deployment.behaviors[0];
      expect(behavior).toBeDefined();

      const sentinel = `repl-d1-${Date.now()}`;

      // Write on the remote node (node A).  The bridge runner's
      // /desktop/test-fixture/remote-save-behavior endpoint targets
      // fixture.remote_core() rather than fixture.desktop_core().
      const fixture = createFixtureHelpers(runner);
      await fixture.saveBehaviorConfigOnRemote({
        agentDid: runner.agentDid,
        behaviorId: behavior!.behaviorId,
        displayName: behavior!.displayName,
        systemPrompt: `${behavior!.systemPrompt ?? ""} ${sentinel}`,
        backendId: behavior!.backendId ?? null,
        toolSelectionId: behavior!.toolSelectionId ?? null,
        inferenceProfileId: behavior!.inferenceProfileId ?? null,
      });
      logTurn(
        `D1/D2 remote write issued behaviorId=${behavior!.behaviorId} sentinel=${sentinel}`,
      );

      // Read on the desktop node (node B).  The assertion must pass inside
      // the 5-second D2 window.  If propagation takes longer, the test
      // fails — that is a real D2 violation, not a test artifact.
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
      logTurn(
        `D1/D2 witnessed: remote write → desktop snapshot behaviorId=${behavior!.behaviorId} sentinel=${sentinel}`,
      );
    });
  }, 180_000);
});
