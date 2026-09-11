import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AgentConfigEditor } from "../src/components/config/AgentConfigPanel";
import { BehaviorConfigEditor } from "../src/components/config/BehaviorConfigPanel";
import type {
  AgentConfigSaveRequest,
  AgentPrincipalView,
  AgentBehavior,
  DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
const behavior: AgentBehavior = {
  agent_did: "owner",
  behavior_id: "behavior",
  display_name: "Default",
  inference_profile_id: "profile",
  context_id: "context",
};
function editorProps() {
  return {
    api: { explainToolSurface: vi.fn() } as unknown as DesktopApiAdapter,
    agentDid: "owner",
    principal: { agent_did: "owner", default_behavior_id: "behavior" },
    behavior,
    contexts: [
      {
        agent_did: "owner",
        context_id: "context",
        system_prompt: "Original prompt",
        skill_ids: ["b", "a"],
      },
    ],
    compactions: [],
    inferenceProfiles: [
      {
        agent_did: "owner",
        profile_id: "profile",
        backend_id: "backend",
        model_name: "model",
      },
    ],
    tools: [],
    skills: [
      { agentDid: "owner", skillId: "a", name: "A", enabled: true },
      { agentDid: "owner", skillId: "b", name: "B", enabled: true },
    ],
    saving: false,
    savedStatus: null,
    onCreateProfile: vi.fn(),
    onCreateTools: vi.fn(),
    onSaved: vi.fn(),
    onSaveAgentConfig: vi.fn().mockResolvedValue(undefined),
    onApplyConfigComponents: vi.fn().mockResolvedValue(undefined),
    onDeleteBehaviorConfig: vi.fn(),
    onDeleted: vi.fn(),
  };
}
describe("BehaviorConfigEditor", () => {
  it("leaves default compaction with its canonical owner instead of copying fields onto behavior", async () => {
    const props = editorProps();
    render(<BehaviorConfigEditor {...props} />);
    expect(screen.getByTestId("behavior-id")).toHaveAttribute("readonly");
    expect(screen.queryByTestId("behavior-backend-id")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() => expect(props.onApplyConfigComponents).toHaveBeenCalledTimes(1));
    const document = props.onApplyConfigComponents.mock.calls[0][0].document;
    expect(document.compactions).toEqual([]);
    expect(document.agent_behaviors[0]).not.toHaveProperty("compaction_strategy");
    expect(document.contexts[0].system_prompt).toBe("Original prompt");
  });
  it("makes the behavior-to-principal default dependency explicit", async () => {
    const props = editorProps();
    render(
      <BehaviorConfigEditor
        {...props}
        principal={{ agent_did: "owner", default_behavior_id: "other" }}
      />,
    );
    fireEvent.click(screen.getByTestId("behavior-default-for-agent"));
    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() =>
      expect(props.onSaveAgentConfig).toHaveBeenCalledWith({
        document: { agent_did: "owner", default_behavior_id: "behavior" },
      }),
    );
    expect(props.onApplyConfigComponents.mock.invocationCallOrder[0]).toBeLessThan(
      props.onSaveAgentConfig.mock.invocationCallOrder[0],
    );
  });
  it("tracks prompt edits with the unsaved chip and heals on revert", () => {
    render(<BehaviorConfigEditor {...editorProps()} />);
    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "Changed" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "Original prompt" },
    });
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });
  it("renders save failures next to the form", async () => {
    render(
      <BehaviorConfigEditor
        {...editorProps()}
        onApplyConfigComponents={vi.fn().mockRejectedValue(new Error("save blocked"))}
      />,
    );
    fireEvent.click(screen.getByTestId("behavior-save"));
    await screen.findByText("save blocked");
  });
  it("does not read a selected skill toggled off and back on as an edit", () => {
    render(<BehaviorConfigEditor {...editorProps()} />);
    fireEvent.click(screen.getByTestId("behavior-skill-ref-b"));
    fireEvent.click(screen.getByTestId("behavior-skill-ref-b"));
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });
  it("selects the document profile over the first profile without dirt", () => {
    const props = editorProps();
    render(
      <BehaviorConfigEditor
        {...props}
        inferenceProfiles={[
          {
            agent_did: "owner",
            profile_id: "first",
            backend_id: "backend",
            model_name: "other",
          },
          ...props.inferenceProfiles,
        ]}
      />,
    );
    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("profile");
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });
  it("fails closed on a missing profile and restores the exact binding", () => {
    const props = editorProps();
    const { rerender } = render(
      <BehaviorConfigEditor {...props} inferenceProfiles={[]} />,
    );
    expect(screen.getByTestId("behavior-save")).toBeDisabled();
    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("profile");
    rerender(<BehaviorConfigEditor {...props} />);
    expect(screen.getByTestId("behavior-save")).not.toBeDisabled();
    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("profile");
  });
});
describe("AgentConfigEditor", () => {
  const agent: AgentPrincipalView = {
    agentDid: "did:key:z6MkAgent",
    displayName: "Local Agent",
    defaultBehaviorId: "did:key:z6MkAgent:default",
    enabled: true,
  };

  function renderAgent(onSaveAgentConfig = vi.fn(() => Promise.resolve())) {
    render(
      <AgentConfigEditor
        agent={agent}
        behaviors={[behavior]}
        bootstrap={null}
        savedStatus={null}
        saving={false}
        onSaved={vi.fn()}
        onSaveAgentConfig={onSaveAgentConfig}
      />,
    );
  }

  it("flags an in-progress rename with the shared unsaved chip", () => {
    renderAgent();
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("agent-display-name"), {
      target: { value: "Renamed Agent" },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
  });

  it("renders save failures next to the form", async () => {
    renderAgent(vi.fn(() => Promise.reject(new Error("store offline"))));
    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.change(screen.getByTestId("agent-display-name"), {
      target: { value: "Renamed Agent" },
    });
    fireEvent.click(screen.getByTestId("agent-save"));
    expect(await screen.findByText(/Save failed: store offline/)).toBeInTheDocument();
  });

  it("clears the save failure when the rename is cancelled", async () => {
    renderAgent(vi.fn(() => Promise.reject(new Error("store offline"))));
    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.change(screen.getByTestId("agent-display-name"), {
      target: { value: "Renamed Agent" },
    });
    fireEvent.click(screen.getByTestId("agent-save"));
    expect(await screen.findByText(/Save failed: store offline/)).toBeInTheDocument();

    fireEvent.click(screen.getByText("Cancel"));
    expect(screen.queryByText(/Save failed/)).not.toBeInTheDocument();
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });
});
