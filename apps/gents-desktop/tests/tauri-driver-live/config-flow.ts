import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { expect } from "vitest";

import type { LiveBridgeRunner } from "../live-bridge-runner";
import type { LiveDesktopDriver } from "./harness";
import { waitForConfigFlowDocuments, waitForDeploymentDocument } from "./helpers";

export type ConfigFlowIds = {
  suffix: string;
  backendId: string;
  profileId: string;
  toolServiceId: string;
  toolsId: string;
  agentId: string;
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
    agentId: `config-agent-${suffix}`,
    taskId: `config-task-${suffix}`,
    scheduleId: `config-schedule-${suffix}`,
    eventSourceId: `config-event-source-${suffix}`,
    triggerDocId: `config-event-trigger-${suffix}`,
  };
}

function field(id: string): HTMLInputElement | HTMLTextAreaElement {
  const control = document.getElementById(id);
  if (
    !(control instanceof HTMLInputElement) &&
    !(control instanceof HTMLTextAreaElement)
  ) {
    throw new Error(`No editable configuration field ${id}`);
  }
  return control;
}

function changeField(id: string, value: string) {
  fireEvent.change(field(id), { target: { value } });
}

async function waitForField(id: string) {
  await waitFor(() => expect(field(id)).toBeInTheDocument(), { timeout: 30_000 });
}

async function chooseField(driver: LiveDesktopDriver, id: string, option: RegExp) {
  const trigger = document.getElementById(id);
  if (!trigger) throw new Error(`No configuration choice ${id}`);
  await driver.user.click(trigger);
  await driver.user.click(await screen.findByRole("option", { name: option }));
}

async function saveEditor(driver: LiveDesktopDriver) {
  await driver.user.click(screen.getByRole("button", { name: "Save" }));
}

