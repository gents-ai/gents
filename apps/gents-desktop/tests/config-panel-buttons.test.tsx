import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { BackendConfigEditor } from "../src/components/config/BackendConfigPanel";
import { EventSourceConfigEditor } from "../src/components/config/EventSourceConfigPanel";
import { InferenceProfileConfigEditor } from "../src/components/config/InferenceProfileConfigPanel";
import { ScheduleConfigEditor } from "../src/components/config/ScheduleConfigPanel";
import { TaskConfigEditor } from "../src/components/config/TaskConfigPanel";
import { ToolsConfigEditor } from "../src/components/config/ToolsConfigPanel";
import { ToolServiceConfigEditor } from "../src/components/config/ToolServiceConfigPanel";
import type {
  ConfigComponentsPatchRequest,
  ConfigComponentsApplyRequest,
  EventSourceSaveRequest,
  ScheduleSaveRequest,
  TaskSaveRequest,
  ToolServiceSaveRequest,
} from "@source-inc/gents-desktop-client";
import {
  backend,
  eventSource,
  schedule,
  task,
  toolService,
} from "./config-panel-buttons/fixtures";

describe("config panel action buttons", () => {
  it("keeps persisted document IDs immutable when saving existing rows", async () => {
    const onPatchConfigComponents = vi.fn<
      [(request: ConfigComponentsPatchRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <BackendConfigEditor
        agentDid="did:key:z6MkAgent"
        backend={backend}
        savedStatus={null}
        saving={false}
        onSaveBackendConfig={vi.fn()}
        onPatchConfigComponents={onPatchConfigComponents}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("backend-id"), {
      target: { value: "renamed-backend" },
    });
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(onPatchConfigComponents).toHaveBeenCalledWith({
        agentDid: "did:key:z6MkAgent",
        patches: [
          { collection: "InferenceBackend", id: "default-backend", changes: {} },
        ],
      }),
    );
    expect(screen.getByTestId("backend-id")).toHaveAttribute("readonly");

    const onSaveProfileConfig = vi.fn<
      [(request: ConfigComponentsApplyRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <InferenceProfileConfigEditor
        agentDid="did:key:z6MkAgent"
        profile={{
          agent_did: "did:key:z6MkAgent",
          profile_id: "default-profile",
          backend_id: "default-backend",
          model_name: "model",
        }}
        samplingConfigs={[]}
        executionConfigs={[]}
        savedStatus={null}
        saving={false}
        onApplyConfigComponents={onSaveProfileConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("profile-id"), {
      target: { value: "renamed-profile" },
    });
    fireEvent.click(screen.getByTestId("profile-save"));
    await waitFor(() =>
      expect(onSaveProfileConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          document: expect.objectContaining({
            inference_profiles: [
              expect.objectContaining({ profile_id: "default-profile" }),
            ],
          }),
        }),
      ),
    );
    expect(screen.getByTestId("profile-id")).toHaveAttribute("readonly");

    const onApplyTools = vi.fn().mockResolvedValue(undefined);
    render(
      <ToolsConfigEditor
        agentDid="did:key:z6MkAgent"
        tools={{ agent_did: "did:key:z6MkAgent", tools_id: "default-tools" }}
        toolServices={[]}
        subagentTargets={[]}
        savedStatus={null}
        saving={false}
        onApplyConfigComponents={onApplyTools}
        onDeleteToolsConfig={vi.fn()}
        onDeleted={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("tools-id"), {
      target: { value: "renamed-tools" },
    });
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(onApplyTools).toHaveBeenCalledWith({
        document: {
          agent_principal: { agent_did: "did:key:z6MkAgent" },
          tools: [{ agent_did: "did:key:z6MkAgent", tools_id: "default-tools" }],
          subagent_targets: [],
        },
      }),
    );
    expect(screen.getByTestId("tools-id")).toHaveAttribute("readonly");

    const onSaveToolServiceConfig = vi.fn<
      [(request: ToolServiceSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <ToolServiceConfigEditor
        savedStatus={null}
        saving={false}
        agentDid="did:key:z6MkAgent"
        toolService={{
          agent_did: "did:key:z6MkAgent",
          service_id: "mcp-local",
          hostname: "localhost",
          mcp_port: 7331,
        }}
        onSaveToolServiceConfig={onSaveToolServiceConfig}
        onSaved={vi.fn()}
        onTestToolService={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("tool-service-id"), {
      target: { value: "renamed-service" },
    });
    fireEvent.click(screen.getByTestId("tool-service-save"));
    await waitFor(() =>
      expect(onSaveToolServiceConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          document: expect.objectContaining({ service_id: "mcp-local" }),
        }),
      ),
    );
    expect(screen.getByTestId("tool-service-id")).toHaveAttribute("readonly");

    const onSaveTaskConfig = vi.fn<[(request: TaskSaveRequest) => Promise<unknown>]>(
      () => Promise.resolve(),
    );
    render(
      <TaskConfigEditor
        behaviors={[
          {
            behaviorId: "default",
            displayName: "Default",
            enabled: true,
            isDefault: true,
          },
        ]}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedBehavior={null}
        task={task}
        onRunTask={vi.fn()}
        onSaveTaskConfig={onSaveTaskConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("task-id"), {
      target: { value: "renamed-task" },
    });
    fireEvent.click(screen.getByTestId("task-save"));
    await waitFor(() =>
      expect(onSaveTaskConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          document: expect.objectContaining({ task_id: "task-a" }),
        }),
      ),
    );
    expect(screen.getByTestId("task-id")).toHaveAttribute("readonly");

    const onSaveScheduleConfig = vi.fn<
      [(request: ScheduleSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <ScheduleConfigEditor
        agentDid="agent-a"
        runningTask={false}
        savedStatus={null}
        saving={false}
        schedule={schedule}
        onRunSchedule={vi.fn()}
        onSaveScheduleConfig={onSaveScheduleConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("schedule-id"), {
      target: { value: "renamed-schedule" },
    });
    fireEvent.click(screen.getByTestId("schedule-save"));
    await waitFor(() =>
      expect(onSaveScheduleConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          document: expect.objectContaining({ schedule_id: "timer-a" }),
        }),
      ),
    );
    expect(screen.getByTestId("schedule-id")).toHaveAttribute("readonly");

    const onSaveEventSourceConfig = vi.fn<
      [(request: EventSourceSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <EventSourceConfigEditor
        agentDid="agent-a"
        eventSource={eventSource}
        savedStatus={null}
        saving={false}
        onSaveEventSourceConfig={onSaveEventSourceConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("event-source-id"), {
      target: { value: "renamed-event" },
    });
    fireEvent.click(screen.getByTestId("event-source-save"));
    await waitFor(() =>
      expect(onSaveEventSourceConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          document: expect.objectContaining({
            event_source_id: "source-a",
          }),
        }),
      ),
    );
    expect(screen.getByTestId("event-source-id")).toHaveAttribute("readonly");
  });

  it("keeps all canonical command/datastore/timeouts through advanced authoring", async () => {
    const onApply = vi.fn().mockResolvedValue(undefined);
    const document = {
      agent_did: "owner",
      tools_id: "tools",
      host: {
        bash: {
          mode: "Unrestricted" as const,
          execution_mode: "unrestricted" as const,
          network_mode: "enabled" as const,
          timeout_secs: 45,
        },
      },
      datastore: {
        enable_defra_query: true,
        defra_query_collections: ["Note"],
        datastore_tool_surface_ids: ["surface"],
      },
    };
    render(
      <ToolsConfigEditor
        agentDid="owner"
        tools={document}
        toolServices={[]}
        subagentTargets={[]}
        saving={false}
        savedStatus={null}
        onApplyConfigComponents={onApply}
        onDeleteToolsConfig={vi.fn()}
        onDeleted={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(onApply).toHaveBeenCalledWith({
        document: {
          agent_principal: { agent_did: "owner" },
          tools: [document],
          subagent_targets: [],
        },
      }),
    );
  });
  it("keeps goal capabilities independent of remote service presentation", async () => {
    const onApply = vi.fn().mockResolvedValue(undefined);
    render(
      <ToolsConfigEditor
        agentDid="owner"
        tools={{
          agent_did: "owner",
          tools_id: "tools",
          built_ins: { enable_goal_tools: true, enable_goal_creation: false },
        }}
        toolServices={[]}
        subagentTargets={[]}
        saving={false}
        savedStatus={null}
        onApplyConfigComponents={onApply}
        onDeleteToolsConfig={vi.fn()}
        onDeleted={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    expect(screen.getByTestId("tools-goal-tools")).toBeChecked();
    expect(screen.getByTestId("tools-goal-creation")).not.toBeChecked();
    fireEvent.click(screen.getByTestId("tools-goal-tools"));
    fireEvent.click(screen.getByTestId("tools-goal-creation"));
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() => expect(onApply).toHaveBeenCalledTimes(1));
    expect(onApply.mock.calls[0][0].document.tools[0].built_ins).toEqual({
      enable_goal_tools: false,
      enable_goal_creation: true,
    });
  });
  it("preserves authored host config while displaying the runtime ceiling", async () => {
    const onApply = vi.fn().mockResolvedValue(undefined);
    const document = {
      agent_did: "owner",
      tools_id: "tools",
      host: { files: { mode: "ReadOnly" as const } },
    };
    render(
      <ToolsConfigEditor
        agentDid="owner"
        tools={document}
        toolServices={[]}
        subagentTargets={[]}
        toolCeiling="MetaOnly"
        saving={false}
        savedStatus={null}
        onApplyConfigComponents={onApply}
        onDeleteToolsConfig={vi.fn()}
        onDeleted={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    expect(
      screen.getByText(/Current runtime tool ceiling: MetaOnly/),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("tools-save"));
    await waitFor(() =>
      expect(onApply).toHaveBeenCalledWith({
        document: {
          agent_principal: { agent_did: "owner" },
          tools: [document],
          subagent_targets: [],
        },
      }),
    );
  });
});
