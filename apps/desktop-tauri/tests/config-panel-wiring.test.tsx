import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";

import { ConfigWorkspace } from "../src/components/ConfigWorkspace";
import {
  AgentConfigPanel,
  AgentConfigEditor,
  BackendConfigPanel,
  BehaviorConfigPanel,
  EventTriggerConfigPanel,
  InferenceProfileConfigPanel,
  ScheduleConfigPanel,
  TaskConfigPanel,
  ToolSelectionConfigPanel,
  ToolServiceConfigPanel,
} from "../src/components/config";
import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  BootstrapSummary,
  DeploymentView,
  EventTriggerSaveRequest,
  InferenceProfileSaveRequest,
  ScheduleSaveRequest,
  TaskRunResult,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "../src/lib/types";

const runResult: TaskRunResult = {
  requestDocId: "bae-run",
  requestId: "run-1",
  sessionId: "session-1",
  agentDid: "did:key:z6MkAgent",
  behaviorId: "default",
  status: "submitted",
  lifecycleState: "queued",
};

const deployment: DeploymentView = {
  peerId: "peer-1",
  label: "Local Agent",
  agentDid: "did:key:z6MkAgent",
  addr: "iroh://local",
  source: "local",
  graphql: null,
  dialSucceeded: true,
  defaultBehaviorId: "default",
  agentPrincipal: {
    agentDid: "did:key:z6MkAgent",
    displayName: "Local Agent",
    defaultBehaviorId: "default",
    enabled: true,
  },
  runtime: null,
  behaviors: [
    {
      behaviorId: "default",
      displayName: "Default",
      systemPrompt: "You are the default behavior.",
      backendId: "backend-a",
      inferenceProfileId: "profile-a",
      toolSelectionId: "tools-a",
      enabled: true,
      isDefault: true,
    },
    {
      behaviorId: "ops",
      displayName: "Ops",
      systemPrompt: "You are the ops behavior.",
      backendId: "backend-b",
      inferenceProfileId: "profile-b",
      toolSelectionId: "tools-b",
      enabled: true,
      isDefault: false,
    },
  ],
  inferenceBackends: [
    {
      backendId: "backend-a",
      name: "Backend A",
      providerKind: "openai",
      endpoint: "http://127.0.0.1:8000/v1",
      apiKeyConfigured: false,
      enabled: true,
      models: ["model-a"],
    },
    {
      backendId: "backend-b",
      name: "Backend B",
      providerKind: "openai",
      endpoint: "http://127.0.0.1:9000/v1",
      apiKeyConfigured: false,
      enabled: true,
      models: ["model-b"],
    },
  ],
  inferenceProfiles: [
    {
      profileId: "profile-a",
      displayName: "Profile A",
      contextWindow: 131072,
    },
    {
      profileId: "profile-b",
      displayName: "Profile B",
      contextWindow: 65536,
    },
  ],
  toolSelections: [
    {
      selectionId: "tools-a",
      agentDid: "did:key:z6MkAgent",
      displayName: "Tools A",
      enableFileTools: true,
      fileToolsMode: "ReadOnly",
      enableBash: true,
      bashMode: "ReadOnly",
      cliToolNames: ["grep"],
      enableMetaTools: true,
      delegateTo: ["service-a"],
    },
    {
      selectionId: "tools-b",
      agentDid: "did:key:z6MkAgent",
      displayName: "Tools B",
      enableFileTools: false,
      fileToolsMode: "ReadOnly",
      enableBash: false,
      bashMode: "ReadOnly",
      cliToolNames: [],
      enableMetaTools: false,
      delegateTo: [],
    },
  ],
  toolServiceRegistries: [
    {
      serviceId: "service-a",
      displayName: "Service A",
      hostname: "localhost",
      mcpPort: 7331,
      mcpPath: "/mcp",
      status: "online",
    },
  ],
  tasks: [
    {
      taskId: "task-a",
      name: "Task A",
      behaviorId: "default",
      promptTemplate: "Run task A",
      enabled: true,
      recentRuns: {
        totalFires: 0,
        scheduleCount: 1,
        eventTriggerCount: 1,
      },
      runHistory: [],
    },
    {
      taskId: "task-b",
      name: "Task B",
      behaviorId: "ops",
      promptTemplate: "Run task B",
      enabled: true,
      recentRuns: {
        totalFires: 0,
        scheduleCount: 0,
        eventTriggerCount: 0,
      },
      runHistory: [],
    },
  ],
  schedules: [
    {
      scheduleId: "timer-a",
      taskId: "task-a",
      intervalSecs: 60,
      enabled: true,
      concurrency: "serial",
      fireCount: 0,
    },
  ],
  eventTriggers: [
    {
      triggerId: "event-a",
      taskId: "task-a",
      sourceCollection: "AgentRequest",
      eventKind: "created",
      enabled: true,
      concurrency: "serial",
      fireCount: 0,
    },
  ],
  conversations: [],
};