export async function createBackend({
  runner,
  driver,
  ids,
  inferenceUrl,
  modelName,
}: BackendConfigFlowContext) {
  const before = await runner.fetchSnapshot();
  const existing = new Set(
    before.client?.deployments[0]?.inferenceBackends.map(
      (backend) => backend.backendId,
    ),
  );
  await driver.openConfigSection("profiles");
  await driver.user.click(screen.getByRole("button", { name: "New backend" }));
  await driver.user.click(await screen.findByRole("menuitem", { name: "Local" }));
  const setup = await screen.findByTestId("inference-setup-panel");
  const endpointLabel = await within(setup).findByText(
    "Endpoint",
    {},
    { timeout: 30_000 },
  );
  const endpointInput = endpointLabel.parentElement?.querySelector("input");
  if (!endpointInput) throw new Error("Local backend endpoint input is missing");
  fireEvent.change(endpointInput, { target: { value: inferenceUrl } });
  await driver.user.click(
    within(setup).getByRole("button", { name: "Connect and find models" }),
  );
  const models = await within(setup).findByRole(
    "listbox",
    { name: "Advertised models" },
    { timeout: 30_000 },
  );
  await driver.user.click(
    await within(models).findByRole(
      "option",
      { name: new RegExp(modelName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i") },
      { timeout: 30_000 },
    ),
  );
  await driver.user.click(
    await within(setup).findByRole(
      "button",
      { name: "Save backend" },
      { timeout: 30_000 },
    ),
  );
  await waitForDeploymentDocument(runner, (current) => {
    const backend = current.inferenceBackends.find(
      (candidate) =>
        !existing.has(candidate.backendId) &&
        candidate.endpoint?.replace(/\/+$/, "") === inferenceUrl.replace(/\/+$/, ""),
    );
    expect(backend).toBeDefined();
    const profile = current.inferenceProfiles.find(
      (candidate) =>
        candidate.backend_id === backend?.backendId &&
        candidate.model_name === modelName,
    );
    expect(profile).toBeDefined();
    ids.backendId = backend!.backendId;
    ids.profileId = profile!.profile_id;
  });
  await driver.openConfigSection("profiles");
  await driver.openConfigItem(ids.backendId);
  changeField(`${ids.backendId}-name`, "Desktop Live Backend");
  changeField(`${ids.backendId}-conc`, "2");
  changeField(`${ids.backendId}-queue`, "100");
  await saveEditor(driver);
  await waitForDeploymentDocument(runner, (current) => {
    const backend = current.inferenceBackends.find(
      (candidate) => candidate.backendId === ids.backendId,
    );
    expect(backend?.name).toBe("Desktop Live Backend");
    expect(backend?.maxConcurrent).toBe(2);
    expect(backend?.maxQueueDepth).toBe(100);
  });
}

export async function createInferenceProfile({
  runner,
  driver,
  ids,
}: ConfigFlowContext) {
  await driver.openConfigSection("profiles");
  const backend = (
    await runner.fetchSnapshot()
  ).client?.deployments[0]?.inferenceBackends.find(
    (candidate) => candidate.backendId === ids.backendId,
  );
  expect(backend).toBeDefined();
  await driver.user.click(
    await screen.findByRole(
      "button",
      {
        name: `Show what ${backend!.name ?? backend!.backendId} serves`,
      },
      { timeout: 30_000 },
    ),
  );
  await driver.openConfigItem(ids.profileId);
  await screen.findByText("Model-aware defaults", {}, { timeout: 30_000 });
  changeField(`${ids.profileId}-name`, "Desktop Live Profile");
  changeField(`${ids.profileId}-execution`, `${ids.profileId}-live-execution`);
  changeField(`${ids.profileId}-max-turns`, "20");
  changeField(`${ids.profileId}-batch`, "250");
  changeField(`${ids.profileId}-deadline`, "300");
  await saveEditor(driver);
  await waitForDeploymentDocument(runner, (current) => {
    const profile = current.inferenceProfiles.find(
      (candidate) => candidate.profile_id === ids.profileId,
    );
    expect(profile?.display_name).toBe("Desktop Live Profile");
    const execution = current.inferenceExecution.find(
      (candidate) => candidate.execution_id === profile?.execution_id,
    );
    expect(execution?.max_turns).toBe(20);
    expect(execution?.stream_batch_ms).toBe(250);
    expect(execution?.deadline_duration_secs).toBe(300);
  });
}

export async function createToolService({ runner, driver, ids }: ConfigFlowContext) {
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.toolServiceRegistries.map(
      (service) => service.service_id,
    ),
  );
  await driver.openConfigSection("tool-services");
  await driver.user.click(screen.getByRole("button", { name: "New remote tools" }));
  await waitForDeploymentDocument(runner, (current) => {
    const created = current.toolServiceRegistries.find(
      (service) => !before.has(service.service_id),
    );
    expect(created).toBeDefined();
    ids.toolServiceId = created!.service_id;
  });
  await waitForField(`${ids.toolServiceId}-name`);
  changeField(`${ids.toolServiceId}-name`, "HTTP MCP Service");
  changeField(
    `${ids.toolServiceId}-description`,
    "Live acceptance HTTP MCP endpoint document.",
  );
  changeField(`${ids.toolServiceId}-host`, "desktop-mcp.local");
  changeField(`${ids.toolServiceId}-tailscale`, "100.73.235.38");
  changeField(`${ids.toolServiceId}-port`, "8000");
  changeField(`${ids.toolServiceId}-path`, "/mcp");
  await saveEditor(driver);
  await waitForDeploymentDocument(runner, (current) => {
    const service = current.toolServiceRegistries.find(
      (candidate) => candidate.service_id === ids.toolServiceId,
    );
    expect(service?.hostname).toBe("desktop-mcp.local");
    expect(service?.mcp_port).toBe(8000);
  });
}

export async function createTools({
  runner,
  driver,
  ids,
  fileToolRoot,
}: ToolsConfigFlowContext) {
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.tools.map(
      (tools) => tools.tools_id,
    ),
  );
  await driver.openConfigSection("tools");
  await driver.user.click(screen.getByRole("button", { name: "New tools" }));
  await waitForDeploymentDocument(runner, (current) => {
    const created = current.tools.find((tools) => !before.has(tools.tools_id));
    expect(created).toBeDefined();
    ids.toolsId = created!.tools_id;
  });
  await waitForField(`${ids.toolsId}-name`);
  changeField(`${ids.toolsId}-name`, "Repo Audit Readonly Tools");
  changeField(`${ids.toolsId}-root`, fileToolRoot);
  await chooseField(driver, `${ids.toolsId}-files`, /^Read \/ write$/);
  await chooseField(driver, `${ids.toolsId}-bash`, /^Read only$/);
  await driver.user.click(
    screen.getByRole("switch", { name: "Start and message agent sessions" }),
  );
  const serviceOption = screen.getByText("HTTP MCP Service").closest("label");
  if (!serviceOption) throw new Error("Remote service selection is missing");
  await driver.user.click(serviceOption);
  await saveEditor(driver);
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
    expect(tools?.agents?.enabled).toBe(true);
  });
}

