import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  BackendConfigPanel,
  BehaviorConfigEditor,
  SkillConfigPanel,
  ToolsConfigEditor,
} from "../src/components/config";
import type {
  AgentBehavior,
  DesktopApiAdapter,
  DeploymentView,
  InferenceProfile,
  Tools,
  ToolServiceRegistry,
} from "@source-inc/gents-desktop-client";

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
        providerKind: "OpenAiCompatible",
        endpoint: "http://localhost:1234/v1",
        models: ["m-1"],
        enabled: true,
      },
      {
        backendId: "backend-b",
        name: "Backend B",
        providerKind: "OpenAiCompatible",
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

    rerender(
      <BackendConfigPanel
        deployment={makeDeployment()}
        selectedBackendId="backend-a"
        {...noopHandlers}
      />,
    );
    expect(screen.getByTestId("backend-endpoint")).toHaveValue("http://edited:9999/v1");

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
    const behavior = (): AgentBehavior => ({
      agent_did: "did:test:operator",
      behavior_id: "default",
      context_id: "context",
      inference_profile_id: "profile-a",
    });
    const profile = (id: string): InferenceProfile => ({
      agent_did: "did:test:operator",
      profile_id: id,
      backend_id: "backend",
      model_name: "model",
    });
    const editorProps = {
      api: {} as DesktopApiAdapter,
      agentDid: "did:test:operator",
      principal: { agent_did: "did:test:operator" },
      contexts: [
        {
          agent_did: "did:test:operator",
          context_id: "context",
          system_prompt: "original prompt",
        },
      ],
      compactions: [],
      skills: [],
      tools: [],
      saving: false,
      savedStatus: null,
      onCreateProfile: vi.fn(),
      onCreateTools: vi.fn(),
      onSaved: vi.fn(),
      onSaveAgentConfig: vi.fn(),
      onApplyConfigComponents: vi.fn(),
      onDeleteBehaviorConfig: vi.fn(),
      onDeleted: vi.fn(),
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
    const selection: Tools = {
      agent_did: "did:test:operator",
      tools_id: "tools-a",
      display_name: "Tools A",
    };
    const lateService: ToolServiceRegistry = {
      agent_did: "did:test:operator",
      service_id: "mcp-late",
    };
    const props = {
      agentDid: "did:test:operator",
      tools: selection,
      subagentTargets: [],
      toolCeiling: "Readwrite",
      toolRoot: "/tmp/work",
      saving: false,
      savedStatus: null,
      onSaved: vi.fn(),
      onApplyConfigComponents: vi.fn(),
      onDeleteToolsConfig: vi.fn(),
      onDeleted: vi.fn(),
    };
    const { rerender } = render(<ToolsConfigEditor {...props} toolServices={[]} />);

    fireEvent.change(screen.getByTestId("tools-display-name"), {
      target: { value: "Edited Tools" },
    });

    rerender(<ToolsConfigEditor {...props} toolServices={[lateService]} />);
    expect(screen.getByTestId("tools-display-name")).toHaveValue("Edited Tools");
    expect(screen.getByTestId("tools-service-mcp-late")).not.toBeChecked();
  });
});
