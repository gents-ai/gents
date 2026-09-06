import { screen, waitFor, within } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

/**
 * Live conformance witnesses for cascade-cancel properties.
 *
 * Lean source:
 *   - cascade_cancels_child (B3): crates/gents/proofs/Proofs/Background/Properties/Cancellation.lean:22
 *   - detach_does_not_cancel_child (B3'): :190 (not exercised here)
 *   - interrupted_request_cancels_live_linked_tools (C2): crates/gents/proofs/Proofs/CrossMachineComposed/ToolTermination.lean:54
 *
 * Each `it()` block is annotated with the Lean theorem(s) it witnesses.
 * If a theorem's statement changes, this file MUST be updated to match —
 * see AGENTS.md "Foundation": spec changes are authoritative.
 *
 * This suite is intentionally excluded from the default live sweep because it
 * requires live inference and can take several minutes per run. Run it explicitly:
 *
 *   npm run test:live:cascade -- --inference-url <url> --model-name <model>
 */
describeLive("Tauri app live cascade interrupt (B3 + C2 witnesses)", () => {
  it("B3+C2: cascade-mode interrupt drives a running child subagent to interrupted", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      const defaultBehavior = deployment.behaviors.find(
        (behavior) =>
          behavior.behaviorId === deployment.agentPrincipal.defaultBehaviorId,
      );
      const defaultTools = deployment.toolSelections.find(
        (s) => s.selectionId === defaultBehavior?.toolSelectionId,
      );
      const subagentTarget = defaultTools?.subagentTargets[0];
      expect(subagentTarget, "fixture must expose a subagent target").toBeDefined();

      await driver.ready();
      await driver.openChat();

      await driver.typeComposer(
        'Use the configured local subagent target. Call spawn_subagent with await_mode "background" and give the child this exact task: "Step 1: read workspace/AGENTS.md. Step 2: read workspace/README.md. Step 3: read workspace/Cargo.toml. Step 4: read workspace/Cargo.lock. Step 5: read workspace/scripts/run-live-test.mjs if it exists, otherwise read workspace/release/README.md. Step 6: Slowly and carefully write an exhaustive analysis of each file you read, quoting every section verbatim and adding commentary. Do not rush — take your time with each file." After spawning the child, immediately call wait_subagent for that child request.',
      );
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = runner.sendResults.at(-1)!;

      await waitFor(
        async () => {
          const tree = await runner.adapter.listSubagentTree({
            rootRequestId: submitted.requestId,
            agentDid: runner.agentDid,
            includeTerminal: false,
          });
          expect(tree.edges.length).toBeGreaterThan(0);
          const child = tree.nodes.find((n) => n.behaviorId === subagentTarget);
          expect(child?.lifecycleState).toMatch(/processing|claimed|pending/i);
        },
        { timeout: 60_000, interval: 500 },
      );
      logTurn("child spawned and non-terminal; clicking interrupt immediately");

      await waitFor(
        () => {
          const btn = driver.cancelButton();
          expect(btn).toBeTruthy();
          expect(btn).toBeEnabled();
        },
        { timeout: 5_000 },
      );
      await driver.clickCancel();
      const dialog = await screen
        .findByRole(
          "dialog",
          { name: /interrupt parent request/i },
          { timeout: 10_000 },
        )
        .catch(() => null);
      if (dialog) {
        logTurn("cascade dialog opened; confirming cascade (B3 cascade path)");
        for (let attempt = 1; attempt <= 4; attempt += 1) {
          const activeDialog = screen.queryByRole("dialog", {
            name: /interrupt parent request/i,
          });
          if (!activeDialog) break;
          if (within(activeDialog).queryByText(/preview updated/i)) {
            logTurn(`cascade preview refreshed; re-confirming attempt ${attempt}`);
          }
          await driver.user.click(
            within(activeDialog).getByRole("button", {
              name: /interrupt parent and cascade/i,
            }),
          );
          await waitFor(
            () => {
              const currentDialog = screen.queryByRole("dialog", {
                name: /interrupt parent request/i,
              });
              if (!currentDialog) return;
              expect(
                within(currentDialog).getByRole("button", {
                  name: /interrupt parent and cascade/i,
                }),
              ).toBeEnabled();
            },
            { timeout: 15_000, interval: 250 },
          );
        }
        expect(
          screen.queryByRole("dialog", { name: /interrupt parent request/i }),
          "cascade dialog should close after an accepted confirmation",
        ).toBeNull();
      } else {
        logTurn(
          "no cascade dialog appeared — child completed before preview; direct-interrupt path taken",
        );
      }

      const parentSession = await runner.waitForRequestCompletion(submitted);
      logTurn(
        `parent session terminal: turnState=${parentSession.turnState} cancelCause=${JSON.stringify(parentSession.latestResponse?.cancelCause)}`,
      );
      expect(
        parentSession.turnState,
        `parent must reach a terminal state after cascade; saw ${parentSession.turnState}`,
      ).toMatch(/interrupted|failed|completed|superseded/i);

      await waitFor(
        async () => {
          const tree = await runner.adapter.listSubagentTree({
            rootRequestId: submitted.requestId,
            agentDid: runner.agentDid,
            includeTerminal: true,
          });
          const child = tree.nodes.find((n) => n.behaviorId === subagentTarget);
          expect(
            child?.lifecycleState,
            `B3 requires child.lifecycleState == "interrupted"; saw ${child?.lifecycleState}`,
          ).toMatch(/^interrupted$/i);
        },
        { timeout: 180_000, interval: 2_000 },
      );

      const parentSnapshot = await runner.adapter.fetchSessionSnapshot(
        submitted.sessionId,
        runner.agentDid,
        submitted.requestId,
      );
      const spawnToolCalls = (parentSnapshot?.timelineItems ?? []).flatMap((item) =>
        item.kind === "toolGroup"
          ? item.tools.filter((t) => /spawn_subagent/i.test(t.toolName))
          : [],
      );
      expect(spawnToolCalls.length).toBeGreaterThan(0);
      const cascadedCall = spawnToolCalls.find(
        (call) => call.cancelCause?.cause === "interrupted",
      );
      expect(
        cascadedCall,
        `C2 requires spawn_subagent tool call to carry cancelCause.cause="interrupted"; saw ${JSON.stringify(spawnToolCalls.map((c) => c.cancelCause))}`,
      ).toBeDefined();
    });
  }, 600_000);
});
