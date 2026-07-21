import { waitFor } from "@testing-library/react";
import { describe, expect } from "vitest";

import { LiveBridgeRunner } from "../live-bridge-runner";
import type { DeploymentView, DesktopSessionSnapshot } from "../../src/lib/types";

export const describeLive =
  process.env.GENTS_TAURI_LIVE === "1" ? describe.sequential : describe.skip;

export const FIRST_PROMPT =
  "Read workspace/README.md, then summarize what gents is in two short bullets.";
export const SECOND_PROMPT =
  "Read workspace/CLAUDE.md, then summarize the document-driven request model in two short bullets.";
export const THIRD_PROMPT =
  "Without calling more tools, give one concise desktop integration-test risk and one coverage idea.";

export function logTurn(message: string) {
  console.info(`[live-tauri] ${message}`);
}

export function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function sessionDiagnosticMessage(label: string, session: DesktopSessionSnapshot) {
  return `${label} did not complete: ${JSON.stringify({
    turnState: session.turnState ?? null,
    latestRequestId: session.latestRequestId ?? null,
    latestResponse: session.latestResponse ?? null,
    activeResponseOverlay: session.activeResponseOverlay ?? null,
    pendingTurn: session.pendingTurn ?? null,
    timelineTail: session.timelineItems.slice(-6),
  })}`;
}

export function expectCompletedSession(label: string, session: DesktopSessionSnapshot) {
  expect(session.turnState, sessionDiagnosticMessage(label, session)).toBe("completed");
}

export async function waitForBehaviorConfig(
  runner: LiveBridgeRunner,
  behaviorId: string,
  expectedDisplayName: string,
  expectedSystemPrompt: string,
) {
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      const behavior = snapshot.client?.deployments[0]?.behaviors.find(
        (candidate) => candidate.behaviorId === behaviorId,
      );
      expect(behavior?.displayName).toBe(expectedDisplayName);
      expect(behavior?.systemPrompt).toBe(expectedSystemPrompt);
    },
    { timeout: 30_000 },
  );
}

export async function waitForConfigFlowDocuments(
  runner: LiveBridgeRunner,
  expected: {
    backendId: string;
    profileId: string;
    toolServiceId: string;
    toolSelectionId: string;
    behaviorId: string;
    taskId: string;
    scheduleId: string;
    eventTriggerId: string;
  },
) {
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      const deployment = snapshot.client?.deployments[0];
      expect(
        deployment?.inferenceBackends.some(
          (backend) => backend.backendId === expected.backendId,
        ),
      ).toBe(true);
      expect(
        deployment?.inferenceProfiles.some(
          (profile) => profile.profileId === expected.profileId,
        ),
      ).toBe(true);
      expect(
        deployment?.toolSelections.some(
          (selection) => selection.selectionId === expected.toolSelectionId,
        ),
      ).toBe(true);
      expect(
        deployment?.toolServiceRegistries.some(
          (service) => service.serviceId === expected.toolServiceId,
        ),
      ).toBe(true);
      const behavior = deployment?.behaviors.find(
        (candidate) => candidate.behaviorId === expected.behaviorId,
      );
      expect(behavior?.backendId).toBe(expected.backendId);
      expect(behavior?.inferenceProfileId).toBe(expected.profileId);
      expect(behavior?.toolSelectionId).toBe(expected.toolSelectionId);
      const task = deployment?.tasks.find(
        (candidate) => candidate.taskId === expected.taskId,
      );
      expect(task?.behaviorId).toBe(expected.behaviorId);
      const schedule = deployment?.schedules.find(
        (candidate) => candidate.scheduleId === expected.scheduleId,
      );
      expect(schedule?.taskId).toBe(expected.taskId);
      const eventTrigger = deployment?.eventTriggers.find(
        (candidate) => candidate.triggerId === expected.eventTriggerId,
      );
      expect(eventTrigger?.taskId).toBe(expected.taskId);
    },
    { timeout: 30_000 },
  );
}

export async function waitForDeploymentDocument(
  runner: LiveBridgeRunner,
  predicate: (deployment: DeploymentView) => void,
) {
  await waitFor(
    async () => {
      const snapshot = await runner.fetchSnapshot();
      const deployment = snapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      predicate(deployment!);
    },
    { timeout: 30_000 },
  );
}
