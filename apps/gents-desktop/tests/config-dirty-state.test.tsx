import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  BackendConfigPanel,
  ScheduleConfigPanel,
  SkillConfigPanel,
} from "../src/components/config";
import type { DeploymentView } from "../src/lib/types";

function makeDeployment(): DeploymentView {
  return {
    deploymentId: "dep-1",
    agentDid: "did:test:operator",
    displayName: "test",
    defaultBehaviorId: "default",
    behaviors: [{ behaviorId: "default", displayName: "default" }],
    conversations: [],
    process: null,
    runtime: null,
    inbox: { hasUnread: false, count: 0 },
    inferenceBackends: [
      {
        backendId: "backend-a",
        name: "Backend A",
        providerKind: "openai",
        endpoint: "http://localhost:1234/v1",
        models: ["m-1"],
        enabled: true,
      },
    ],
    skills: [
      {
        skillId: "review-skill",
        name: "Review",
        instructions: "review things",
        toolRefs: [],
        scope: "behavior",
        enabled: true,
      },
    ],
  };
}

const backendHandlers = {
  saving: false,
  savedStatus: null,
  onSelectBackend: vi.fn(),
  onCreateBackend: vi.fn(),
  onSavedStatusChange: vi.fn(),
  onSaveBackendConfig: vi.fn(),
};

describe("config dirty state", () => {
  it("marks the backend editor dirty on edit and clean when the view catches up", () => {
    const { rerender } = render(
      <BackendConfigPanel
        deployment={makeDeployment()}
        selectedBackendId="backend-a"
        {...backendHandlers}
      />,
    );
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId("backend-endpoint"), {
      target: { value: "http://edited:9999/v1" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();

    // Post-save snapshot refresh: the view now carries the edited value.
    const saved = makeDeployment();
    saved.inferenceBackends[0].endpoint = "http://edited:9999/v1";
    rerender(
      <BackendConfigPanel
        deployment={saved}
        selectedBackendId="backend-a"
        {...backendHandlers}
      />,
    );
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });

  it("treats model separators semantically and clears a saved API key", async () => {
    const deployment = makeDeployment();
    deployment.inferenceBackends[0].models = ["m-1", "m-2"];
    const onSaveBackendConfig = vi.fn().mockResolvedValue(undefined);
    render(
      <BackendConfigPanel
        deployment={deployment}
        selectedBackendId="backend-a"
        {...backendHandlers}
        onSaveBackendConfig={onSaveBackendConfig}
      />,
    );

    fireEvent.change(screen.getByTestId("backend-models"), {
      target: { value: "m-1, m-2" },
    });
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId("backend-api-key"), {
      target: { value: "temporary-secret" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
    fireEvent.submit(screen.getByTestId("backend-save").closest("form")!);

    await waitFor(() => expect(screen.getByTestId("backend-api-key")).toHaveValue(""));
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    expect(onSaveBackendConfig).toHaveBeenCalledWith(
      expect.objectContaining({ models: ["m-1", "m-2"] }),
    );
  });

  it("marks the skill editor dirty on edit", () => {
    render(
      <SkillConfigPanel
        deployment={makeDeployment()}
        selectedSkillId="review-skill"
        saving={false}
        savedStatus={null}
        onSelectSkill={vi.fn()}
        onCreateSkill={vi.fn()}
        onDeletedSkill={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onDeleteSkillConfig={vi.fn()}
        onSaveSkillConfig={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("skill-name"), {
      target: { value: "Edited" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
  });

  it("marks the schedule editor dirty on edit", () => {
    const dep = makeDeployment();
    dep.tasks = [];
    dep.schedules = [
      {
        scheduleId: "sched-1",
        taskId: null,
        intervalSecs: 60,
        enabled: true,
        concurrency: "serial",
      },
    ];
    render(
      <ScheduleConfigPanel
        deployment={dep}
        selectedScheduleId="sched-1"
        selectedTaskId={null}
        saving={false}
        runningTask={false}
        savedStatus={null}
        onSelectSchedule={vi.fn()}
        onCreateSchedule={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSaveScheduleConfig={vi.fn()}
        onRunSchedule={vi.fn()}
      />,
    );
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("schedule-interval-secs"), {
      target: { value: "120" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
  });

  it("renders a save failure next to the form, not just the global banner", async () => {
    const onSaveSkillConfig = vi.fn().mockRejectedValue(new Error("schema mismatch"));
    render(
      <SkillConfigPanel
        deployment={makeDeployment()}
        selectedSkillId="review-skill"
        saving={false}
        savedStatus={null}
        onSelectSkill={vi.fn()}
        onCreateSkill={vi.fn()}
        onDeletedSkill={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onDeleteSkillConfig={vi.fn()}
        onSaveSkillConfig={onSaveSkillConfig}
      />,
    );
    fireEvent.submit(screen.getByTestId("skill-save").closest("form")!);
    expect(await screen.findByText(/Save failed: schema mismatch/)).toBeInTheDocument();
  });
});
