import { screen, waitFor } from "@testing-library/react";
import { expect } from "vitest";

import type { LiveBridgeRunner } from "../live-bridge-runner";
import type { LiveDesktopDriver } from "./harness";
import {
  delay,
  waitForConfigFlowDocuments,
  waitForDeploymentDocument,
} from "./helpers";

export type ConfigFlowIds = {
  suffix: string;
  backendId: string;
  profileId: string;
  toolServiceId: string;
  toolSelectionId: string;
  behaviorId: string;
  taskId: string;
  scheduleId: string;
  eventTriggerId: string;
};

type ConfigFlowContext = {
  runner: LiveBridgeRunner;
  driver: LiveDesktopDriver;
  ids: ConfigFlowIds;
};

type BackendConfigFlowContext = ConfigFlowContext & {
  inferenceUrl: string;
  modelName: string;
};

type ToolSelectionConfigFlowContext = ConfigFlowContext & {
  fileToolRoot: string;
};

type DriverConfigFlowContext = Pick<ConfigFlowContext, "driver" | "ids">;

export function createConfigFlowIds(suffix = Date.now().toString()): ConfigFlowIds {
  return {
    suffix,
    backendId: `minimax-backend-${suffix}`,
    profileId: `minimax-profile-${suffix}`,
    toolServiceId: `http-mcp-${suffix}`,
    toolSelectionId: `repo-tools-${suffix}`,
    behaviorId: `config-behavior-${suffix}`,
    taskId: `config-task-${suffix}`,
    scheduleId: `config-timer-${suffix}`,
    eventTriggerId: `config-event-${suffix}`,
  };
}

export async function createBackend({
  runner,
  driver,
  ids,
  inferenceUrl,
  modelName,
}: BackendConfigFlowContext) {
  await driver.openConfigSection("backends");
  await driver.user.click(screen.getByTestId("backend-new"));
  await driver.replaceInput("backend-id", ids.backendId);
  await driver.replaceInput("backend-name", "MiniMax Live Backend");
  await driver.selectOption("backend-provider-kind", "openai");
  await driver.replaceInput("backend-endpoint", inferenceUrl);
  await driver.replaceTextarea("backend-models", modelName);
  await driver.replaceInput("backend-max-concurrent", "2");
  await driver.replaceInput("backend-max-queue-depth", "100");
  await driver.user.click(screen.getByTestId("backend-save"));
  await waitForDeploymentDocument(runner, (current) => {
    expect(
      current.inferenceBackends.some((backend) => backend.backendId === ids.backendId),
    ).toBe(true);
  });
}

export async function createInferenceProfile({
  runner,
  driver,
  ids,
}: ConfigFlowContext) {
  await driver.openConfigSection("profiles");
  await driver.user.click(screen.getByTestId("profile-new"));
  await driver.replaceInput("profile-id", ids.profileId);
  await driver.replaceInput("profile-display-name", "MiniMax Live Profile");
  await driver.replaceInput("profile-context-window", "131072");
  await driver.replaceInput("profile-max-output-tokens", "1024");
  await driver.replaceInput("profile-max-turns", "20");
  await driver.replaceInput("profile-temperature", "0");
  await driver.replaceInput("profile-stream-batch-ms", "250");
  await driver.replaceInput("profile-deadline-duration-secs", "300");
  await driver.user.click(screen.getByTestId("profile-save"));
  await waitForDeploymentDocument(runner, (current) => {
    expect(
      current.inferenceProfiles.some((profile) => profile.profileId === ids.profileId),
    ).toBe(true);
  });
}

export async function createToolService({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("metaTools");
  await driver.user.click(screen.getByTestId("tool-service-new"));
  await driver.replaceInput("tool-service-id", ids.toolServiceId);
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
    expect(
      current.toolServiceRegistries.some(
        (service) => service.serviceId === ids.toolServiceId,
      ),
    ).toBe(true);
  });
}

export async function createToolSelection({
  runner,
  driver,
  ids,
  fileToolRoot,
}: ToolSelectionConfigFlowContext) {
  await driver.openConfigSection("toolSelections");
  await driver.user.click(screen.getByTestId("tool-selection-new"));
  await driver.replaceInput("tool-selection-id", ids.toolSelectionId);
  await driver.replaceInput("tool-selection-display-name", "Repo Audit Readonly Tools");
  await driver.setChecked("tool-enable-file-tools", true);
  await driver.setChecked("tool-enable-bash", true);
  await driver.setChecked("tool-enable-meta-tools", true);
  await driver.setChecked(`tool-allowed-mcp-service-${ids.toolServiceId}`, true);
  await driver.replaceInput("tool-file-tool-root", fileToolRoot);
  await driver.selectOption("tool-command-execution-policy", "read_only");
  await driver.selectOption("tool-command-network-mode", "disabled");
  await driver.replaceTextarea("tool-command-allowed-argv-prefixes", "rg");
  await driver.replaceTextarea("tool-command-forbidden-argv-prefixes", "rm -rf");
  await driver.replaceTextarea("tool-cli-tool-names", "rg");
  await driver.replaceTextarea("tool-backgroundable-tool-names", "bash");
  await driver.replaceTextarea("tool-subagent-targets", ids.behaviorId);
  await driver.replaceInput("tool-cross-deployment-spawn-timeout", "45");
  await driver.setChecked("tool-subagent-spawn-enabled", true);
  await driver.setChecked("tool-subagent-steering-enabled", true);
  await driver.setChecked("tool-subagent-background-enabled", true);
  await driver.user.click(screen.getByTestId("tool-selection-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const selection = current.toolSelections.find(
      (candidate) => candidate.selectionId === ids.toolSelectionId,
    );
    expect(selection?.allowedMcpServiceIds).toContain(ids.toolServiceId);
    expect(selection?.fileToolRoot).toBe(fileToolRoot);
    expect(selection?.commandExecutionPolicy).toBe("read_only");
    expect(selection?.commandAllowedArgvPrefixes).toContain("rg");
    expect(selection?.commandForbiddenArgvPrefixes).toContain("rm -rf");
    expect(selection?.commandNetworkMode).toBe("disabled");
    expect(selection?.backgroundableToolNames).toContain("bash");
    expect(selection?.subagentTargets).toContain(ids.behaviorId);
    expect(selection?.subagentSpawnEnabled).toBe(true);
    expect(selection?.subagentSteeringEnabled).toBe(true);
    expect(selection?.subagentBackgroundEnabled).toBe(true);
    expect(selection?.crossDeploymentSpawnTimeoutSeconds).toBe(45);
  });
}

