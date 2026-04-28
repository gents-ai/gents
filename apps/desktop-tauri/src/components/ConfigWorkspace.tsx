import { useEffect, useMemo, useState } from "react";

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
} from "../lib/types";
import {
  AgentConfigPanel,
  BackendConfigPanel,
  BehaviorConfigPanel,
  EventTriggerConfigPanel,
  InferenceProfileConfigPanel,
  ScheduleConfigPanel,
  TaskConfigPanel,
  ToolSelectionConfigPanel,
  ToolServiceConfigPanel,
} from "./config";
import sourceLogoUrl from "../../src-tauri/icons/icon.png";

type ConfigTab =
  | "agent"
  | "behavior"
  | "backends"
  | "profiles"
  | "toolSelections"
  | "metaTools"
  | "tasks"
  | "timerTriggers"
  | "eventTriggers";

type ConfigWorkspaceProps = {
  bootstrap: BootstrapSummary | null;
  selectedDeployment: DeploymentView | null;
  selectedBehaviorId: string | null;
  saving: boolean;
  runningTask: boolean;
  onBack: () => void;
  onSaveAgentConfig: (request: AgentConfigSaveRequest) => Promise<unknown>;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<unknown>;
  onSaveToolSelectionConfig: (
    request: ToolSelectionSaveRequest,
  ) => Promise<unknown>;
  onSaveToolServiceConfig: (
    request: ToolServiceSaveRequest,
  ) => Promise<unknown>;
  onTestToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onSaveTaskConfig: (request: TaskSaveRequest) => Promise<unknown>;
  onSaveScheduleConfig: (request: ScheduleSaveRequest) => Promise<unknown>;
  onRunSchedule: (request: { scheduleId: string }) => Promise<TaskRunResult>;
  onSaveEventTriggerConfig: (
    request: EventTriggerSaveRequest,
  ) => Promise<unknown>;
  onRunTask: (request: {
    taskId: string;
    args?: unknown;
  }) => Promise<TaskRunResult>;
};

const TABS: Array<{ id: ConfigTab; label: string }> = [
  { id: "agent", label: "Agent" },
  { id: "behavior", label: "Behavior" },
  { id: "backends", label: "Backends" },
  { id: "profiles", label: "Profiles" },
  { id: "toolSelections", label: "Tool Selections" },
  { id: "metaTools", label: "Meta Tools" },
  { id: "tasks", label: "Tasks" },
  { id: "timerTriggers", label: "Timer Triggers" },
  { id: "eventTriggers", label: "Event Triggers" },
];

const NEW_DOCUMENT_ID = "__new__";

