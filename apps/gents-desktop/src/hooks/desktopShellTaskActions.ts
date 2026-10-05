import type { Dispatch, MutableRefObject, SetStateAction } from "react";

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

type TaskActionParams = {
  acceptsComposeIntent: (capturedGeneration: number) => boolean;
  api: DesktopApiAdapter;
  captureComposeIntent: () => number;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  runningTaskCountRef: MutableRefObject<number>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRunningTask: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
};

export function createDesktopShellTaskActions({
  acceptsComposeIntent,
  api,
  captureComposeIntent,
  mutateSnapshot,
  refreshSnapshot,
  runningTaskCountRef,
  setError,
  setRunningTask,
  setSavingConfig,
}: TaskActionParams) {
  function beginTaskRun() {
    runningTaskCountRef.current += 1;
    setRunningTask(true);
  }

  function finishTaskRun() {
    runningTaskCountRef.current = Math.max(0, runningTaskCountRef.current - 1);
    if (runningTaskCountRef.current === 0) setRunningTask(false);
  }

  async function observeAcceptedRun(kind: "task" | "schedule") {
    try {
      await refreshSnapshot();
    } catch (error) {
      logShellEvent(
        `${kind} run accepted but observation refresh failed: ${String(error)}`,
      );
    }
  }

  /** A configuration change: busy while it runs, a fresh read once it lands,
      and one report if it fails, naming what failed. */
  async function change<T>(label: string, run: () => Promise<T>) {
    setSavingConfig(true);
    setError(null);
    try {
      return await mutateSnapshot(run);
    } catch (error) {
      setError(actionFailure(label, error));
      throw shownFailure(error);
    } finally {
      setSavingConfig(false);
    }
  }

  function onSaveTaskConfig(request: TaskSaveRequest) {
    return change("save the task", () => api.saveTaskConfig(request));
  }

  function onSaveScheduleConfig(request: ScheduleSaveRequest) {
    return change("save the schedule", () => api.saveScheduleConfig(request));
  }

  async function onRunSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
    const intentGeneration = captureComposeIntent();
    beginTaskRun();
    setError(null);
    try {
      const result = await api.runSchedule(request);
      await observeAcceptedRun("schedule");
      return result;
    } catch (err) {
      if (!acceptsComposeIntent(intentGeneration)) throw err;
      setError(actionFailure("run the schedule", err));
      throw shownFailure(err);
    } finally {
      finishTaskRun();
    }
  }

  function onSaveTriggerConfig(request: TriggerSaveRequest) {
    return change("save the trigger", () => api.saveTriggerConfig(request));
  }

  function onSaveEventSourceConfig(request: EventSourceSaveRequest) {
    return change("save the event source", () => api.saveEventSourceConfig(request));
  }

  async function onRunTask(request: TaskRunRequest): Promise<TaskRunResult> {
    const intentGeneration = captureComposeIntent();
    beginTaskRun();
    setError(null);
    try {
      const result = await api.runTask(request);
      await observeAcceptedRun("task");
      return result;
    } catch (err) {
      if (!acceptsComposeIntent(intentGeneration)) throw err;
      setError(actionFailure("run the task", err));
      throw shownFailure(err);
    } finally {
      finishTaskRun();
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
