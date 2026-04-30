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

export const saveBoundaryCases: SaveBoundaryCase[] = [
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