export async function createAgent({ runner, driver, ids }: ConfigFlowContext) {
  await driver.openConfigSection("agents");
  await driver.user.click(screen.getByRole("button", { name: "New agent" }));
  const name = screen.getByRole("textbox", { name: "Display name" });
  ids.agentId = name.id.replace(/-name$/, "");
  fireEvent.change(name, { target: { value: "Desktop Live Agent" } });
  changeField(
    `${ids.agentId}-prompt`,
    `You are Amy running a desktop config acceptance flow. Include sentinel ${ids.suffix} when asked about this test.`,
  );
  await chooseField(driver, `${ids.agentId}-tools`, /^Repo Audit Readonly Tools/);
  await chooseField(driver, `${ids.agentId}-profile`, /Desktop Live Profile/);
  await driver.user.click(screen.getByRole("button", { name: "Create" }));
  await waitForDeploymentDocument(runner, (current) => {
    expect(current.agents.some((candidate) => candidate.agentId === ids.agentId)).toBe(
      true,
    );
  });
  await driver.user.click(
    await screen.findByRole("switch", { name: "Desktop Live Agent is disabled" }),
  );
  await waitForDeploymentDocument(runner, (current) => {
    const agent = current.agents.find((candidate) => candidate.agentId === ids.agentId);
    expect(agent?.inferenceProfileId).toBe(ids.profileId);
    expect(agent?.enabled).toBe(true);
    const context = current.contexts.find(
      (candidate) => candidate.context_id === agent?.contextId,
    );
    expect(context?.tools_id).toBe(ids.toolsId);
    expect(context?.system_prompt).toContain(`${ids.suffix}`);
  });
}

export async function createTask({ runner, driver, ids }: ConfigFlowContext) {
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.tasks.map(
      (task) => task.taskId,
    ),
  );
  await driver.openConfigSection("tasks");
  await driver.user.click(screen.getByRole("button", { name: "New task" }));
  changeField("auto-name", "Config Flow Smoke Task");
  changeField(
    "auto-prompt",
    `In one short paragraph, say the desktop config flow reached task execution and include sentinel ${ids.suffix}.`,
  );
  await chooseField(driver, "auto-agent", /^Desktop Live Agent/);
  await driver.user.click(
    within(screen.getByRole("dialog", { name: "New task" })).getByRole("button", {
      name: "Create",
    }),
  );
  await waitForDeploymentDocument(runner, (current) => {
    const task = current.tasks.find((candidate) => !before.has(candidate.taskId));
    expect(task).toBeDefined();
    ids.taskId = task!.taskId;
    expect(task!.agentId).toBe(ids.agentId);
  });
  await waitFor(
    () => {
      expect(screen.queryByRole("dialog", { name: "New task" })).toBeNull();
    },
    { timeout: 30_000 },
  );
}

