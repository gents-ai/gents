import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfigWorkspace } from "../src/components/ConfigWorkspace";
import {
  AgentConfigPanel,
  AgentConfigEditor,
  BehaviorConfigPanel,
} from "../src/components/config";
import type { AgentConfigSaveRequest } from "../src/lib/types";

import {
  bootstrap,
  deployment,
  workspaceHandlers,
} from "./config-panel-wiring/fixtures";
import { listCases } from "./config-panel-wiring/listCases";
import { saveBoundaryCases } from "./config-panel-wiring/saveBoundaryCases";

describe("config panel wiring", () => {
  it.each(listCases)(
    "wires Add New and list selection for $createTestId",
    ({ createTestId, rowTestId, selectId, renderPanel }) => {
      const onCreate = vi.fn();
      const onSelect = vi.fn();

      render(renderPanel(onCreate, onSelect));

      fireEvent.click(screen.getByTestId(createTestId));
      expect(onCreate).toHaveBeenCalledTimes(1);

      fireEvent.click(screen.getByTestId(rowTestId));
      expect(onSelect).toHaveBeenCalledWith(selectId);
    },
  );

  it("wires behavior linked-document create buttons", () => {
    const onCreateBackend = vi.fn();
    const onCreateProfile = vi.fn();
    const onCreateToolSelection = vi.fn();

    render(
      <BehaviorConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        onCreateBackend={onCreateBackend}
        onCreateBehavior={vi.fn()}
        onCreateProfile={onCreateProfile}
        onCreateToolSelection={onCreateToolSelection}
        onSaveAgentConfig={vi.fn()}
        onSaveBehaviorConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectBehavior={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByTestId("behavior-create-backend"));
    fireEvent.click(screen.getByTestId("behavior-create-profile"));
    fireEvent.click(screen.getByTestId("behavior-create-tool-selection"));

    expect(onCreateBackend).toHaveBeenCalledTimes(1);
    expect(onCreateProfile).toHaveBeenCalledTimes(1);
    expect(onCreateToolSelection).toHaveBeenCalledTimes(1);
  });

  it.each(saveBoundaryCases)(
    "wires save completion across the $name panel boundary",
    async ({ saveTestId, selectedId, savedStatus, renderPanel }) => {
      const onSave = vi.fn(async (_request: unknown) => undefined);
      const onSelect = vi.fn();
      const onSavedStatusChange = vi.fn();

      render(renderPanel(onSave, onSelect, onSavedStatusChange));
      fireEvent.click(screen.getByTestId(saveTestId));

      await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1));
      expect(onSelect).toHaveBeenCalledWith(selectedId);
      expect(onSavedStatusChange).toHaveBeenCalledWith(savedStatus);
    },
  );

  it("wires save completion across the agent panel boundary", async () => {
    const onSaveAgentConfig = vi.fn<
      [(request: AgentConfigSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    const onSavedStatusChange = vi.fn();

    render(
      <AgentConfigPanel
        bootstrap={bootstrap}
        deployment={deployment}
        savedStatus={null}
        saving={false}
        onSaveAgentConfig={onSaveAgentConfig}
        onSavedStatusChange={onSavedStatusChange}
      />,
    );

    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.click(screen.getByTestId("agent-save"));

    await waitFor(() => expect(onSaveAgentConfig).toHaveBeenCalledTimes(1));
    expect(onSavedStatusChange).toHaveBeenCalledWith("agent:did:key:z6MkAgent");
  });

  it("wires agent edit, cancel, and save buttons", async () => {
    const onSaveAgentConfig = vi.fn<
      [(request: AgentConfigSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve());
    const onSaved = vi.fn();

    render(
      <AgentConfigEditor
        agent={deployment.agentPrincipal}
        behaviors={deployment.behaviors}
        bootstrap={bootstrap}
        savedStatus={null}
        saving={false}
        onSaveAgentConfig={onSaveAgentConfig}
        onSaved={onSaved}
      />,
    );

    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.change(screen.getByTestId("agent-display-name"), {
      target: { value: "Edited Agent" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByTestId("agent-display-name")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Local Agent" })).toBeInTheDocument();
    expect(onSaveAgentConfig).not.toHaveBeenCalled();

    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.change(screen.getByTestId("agent-display-name"), {
      target: { value: "Edited Agent" },
    });
    fireEvent.click(screen.getByTestId("agent-save"));

    await waitFor(() =>
      expect(onSaveAgentConfig).toHaveBeenCalledWith({
        agentDid: "did:key:z6MkAgent",
        displayName: "Edited Agent",
        defaultBehaviorId: "default",
        enabled: true,
      }),
    );
    expect(onSaved).toHaveBeenCalledWith("did:key:z6MkAgent");
  });

  it("wires workspace back buttons and tab buttons", () => {
    const emptyHandlers = workspaceHandlers();
    const emptyRender = render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={null}
        {...emptyHandlers}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Back to Chat" }));
    expect(emptyHandlers.onBack).toHaveBeenCalledTimes(1);
    emptyRender.unmount();

    const handlers = workspaceHandlers();
    const { unmount } = render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        {...handlers}
      />,
    );

    fireEvent.click(screen.getByTestId("config-back-tab"));
    expect(handlers.onBack).toHaveBeenCalledTimes(1);

    const tabExpectations: Array<[string, () => void]> = [
      [
        "config-tab-agent",
        () => expect(screen.getByTestId("agent-edit-display-name")).toBeInTheDocument(),
      ],
      [
        "config-tab-behavior",
        () =>
          expect(
            screen.getByRole("heading", { name: "Agent Behaviors" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-backends",
        () =>
          expect(screen.getByRole("heading", { name: "Backends" })).toBeInTheDocument(),
      ],
      [
        "config-tab-profiles",
        () =>
          expect(
            screen.getByRole("heading", { name: "Inference Profiles" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-toolSelections",
        () =>
          expect(
            screen.getByRole("heading", { name: "Tool Selections" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-metaTools",
        () =>
          expect(
            screen.getByRole("heading", { name: "HTTP MCP Services" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-tasks",
        () =>
          expect(
            screen.getByRole("heading", { name: "Task Prompts" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-timerTriggers",
        () =>
          expect(
            screen.getByRole("heading", { name: "Timer Triggers" }),
          ).toBeInTheDocument(),
      ],
      [
        "config-tab-eventTriggers",
        () =>
          expect(
            screen.getByRole("heading", { name: "Event Triggers" }),
          ).toBeInTheDocument(),
      ],
    ];

    for (const [tabId, assertActivePanel] of tabExpectations) {
      fireEvent.click(screen.getByTestId(tabId));
      assertActivePanel();
    }

    unmount();
  });

  it("wires workspace behavior shortcuts into new config drafts", () => {
    const handlers = workspaceHandlers();
    render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        {...handlers}
      />,
    );

    fireEvent.click(screen.getByTestId("behavior-create-backend"));
    expect(screen.getByTestId("backend-id")).not.toHaveAttribute("readonly");
    expect(screen.getByTestId("backend-id")).toHaveValue("");

    fireEvent.click(screen.getByTestId("config-tab-behavior"));
    fireEvent.click(screen.getByTestId("behavior-create-profile"));
    expect(screen.getByTestId("profile-id")).not.toHaveAttribute("readonly");
    expect(screen.getByTestId("profile-id")).toHaveValue("");

    fireEvent.click(screen.getByTestId("config-tab-behavior"));
    fireEvent.click(screen.getByTestId("behavior-create-tool-selection"));
    expect(screen.getByTestId("tool-selection-id")).not.toHaveAttribute("readonly");
    expect(screen.getByTestId("tool-selection-id")).toHaveValue("");
  });

  it("makes selected behavior dependencies explicit across linked panes", () => {
    const handlers = workspaceHandlers();
    render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        {...handlers}
      />,
    );

    fireEvent.click(screen.getByTestId("config-behavior-ops"));

    fireEvent.click(screen.getByTestId("config-tab-backends"));
    expect(screen.getByTestId("backend-id")).toHaveValue("backend-b");

    fireEvent.click(screen.getByTestId("config-tab-profiles"));
    expect(screen.getByTestId("profile-id")).toHaveValue("profile-b");

    fireEvent.click(screen.getByTestId("config-tab-toolSelections"));
    expect(screen.getByTestId("tool-selection-id")).toHaveValue("tools-b");

    fireEvent.click(screen.getByTestId("config-tab-tasks"));
    fireEvent.click(screen.getByTestId("task-new"));
    expect(screen.getByTestId("task-behavior-id")).toHaveValue("ops");
  });

  it("uses the selected task when creating timer and event trigger drafts", () => {
    const handlers = workspaceHandlers();
    render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        {...handlers}
      />,
    );

    fireEvent.click(screen.getByTestId("config-tab-tasks"));
    fireEvent.click(screen.getByTestId("config-task-task-b"));

    fireEvent.click(screen.getByTestId("config-tab-timerTriggers"));
    fireEvent.click(screen.getByTestId("schedule-new"));
    expect(screen.getByTestId("schedule-task-id")).toHaveValue("task-b");

    fireEvent.click(screen.getByTestId("config-tab-eventTriggers"));
    fireEvent.click(screen.getByTestId("event-trigger-new"));
    expect(screen.getByTestId("event-trigger-task-id")).toHaveValue("task-b");
  });

  it("routes workspace save buttons to the active panel handlers", async () => {
    const handlers = workspaceHandlers();
    render(
      <ConfigWorkspace
        bootstrap={bootstrap}
        runningTask={false}
        saving={false}
        selectedBehaviorId="default"
        selectedDeployment={deployment}
        {...handlers}
      />,
    );

    fireEvent.click(screen.getByTestId("behavior-save"));
    await waitFor(() =>
      expect(handlers.onSaveBehaviorConfig).toHaveBeenCalledWith(
        expect.objectContaining({ behaviorId: "default" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-agent"));
    fireEvent.click(screen.getByTestId("agent-edit-display-name"));
    fireEvent.click(screen.getByTestId("agent-save"));
    await waitFor(() =>
      expect(handlers.onSaveAgentConfig).toHaveBeenCalledWith(
        expect.objectContaining({ agentDid: "did:key:z6MkAgent" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-backends"));
    fireEvent.click(screen.getByTestId("backend-save"));
    await waitFor(() =>
      expect(handlers.onSaveBackendConfig).toHaveBeenCalledWith(
        expect.objectContaining({ backendId: "backend-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-profiles"));
    fireEvent.click(screen.getByTestId("profile-save"));
    await waitFor(() =>
      expect(handlers.onSaveInferenceProfileConfig).toHaveBeenCalledWith(
        expect.objectContaining({ profileId: "profile-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-toolSelections"));
    fireEvent.click(screen.getByTestId("tool-selection-save"));
    await waitFor(() =>
      expect(handlers.onSaveToolSelectionConfig).toHaveBeenCalledWith(
        expect.objectContaining({ selectionId: "tools-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-metaTools"));
    fireEvent.click(screen.getByTestId("tool-service-save"));
    await waitFor(() =>
      expect(handlers.onSaveToolServiceConfig).toHaveBeenCalledWith(
        expect.objectContaining({ serviceId: "service-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-tasks"));
    fireEvent.click(screen.getByTestId("task-save"));
    await waitFor(() =>
      expect(handlers.onSaveTaskConfig).toHaveBeenCalledWith(
        expect.objectContaining({ taskId: "task-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-timerTriggers"));
    fireEvent.click(screen.getByTestId("schedule-save"));
    await waitFor(() =>
      expect(handlers.onSaveScheduleConfig).toHaveBeenCalledWith(
        expect.objectContaining({ scheduleId: "timer-a" }),
      ),
    );

    fireEvent.click(screen.getByTestId("config-tab-eventTriggers"));
    fireEvent.click(screen.getByTestId("event-trigger-save"));
    await waitFor(() =>
      expect(handlers.onSaveEventTriggerConfig).toHaveBeenCalledWith(
        expect.objectContaining({ triggerId: "event-a" }),
      ),
    );
  });
});
