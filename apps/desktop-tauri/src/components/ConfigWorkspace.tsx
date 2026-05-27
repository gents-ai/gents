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
import { NEW_DOCUMENT_ID, TABS } from "./config-workspace/model";
import { useConfigWorkspaceSelection } from "./config-workspace/useConfigWorkspaceSelection";
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
import sourceMarkUrl from "../assets/source-mark-light.png";

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
  onSaveToolSelectionConfig: (request: ToolSelectionSaveRequest) => Promise<unknown>;
  onSaveToolServiceConfig: (request: ToolServiceSaveRequest) => Promise<unknown>;
  onTestToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onSaveTaskConfig: (request: TaskSaveRequest) => Promise<unknown>;
  onSaveScheduleConfig: (request: ScheduleSaveRequest) => Promise<unknown>;
  onRunSchedule: (request: { scheduleId: string }) => Promise<TaskRunResult>;
  onSaveEventTriggerConfig: (request: EventTriggerSaveRequest) => Promise<unknown>;
  onRunTask: (request: { taskId: string; args?: unknown }) => Promise<TaskRunResult>;
};

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
  const {
    activeTab,
    savedStatus,
    selectConfigBehavior,
    selectedBackendId,
    selectedBehavior,
    selectedConfigBehaviorId,
    selectedEventTriggerId,
    selectedProfileId,
    selectedScheduleId,
    selectedTaskId,
    selectedToolSelectionId,
    selectedToolServiceId,
    setActiveTab,
    setSavedStatus,
    setSelectedBackendId,
    setSelectedConfigBehaviorId,
    setSelectedEventTriggerId,
    setSelectedProfileId,
    setSelectedScheduleId,
    setSelectedTaskId,
    setSelectedToolSelectionId,
    setSelectedToolServiceId,
  } = useConfigWorkspaceSelection(selectedDeployment, selectedBehaviorId);

  if (!selectedDeployment) {
    return (
      <article className="panel centered-panel">
        <p className="eyebrow">Config</p>
        <h2>Select a deployment</h2>
        <button className="ghost-button" onClick={onBack} type="button">
          Back to Chat
        </button>
      </article>
    );
  }

  return (
    <section className="config-workspace config-workspace-full">
      <header className="config-header">
        <div className="config-brand">
          <img alt="Source" className="config-brand-logo" src={sourceMarkUrl} />
          <div className="config-title-block">
            <p className="eyebrow">Source Network Config</p>
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
            Back to Chat
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
          onSelectBehavior={selectConfigBehavior}
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
