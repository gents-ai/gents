import type {
  DesktopApiAdapter,
  EventSourceSaveRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  TriggerSaveRequest,
} from "@source-inc/gents-desktop-client";
import { actionFailure, logShellEvent, shownFailure } from "./desktopShellRuntime";
import { selection, type SelectionStore } from "./selectionStore";

type TaskActionParams = {
  api: DesktopApiAdapter;
  /** the selection, whose intent a run's result is checked against */
  store: SelectionStore;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  setError: (error: string | null) => void;
};

export function createDesktopShellTaskActions({
  api,
  store,
  mutateSnapshot,
  refreshSnapshot,
  setError,
}: TaskActionParams) {
  async function observeAcceptedRun(kind: "task" | "schedule") {
    try {
      await refreshSnapshot();
    } catch (error) {
      logShellEvent(
        `${kind} run accepted but observation refresh failed: ${String(error)}`,
      );
    }
  }

  /** A configuration change: a fresh read once it lands, and one report if
      it fails, naming what failed. */
  async function change<T>(label: string, run: () => Promise<T>) {
    setError(null);
    try {
      return await mutateSnapshot(run);
    } catch (error) {
      setError(actionFailure(label, error));
      throw shownFailure(error);
    }
  }

  function onSaveTaskConfig(request: TaskSaveRequest) {
    return change("save the task", () => api.saveTaskConfig(request));
  }

  function onSaveScheduleConfig(request: ScheduleSaveRequest) {
    return change("save the schedule", () => api.saveScheduleConfig(request));
  }

  async function onRunSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
    const intentGeneration = selection.captureIntent(store);
    setError(null);
    try {
      const result = await api.runSchedule(request);
      await observeAcceptedRun("schedule");
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) throw err;
      setError(actionFailure("run the schedule", err));
      throw shownFailure(err);
    }
  }

  function onSaveTriggerConfig(request: TriggerSaveRequest) {
    return change("save the trigger", () => api.saveTriggerConfig(request));
  }

  function onSaveEventSourceConfig(request: EventSourceSaveRequest) {
    return change("save the event source", () => api.saveEventSourceConfig(request));
  }

  async function onRunTask(request: TaskRunRequest): Promise<TaskRunResult> {
    const intentGeneration = selection.captureIntent(store);
    setError(null);
    try {
      const result = await api.runTask(request);
      await observeAcceptedRun("task");
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) throw err;
      setError(actionFailure("run the task", err));
      throw shownFailure(err);
    }
  }

  return {
    onRunSchedule,
    onRunTask,
    onSaveEventSourceConfig,
    onSaveScheduleConfig,
    onSaveTaskConfig,
    onSaveTriggerConfig,
  };
}
