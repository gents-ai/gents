import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SkillConfigPanel } from "../src/components/config";
import type { DeploymentView } from "../src/lib/types";

const deployment: DeploymentView = {
  deploymentId: "dep-1",
  agentDid: "did:test:operator",
  displayName: "test",
  defaultBehaviorId: "default",
  behaviors: [{ behaviorId: "default", displayName: "default" }],
  conversations: [],
  process: null,
  runtime: null,
  inbox: { hasUnread: false, count: 0 },
  skills: [
    {
      skillId: "review-skill",
      agentDid: "did:test:skill-source",
      name: "Review",
      instructions: "review things",
      toolRefs: [],
      scope: "behavior",
      enabled: true,
    },
  ],
};

function renderPanel(overrides: { onDeleteSkillConfig?: ReturnType<typeof vi.fn> }) {
  const onDeleteSkillConfig = overrides.onDeleteSkillConfig ?? vi.fn();
  const onDeletedSkill = vi.fn();
  render(
    <SkillConfigPanel
      deployment={deployment}
      selectedSkillId="review-skill"
      saving={false}
      savedStatus={null}
      onSelectSkill={vi.fn()}
      onCreateSkill={vi.fn()}
      onDeletedSkill={onDeletedSkill}
      onSavedStatusChange={vi.fn()}
      onDeleteSkillConfig={onDeleteSkillConfig}
      onSaveSkillConfig={vi.fn()}
    />,
  );
  return { onDeleteSkillConfig, onDeletedSkill };
}

describe("SkillConfigPanel delete confirmation", () => {
  it("opens the in-app confirm dialog instead of window.confirm and deletes on confirm", async () => {
    const onDeleteSkillConfig = vi.fn().mockResolvedValue(undefined);
    const confirmSpy = vi.spyOn(window, "confirm");
    const { onDeletedSkill } = renderPanel({ onDeleteSkillConfig });

    fireEvent.click(screen.getByTestId("skill-delete"));

    expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    expect(confirmSpy).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));

    await waitFor(() =>
      expect(onDeleteSkillConfig).toHaveBeenCalledWith({
        skillId: "review-skill",
        agentDid: "did:test:skill-source",
      }),
    );
    await waitFor(() => expect(onDeletedSkill).toHaveBeenCalled());
    expect(screen.queryByTestId("confirm-dialog")).not.toBeInTheDocument();
  });

  it("does not delete when the dialog is cancelled", () => {
    const onDeleteSkillConfig = vi.fn();
    renderPanel({ onDeleteSkillConfig });

    fireEvent.click(screen.getByTestId("skill-delete"));
    fireEvent.click(screen.getByTestId("confirm-dialog-cancel"));

    expect(onDeleteSkillConfig).not.toHaveBeenCalled();
    expect(screen.queryByTestId("confirm-dialog")).not.toBeInTheDocument();
  });
});
