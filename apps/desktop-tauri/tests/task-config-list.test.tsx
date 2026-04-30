import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { TaskConfigPanel } from "../src/components/config/TaskConfigPanel";
import type { DeploymentView, TaskView } from "../src/lib/types";

function task(taskId: string, name: string): TaskView {
  return {
    taskId,
    name,
    behaviorId: "default",
    enabled: true,
    promptTemplate: "Run the task.",
    recentRuns: {
      totalFires: 0,
      scheduleCount: 0,
      eventTriggerCount: 0,
    },
    runHistory: [],
  };
}

const deployment: DeploymentView = {
  peerId: "peer-1",
  label: "mini-1-steward",
  agentDid: "did:key:z6Mini",
  addr: "iroh://mini-1",
  dialSucceeded: true,
  agentPrincipal: {
    agentDid: "did:key:z6Mini",
  },
  behaviors: [
    {
      behaviorId: "default",
      displayName: "Mini 1 Host Steward",
      enabled: true,
      isDefault: true,
    },
  ],
  inferenceBackends: [],
  inferenceProfiles: [],
  toolSelections: [],
  toolServiceRegistries: [],
  tasks: [
    task("host-health-6h", "DEFAULT"),
    task("freshness-check", "Freshness Check"),
  ],
  schedules: [],
  eventTriggers: [],
  conversations: [],
};

describe("TaskConfigPanel", () => {
  it("uses task ids instead of repeated DEFAULT labels in the task list", () => {
    render(
      <TaskConfigPanel
        deployment={deployment}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        selectedTaskId="host-health-6h"
        onCreateTask={vi.fn()}
        onRunTask={vi.fn()}
        onSaveTaskConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectTask={vi.fn()}
      />,
    );

    expect(
      within(screen.getByTestId("config-task-host-health-6h")).getByText(
        "host-health-6h",
      ),
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId("config-task-freshness-check")).getByText(
        "Freshness Check",
      ),
    ).toBeInTheDocument();
  });
});
