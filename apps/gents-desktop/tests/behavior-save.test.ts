import { describe, expect, it, vi } from "vitest";
import type { AgentContext, AgentView } from "@source-inc/gents-desktop-client";

import type { NodeView } from "../src/hooks/fleetStore";
import type { Draft } from "../src/ui/screens/agent/behaviorDraft";
import { writeAgentDraft } from "../src/ui/screens/agent/behaviorSave";

const context: AgentContext = {
  context_id: "ctx-shared",
  node_did: "did:key:node",
  display_name: "Shared context",
  description: "Kept as stored",
  system_prompt: "Before",
  tools_id: "tools-a",
  compaction_id: null,
  skill_ids: ["skill-a"],
  tags: ["team"],
};

const deployment = {
  nodeDid: "did:key:node",
  contexts: [context],
} as unknown as NodeView;

const agent = {
  agentDid: "did:key:draft-agent",
  enabled: false,
  createdAt: "2026-11-09T00:00:00Z",
} as unknown as AgentView;

const draft: Draft = {
  displayName: "Reviewer",
  description: "",
  tags: [],
  contextChoice: context.context_id,
  contextName: "",
  copiedFrom: "",
  systemPrompt: "After",
  toolsId: "tools-a",
  compactionId: "",
  skillIds: [],
  inferenceProfileId: "profile-a",
};

describe("writeAgentDraft", () => {
  it("creates a draft agent and its edited context in one apply", async () => {
    const changeConfig = vi.fn().mockResolvedValue({});

    await writeAgentDraft({
      changeConfig,
      identity: async () => ({
        agent_did: agent.agentDid,
        creation_nonce: "nonce",
      }),
      deployment,
      agent,
      next: draft,
      asCopy: false,
      draftMode: { onSaved: vi.fn(), onCancel: vi.fn(), enabled: true },
      newContextId: () => "unused",
    });

    expect(changeConfig).toHaveBeenCalledOnce();
    expect(changeConfig).toHaveBeenCalledWith("applyConfigComponents", {
      document: {
        node: { node_did: deployment.nodeDid },
        contexts: [{ ...context, system_prompt: "After", skill_ids: null }],
        agents: [
          expect.objectContaining({
            agent_did: agent.agentDid,
            creation_nonce: "nonce",
            context_id: context.context_id,
            enabled: true,
          }),
        ],
      },
    });
  });
});
