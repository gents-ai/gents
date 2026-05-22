import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { BackendConfigEditor } from "../src/components/config/BackendConfigPanel";
import { EventTriggerConfigEditor } from "../src/components/config/EventTriggerConfigPanel";
import { InferenceProfileConfigEditor } from "../src/components/config/InferenceProfileConfigPanel";
import { ScheduleConfigEditor } from "../src/components/config/ScheduleConfigPanel";
import { TaskConfigEditor } from "../src/components/config/TaskConfigPanel";
import { ToolSelectionConfigEditor } from "../src/components/config/ToolSelectionConfigPanel";
import { ToolServiceConfigEditor } from "../src/components/config/ToolServiceConfigPanel";
import type {
  BackendSaveRequest,
  EventTriggerSaveRequest,
  InferenceProfileSaveRequest,
  ScheduleSaveRequest,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
} from "../src/lib/types";
import {
  backend,
  profile,
  schedule,
  task,
  toolSelection,
  toolService,
} from "./config-panel-buttons/fixtures";

describe("config panel action buttons", () => {
  it("keeps persisted document IDs immutable when saving existing rows", async () => {
    const onSaveBackendConfig = vi.fn<
      [(request: BackendSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <BackendConfigEditor
        backend={backend}
        savedStatus={null}
        saving={false}
        onSaveBackendConfig={onSaveBackendConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("backend-id"), {
      target: { value: "renamed-backend" },
    });
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(onSaveBackendConfig).toHaveBeenCalledWith(
        expect.objectContaining({ backendId: "default-backend" }),
      ),
    );
    expect(screen.getByTestId("backend-id")).toHaveAttribute("readonly");

    const onSaveProfileConfig = vi.fn<
      [(request: InferenceProfileSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <InferenceProfileConfigEditor
        profile={profile}
        savedStatus={null}
        saving={false}
        onSaveInferenceProfileConfig={onSaveProfileConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("profile-id"), {
      target: { value: "renamed-profile" },
    });
    fireEvent.click(screen.getByTestId("profile-save"));
    await waitFor(() =>
      expect(onSaveProfileConfig).toHaveBeenCalledWith(
        expect.objectContaining({ profileId: "default-profile" }),
      ),
    );
    expect(screen.getByTestId("profile-id")).toHaveAttribute("readonly");

    const onSaveToolSelectionConfig = vi.fn<
      [(request: ToolSelectionSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <ToolSelectionConfigEditor
        agentDid="did:key:z6MkAgent"
        savedStatus={null}
        saving={false}
        toolCeiling="Readwrite"
        toolRoot="/tmp/work"
        toolSelection={toolSelection}
        toolServiceRegistries={[toolService]}
        onSaveToolSelectionConfig={onSaveToolSelectionConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("tool-selection-id"), {
      target: { value: "renamed-tools" },
    });
    fireEvent.click(screen.getByTestId("tool-selection-save"));
    await waitFor(() =>
      expect(onSaveToolSelectionConfig).toHaveBeenCalledWith(
        expect.objectContaining({ selectionId: "default-tools" }),
      ),
    );
    expect(screen.getByTestId("tool-selection-id")).toHaveAttribute("readonly");

    const onSaveToolServiceConfig = vi.fn<
      [(request: ToolServiceSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <ToolServiceConfigEditor
        savedStatus={null}
        saving={false}
        toolService={toolService}
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
        expect.objectContaining({ serviceId: "mcp-local" }),
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
        expect.objectContaining({ taskId: "task-a" }),
      ),
    );
    expect(screen.getByTestId("task-id")).toHaveAttribute("readonly");

    const onSaveScheduleConfig = vi.fn<
      [(request: ScheduleSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <ScheduleConfigEditor
        runningTask={false}
        savedStatus={null}
        saving={false}
        schedule={schedule}
        selectedTask={task}
        tasks={[task]}
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
        expect.objectContaining({ scheduleId: "timer-a" }),
      ),
    );
    expect(screen.getByTestId("schedule-id")).toHaveAttribute("readonly");

    const onSaveEventTriggerConfig = vi.fn<
      [(request: EventTriggerSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    render(
      <EventTriggerConfigEditor
        eventTrigger={{
          triggerId: "event-a",
          taskId: "task-a",
          sourceCollection: "AgentRequest",
          eventKind: "created",
          enabled: true,
          concurrency: "serial",
          fireCount: 0,
        }}
        savedStatus={null}
        saving={false}
        selectedTask={task}
        tasks={[task]}
        onSaveEventTriggerConfig={onSaveEventTriggerConfig}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("event-trigger-id"), {
      target: { value: "renamed-event" },
    });
    fireEvent.click(screen.getByTestId("event-trigger-save"));
    await waitFor(() =>
      expect(onSaveEventTriggerConfig).toHaveBeenCalledWith(
        expect.objectContaining({ triggerId: "event-a" }),
      ),
    );
    expect(screen.getByTestId("event-trigger-id")).toHaveAttribute("readonly");
  });

  it("migrates legacy service delegates into the MCP allowlist on save", async () => {
    const onSaveToolSelectionConfig = vi.fn<
      [(request: ToolSelectionSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());

    render(
      <ToolSelectionConfigEditor
        agentDid="did:key:z6MkAgent"
        savedStatus={null}
        saving={false}
        toolCeiling="Readwrite"
        toolRoot="/tmp/work"
        toolSelection={{
          ...toolSelection,
          allowedMcpServiceIds: [],
          delegateTo: ["mcp-local", "did:key:zDelegate"],
        }}
        toolServiceRegistries={[toolService]}
        onSaveToolSelectionConfig={onSaveToolSelectionConfig}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByTestId("tool-allowed-mcp-service-mcp-local")).toBeChecked();
    fireEvent.click(screen.getByTestId("tool-selection-save"));

    await waitFor(() =>
      expect(onSaveToolSelectionConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          allowedMcpServiceIds: ["mcp-local"],
          delegateTo: ["did:key:zDelegate"],
        }),
      ),
    );
  });

  it("applies the server tool ceiling when saving tool selections", async () => {
    const onSaveToolSelectionConfig = vi.fn<
      [(request: ToolSelectionSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());

    render(
      <ToolSelectionConfigEditor
        agentDid="did:key:z6MkAgent"
        savedStatus={null}
        saving={false}
        toolCeiling="MetaOnly"
        toolRoot="/tmp/work"
        toolSelection={toolSelection}
        toolServiceRegistries={[toolService]}
        onSaveToolSelectionConfig={onSaveToolSelectionConfig}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByTestId("tool-enable-file-tools")).toBeDisabled();
    expect(screen.getByTestId("tool-enable-bash")).toBeDisabled();
    fireEvent.click(screen.getByTestId("tool-selection-save"));

    await waitFor(() =>
      expect(onSaveToolSelectionConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          enableFileTools: false,
          enableBash: false,
        }),
      ),
    );
  });
});
