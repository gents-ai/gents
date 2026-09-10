import type { Dispatch, SetStateAction } from "react";

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

type TaskActionParams = {
  api: DesktopApiAdapter;
  refreshSession: (
    nextSessionId: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  refreshSnapshot: () => Promise<void>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRunningTask: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
};

export function createDesktopShellTaskActions({
  api,
  refreshSession,
  refreshSnapshot,
  setError,
  setRunningTask,
  setSavingConfig,
  setSelectedSessionId,
  setSnapshot,
}: TaskActionParams) {
  async function onSaveTaskConfig(request: TaskSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.saveTaskConfig(request);
      setSnapshot(next);
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
      const next = await api.saveScheduleConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
    setRunningTask(true);
    setError(null);
    try {
      const result = await api.runSchedule(request);
      await refreshSnapshot();
      if (result.sessionId) {
        setSelectedSessionId(result.sessionId);
        await refreshSession(result.sessionId);
      }
      return result;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setRunningTask(false);
    }
  }

  async function onSaveTriggerConfig(request: TriggerSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.saveTriggerConfig(request);
      setSnapshot(next);
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
      const next = await api.saveEventSourceConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunTask(request: TaskRunRequest): Promise<TaskRunResult> {
    setRunningTask(true);
    setError(null);
    try {
      const result = await api.runTask(request);
      await refreshSnapshot();
      if (result.sessionId) {
        setSelectedSessionId(result.sessionId);
        await refreshSession(result.sessionId);
      }
      return result;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setRunningTask(false);
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