export async function createBehavior({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("behavior");
  await driver.user.click(screen.getByTestId("behavior-new"));
  await waitFor(() => {
    expect(driver.behaviorKey()).toBeInTheDocument();
  });
  expect(
    Array.from(
      (screen.getByTestId("behavior-profile-id") as HTMLSelectElement).options,
    ).some((option) => option.value === ""),
  ).toBe(false);
  await driver.replaceBehaviorKey(ids.behaviorId);
  await driver.selectOption("behavior-backend-id", ids.backendId);
  await driver.selectOption("behavior-profile-id", ids.profileId);
  await driver.selectOption("behavior-tool-selection-id", ids.toolSelectionId);
  await driver.replaceBehaviorSystemPrompt(
    `You are Amy running a desktop config acceptance flow. Include sentinel ${ids.suffix} when asked about this test.`,
  );
  await driver.saveBehaviorConfig();
  await waitForDeploymentDocument(runner, (current) => {
    const behavior = current.behaviors.find(
      (candidate) => candidate.behaviorId === ids.behaviorId,
    );
    expect(behavior?.backendId).toBe(ids.backendId);
    expect(behavior?.inferenceProfileId).toBe(ids.profileId);
    expect(behavior?.toolSelectionId).toBe(ids.toolSelectionId);
  });
}

export async function createTask({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("tasks");
  await driver.user.click(screen.getByTestId("task-new"));
  await driver.replaceInput("task-id", ids.taskId);
  await driver.replaceInput("task-name", "Config Flow Smoke Task");
  await driver.selectOption("task-behavior-id", ids.behaviorId);
  await driver.replaceTextarea(
    "task-description",
    "Exercises manual task execution from the desktop config UI.",
  );
  await driver.replaceTextarea(
    "task-prompt-template",
    `In one short paragraph, say the desktop config flow reached task execution and include sentinel ${ids.suffix}.`,
  );
  await driver.user.click(screen.getByTestId("task-save"));
  await waitForDeploymentDocument(runner, (current) => {
    expect(current.tasks.some((task) => task.taskId === ids.taskId)).toBe(true);
  });
}

export async function createSchedule({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("timerTriggers");
  await driver.user.click(screen.getByTestId("schedule-new"));
  await driver.replaceInput("schedule-id", ids.scheduleId);
  await driver.selectOption("schedule-task-id", ids.taskId);
  await driver.replaceInput("schedule-interval-secs", "3600");
  await driver.selectOption("schedule-concurrency", "serial");
  await driver.user.click(screen.getByTestId("schedule-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const schedule = current.schedules.find(
      (candidate) => candidate.scheduleId === ids.scheduleId,
    );
    expect(schedule?.taskId).toBe(ids.taskId);
  });
}

export async function createEventTrigger({ driver, ids }: DriverConfigFlowContext) {
  await driver.openConfigSection("eventTriggers");
  await driver.user.click(screen.getByTestId("event-trigger-new"));
  await driver.replaceInput("event-trigger-id", ids.eventTriggerId);
  await driver.selectOption("event-trigger-task-id", ids.taskId);
  await driver.replaceInput("event-trigger-source-collection", "AgentRequest");
  await driver.selectOption("event-trigger-event-kind", "created");
  await driver.selectOption("event-trigger-concurrency", "latest_only");
  await driver.replaceTextarea(
    "event-trigger-filter",
    JSON.stringify({ status: "completed" }),
  );
  await driver.user.click(screen.getByTestId("event-trigger-save"));
}

export async function waitForConfigFlowReady(
  runner: LiveBridgeRunner,
  ids: ConfigFlowIds,
) {
  await waitForConfigFlowDocuments(runner, ids);
  await delay(6_500);
  await waitForDeploymentDocument(runner, (current) => {
    expect(current.behaviorReadiness.source.state).toBe("current");
    expect(current.runtime?.reconcilePhase).toBe("idle");
    expect(current.runtime?.lastReconcileResult).not.toBe("error");
  });
}
