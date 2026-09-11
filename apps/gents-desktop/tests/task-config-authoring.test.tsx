import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { TaskSaveRequest, TaskView } from "@source-inc/gents-desktop-client";
import { TaskConfigEditor } from "../src/components/config/TaskConfigPanel";

const task: TaskView = {
  taskId: "verify-release",
  name: "Verify release",
  description: "Run release checks",
  behaviorId: "default",
  promptTemplate: "Verify {{version}}",
  goalObjectiveTemplate: null,
  goalTokenBudget: null,
  hooks: [
    {
      hook_id: "prepare",
      phase: "before",
      command: ["sh", "-c", "./prepare.sh && ./verify.sh"],
      timeout_secs: 45,
    },
    {
      hook_id: "cleanup",
      phase: "finally",
      command: ["./cleanup", "--all"],
      timeout_secs: null,
    },
  ],
  enabled: true,
  outputSchemaRef: "schemas/release-result.json",
  tags: ["release", "verification"],
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

function renderEditor(onSaveTaskConfig = vi.fn()) {
  return render(
    <TaskConfigEditor
      agentDid="did:test:agent"
      behaviors={[
        {
          behaviorId: "default",
          agentDid: "did:test:agent",
          displayName: "Default",
          description: null,
          contextId: null,
          inferenceProfileId: null,
          enabled: true,
          isDefault: true,
          tags: [],
          createdAt: null,
        },
      ]}
      runningTask={false}
      savedStatus={null}
      saving={false}
      selectedBehavior={null}
      task={task}
      onDeleteTaskConfig={vi.fn()}
      onDeleted={vi.fn()}
      onRunTask={vi.fn()}
      onSaveTaskConfig={onSaveTaskConfig}
      onSaved={vi.fn()}
    />,
  );
}

describe("task hook and tag authoring", () => {
  it("hydrates canonical hooks and saves literal argv, phases, timeouts, and tags", async () => {
    const onSaveTaskConfig = vi.fn<[(request: TaskSaveRequest) => Promise<unknown>]>(
      () => Promise.resolve(),
    );
    renderEditor(onSaveTaskConfig);

    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    expect(screen.getByTestId("task-hook-id-0")).toHaveValue("prepare");
    expect(screen.getByTestId("task-hook-phase-1")).toHaveValue("finally");
    expect(screen.getByTestId("task-hook-command-0")).toHaveValue(
      '["sh","-c","./prepare.sh && ./verify.sh"]',
    );
    expect(screen.getByTestId("task-hook-timeout-0")).toHaveValue(45);
    expect(screen.getByTestId("task-hook-timeout-1")).toHaveValue(null);
    expect(screen.getByTestId("task-output-schema-ref")).toHaveValue(
      "schemas/release-result.json",
    );
    expect(screen.getByTestId("task-tags")).toHaveValue("release\nverification");

    fireEvent.change(screen.getByTestId("task-hook-phase-0"), {
      target: { value: "after_success" },
    });
    fireEvent.change(screen.getByTestId("task-hook-command-0"), {
      target: { value: '["cargo","test","-p","gents"]' },
    });
    fireEvent.change(screen.getByTestId("task-tags"), {
      target: { value: "release, smoke\nnightly" },
    });

    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("task-save"));

    await waitFor(() =>
      expect(onSaveTaskConfig).toHaveBeenCalledWith({
        document: {
          agent_did: "did:test:agent",
          task_id: "verify-release",
          display_name: "Verify release",
          description: "Run release checks",
          behavior_id: "default",
          prompt_template: "Verify {{version}}",
          goal_objective_template: "",
          goal_token_budget: null,
          hooks: [
            {
              hook_id: "prepare",
              phase: "after_success",
              command: ["cargo", "test", "-p", "gents"],
              timeout_secs: 45,
            },
            {
              hook_id: "cleanup",
              phase: "finally",
              command: ["./cleanup", "--all"],
              timeout_secs: null,
            },
          ],
          enabled: true,
          output_schema_ref: "schemas/release-result.json",
          tags: ["release", "smoke", "nightly"],
        },
      }),
    );
  });

  it("rejects ambiguous command text, duplicate hook IDs, and invalid timeouts", () => {
    renderEditor();

    fireEvent.change(screen.getByTestId("task-hook-command-0"), {
      target: { value: "cargo test" },
    });
    expect(
      screen.getByText("Enter a non-empty JSON array of string arguments."),
    ).toBeInTheDocument();
    expect(screen.getByTestId("task-save")).toBeDisabled();

    fireEvent.change(screen.getByTestId("task-hook-command-0"), {
      target: { value: '["cargo","test"]' },
    });
    fireEvent.change(screen.getByTestId("task-hook-id-1"), {
      target: { value: "prepare" },
    });
    expect(
      screen.getByText("Hook IDs must be unique within the task."),
    ).toBeInTheDocument();
    expect(screen.getByTestId("task-save")).toBeDisabled();

    fireEvent.change(screen.getByTestId("task-hook-id-1"), {
      target: { value: "cleanup" },
    });
    fireEvent.change(screen.getByTestId("task-hook-timeout-1"), {
      target: { value: "0" },
    });
    expect(
      screen.getByText("Enter a positive whole number or leave it blank."),
    ).toBeInTheDocument();
    expect(screen.getByTestId("task-save")).toBeDisabled();
  });
});
