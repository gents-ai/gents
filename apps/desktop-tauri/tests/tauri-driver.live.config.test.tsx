import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { LiveBridgeRunner } from "./live-bridge-runner";
import { renderTauriAppDriverWithBridge } from "./tauri-driver";
import {
  delay,
  describeLive,
  expectCompletedSession,
  logTurn,
  waitForConfigFlowDocuments,
  waitForDeploymentDocument,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner config flow", () => {
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
        await driver.user.click(screen.getByTestId("behavior-new"));
        await waitFor(() => {
          expect(driver.behaviorKey()).toBeInTheDocument();
        });
        expect(
          Array.from(
            (screen.getByTestId("behavior-profile-id") as HTMLSelectElement)
              .options,
          ).some((option) => option.value === ""),
        ).toBe(false);
        await driver.replaceBehaviorKey(behaviorId);
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
});