const bootstrap: BootstrapSummary = {
  defaultAgentHome: "/tmp/agent",
  initAgentName: "Local Agent",
  initAgentDid: "did:key:z6MkAgent",
  initToolCeiling: "Readwrite",
  initToolRoot: "/tmp/work",
  desktopHome: "/tmp/defra-agent",
  peerDirectoryPath: "/tmp/defra-agent/peers.json",
  nodeDataDir: "/tmp/defra-agent/node",
  logFilePath: "/tmp/defra-agent/logs/desktop.log",
  agentHomeExists: true,
  desktopHomeExists: true,
  peerDirectoryExists: true,
  savedPeers: [],
};

type ListCase = {
  createTestId: string;
  rowTestId: string;
  selectId: string;
  renderPanel: (onCreate: () => void, onSelect: (id: string) => void) => ReactElement;
};

const listCases: ListCase[] = [
  {
    createTestId: "behavior-new",
    rowTestId: "config-behavior-ops",
    selectId: "ops",
    renderPanel: (onCreate, onSelect) => (
      <BehaviorConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        onCreateBackend={vi.fn()}
        onCreateBehavior={onCreate}
        onCreateProfile={vi.fn()}
        onCreateToolSelection={vi.fn()}
        onSaveAgentConfig={vi.fn()}
        onSaveBehaviorConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectBehavior={onSelect}
      />
    ),
  },
  {
    createTestId: "backend-new",
    rowTestId: "config-backend-backend-a",
    selectId: "backend-a",
    renderPanel: (onCreate, onSelect) => (
      <BackendConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedBackendId={deployment.inferenceBackends[0].backendId}
        onCreateBackend={onCreate}
        onSaveBackendConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectBackend={onSelect}
      />
    ),
  },
  {
    createTestId: "profile-new",
    rowTestId: "config-profile-profile-a",
    selectId: "profile-a",
    renderPanel: (onCreate, onSelect) => (
      <InferenceProfileConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedProfileId={deployment.inferenceProfiles[0].profileId}
        onCreateProfile={onCreate}
        onSaveInferenceProfileConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectProfile={onSelect}
      />
    ),
  },
  {
    createTestId: "tool-selection-new",
    rowTestId: "config-tool-selection-tools-a",
    selectId: "tools-a",
    renderPanel: (onCreate, onSelect) => (
      <ToolSelectionConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedToolSelectionId={deployment.toolSelections[0].selectionId}
        toolCeiling="Readwrite"
        toolRoot="/tmp/work"
        onCreateToolSelection={onCreate}
        onSaveToolSelectionConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectToolSelection={onSelect}
      />
    ),
  },
  {
    createTestId: "tool-service-new",
    rowTestId: "config-tool-service-service-a",
    selectId: "service-a",
    renderPanel: (onCreate, onSelect) => (
      <ToolServiceConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedToolServiceId={deployment.toolServiceRegistries[0].serviceId}
        onCreateToolService={onCreate}
        onSaveToolServiceConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectToolService={onSelect}
        onTestToolService={vi.fn()}
      />
    ),
  },
  {
    createTestId: "task-new",
    rowTestId: "config-task-task-a",
    selectId: "task-a",
    renderPanel: (onCreate, onSelect) => (
      <TaskConfigPanel
        deployment={deployment}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateTask={onCreate}
        onRunTask={vi.fn()}
        onSaveTaskConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectTask={onSelect}
      />
    ),
  },
  {
    createTestId: "schedule-new",
    rowTestId: "config-schedule-timer-a",
    selectId: "timer-a",
    renderPanel: (onCreate, onSelect) => (
      <ScheduleConfigPanel
        deployment={deployment}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedScheduleId={deployment.schedules[0].scheduleId}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateSchedule={onCreate}
        onRunSchedule={vi.fn()}
        onSaveScheduleConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectSchedule={onSelect}
      />
    ),
  },
  {
    createTestId: "event-trigger-new",
    rowTestId: "config-event-trigger-event-a",
    selectId: "event-a",
    renderPanel: (onCreate, onSelect) => (
      <EventTriggerConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedEventTriggerId={deployment.eventTriggers[0].triggerId}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateEventTrigger={onCreate}
        onSaveEventTriggerConfig={vi.fn()}
        onSavedStatusChange={vi.fn()}
        onSelectEventTrigger={onSelect}
      />
    ),
  },
];

