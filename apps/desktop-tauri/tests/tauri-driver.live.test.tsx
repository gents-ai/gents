import { screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import type { DeploymentView, DesktopSessionSnapshot } from "../src/lib/types";

const describeLive =
  process.env.DEFRA_AGENT_TAURI_LIVE === "1" ? describe.sequential : describe.skip;

const FIRST_PROMPT =
  "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?";
const SECOND_PROMPT =
  "awesome breakdown, can you please tell me what you like about the architecture? use details and point to files";
const THIRD_PROMPT =
  "can you please tell me what you don't like about the architecture? use details and point to files";

function logTurn(message: string) {
  console.info(`[live-tauri] ${message}`);
}

function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function sessionDiagnosticMessage(
  label: string,
  session: DesktopSessionSnapshot,
) {
  return `${label} did not complete: ${JSON.stringify({
    turnState: session.turnState ?? null,
    latestRequestId: session.latestRequestId ?? null,
    latestResponse: session.latestResponse ?? null,
    activeResponseOverlay: session.activeResponseOverlay ?? null,
    pendingTurn: session.pendingTurn ?? null,
    timelineTail: session.timelineItems.slice(-6),
  })}`;
}

function expectCompletedSession(
  label: string,
  session: DesktopSessionSnapshot,
) {
  expect(session.turnState, sessionDiagnosticMessage(label, session)).toBe(
    "completed",
  );
}

async function waitForBehaviorConfig(
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

async function waitForConfigFlowDocuments(
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
      expect(deployment?.inferenceBackends.some(
        (backend) => backend.backendId === expected.backendId,
      )).toBe(true);
      expect(deployment?.inferenceProfiles.some(
        (profile) => profile.profileId === expected.profileId,
      )).toBe(true);
      expect(deployment?.toolSelections.some(
        (selection) => selection.selectionId === expected.toolSelectionId,
      )).toBe(true);
      expect(deployment?.toolServiceRegistries.some(
        (service) => service.serviceId === expected.toolServiceId,
      )).toBe(true);
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

async function waitForDeploymentDocument(
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

describeLive("Tauri app live bridge runner", () => {
  it(
    "edits behavior config through the real UI and observes replication",
    async () => {
      const runner = await LiveBridgeRunner.start({
        inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
        modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
        provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
        apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
        apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
      });
      const initialSnapshot = await runner.fetchSnapshot();
      const deployment = initialSnapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      const behavior =
        deployment!.behaviors.find((candidate) => candidate.isDefault) ??
        deployment!.behaviors[0];
      expect(behavior).toBeDefined();
      const driver = renderTauriAppDriverWithBridge(runner, deployment!.peerId);
      const suffix = Date.now().toString();
      const displayName = `Live Config ${suffix}`;
      const systemPrompt =
        `You are Amy, a repository analysis agent. Config sentinel ${suffix}.`;

      try {
        await driver.ready();
        await driver.openConfig();
        await waitFor(() => {
          expect(driver.behaviorDisplayName()).toBeInTheDocument();
        });

        await driver.replaceBehaviorDisplayName(displayName);
        await driver.replaceBehaviorSystemPrompt(systemPrompt);
        await driver.saveBehaviorConfig();

        await waitFor(() => {
          expect(driver.behaviorSaveStatus()).toHaveTextContent("Saved");
        });
        await waitForBehaviorConfig(
          runner,
          behavior!.behaviorId,
          displayName,
          systemPrompt,
        );
        logTurn(
          `behavior config saved behaviorId=${behavior!.behaviorId} displayName="${displayName}"`,
        );
      } finally {
        await driver.dispose();
      }
    },
    240_000,
  );

  it(
    "configures backend profile tools behavior task and runs it",
    async () => {
      const runner = await LiveBridgeRunner.start({
        inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
        modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
        provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
        apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
        apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
      });
      const initialSnapshot = await runner.fetchSnapshot();
      const deployment = initialSnapshot.client?.deployments[0];
      expect(deployment).toBeDefined();
      const driver = renderTauriAppDriverWithBridge(runner, deployment!.peerId);
      const suffix = Date.now().toString();
      const backendId = `minimax-backend-${suffix}`;
      const profileId = `minimax-profile-${suffix}`;
      const toolServiceId = `http-mcp-${suffix}`;
      const toolSelectionId = `repo-tools-${suffix}`;
      const behaviorId = `config-behavior-${suffix}`;
      const taskId = `config-task-${suffix}`;
      const scheduleId = `config-timer-${suffix}`;
      const eventTriggerId = `config-event-${suffix}`;
      const inferenceUrl =
        process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL ??
        "http://100.73.235.38:8000/v1";
      const modelName =
        process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ?? "MiniMax-M2.7-NVFP4";
      const fileToolRoot = `${runner.toolRoot}/workspace`;

      try {
        await driver.ready();
        await driver.openConfig();

        await driver.openConfigSection("backends");
        await driver.replaceInput("backend-id", backendId);
        await driver.replaceInput("backend-name", "MiniMax Live Backend");
        await driver.selectOption("backend-provider-kind", "openai");
        await driver.replaceInput("backend-endpoint", inferenceUrl);
        await driver.replaceTextarea("backend-models", modelName);
        await driver.replaceInput("backend-max-concurrent", "2");
        await driver.replaceInput("backend-max-queue-depth", "100");
        await driver.user.click(screen.getByTestId("backend-save"));
        await waitForDeploymentDocument(runner, (current) => {
          expect(current.inferenceBackends.some(
            (backend) => backend.backendId === backendId,
          )).toBe(true);
        });

        await driver.openConfigSection("profiles");
        await driver.replaceInput("profile-id", profileId);
        await driver.replaceInput("profile-display-name", "MiniMax Live Profile");
        await driver.replaceInput("profile-context-window", "131072");
        await driver.replaceInput("profile-max-output-tokens", "1024");
        await driver.replaceInput("profile-max-turns", "20");
        await driver.replaceInput("profile-temperature", "0");
        await driver.replaceInput("profile-stream-batch-ms", "250");
        await driver.replaceInput("profile-deadline-duration-secs", "300");
        await driver.user.click(screen.getByTestId("profile-save"));
        await waitForDeploymentDocument(runner, (current) => {
          expect(current.inferenceProfiles.some(
            (profile) => profile.profileId === profileId,
          )).toBe(true);
        });

        await driver.openConfigSection("metaTools");
        await driver.replaceInput("tool-service-id", toolServiceId);
        await driver.replaceInput("tool-service-display-name", "HTTP MCP Service");
        await driver.replaceTextarea(
          "tool-service-description",
          "Live acceptance HTTP MCP endpoint document.",
        );
        await driver.replaceInput("tool-service-hostname", "desktop-mcp.local");
        await driver.replaceInput("tool-service-tailscale-ip", "100.73.235.38");
        await driver.replaceInput("tool-service-mcp-port", "8000");
        await driver.replaceInput("tool-service-mcp-path", "/mcp");
        await driver.selectOption("tool-service-status", "online");
        await driver.user.click(screen.getByTestId("tool-service-save"));
        await waitForDeploymentDocument(runner, (current) => {
          expect(current.toolServiceRegistries.some(
            (service) => service.serviceId === toolServiceId,
          )).toBe(true);
        });

        await driver.openConfigSection("toolSelections");
        await driver.replaceInput("tool-selection-id", toolSelectionId);
        await driver.replaceInput(
          "tool-selection-display-name",
          "Repo Audit Readonly Tools",
        );
        await driver.setChecked("tool-enable-file-tools", true);
        await driver.setChecked("tool-enable-bash", true);
        await driver.setChecked("tool-enable-meta-tools", true);
        await driver.setChecked(`tool-delegate-${toolServiceId}`, true);
        await driver.replaceInput(
          "tool-file-tool-root",
          fileToolRoot,
        );
        await driver.replaceTextarea("tool-cli-tool-names", "rg");
        await driver.user.click(screen.getByTestId("tool-selection-save"));
        await waitForDeploymentDocument(runner, (current) => {
          const selection = current.toolSelections.find(
            (selection) => selection.selectionId === toolSelectionId,
          );
          expect(selection?.delegateTo).toContain(toolServiceId);
          expect(selection?.fileToolRoot).toBe(fileToolRoot);
        });

        await driver.openConfigSection("behavior");
        expect(
          Array.from(
            (screen.getByTestId("behavior-profile-id") as HTMLSelectElement)
              .options,
          ).some((option) => option.value === ""),
        ).toBe(false);
        await driver.replaceInput("behavior-id", behaviorId);
        await driver.replaceBehaviorDisplayName("Config Flow Behavior");
        await driver.selectOption("behavior-backend-id", backendId);
        await driver.selectOption("behavior-profile-id", profileId);
        await driver.selectOption("behavior-tool-selection-id", toolSelectionId);
        await driver.replaceBehaviorSystemPrompt(
          `You are Amy running a desktop config acceptance flow. Include sentinel ${suffix} when asked about this test.`,
        );
        await driver.saveBehaviorConfig();
        await waitForDeploymentDocument(runner, (current) => {
          const behavior = current.behaviors.find(
            (candidate) => candidate.behaviorId === behaviorId,
          );
          expect(behavior?.backendId).toBe(backendId);
          expect(behavior?.inferenceProfileId).toBe(profileId);
          expect(behavior?.toolSelectionId).toBe(toolSelectionId);
        });

        await driver.openConfigSection("tasks");
        await driver.replaceInput("task-id", taskId);
        await driver.replaceInput("task-name", "Config Flow Smoke Task");
        await driver.selectOption("task-behavior-id", behaviorId);
        await driver.replaceTextarea(
          "task-description",
          "Exercises manual task execution from the desktop config UI.",
        );
        await driver.replaceTextarea(
          "task-prompt-template",
          `In one short paragraph, say the desktop config flow reached task execution and include sentinel ${suffix}.`,
        );
        await driver.user.click(screen.getByTestId("task-save"));

        await waitForDeploymentDocument(runner, (current) => {
          expect(current.tasks.some((task) => task.taskId === taskId)).toBe(true);
        });

        await driver.openConfigSection("timerTriggers");
        await driver.replaceInput("schedule-id", scheduleId);
        await driver.selectOption("schedule-task-id", taskId);
        await driver.replaceInput("schedule-interval-secs", "3600");
        await driver.selectOption("schedule-concurrency", "serial");
        await driver.user.click(screen.getByTestId("schedule-save"));
        await waitForDeploymentDocument(runner, (current) => {
          const schedule = current.schedules.find(
            (candidate) => candidate.scheduleId === scheduleId,
          );
          expect(schedule?.taskId).toBe(taskId);
        });

        await driver.openConfigSection("eventTriggers");
        await driver.replaceInput("event-trigger-id", eventTriggerId);
        await driver.selectOption("event-trigger-task-id", taskId);
        await driver.replaceInput("event-trigger-source-collection", "AgentRequest");
        await driver.selectOption("event-trigger-event-kind", "created");
        await driver.selectOption("event-trigger-concurrency", "latest_only");
        await driver.replaceTextarea(
          "event-trigger-filter",
          JSON.stringify({ status: "completed" }),
        );
        await driver.user.click(screen.getByTestId("event-trigger-save"));

        await waitForConfigFlowDocuments(runner, {
          backendId,
          profileId,
          toolServiceId,
          toolSelectionId,
          behaviorId,
          taskId,
          scheduleId,
          eventTriggerId,
        });
        await delay(6_500);
        await waitForDeploymentDocument(runner, (current) => {
          expect(current.runtime?.processState).toBe("ready");
          expect(current.runtime?.reconcilePhase).toBe("idle");
          expect(current.runtime?.lastReconcileResult).not.toBe("error");
        });

        await driver.openConfigSection("tasks");
        await driver.user.click(screen.getByTestId("task-run"));
        await waitFor(() => {
          expect(runner.taskRunResults).toHaveLength(1);
        });
        const taskRun = runner.taskRunResults[0];
        expect(taskRun.behaviorId).toBe(behaviorId);
        logTurn(
          `task run submitted taskId=${taskId} requestId=${taskRun.requestId}`,
        );
        const session = await runner.waitForRequestCompletion(taskRun);
        if (session.turnState !== "completed") {
          const diagnostics = await runner.fetchRequestDiagnostics(
            taskRun.sessionId,
            taskRun.requestId,
          );
          throw new Error(
            `config task run failed diagnostics=${JSON.stringify(diagnostics)}`,
          );
        }
        expectCompletedSession("config task run", session);
        expect(session.latestRequestId).toBe(taskRun.requestId);
      } finally {
        await driver.dispose();
      }
    },
    600_000,
  );

  it(
    "drives three real UI turns through a single live deployment",
    async () => {
      const runner = await LiveBridgeRunner.start({
        inferenceUrl: process.env.DEFRA_AGENT_TAURI_LIVE_INFERENCE_URL,
        modelName: process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME,
        provider: process.env.DEFRA_AGENT_TAURI_LIVE_PROVIDER,
        apiKey: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY,
        apiKeyEnvVar: process.env.DEFRA_AGENT_TAURI_LIVE_API_KEY_ENV_VAR,
      });
      const initialSnapshot = await runner.fetchSnapshot();
      const firstPeerId = initialSnapshot.client?.deployments[0]?.peerId ?? null;
      const driver = renderTauriAppDriverWithBridge(runner, firstPeerId);

      try {
        await driver.ready();
        await driver.openChat();
        logTurn(`driver ready deployment=${runner.deploymentLabel} agentDid=${runner.agentDid}`);

        await driver.typeComposer(FIRST_PROMPT);
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(1);
        });
        const firstResult = runner.sendResults.at(-1);
        expect(firstResult).toBeDefined();
        logTurn(
          `turn 1 submitted sessionId=${firstResult!.sessionId} requestId=${firstResult!.requestId}`,
        );
        await waitFor(() => {
          expect(driver.conversation(firstResult!.sessionId)).toBeInTheDocument();
        });
        const firstSession = await runner.waitForRequestCompletion(firstResult!);
        expectCompletedSession("turn 1", firstSession);
        logTurn(`turn 1 completed requestId=${firstResult!.requestId}`);

        await driver.typeComposer(SECOND_PROMPT);
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(2);
        });
        const secondResult = runner.sendResults.at(-1);
        expect(secondResult).toBeDefined();
        expect(secondResult!.sessionId).toBe(firstResult!.sessionId);
        logTurn(
          `turn 2 submitted sessionId=${secondResult!.sessionId} requestId=${secondResult!.requestId}`,
        );
        const secondSession = await runner.waitForRequestCompletion(secondResult!);
        expectCompletedSession("turn 2", secondSession);
        logTurn(`turn 2 completed requestId=${secondResult!.requestId}`);

        await driver.typeComposer(THIRD_PROMPT);
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(3);
        });
        const thirdResult = runner.sendResults.at(-1);
        expect(thirdResult).toBeDefined();
        expect(thirdResult!.sessionId).toBe(firstResult!.sessionId);
        logTurn(
          `turn 3 submitted sessionId=${thirdResult!.sessionId} requestId=${thirdResult!.requestId}`,
        );
        const finalSession = await runner.waitForRequestCompletion(thirdResult!);
        logTurn(`turn 3 completed requestId=${thirdResult!.requestId}`);

        expect(runner.sentRequests).toHaveLength(3);
        expect(finalSession.sessionId).toBe(firstResult!.sessionId);
        expectCompletedSession("turn 3", finalSession);
        expect(finalSession.latestRequestId).toBe(thirdResult!.requestId);
        expect(finalSession.timelineItems.length).toBeGreaterThanOrEqual(6);
        expect(
          finalSession.timelineItems.some((item) => item.kind === "toolGroup"),
        ).toBe(true);

        const latestSnapshot = await runner.fetchSnapshot();
        const deployment = latestSnapshot.client?.deployments[0];
        expect(deployment).toBeDefined();
        expect(deployment?.conversations[0]?.sessionId).toBe(firstResult!.sessionId);
        expect(deployment?.conversations[0]?.messageCount).toBeGreaterThanOrEqual(6);
        expect(deployment?.conversations[0]?.toolCallCount).toBeGreaterThan(0);
        logTurn(
          `final snapshot sessionId=${firstResult!.sessionId} messageCount=${deployment?.conversations[0]?.messageCount ?? 0} toolCallCount=${deployment?.conversations[0]?.toolCallCount ?? 0}`,
        );
      } finally {
        await driver.dispose();
      }
    },
    600_000,
  );
});
