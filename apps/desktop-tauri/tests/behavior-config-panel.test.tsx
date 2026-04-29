import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { BehaviorConfigEditor } from "../src/components/config/BehaviorConfigPanel";
import type {
  AgentConfigSaveRequest,
  BehaviorSaveRequest,
  BehaviorView,
  InferenceBackendView,
  InferenceProfileView,
  ToolSelectionView,
} from "../src/lib/types";

const behavior: BehaviorView = {
  behaviorId: "did:key:z6MkAgent:default",
  displayName: "Default",
  systemPrompt: "You are the default agent.",
  backendId: "default-backend",
  modelName: "default-model",
  toolSelectionId: "default-tools",
  inferenceProfileId: "default-profile",
  compactionStrategy: null,
  compactionThreshold: null,
  enabled: true,
  isDefault: true,
};

const inferenceBackends: InferenceBackendView[] = [
  {
    backendId: "default-backend",
    name: "Default Backend",
    providerKind: "openai",
    endpoint: "http://127.0.0.1:8000/v1",
    apiKeyConfigured: false,
    maxConcurrent: 2,
    maxQueueDepth: 100,
    enabled: true,
    models: ["default-model"],
  },
];

const inferenceProfiles: InferenceProfileView[] = [
  {
    profileId: "default-profile",
    displayName: "Default Profile",
  },
];

const toolSelections: ToolSelectionView[] = [
  {
    selectionId: "default-tools",
    displayName: "Default Tools",
  },
];

describe("BehaviorConfigEditor", () => {
  it("saves explicit compaction defaults onto the selected behavior", async () => {
    const onSaveAgentConfig = vi.fn<[(request: AgentConfigSaveRequest) => Promise<unknown>]>();
    const onSaveBehaviorConfig = vi.fn<
      [(request: BehaviorSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    const onSaved = vi.fn();

    render(
      <BehaviorConfigEditor
        agentDid="did:key:z6MkAgent"
        agentDisplayName="Local Agent"
        agentEnabled
        behavior={behavior}
        currentDefaultBehaviorId={behavior.behaviorId}
        inferenceBackends={inferenceBackends}
        inferenceProfiles={inferenceProfiles}
        savedStatus={null}
        saving={false}
        toolSelections={toolSelections}
        onCreateBackend={vi.fn()}
        onCreateProfile={vi.fn()}
        onCreateToolSelection={vi.fn()}
        onSaveAgentConfig={onSaveAgentConfig}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
        onSaved={onSaved}
      />,
    );

    expect(screen.queryByTestId("behavior-compaction-enabled")).not.toBeInTheDocument();
    expect(screen.queryByTestId("behavior-id")).not.toBeInTheDocument();
    expect(screen.queryByTestId("behavior-edit-key")).not.toBeInTheDocument();
    expect(screen.getByTestId("behavior-compaction-strategy")).toHaveValue(
      "StripThenSummarize",
    );
    expect(screen.getByTestId("behavior-compaction-threshold")).toHaveValue(0.75);

    fireEvent.click(screen.getByTestId("behavior-save"));

    await waitFor(() => {
      expect(onSaveBehaviorConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          agentDid: "did:key:z6MkAgent",
          behaviorId: "did:key:z6MkAgent:default",
          compactionStrategy: "StripThenSummarize",
          compactionThreshold: 0.75,
        }),
      );
    });
    expect(onSaveAgentConfig).not.toHaveBeenCalled();
    expect(onSaved).toHaveBeenCalledWith("did:key:z6MkAgent:default");
  });

  it("makes the behavior-to-agent default dependency explicit", async () => {
    const opsBehavior = {
      ...behavior,
      behaviorId: "did:key:z6MkAgent:ops",
      displayName: "Ops",
      isDefault: false,
    };
    const onSaveAgentConfig = vi.fn<
      [(request: AgentConfigSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    const onSaveBehaviorConfig = vi.fn<
      [(request: BehaviorSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());

    render(
      <BehaviorConfigEditor
        agentDid="did:key:z6MkAgent"
        agentDisplayName="Local Agent"
        agentEnabled
        behavior={opsBehavior}
        currentDefaultBehaviorId={behavior.behaviorId}
        inferenceBackends={inferenceBackends}
        inferenceProfiles={inferenceProfiles}
        savedStatus={null}
        saving={false}
        toolSelections={toolSelections}
        onCreateBackend={vi.fn()}
        onCreateProfile={vi.fn()}
        onCreateToolSelection={vi.fn()}
        onSaveAgentConfig={onSaveAgentConfig}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
        onSaved={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("behavior-default-for-agent"));
    fireEvent.click(screen.getByTestId("behavior-save"));

    await waitFor(() => expect(onSaveBehaviorConfig).toHaveBeenCalledTimes(1));
    expect(onSaveAgentConfig).toHaveBeenCalledWith({
      agentDid: "did:key:z6MkAgent",
      displayName: "Local Agent",
      defaultBehaviorId: "did:key:z6MkAgent:ops",
      enabled: true,
    });
  });
});
