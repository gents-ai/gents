import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { AgentConfigEditor } from "../src/components/config/AgentConfigPanel";
import { BehaviorConfigEditor } from "../src/components/config/BehaviorConfigPanel";
import type {
  AgentConfigSaveRequest,
  AgentPrincipalView,
  SkillView,
  BehaviorSaveRequest,
  BehaviorView,
  InferenceBackendView,
  InferenceProfileView,
  ToolSelectionView,
} from "@source-inc/gents-desktop-client";

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
    cliToolNames: [],
    allowedMcpServiceIds: [],
    delegateTo: [],
  },
];

describe("BehaviorConfigEditor", () => {
  it("saves explicit compaction defaults onto the selected behavior", async () => {
    const onSaveAgentConfig =
      vi.fn<[(request: AgentConfigSaveRequest) => Promise<unknown>]>();
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

  it("tracks edits with the shared unsaved chip and heals on revert", () => {
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
        onSaveAgentConfig={vi.fn()}
        onSaveBehaviorConfig={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "You are the default agent. Be brief." },
    });
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "You are the default agent." },
    });
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });

  it("renders save failures next to the form", async () => {
    const onSaveBehaviorConfig = vi.fn(() => Promise.reject(new Error("acp denied")));

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
        onSaveAgentConfig={vi.fn()}
        onSaveBehaviorConfig={onSaveBehaviorConfig}
        onSaved={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("behavior-save"));
    expect(await screen.findByText(/Save failed: acp denied/)).toBeInTheDocument();
  });

  function editorProps(overrides: Record<string, unknown> = {}) {
    return {
      agentDid: "did:key:z6MkAgent",
      agentDisplayName: "Local Agent",
      agentEnabled: true,
      behavior,
      currentDefaultBehaviorId: behavior.behaviorId,
      inferenceBackends,
      inferenceProfiles,
      savedStatus: null,
      saving: false,
      toolSelections,
      onCreateBackend: vi.fn(),
      onCreateProfile: vi.fn(),
      onCreateToolSelection: vi.fn(),
      onSaveAgentConfig: vi.fn(),
      onSaveBehaviorConfig: vi.fn(),
      onSaved: vi.fn(),
      ...overrides,
    };
  }

  it("does not read a skill toggled off and back on as an edit", () => {
    const skills: SkillView[] = [
      { skillId: "writer", name: "Writer", scope: "behavior" },
      { skillId: "ops", name: "Ops", scope: "behavior" },
    ];
    render(
      <BehaviorConfigEditor
        {...editorProps({
          behavior: { ...behavior, skillRefs: ["writer"] },
          skills,
        })}
      />,
    );

    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("behavior-skill-ref-writer"));
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("behavior-skill-ref-writer"));
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("behavior-skill-ref-ops"));
    expect(screen.getByTestId("unsaved-chip")).toBeInTheDocument();
  });

  it("selects the document profile over the first profile, without dirt", () => {
    render(
      <BehaviorConfigEditor
        {...editorProps({
          inferenceProfiles: [{ profileId: "other-profile" }, ...inferenceProfiles],
        })}
      />,
    );

    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("default-profile");
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
  });

  it("preserves the document profile when it drops out, then restores it", async () => {
    const remote = { ...behavior, inferenceProfileId: "profile-remote" };
    const onSaveBehaviorConfig = vi.fn(() => Promise.resolve());
    const { rerender } = render(
      <BehaviorConfigEditor
        {...editorProps({ behavior: remote, onSaveBehaviorConfig })}
      />,
    );

    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("default-profile");
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();

    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "edited while the profile is registering" },
    });
    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() =>
      expect(onSaveBehaviorConfig).toHaveBeenCalledWith(
        expect.objectContaining({ inferenceProfileId: "profile-remote" }),
      ),
    );

    rerender(
      <BehaviorConfigEditor
        {...editorProps({
          behavior: {
            ...remote,
            systemPrompt: "edited while the profile is registering",
          },
          inferenceProfiles: [...inferenceProfiles, { profileId: "profile-remote" }],
          onSaveBehaviorConfig,
        })}
      />,
    );
    expect(screen.getByTestId("behavior-profile-id")).toHaveValue("profile-remote");
    expect(screen.queryByTestId("unsaved-chip")).not.toBeInTheDocument();
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
