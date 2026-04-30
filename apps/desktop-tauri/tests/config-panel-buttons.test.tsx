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
  TaskRunResult,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "../src/lib/types";
import {
  backend,
  profile,
  runResult,
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

    const onSaveTaskConfig = vi.fn<
      [(request: TaskSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
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

  it("keeps manual run buttons disabled for drafts and invalid args", async () => {
    const onRunTask = vi.fn<[(request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>]>(
      () => Promise.resolve(runResult),
    );
    const draftTask = render(
      <TaskConfigEditor
        behaviors={[]}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedBehavior={null}
        task={null}
        onRunTask={onRunTask}
        onSaveTaskConfig={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("task-id"), {
      target: { value: "draft-task" },
    });
    expect(screen.getByTestId("task-run")).toBeDisabled();
    draftTask.unmount();

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
        onRunTask={onRunTask}
        onSaveTaskConfig={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("task-run-args"), {
      target: { value: "{bad json" },
    });
    expect(screen.getByTestId("task-run")).toBeDisabled();

    fireEvent.change(screen.getByTestId("task-run-args"), {
      target: { value: '{"foo":"bar"}' },
    });
    fireEvent.click(screen.getByTestId("task-run"));
    await waitFor(() =>
      expect(onRunTask).toHaveBeenCalledWith({
        taskId: "task-a",
        args: { foo: "bar" },
      }),
    );
  });

  it("keeps timer run disabled for drafts and runs existing timers", async () => {
    const onRunSchedule = vi.fn<[(request: { scheduleId: string }) => Promise<TaskRunResult>]>(
      () => Promise.resolve(runResult),
    );
    const draftSchedule = render(
      <ScheduleConfigEditor
        runningTask={false}
        savedStatus={null}
        saving={false}
        schedule={null}
        selectedTask={task}
        tasks={[task]}
        onRunSchedule={onRunSchedule}
        onSaveScheduleConfig={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByTestId("schedule-id"), {
      target: { value: "draft-timer" },
    });
    expect(screen.getByTestId("schedule-run")).toBeDisabled();
    draftSchedule.unmount();

    render(
      <ScheduleConfigEditor
        runningTask={false}
        savedStatus={null}
        saving={false}
        schedule={schedule}
        selectedTask={task}
        tasks={[task]}
        onRunSchedule={onRunSchedule}
        onSaveScheduleConfig={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-run"));
    await waitFor(() =>
      expect(onRunSchedule).toHaveBeenCalledWith({ scheduleId: "timer-a" }),
    );
  });

  it("surfaces tool service test success and errors from the test button", async () => {
    const result: ToolServiceTestResult = {
      serviceId: "mcp-local",
      endpoint: "http://localhost:7331/mcp",
      status: "ok",
      toolCount: 1,
      tools: [{ name: "list_files", description: "List files" }],
    };
    const onTestToolService = vi.fn<
      [(request: ToolServiceTestRequest) => Promise<ToolServiceTestResult>]
    >(() => Promise.resolve(result));

    render(
      <ToolServiceConfigEditor
        savedStatus={null}
        saving={false}
        toolService={toolService}
        onSaveToolServiceConfig={vi.fn()}
        onSaved={vi.fn()}
        onTestToolService={onTestToolService}
      />,
    );
    fireEvent.click(screen.getByTestId("tool-service-test"));
    await waitFor(() =>
      expect(onTestToolService).toHaveBeenCalledWith(
        expect.objectContaining({
          serviceId: "mcp-local",
          hostname: "localhost",
          mcpPort: 7331,
          mcpPath: "/mcp",
        }),
      ),
    );
    expect(await screen.findByTestId("tool-service-test-result")).toHaveTextContent(
      "list_files",
    );

    onTestToolService.mockRejectedValueOnce(new Error("connection refused"));
    fireEvent.click(screen.getByTestId("tool-service-test"));
    expect(await screen.findByTestId("tool-service-test-error")).toHaveTextContent(
      "connection refused",
    );
  });

  it("makes tool service delegates explicit in tool selections", async () => {
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
          enableMetaTools: false,
          delegateTo: [],
        }}
        toolServiceRegistries={[toolService]}
        onSaveToolSelectionConfig={onSaveToolSelectionConfig}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByTestId("tool-delegate-mcp-local")).toBeDisabled();
    fireEvent.click(screen.getByTestId("tool-enable-meta-tools"));
    expect(screen.getByTestId("tool-delegate-mcp-local")).not.toBeDisabled();
    fireEvent.click(screen.getByTestId("tool-delegate-mcp-local"));
    fireEvent.click(screen.getByTestId("tool-selection-save"));

    await waitFor(() =>
      expect(onSaveToolSelectionConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          enableMetaTools: true,
          delegateTo: ["mcp-local"],
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
