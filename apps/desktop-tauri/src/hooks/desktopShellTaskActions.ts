import type { Dispatch, SetStateAction } from "react";

import {
  runSchedule,
  runTask,
  saveEventTriggerConfig,
  saveScheduleConfig,
  saveTaskConfig,
} from "../lib/desktop-api";
import type {
  DesktopClientSnapshot,
  EventTriggerSaveRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
} from "../lib/types";

type TaskActionParams = {
  refreshSession: (nextSessionId: string | null) => Promise<void>;
  refreshSnapshot: () => Promise<void>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRunningTask: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedSessionId: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
};

export function createDesktopShellTaskActions({
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
      const next = await saveTaskConfig(request);
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
      const next = await saveScheduleConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onRunSchedule(
    request: ScheduleRunRequest,
  ): Promise<TaskRunResult> {
    setRunningTask(true);
    setError(null);
    try {
      const result = await runSchedule(request);
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

  async function onSaveEventTriggerConfig(request: EventTriggerSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveEventTriggerConfig(request);
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
      const result = await runTask(request);
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
    onSaveEventTriggerConfig,
    onSaveScheduleConfig,
    onSaveTaskConfig,
  };
}
