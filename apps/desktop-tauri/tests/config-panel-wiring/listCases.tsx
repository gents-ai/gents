import type { ReactElement } from "react";
import { vi } from "vitest";

import {
  BackendConfigPanel,
  BehaviorConfigPanel,
  EventTriggerConfigPanel,
  InferenceProfileConfigPanel,
  ScheduleConfigPanel,
  TaskConfigPanel,
  ToolSelectionConfigPanel,
  ToolServiceConfigPanel,
} from "../../src/components/config";
import { deployment } from "./fixtures";

type ListCase = {
  createTestId: string;
  rowTestId: string;
  selectId: string;
  renderPanel: (onCreate: () => void, onSelect: (id: string) => void) => ReactElement;
};

export const listCases: ListCase[] = [
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
