import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  AgentBehavior,
  AgentPrincipal,
  AgentContext,
  CompactionConfig,
  DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";
import { BehaviorConfigEditor } from "../src/components/config/BehaviorConfigPanel";
const principal: AgentPrincipal = {
  agent_did: "owner",
  display_name: "Principal",
  created_by: "creator",
  created_at: "time",
  tags: ["principal"],
};
const behavior: AgentBehavior = {
  agent_did: "owner",
  behavior_id: " behavior ",
  inference_profile_id: "profile",
  context_id: "context",
  tags: ["behavior"],
  created_at: "created",
};
const context: AgentContext = {
  agent_did: "owner",
  context_id: "context",
  system_prompt: "Literal {{ text }}",
  compaction_id: "compaction",
  skill_ids: [],
  description: "preserve",
  tags: ["context"],
};
const compaction: CompactionConfig = {
  agent_did: "owner",
  compaction_id: "compaction",
  keep_recent_tokens: 200,
  summary_max_output_tokens: 100,
  inference_profile_id: "summary",
  tags: ["compaction"],
};
function props() {
  return {
    api: { explainToolSurface: vi.fn() } as unknown as DesktopApiAdapter,
    agentDid: "owner",
    principal,
    behavior,
    contexts: [context],
    compactions: [compaction],
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
      { agentDid: "owner", skillId: "selected", name: "Selected", enabled: true },
      { agentDid: "owner", skillId: "unselected", name: "Unselected", enabled: true },
      { agentDid: "foreign", skillId: "foreign", name: "Foreign", enabled: true },
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
describe("canonical behavior and context editing", () => {
  it("uses an explicit skill whitelist and preserves literal prompt plus canonical policy fields", async () => {
    const handlers = props();
    render(<BehaviorConfigEditor {...handlers} />);
    expect(screen.getByTestId("behavior-skill-ref-selected")).not.toBeChecked();
    expect(screen.queryByTestId("behavior-skill-ref-foreign")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("behavior-skill-ref-selected"));
    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() =>
      expect(handlers.onApplyConfigComponents).toHaveBeenCalledTimes(1),
    );
    const { document } = handlers.onApplyConfigComponents.mock.calls[0][0];
    expect(document.agent_behaviors[0]).toMatchObject(behavior);
    expect(document.contexts[0]).toMatchObject({ ...context, skill_ids: ["selected"] });
    expect(document.compactions[0]).toMatchObject(compaction);
    expect(document.agent_behaviors[0]).not.toHaveProperty("backend_id");
  });
  it("updates the default through the canonical principal writer without dropping metadata", async () => {
    const handlers = props();
    render(<BehaviorConfigEditor {...handlers} />);
    fireEvent.click(screen.getByTestId("behavior-default-for-agent"));
    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() =>
      expect(handlers.onSaveAgentConfig).toHaveBeenCalledWith({
        document: { ...principal, default_behavior_id: " behavior " },
      }),
    );
    expect(handlers.onApplyConfigComponents.mock.invocationCallOrder[0]).toBeLessThan(
      handlers.onSaveAgentConfig.mock.invocationCallOrder[0],
    );
  });
});