export function ConfigWorkspace({
  bootstrap,
  selectedDeployment,
  selectedBehaviorId,
  saving,
  runningTask,
  onBack,
  onSaveAgentConfig,
  onSaveBackendConfig,
  onSaveInferenceProfileConfig,
  onSaveToolSelectionConfig,
  onSaveToolServiceConfig,
  onTestToolService,
  onSaveBehaviorConfig,
  onSaveTaskConfig,
  onSaveScheduleConfig,
  onRunSchedule,
  onSaveEventTriggerConfig,
  onRunTask,
}: ConfigWorkspaceProps) {
  const [activeTab, setActiveTab] = useState<ConfigTab>("behavior");
  const [selectedConfigBehaviorId, setSelectedConfigBehaviorId] = useState<string | null>(null);
  const [selectedBackendId, setSelectedBackendId] = useState<string | null>(null);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [selectedToolSelectionId, setSelectedToolSelectionId] =
    useState<string | null>(null);
  const [selectedToolServiceId, setSelectedToolServiceId] =
    useState<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [selectedScheduleId, setSelectedScheduleId] = useState<string | null>(
    null,
  );
  const [selectedEventTriggerId, setSelectedEventTriggerId] = useState<
    string | null
  >(null);
  const [savedStatus, setSavedStatus] = useState<string | null>(null);

  const selectedBehavior = useMemo(() => {
    if (!selectedDeployment) {
      return null;
    }
    return (
      selectedDeployment.behaviors.find(
        (behavior) => behavior.behaviorId === selectedConfigBehaviorId,
      ) ??
      selectedDeployment.behaviors.find(
        (behavior) => behavior.behaviorId === selectedBehaviorId,
      ) ??
      selectedDeployment.behaviors.find((behavior) => behavior.isDefault) ??
      selectedDeployment.behaviors[0] ??
      null
    );
  }, [selectedBehaviorId, selectedConfigBehaviorId, selectedDeployment]);

  useEffect(() => {
    if (!selectedDeployment) {
      setSelectedConfigBehaviorId(null);
      setSelectedBackendId(null);
      setSelectedProfileId(null);
      setSelectedToolSelectionId(null);
      setSelectedToolServiceId(null);
      setSelectedTaskId(null);
      setSelectedScheduleId(null);
      setSelectedEventTriggerId(null);
      return;
    }

    ensureSelection(
      selectedConfigBehaviorId,
      selectedBehaviorId ??
        selectedDeployment.defaultBehaviorId ??
        selectedDeployment.behaviors.find((behavior) => behavior.isDefault)?.behaviorId ??
        selectedDeployment.behaviors[0]?.behaviorId ??
        null,
      (id) => selectedDeployment.behaviors.some((behavior) => behavior.behaviorId === id),
      setSelectedConfigBehaviorId,
    );
    ensureSelection(
      selectedBackendId,
      selectedBehavior?.backendId ??
        selectedDeployment.inferenceBackends[0]?.backendId ??
        null,
      (id) =>
        selectedDeployment.inferenceBackends.some(
          (backend) => backend.backendId === id,
        ),
      setSelectedBackendId,
    );
    ensureSelection(
      selectedProfileId,
      selectedBehavior?.inferenceProfileId ??
        selectedDeployment.inferenceProfiles[0]?.profileId ??
        null,
      (id) =>
        selectedDeployment.inferenceProfiles.some(
          (profile) => profile.profileId === id,
        ),
      setSelectedProfileId,
    );
    ensureSelection(
      selectedToolSelectionId,
      selectedBehavior?.toolSelectionId ??
        selectedDeployment.toolSelections[0]?.selectionId ??
        null,
      (id) =>
        selectedDeployment.toolSelections.some(
          (selection) => selection.selectionId === id,
        ),
      setSelectedToolSelectionId,
    );
    ensureSelection(
      selectedToolServiceId,
      selectedDeployment.toolServiceRegistries[0]?.serviceId ?? null,
      (id) =>
        selectedDeployment.toolServiceRegistries.some(
          (service) => service.serviceId === id,
        ),
      setSelectedToolServiceId,
    );
    ensureSelection(
      selectedTaskId,
      selectedDeployment.tasks[0]?.taskId ?? null,
      (id) => selectedDeployment.tasks.some((task) => task.taskId === id),
      setSelectedTaskId,
    );
    ensureSelection(
      selectedScheduleId,
      selectedDeployment.schedules[0]?.scheduleId ?? null,
      (id) =>
        selectedDeployment.schedules.some(
          (schedule) => schedule.scheduleId === id,
        ),
      setSelectedScheduleId,
    );
    ensureSelection(
      selectedEventTriggerId,
      selectedDeployment.eventTriggers[0]?.triggerId ?? null,
      (id) =>
        selectedDeployment.eventTriggers.some(
          (trigger) => trigger.triggerId === id,
        ),
      setSelectedEventTriggerId,
    );
  }, [
    selectedBackendId,
    selectedBehavior,
    selectedBehaviorId,
    selectedConfigBehaviorId,
    selectedDeployment,
    selectedEventTriggerId,
    selectedProfileId,
    selectedScheduleId,
    selectedTaskId,
    selectedToolSelectionId,
    selectedToolServiceId,
  ]);

  if (!selectedDeployment) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Config</p>
        <h2>Select a deployment</h2>
        <button className="ghost-button" onClick={onBack} type="button">
          Back to Fleet
        </button>
      </article>
    );
  }

  return (
    <section className="config-workspace config-workspace-full">
      <header className="config-header">
        <div className="config-brand">
          <img alt="" className="config-brand-logo" src={sourceLogoUrl} />
          <div className="config-title-block">
            <p className="eyebrow">Defra Agent Config</p>
            <h1>{selectedDeployment.label}</h1>
            <p className="muted mono" title={selectedDeployment.agentDid}>
              {selectedDeployment.agentDid}
            </p>
          </div>
        </div>
        <div className="config-header-actions">
          <span
            aria-hidden="true"
            className={
              selectedDeployment.dialSucceeded
                ? "config-status-dot green"
                : "config-status-dot yellow"
            }
            title={
              selectedDeployment.dialSucceeded
                ? "P2P connected"
                : "P2P connection saved"
            }
          />
          <span className="chip">
            {selectedDeployment.dialSucceeded ? "connected" : "saved"}
          </span>
          <button
            className="ghost-button"
            data-testid="config-back-tab"
            onClick={onBack}
            type="button"
          >
            Back to Fleet
          </button>
        </div>
      </header>

      <nav className="config-screen-nav" role="tablist">
        {TABS.map((tab) => (
          <button
            className={activeTab === tab.id ? "tab-button selected" : "tab-button"}
            data-testid={`config-tab-${tab.id}`}
            key={tab.id}
            onClick={() => {
              setActiveTab(tab.id);
              setSavedStatus(null);
            }}
            type="button"
          >
            {tab.label}
          </button>
        ))}
      </nav>

      {activeTab === "agent" ? (
        <AgentConfigPanel
          bootstrap={bootstrap}
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          onSaveAgentConfig={onSaveAgentConfig}
          onSavedStatusChange={setSavedStatus}
        />
      ) : null}

      {activeTab === "behavior" ? (
        <BehaviorConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedBehavior={
            selectedConfigBehaviorId === NEW_DOCUMENT_ID ? null : selectedBehavior
          }
          onCreateBehavior={() => setSelectedConfigBehaviorId(NEW_DOCUMENT_ID)}
          onCreateBackend={() => {
            setActiveTab("backends");
            setSelectedBackendId(NEW_DOCUMENT_ID);
            setSavedStatus(null);
          }}
          onCreateProfile={() => {
            setActiveTab("profiles");
            setSelectedProfileId(NEW_DOCUMENT_ID);
            setSavedStatus(null);
          }}
          onCreateToolSelection={() => {
            setActiveTab("toolSelections");
            setSelectedToolSelectionId(NEW_DOCUMENT_ID);
            setSavedStatus(null);
          }}
          onSaveAgentConfig={onSaveAgentConfig}
          onSaveBehaviorConfig={onSaveBehaviorConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectBehavior={setSelectedConfigBehaviorId}
        />
      ) : null}

      {activeTab === "backends" ? (
        <BackendConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedBackendId={selectedBackendId}
          onCreateBackend={() => setSelectedBackendId(NEW_DOCUMENT_ID)}
          onSaveBackendConfig={onSaveBackendConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectBackend={setSelectedBackendId}
        />
      ) : null}

      {activeTab === "profiles" ? (
        <InferenceProfileConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedProfileId={selectedProfileId}
          onCreateProfile={() => setSelectedProfileId(NEW_DOCUMENT_ID)}
          onSaveInferenceProfileConfig={onSaveInferenceProfileConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectProfile={setSelectedProfileId}
        />
      ) : null}

      {activeTab === "toolSelections" ? (
        <ToolSelectionConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedToolSelectionId={selectedToolSelectionId}
          toolCeiling={bootstrap?.initToolCeiling ?? null}
          toolRoot={bootstrap?.initToolRoot ?? null}
          onCreateToolSelection={() => setSelectedToolSelectionId(NEW_DOCUMENT_ID)}
          onSaveToolSelectionConfig={onSaveToolSelectionConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectToolSelection={setSelectedToolSelectionId}
        />
      ) : null}

      {activeTab === "metaTools" ? (
        <ToolServiceConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedToolServiceId={selectedToolServiceId}
          onCreateToolService={() => setSelectedToolServiceId(NEW_DOCUMENT_ID)}
          onSaveToolServiceConfig={onSaveToolServiceConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectToolService={setSelectedToolServiceId}
          onTestToolService={onTestToolService}
        />
      ) : null}

      {activeTab === "tasks" ? (
        <TaskConfigPanel
          deployment={selectedDeployment}
          runningTask={runningTask}
          savedStatus={savedStatus}
          saving={saving}
          selectedBehavior={selectedBehavior}
          selectedTaskId={selectedTaskId}
          onCreateTask={() => setSelectedTaskId(NEW_DOCUMENT_ID)}
          onRunTask={onRunTask}
          onSaveTaskConfig={onSaveTaskConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectTask={setSelectedTaskId}
        />
      ) : null}

      {activeTab === "timerTriggers" ? (
        <ScheduleConfigPanel
          deployment={selectedDeployment}
          runningTask={runningTask}
          savedStatus={savedStatus}
          saving={saving}
          selectedScheduleId={selectedScheduleId}
          selectedTaskId={selectedTaskId}
          onCreateSchedule={() => setSelectedScheduleId(NEW_DOCUMENT_ID)}
          onRunSchedule={onRunSchedule}
          onSaveScheduleConfig={onSaveScheduleConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectSchedule={setSelectedScheduleId}
        />
      ) : null}

      {activeTab === "eventTriggers" ? (
        <EventTriggerConfigPanel
          deployment={selectedDeployment}
          savedStatus={savedStatus}
          saving={saving}
          selectedEventTriggerId={selectedEventTriggerId}
          selectedTaskId={selectedTaskId}
          onCreateEventTrigger={() => setSelectedEventTriggerId(NEW_DOCUMENT_ID)}
          onSaveEventTriggerConfig={onSaveEventTriggerConfig}
          onSavedStatusChange={setSavedStatus}
          onSelectEventTrigger={setSelectedEventTriggerId}
        />
      ) : null}
    </section>
  );
}

function ensureSelection(
  current: string | null,
  fallback: string | null,
  exists: (id: string) => boolean,
  setSelection: (id: string | null) => void,
) {
  if (current === NEW_DOCUMENT_ID) {
    return;
  }
  if (current && exists(current)) {
    return;
  }
  if (current !== fallback) {
    setSelection(fallback);
  }
}
