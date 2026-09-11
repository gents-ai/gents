import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { TaskConfigEditor } from "../src/components/config/TaskConfigPanel";
import { ScheduleConfigEditor } from "../src/components/config/ScheduleConfigPanel";
import { BackendConfigEditor } from "../src/components/config/BackendConfigPanel";
import { BehaviorConfigEditor } from "../src/components/config/BehaviorConfigPanel";
import { EventSourceConfigEditor } from "../src/components/config/EventSourceConfigPanel";
import { InferenceProfileConfigEditor } from "../src/components/config/InferenceProfileConfigPanel";
import { ToolsConfigEditor } from "../src/components/config/ToolsConfigPanel";
import { ToolServiceConfigEditor } from "../src/components/config/ToolServiceConfigPanel";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import {
  eventSource,
  schedule,
  tools,
  toolService,
} from "./config-panel-buttons/fixtures";

const sourceAgentDid = "did:test:source";

const task = {
  taskId: "nightly-report",
  name: "Nightly report",
  behaviorId: "default",
  promptTemplate: "Summarize the day.",
  goalObjectiveTemplate: null,
  goalTokenBudget: null,
  hooks: [],
  enabled: true,
  outputSchemaRef: null,
  tags: [],
  recentRuns: {
    totalFires: 0,
    lastAttemptAt: null,
    lastStatus: null,
    lastError: null,
    scheduleCount: 0,
    eventCount: 0,
  },
  runHistory: [],
};

const api = {} as DesktopApiAdapter;

