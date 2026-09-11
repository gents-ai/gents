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
  toolsId: string;
  behaviorId: string;
  taskId: string;
  scheduleId: string;
  eventSourceId: string;
  triggerDocId: string;
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

type ToolsConfigFlowContext = ConfigFlowContext & {
  fileToolRoot: string;
};

export function createConfigFlowIds(suffix = Date.now().toString()): ConfigFlowIds {
  return {
    suffix,
    backendId: `minimax-backend-${suffix}`,
    profileId: `minimax-profile-${suffix}`,
    toolServiceId: `http-mcp-${suffix}`,
    toolsId: `repo-tools-${suffix}`,
    behaviorId: `config-behavior-${suffix}`,
    taskId: `config-task-${suffix}`,
    scheduleId: `config-schedule-${suffix}`,
    eventSourceId: `config-event-source-${suffix}`,
    triggerDocId: `config-event-trigger-${suffix}`,
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
      current.inferenceProfiles.some((profile) => profile.profile_id === ids.profileId),
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
        (service) => service.service_id === ids.toolServiceId,
      ),
    ).toBe(true);
  });
}

export async function createTools({
  runner,
  driver,
  ids,
  fileToolRoot,
}: ToolsConfigFlowContext) {
  await driver.openConfigSection("tools");
  await driver.user.click(screen.getByTestId("tools-new"));
  await driver.replaceInput("tools-id", ids.toolsId);
  await driver.replaceInput("tools-display-name", "Repo Audit Readonly Tools");
  await driver.replaceInput("tools-root", fileToolRoot);
  await driver.selectOption("tools-files-mode", "ReadWrite");
  await driver.selectOption("tools-bash-mode", "ReadOnly");
  await driver.setChecked("tools-bash-background", false);
  await driver.user.click(screen.getByTestId(`tools-service-${ids.toolServiceId}`));
  await driver.replaceTextarea(`tools-service-names-${ids.toolServiceId}`, "all");
  await driver.replaceTextarea("tools-target-ids", ids.behaviorId);
  await driver.replaceInput("tools-cross-principal-spawn-timeout", "45");
  await driver.setChecked("tools-subagent-spawn", true);
  await driver.setChecked("tools-subagent-steering", true);
  await driver.setChecked("tools-subagent-background", true);
  await driver.user.click(screen.getByTestId("tools-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const tools = current.tools.find((candidate) => candidate.tools_id === ids.toolsId);
    expect(tools).toBeDefined();
    expect(
      tools?.remote?.services?.some(
        (grant) => grant.mcp_service_id === ids.toolServiceId,
      ),
    ).toBe(true);
    expect(tools?.host?.files?.mode).toBe("ReadWrite");
    expect(tools?.host?.bash?.mode).toBe("ReadOnly");
    expect(tools?.host?.root).toBe(fileToolRoot);
    expect(tools?.subagents?.target_ids).toContain(ids.behaviorId);
    expect(tools?.subagents?.spawn_enabled).toBe(true);
    expect(tools?.subagents?.steering_enabled).toBe(true);
    expect(tools?.subagents?.background_enabled).toBe(true);
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
  await driver.selectOption("behavior-profile-id", ids.profileId);
  await driver.replaceInput("behavior-context-id", ids.behaviorId);
  await driver.selectOption("behavior-tools-id", ids.toolsId);
  await driver.replaceBehaviorSystemPrompt(
    `You are Amy running a desktop config acceptance flow. Include sentinel ${ids.suffix} when asked about this test.`,
  );
  await driver.saveBehaviorConfig();
  await waitForDeploymentDocument(runner, (current) => {
    const behavior = current.behaviors.find(
      (candidate) => candidate.behaviorId === ids.behaviorId,
    );
    expect(behavior?.inferenceProfileId).toBe(ids.profileId);
    expect(behavior?.contextId).toBe(ids.behaviorId);
    const context = current.contexts.find(
      (candidate) => candidate.context_id === ids.behaviorId,
    );
    expect(context?.tools_id).toBe(ids.toolsId);
    expect(context?.system_prompt).toContain(`${ids.suffix}`);
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
  await driver.openConfigSection("schedules");
  await driver.user.click(screen.getByTestId("schedule-new"));
  await driver.replaceInput("schedule-id", ids.scheduleId);
  await driver.replaceInput("schedule-display-name", "Config Flow Hourly");
  await driver.selectOption("schedule-cadence-kind", "interval");
  await driver.replaceInput("schedule-interval-secs", "3600");
  await driver.user.click(screen.getByTestId("schedule-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const schedule = current.schedules.find(
      (candidate) => candidate.schedule_id === ids.scheduleId,
    );
    expect(schedule?.cadence).toEqual({
      kind: "interval",
      interval_secs: 3600,
    });
  });
}

export async function createEventSource({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("eventSources");
  await driver.user.click(screen.getByTestId("event-source-new"));
  await driver.replaceInput("event-source-id", ids.eventSourceId);
  await driver.replaceInput("event-source-display-name", "Config Flow Events");
  await driver.replaceInput("event-source-source-collection", "AgentRequest");
  await driver.selectOption("event-source-event-kind", "created");
  await driver.user.click(screen.getByTestId("event-source-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const eventSource = current.eventSources.find(
      (candidate) => candidate.event_source_id === ids.eventSourceId,
    );
    expect(eventSource?.source_collection).toBe("AgentRequest");
    expect(eventSource?.event_kind).toBe("created");
  });
}

export async function createTriggerDocument({
  runner,
  driver,
  ids,
}: ConfigFlowContext) {
  await driver.openConfigSection("triggers");
  await driver.user.click(screen.getByTestId("trigger-new"));
  await driver.replaceInput("trigger-id", ids.triggerDocId);
  await driver.replaceInput("trigger-display-name", "Config Flow Event Trigger");
  await driver.selectOption("trigger-task-id", ids.taskId);
  await driver.selectOption("trigger-source-kind", "event");
  await driver.selectOption("trigger-source-event-source", ids.eventSourceId);
  await driver.selectOption("trigger-concurrency", "latest_only");
  await driver.user.click(screen.getByTestId("trigger-save"));
  await waitForDeploymentDocument(runner, (current) => {
    const trigger = current.triggers.find(
      (candidate) => candidate.config.trigger_id === ids.triggerDocId,
    );
    expect(trigger?.config.task_id).toBe(ids.taskId);
    expect(trigger?.config.source).toEqual({
      kind: "event",
      event_source_id: ids.eventSourceId,
    });
  });
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
