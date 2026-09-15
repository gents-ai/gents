import type { Dispatch, MutableRefObject, SetStateAction } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  EventSourceSaveRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  TriggerSaveRequest,
} from "@source-inc/gents-desktop-client";
import type { SnapshotPublication } from "./desktopSnapshotPublication";
import { logShellEvent } from "./desktopShellRuntime";

type TaskActionParams = {
  acceptsComposeIntent: (capturedGeneration: number) => boolean;
  advanceComposeIntent: () => void;
  api: DesktopApiAdapter;
  captureComposeIntent: () => number;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  runningTaskCountRef: MutableRefObject<number>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRunningTask: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  beginSnapshotPublication: () => SnapshotPublication;
};

export function createDesktopShellTaskActions({
  acceptsComposeIntent,
  advanceComposeIntent,
  api,
  captureComposeIntent,
  refreshSession,
  refreshSnapshot,
  runningTaskCountRef,
  setError,
  setRunningTask,
  setSavingConfig,
  setSelectedSessionId,
  beginSnapshotPublication,
}: TaskActionParams) {
  function beginTaskRun() {
    runningTaskCountRef.current += 1;
    setRunningTask(true);
  }

  function finishTaskRun() {
    runningTaskCountRef.current = Math.max(0, runningTaskCountRef.current - 1);
    if (runningTaskCountRef.current === 0) setRunningTask(false);
  }

  async function observeAcceptedRun(
    kind: "task" | "schedule",
    result: TaskRunResult,
    intentGeneration: number,
  ) {
    try {
      await refreshSnapshot();
      if (result.sessionId && acceptsComposeIntent(intentGeneration)) {
        setSelectedSessionId(result.sessionId);
        await refreshSession(result.sessionId);
      }
    } catch (error) {
      logShellEvent(
        `${kind} run accepted but observation refresh failed: ${String(error)}`,
      );
    }
  }

  async function publishSnapshotResult(
    operation: () => Promise<DesktopClientSnapshot>,
  ) {
    const publication = beginSnapshotPublication();
    const next = await operation();
    publication.publish(next);
    return next;
  }
  async function onSaveTaskConfig(request: TaskSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await publishSnapshotResult(() => api.saveTaskConfig(request));
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
      const next = await publishSnapshotResult(() => api.saveScheduleConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
    advanceComposeIntent();
    const intentGeneration = captureComposeIntent();
    beginTaskRun();
    setError(null);
    try {
      const result = await api.runSchedule(request);
      await observeAcceptedRun("schedule", result, intentGeneration);
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
      const next = await publishSnapshotResult(() => api.saveTriggerConfig(request));
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
      const next = await publishSnapshotResult(() =>
        api.saveEventSourceConfig(request),
      );
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunTask(request: TaskRunRequest): Promise<TaskRunResult> {
    advanceComposeIntent();
    const intentGeneration = captureComposeIntent();
    beginTaskRun();
    setError(null);
    try {
      const result = await api.runTask(request);
      await observeAcceptedRun("task", result, intentGeneration);
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
