import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  BackendConfigPanel,
  BehaviorConfigEditor,
  SkillConfigPanel,
  ToolSelectionConfigEditor,
} from "../src/components/config";
import type {
  BehaviorView,
  DeploymentView,
  InferenceProfileView,
  ToolSelectionView,
  ToolServiceRegistryView,
} from "../src/lib/types";

// Fence for the background-refresh edit wipe: the Tauri bridge emits
// client-updated on every store/health change, which re-fetches the snapshot
// and produces a fresh object tree. Editors must key their reset effects on
// the document id, not object identity, or every background event discards
// in-progress operator edits.

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
      {
        backendId: "backend-b",
        name: "Backend B",
        providerKind: "openai",
        endpoint: "http://localhost:5678/v1",
        models: ["m-2"],
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

const noopHandlers = {
  saving: false,
  savedStatus: null,
  onSelectBackend: vi.fn(),
  onCreateBackend: vi.fn(),
  onSavedStatusChange: vi.fn(),
  onSaveBackendConfig: vi.fn(),
};

describe("config editors preserve in-progress edits across snapshot refreshes", () => {
  it("backend editor keeps typed values when the snapshot object tree is replaced", () => {
    const { rerender } = render(
      <BackendConfigPanel
        deployment={makeDeployment()}
        selectedBackendId="backend-a"
        {...noopHandlers}
      />,
    );

    const endpoint = screen.getByTestId("backend-endpoint");
    fireEvent.change(endpoint, { target: { value: "http://edited:9999/v1" } });

    // Background refresh: same data, fresh object identities.
    rerender(
      <BackendConfigPanel
        deployment={makeDeployment()}
        selectedBackendId="backend-a"
        {...noopHandlers}
      />,
    );
    expect(screen.getByTestId("backend-endpoint")).toHaveValue("http://edited:9999/v1");

    // Selecting a different document still resets the form.
    rerender(
      <BackendConfigPanel
        deployment={makeDeployment()}
        selectedBackendId="backend-b"
        {...noopHandlers}
      />,
    );
    expect(screen.getByTestId("backend-endpoint")).toHaveValue(
      "http://localhost:5678/v1",
    );
  });

  it("skill editor keeps typed values when the snapshot object tree is replaced", () => {
    const props = {
      selectedSkillId: "review-skill",
      saving: false,
      savedStatus: null,
      onSelectSkill: vi.fn(),
      onCreateSkill: vi.fn(),
      onDeletedSkill: vi.fn(),
      onSavedStatusChange: vi.fn(),
      onDeleteSkillConfig: vi.fn(),
      onSaveSkillConfig: vi.fn(),
    };
    const { rerender } = render(
      <SkillConfigPanel deployment={makeDeployment()} {...props} />,
    );

    fireEvent.change(screen.getByTestId("skill-name"), {
      target: { value: "Edited Name" },
    });

    rerender(<SkillConfigPanel deployment={makeDeployment()} {...props} />);
    expect(screen.getByTestId("skill-name")).toHaveValue("Edited Name");
  });

  it("behavior editor keeps typed values when the inference-profile set changes", () => {
    const behavior = (): BehaviorView => ({
      behaviorId: "default",
      displayName: "default",
      systemPrompt: "original prompt",
      inferenceProfileId: "profile-a",
      enabled: true,
      isDefault: true,
      skillRefs: [],
      skillExcludes: [],
    });
    const profile = (id: string): InferenceProfileView => ({ profileId: id });
    const editorProps = {
      agentDisplayName: "test",
      agentDid: "did:test:operator",
      agentEnabled: true,
      currentDefaultBehaviorId: "default",
      inferenceBackends: [],
      skills: [],
      toolSelections: [],
      saving: false,
      savedStatus: null,
      onCreateBackend: vi.fn(),
      onCreateProfile: vi.fn(),
      onCreateToolSelection: vi.fn(),
      onSaved: vi.fn(),
      onSaveAgentConfig: vi.fn(),
      onSaveBehaviorConfig: vi.fn(),
    };
    const { rerender } = render(
      <BehaviorConfigEditor
        {...editorProps}
        behavior={behavior()}
        inferenceProfiles={[profile("profile-a")]}
      />,
    );

    fireEvent.change(screen.getByTestId("behavior-system-prompt"), {
      target: { value: "edited prompt" },
    });

    // A remote profile registration must not wipe the in-progress edit; the
    // valid current profile pick must survive too.
    rerender(
      <BehaviorConfigEditor
        {...editorProps}
        behavior={behavior()}
        inferenceProfiles={[profile("profile-a"), profile("profile-b")]}
      />,
    );
    expect(screen.getByTestId("behavior-system-prompt")).toHaveValue("edited prompt");
  });

  it("tool-selection editor keeps typed values when service registrations change", () => {
    const selection: ToolSelectionView = {
      selectionId: "tools-a",
      displayName: "Tools A",
      commandAllowedArgvPrefixes: [],
      commandForbiddenArgvPrefixes: [],
      cliToolNames: [],
      allowedMcpServiceIds: [],
      delegateTo: ["mcp-late"],
      backgroundableToolNames: [],
      subagentTargets: [],
      defraQueryCollections: [],
      writeTools: [],
    };
    const lateService: ToolServiceRegistryView = { serviceId: "mcp-late" };
    const props = {
      agentDid: "did:test:operator",
      toolSelection: selection,
      toolCeiling: "Readwrite",
      toolRoot: "/tmp/work",
      saving: false,
      savedStatus: null,
      onSaved: vi.fn(),
      onSaveToolSelectionConfig: vi.fn(),
      onDeleteToolSelectionConfig: vi.fn(),
      onDeleted: vi.fn(),
    };
    const { rerender } = render(
      <ToolSelectionConfigEditor {...props} toolServiceRegistries={[]} />,
    );

    fireEvent.change(screen.getByTestId("tool-selection-display-name"), {
      target: { value: "Edited Tools" },
    });

    // A replicated service registration changes the legacy-delegate
    // projection, but must not rehydrate the selected document's whole form.
    rerender(
      <ToolSelectionConfigEditor {...props} toolServiceRegistries={[lateService]} />,
    );
    expect(screen.getByTestId("tool-selection-display-name")).toHaveValue(
      "Edited Tools",
    );
    expect(screen.getByTestId("tool-delegate-to")).toHaveValue("mcp-late");
  });
});