function workspaceHandlers() {
  return {
    onBack: vi.fn(),
    onSaveAgentConfig: vi.fn<[(request: AgentConfigSaveRequest) => Promise<unknown>]>(
      () => Promise.resolve(),
    ),
    onSaveBackendConfig: vi.fn<[(request: BackendSaveRequest) => Promise<unknown>]>(
      () => Promise.resolve(),
    ),
    onSaveInferenceProfileConfig: vi.fn<
      [(request: InferenceProfileSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onSaveToolSelectionConfig: vi.fn<
      [(request: ToolSelectionSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onSaveToolServiceConfig: vi.fn<
      [(request: ToolServiceSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onTestToolService: vi.fn<
      [(request: ToolServiceTestRequest) => Promise<ToolServiceTestResult>]
    >(() =>
      Promise.resolve({
        serviceId: "service-a",
        endpoint: "http://localhost:7331/mcp",
        status: "ok",
        toolCount: 0,
        tools: [],
      }),
    ),
    onSaveBehaviorConfig: vi.fn<
      [(request: BehaviorSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onSaveTaskConfig: vi.fn<[(request: TaskSaveRequest) => Promise<unknown>]>(
      () => Promise.resolve(),
    ),
    onSaveScheduleConfig: vi.fn<
      [(request: ScheduleSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onRunSchedule: vi.fn<[(request: { scheduleId: string }) => Promise<TaskRunResult>]>(
      () => Promise.resolve(runResult),
    ),
    onSaveEventTriggerConfig: vi.fn<
      [(request: EventTriggerSaveRequest) => Promise<unknown>]
    >(() => Promise.resolve()),
    onRunTask: vi.fn<
      [(request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>]
    >(() => Promise.resolve(runResult)),
  };
}

type SaveBoundaryCase = {
  name: string;
  saveTestId: string;
  selectedId: string;
  savedStatus: string;
  renderPanel: (
    onSave: (request: unknown) => Promise<unknown>,
    onSelect: (id: string) => void,
    onSavedStatusChange: (value: string) => void,
  ) => ReactElement;
};

const saveBoundaryCases: SaveBoundaryCase[] = [
  {
    name: "behavior",
    saveTestId: "behavior-save",
    selectedId: "default",
    savedStatus: "behavior:default",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <BehaviorConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        onCreateBackend={vi.fn()}
        onCreateBehavior={vi.fn()}
        onCreateProfile={vi.fn()}
        onCreateToolSelection={vi.fn()}
        onSaveAgentConfig={vi.fn()}
        onSaveBehaviorConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectBehavior={onSelect}
      />
    ),
  },
  {
    name: "backend",
    saveTestId: "backend-save",
    selectedId: "backend-a",
    savedStatus: "backend:backend-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <BackendConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedBackendId={deployment.inferenceBackends[0].backendId}
        onCreateBackend={vi.fn()}
        onSaveBackendConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectBackend={onSelect}
      />
    ),
  },
  {
    name: "profile",
    saveTestId: "profile-save",
    selectedId: "profile-a",
    savedStatus: "profile:profile-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <InferenceProfileConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedProfileId={deployment.inferenceProfiles[0].profileId}
        onCreateProfile={vi.fn()}
        onSaveInferenceProfileConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectProfile={onSelect}
      />
    ),
  },
  {
    name: "tool selection",
    saveTestId: "tool-selection-save",
    selectedId: "tools-a",
    savedStatus: "tool:tools-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <ToolSelectionConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedToolSelectionId={deployment.toolSelections[0].selectionId}
        toolCeiling="Readwrite"
        toolRoot="/tmp/work"
        onCreateToolSelection={vi.fn()}
        onSaveToolSelectionConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectToolSelection={onSelect}
      />
    ),
  },
  {
    name: "tool service",
    saveTestId: "tool-service-save",
    selectedId: "service-a",
    savedStatus: "tool-service:service-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <ToolServiceConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedToolServiceId={deployment.toolServiceRegistries[0].serviceId}
        onCreateToolService={vi.fn()}
        onSaveToolServiceConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectToolService={onSelect}
        onTestToolService={vi.fn()}
      />
    ),
  },
  {
    name: "task",
    saveTestId: "task-save",
    selectedId: "task-a",
    savedStatus: "task:task-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <TaskConfigPanel
        deployment={deployment}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedBehavior={deployment.behaviors[0]}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateTask={vi.fn()}
        onRunTask={vi.fn()}
        onSaveTaskConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectTask={onSelect}
      />
    ),
  },
  {
    name: "schedule",
    saveTestId: "schedule-save",
    selectedId: "timer-a",
    savedStatus: "schedule:timer-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <ScheduleConfigPanel
        deployment={deployment}
        runningTask={false}
        savedStatus={null}
        saving={false}
        selectedScheduleId={deployment.schedules[0].scheduleId}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateSchedule={vi.fn()}
        onRunSchedule={vi.fn()}
        onSaveScheduleConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectSchedule={onSelect}
      />
    ),
  },
  {
    name: "event trigger",
    saveTestId: "event-trigger-save",
    selectedId: "event-a",
    savedStatus: "event-trigger:event-a",
    renderPanel: (onSave, onSelect, onSavedStatusChange) => (
      <EventTriggerConfigPanel
        deployment={deployment}
        savedStatus={null}
        saving={false}
        selectedEventTriggerId={deployment.eventTriggers[0].triggerId}
        selectedTaskId={deployment.tasks[0].taskId}
        onCreateEventTrigger={vi.fn()}
        onSaveEventTriggerConfig={(request) => onSave(request)}
        onSavedStatusChange={onSavedStatusChange}
        onSelectEventTrigger={onSelect}
      />
    ),
  },
];

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
