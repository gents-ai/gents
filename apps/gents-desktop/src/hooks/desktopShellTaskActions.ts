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
import { logShellEvent } from "./desktopShellRuntime";

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

  async function onSaveTaskConfig(request: TaskSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveTaskConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveScheduleConfig(request: ScheduleSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveScheduleConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
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
      if (acceptsComposeIntent(intentGeneration)) setError(String(err));
      throw err;
    } finally {
      finishTaskRun();
    }
  }

  async function onSaveTriggerConfig(request: TriggerSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveTriggerConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveEventSourceConfig(request: EventSourceSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveEventSourceConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
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
      if (acceptsComposeIntent(intentGeneration)) setError(String(err));
      throw err;
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