describe("automation document deletion", () => {
  it("deletes a task only after confirmation", async () => {
    const onDeleteTaskConfig = vi.fn().mockResolvedValue(undefined);
    const onDeleted = vi.fn();
    render(
      <TaskConfigEditor
        agentDid={sourceAgentDid}
        behaviors={[]}
        selectedBehavior={null}
        task={task}
        savedStatus={null}
        saving={false}
        runningTask={false}
        onSaved={vi.fn()}
        onSaveTaskConfig={vi.fn()}
        onDeleteTaskConfig={onDeleteTaskConfig}
        onDeleted={onDeleted}
        onRunTask={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("task-delete"));
    expect(onDeleteTaskConfig).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteTaskConfig).toHaveBeenCalledWith({
        taskId: "nightly-report",
        agentDid: sourceAgentDid,
      }),
    );
    await waitFor(() => expect(onDeleted).toHaveBeenCalled());
  });

  it("keeps the editor mounted when the delete is rejected", async () => {
    const onDeleteTaskConfig = vi
      .fn()
      .mockRejectedValue(new Error("referenced by 1 schedule(s)"));
    const onDeleted = vi.fn();
    render(
      <TaskConfigEditor
        agentDid={sourceAgentDid}
        behaviors={[]}
        selectedBehavior={null}
        task={task}
        savedStatus={null}
        saving={false}
        runningTask={false}
        onSaved={vi.fn()}
        onSaveTaskConfig={vi.fn()}
        onDeleteTaskConfig={onDeleteTaskConfig}
        onDeleted={onDeleted}
        onRunTask={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("task-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() => expect(onDeleteTaskConfig).toHaveBeenCalled());
    expect(onDeleted).not.toHaveBeenCalled();
    expect(screen.getByTestId("task-save")).toBeInTheDocument();
  });

  it("offers no delete button for a new unsaved schedule", () => {
    render(
      <ScheduleConfigEditor
        agentDid={sourceAgentDid}
        schedule={null}
        savedStatus={null}
        saving={false}
        runningTask={false}
        onSaved={vi.fn()}
        onSaveScheduleConfig={vi.fn()}
        onDeleteScheduleConfig={vi.fn()}
        onDeleted={vi.fn()}
        onRunSchedule={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("schedule-delete")).not.toBeInTheDocument();
  });

  it("deletes a backend through its confirm dialog", async () => {
    const onDeleteBackendConfig = vi.fn().mockResolvedValue(undefined);
    const onDeleted = vi.fn();
    render(
      <BackendConfigEditor
        agentDid={sourceAgentDid}
        backend={
          {
            backendId: "openai-main",
            name: "OpenAI",
            providerKind: "OpenAiCompatible",
            endpoint: "http://127.0.0.1:1/v1",
            apiKeyConfigured: false,
            maxConcurrent: 1,
            maxQueueDepth: 1,
            enabled: true,
            models: [],
          } as never
        }
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onSaveBackendConfig={vi.fn()}
        onPatchConfigComponents={vi.fn()}
        onDeleteBackendConfig={onDeleteBackendConfig}
        onDeleted={onDeleted}
      />,
    );

    fireEvent.click(screen.getByTestId("backend-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteBackendConfig).toHaveBeenCalledWith({
        backendId: "openai-main",
        agentDid: sourceAgentDid,
      }),
    );
    await waitFor(() => expect(onDeleted).toHaveBeenCalled());
  });

  it("hides behavior delete for the default behavior and deletes others", async () => {
    const onDeleteBehaviorConfig = vi.fn().mockResolvedValue(undefined);
    const base = {
      api,
      agentDid: "did:key:z6MkAgent",
      principal: {
        agent_did: "did:key:z6MkAgent",
        default_behavior_id: "default",
      },
      contexts: [],
      compactions: [],
      inferenceProfiles: [{ profile_id: "p", agent_did: "did:key:z6MkAgent" }],
      skills: [],
      tools: [],
      savedStatus: null,
      saving: false,
      onCreateProfile: vi.fn(),
      onCreateTools: vi.fn(),
      onSaved: vi.fn(),
      onSaveAgentConfig: vi.fn(),
      onApplyConfigComponents: vi.fn(),
      onDeleteBehaviorConfig,
      onDeleted: vi.fn(),
    };
    const { rerender } = render(
      <BehaviorConfigEditor
        {...base}
        behavior={
          {
            behavior_id: "default",
            display_name: "default",
            inference_profile_id: "p",
            enabled: true,
          } as never
        }
      />,
    );
    expect(screen.queryByTestId("behavior-delete")).not.toBeInTheDocument();

    rerender(
      <BehaviorConfigEditor
        {...base}
        behavior={
          {
            behavior_id: "ops",
            display_name: "ops",
            inference_profile_id: "p",
            enabled: true,
          } as never
        }
      />,
    );
    fireEvent.click(screen.getByTestId("behavior-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteBehaviorConfig).toHaveBeenCalledWith({
        behaviorId: "ops",
        agentDid: "did:key:z6MkAgent",
      }),
    );
  });

  it("routes the selected deployment when deleting a schedule", async () => {
    const onDeleteScheduleConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <ScheduleConfigEditor
        agentDid={sourceAgentDid}
        schedule={schedule}
        savedStatus={null}
        saving={false}
        runningTask={false}
        onSaved={vi.fn()}
        onSaveScheduleConfig={vi.fn()}
        onDeleteScheduleConfig={onDeleteScheduleConfig}
        onDeleted={vi.fn()}
        onRunSchedule={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("schedule-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteScheduleConfig).toHaveBeenCalledWith({
        scheduleId: "timer-a",
        agentDid: sourceAgentDid,
      }),
    );
  });

  it("routes the selected deployment when deleting an event source", async () => {
    const onDeleteEventSourceConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <EventSourceConfigEditor
        agentDid={sourceAgentDid}
        eventSource={eventSource}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onSaveEventSourceConfig={vi.fn()}
        onDeleteEventSourceConfig={onDeleteEventSourceConfig}
        onDeleted={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("event-source-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteEventSourceConfig).toHaveBeenCalledWith({
        eventSourceId: "source-a",
        agentDid: sourceAgentDid,
      }),
    );
  });

  it("routes the selected deployment when deleting an inference profile", async () => {
    const onDeleteInferenceProfileConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <InferenceProfileConfigEditor
        agentDid={sourceAgentDid}
        profile={{ profile_id: "profile-a" }}
        samplingConfigs={[]}
        executionConfigs={[]}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onApplyConfigComponents={vi.fn()}
        onDeleteInferenceProfileConfig={onDeleteInferenceProfileConfig}
        onDeleted={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("profile-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteInferenceProfileConfig).toHaveBeenCalledWith({
        profileId: "profile-a",
        agentDid: sourceAgentDid,
      }),
    );
  });

  it("routes the selected deployment when deleting a tools document", async () => {
    const onDeleteToolsConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <ToolsConfigEditor
        agentDid={sourceAgentDid}
        tools={tools}
        toolServices={[toolService]}
        subagentTargets={[]}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onApplyConfigComponents={vi.fn()}
        onDeleteToolsConfig={onDeleteToolsConfig}
        onDeleted={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("tools-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteToolsConfig).toHaveBeenCalledWith({
        toolsId: "default-tools",
        agentDid: sourceAgentDid,
      }),
    );
  });

  it("routes the selected deployment when deleting a tool service", async () => {
    const onDeleteToolServiceConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <ToolServiceConfigEditor
        agentDid={sourceAgentDid}
        toolService={toolService}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onSaveToolServiceConfig={vi.fn()}
        onDeleteToolServiceConfig={onDeleteToolServiceConfig}
        onDeleted={vi.fn()}
        onTestToolService={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("tool-service-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteToolServiceConfig).toHaveBeenCalledWith({
        serviceId: "mcp-local",
        agentDid: sourceAgentDid,
      }),
    );
  });
});
