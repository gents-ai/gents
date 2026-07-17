import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { BackendConfigPanel, SkillConfigPanel } from "../src/components/config";
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
});
