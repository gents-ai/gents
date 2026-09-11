import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";
import { createFixtureHelpers } from "./live-bridge-runner/adapter";

/**
 * Live conformance witness for EventDelivery D1/D2.
 *
 * Lean source: crates/gents/proofs/Proofs/EventDelivery/Properties.lean
 *   - D1_delivery_convergence at :62
 *   - D2_fair_delivery_latency at :119
 *
 * Each `it()` block is annotated with the Lean theorem it witnesses.
 * If a theorem's statement changes, this file MUST be updated to match —
 * see AGENTS.md "Foundation": spec changes are authoritative.
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
  it("D1/D2: a remote write converges on the desktop node within the subscription window", async () => {
    await withLiveDesktop(async ({ runner, deployment }) => {
      const behavior =
        deployment.behaviors.find((b) => b.isDefault) ?? deployment.behaviors[0];
      expect(behavior).toBeDefined();

      const sentinel = `repl-d1-${Date.now()}`;

      const fixture = createFixtureHelpers(runner);
      // Canonical BehaviorSaveRequest: one AgentBehavior document. The system
      // prompt lives on the referenced AgentContext, so the sentinel is carried
      // on the behavior document's display_name; the context linkage
      // (behavior.contextId → deployment.contexts) is asserted unchanged below.
      await fixture.saveBehaviorConfigOnRemote({
        document: {
          agent_did: behavior!.agentDid,
          behavior_id: behavior!.behaviorId,
          display_name: `${behavior!.displayName} ${sentinel}`,
          description: behavior!.description,
          context_id: behavior!.contextId,
          inference_profile_id: behavior!.inferenceProfileId!,
          enabled: behavior!.enabled,
          tags: behavior!.tags,
          created_at: behavior!.createdAt,
        },
      });
      logTurn(
        `D1/D2 remote write issued behaviorId=${behavior!.behaviorId} sentinel=${sentinel}`,
      );

      await waitFor(
        async () => {
          const snapshot = await runner.fetchSnapshot();
          const replicated = snapshot.client?.deployments[0]?.behaviors.find(
            (b) => b.behaviorId === behavior!.behaviorId,
          );
          expect(replicated?.displayName).toContain(sentinel);
          const replicatedContext = snapshot.client?.deployments[0]?.contexts.find(
            (candidate) => candidate.context_id === replicated?.contextId,
          );
          expect(
            replicatedContext?.tools_id,
            "replicated behavior must keep its canonical context linkage",
          ).toBeDefined();
        },
        { timeout: 5_000, interval: 200 },
      );
      logTurn(
        `D1/D2 witnessed: remote write → desktop snapshot behaviorId=${behavior!.behaviorId} sentinel=${sentinel}`,
      );
    });
  }, 180_000);
});
