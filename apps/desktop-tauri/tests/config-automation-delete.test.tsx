import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { TaskConfigEditor } from "../src/components/config/TaskConfigPanel";
import { ScheduleConfigEditor } from "../src/components/config/ScheduleConfigPanel";
import { BackendConfigEditor } from "../src/components/config/BackendConfigPanel";
import type { TaskView } from "../src/lib/types";

const task: TaskView = {
  taskId: "nightly-report",
  name: "Nightly report",
  behaviorId: "default",
  promptTemplate: "Summarize the day.",
  enabled: true,
  recentRuns: { totalFires: 0, runs: [] },
} as unknown as TaskView;

describe("automation document deletion", () => {
  it("deletes a task only after confirmation", async () => {
    const onDeleteTaskConfig = vi.fn().mockResolvedValue(undefined);
    const onDeleted = vi.fn();
    render(
      <TaskConfigEditor
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
      expect(onDeleteTaskConfig).toHaveBeenCalledWith({ taskId: "nightly-report" }),
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
        schedule={null}
        tasks={[]}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onSaveScheduleConfig={vi.fn()}
        onDeleteScheduleConfig={vi.fn()}
        onDeleted={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("schedule-delete")).not.toBeInTheDocument();
  });

  it("deletes a backend through its confirm dialog", async () => {
    const onDeleteBackendConfig = vi.fn().mockResolvedValue(undefined);
    const onDeleted = vi.fn();
    render(
      <BackendConfigEditor
        backend={
          {
            backendId: "openai-main",
            name: "OpenAI",
            providerKind: "openai",
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
        onDeleteBackendConfig={onDeleteBackendConfig}
        onDeleted={onDeleted}
      />,
    );

    fireEvent.click(screen.getByTestId("backend-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(onDeleteBackendConfig).toHaveBeenCalledWith({ backendId: "openai-main" }),
    );
    await waitFor(() => expect(onDeleted).toHaveBeenCalled());
  });
});