export async function createSchedule({ runner, driver, ids }: ConfigFlowContext) {
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.schedules.map(
      (schedule) => schedule.schedule_id,
    ),
  );
  await driver.openConfigSection("schedules");
  await driver.user.click(screen.getByRole("button", { name: "New schedule" }));
  await waitForDeploymentDocument(runner, (current) => {
    const created = current.schedules.find(
      (schedule) => !before.has(schedule.schedule_id),
    );
    expect(created).toBeDefined();
    ids.scheduleId = created!.schedule_id;
  });
  await waitForField(`${ids.scheduleId}-name`);
  changeField(`${ids.scheduleId}-name`, "Config Flow Hourly");
  await chooseField(driver, `${ids.scheduleId}-cadence`, /^Interval$/);
  changeField(`${ids.scheduleId}-interval`, "3600");
  await saveEditor(driver);
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
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.eventSources.map(
      (source) => source.event_source_id,
    ),
  );
  await driver.openConfigSection("event-sources");
  await driver.user.click(screen.getByRole("button", { name: "New event source" }));
  await waitForDeploymentDocument(runner, (current) => {
    const created = current.eventSources.find(
      (source) => !before.has(source.event_source_id),
    );
    expect(created).toBeDefined();
    ids.eventSourceId = created!.event_source_id;
  });
  await waitForField(`${ids.eventSourceId}-name`);
  changeField(`${ids.eventSourceId}-name`, "Config Flow Events");
  await saveEditor(driver);
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
  const before = new Set(
    (await runner.fetchSnapshot()).client?.deployments[0]?.triggers.map(
      (trigger) => trigger.config.trigger_id,
    ),
  );
  await driver.openConfigSection("triggers");
  await driver.user.click(screen.getByRole("button", { name: "New trigger" }));
  changeField("auto-name", "Config Flow Event Trigger");
  await chooseField(driver, "auto-kind", /^When something happens$/);
  await chooseField(driver, "auto-event", /^Config Flow Events$/);
  await chooseField(driver, "auto-task", /^Config Flow Smoke Task$/);
  await driver.user.click(
    within(screen.getByRole("dialog", { name: "New trigger" })).getByRole("button", {
      name: "Create",
    }),
  );
  await waitForDeploymentDocument(runner, (current) => {
    const created = current.triggers.find(
      (trigger) => !before.has(trigger.config.trigger_id),
    );
    expect(created).toBeDefined();
    ids.triggerDocId = created!.config.trigger_id;
  });
  await waitForField(`${ids.triggerDocId}-name`);
  await chooseField(driver, `${ids.triggerDocId}-concurrency`, /^Latest only$/);
  await saveEditor(driver);
  await waitForDeploymentDocument(runner, (current) => {
    const trigger = current.triggers.find(
      (candidate) => candidate.config.trigger_id === ids.triggerDocId,
    );
    expect(trigger?.config.task_id).toBe(ids.taskId);
    expect(trigger?.config.source).toEqual({
      kind: "event",
      event_source_id: ids.eventSourceId,
    });
    expect(trigger?.config.concurrency).toBe("latest_only");
  });
}

export async function waitForConfigFlowReady(
  runner: LiveBridgeRunner,
  ids: ConfigFlowIds,
) {
  await waitForConfigFlowDocuments(runner, ids);
  try {
    await waitFor(
      async () => {
        const current = (await runner.fetchSnapshot()).client?.deployments[0];
        expect(current).toBeDefined();
        expect(current!.nodeReadiness.source.state).toBe("current");
        expect(current!.runtime?.lastReconcileResult).not.toBe("error");
        expect(current!.nodeReadiness.routerGeneration).toBe(
          current!.nodeReadiness.activeGeneration,
        );
        const readiness = current!.nodeReadiness.agents.find(
          (status) => status.agentId === ids.agentId,
        );
        expect(readiness?.state).toBe("ready");
      },
      { timeout: 90_000 },
    );
  } catch (error) {
    const current = (await runner.fetchSnapshot()).client?.deployments[0];
    const health = await runner.adapter.listBackendsWithHealth();
    throw new Error(
      `Config agent did not become ready: ${String(error).split("\n")[0]}; readiness=${JSON.stringify(current?.nodeReadiness)}; backendHealth=${JSON.stringify(health)}`,
    );
  }
}
