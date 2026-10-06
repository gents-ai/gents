import type {
  DesktopApiAdapter,
  ScheduleRunRequest,
  TaskRunRequest,
  TaskRunResult,
} from "@source-inc/gents-desktop-client";
import { actionFailure, logShellEvent, shownFailure } from "./desktopShellRuntime";
import { selection, type SelectionStore } from "./selectionStore";

type TaskActionParams = {
  api: DesktopApiAdapter;
  /** the selection, whose intent a run's result is checked against */
  store: SelectionStore;
  refreshSnapshot: () => Promise<void>;
  setError: (error: string | null) => void;
};

export function createDesktopShellTaskActions({
  api,
  store,
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

  async function runSchedule(request: ScheduleRunRequest): Promise<TaskRunResult> {
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

  async function runTask(request: TaskRunRequest): Promise<TaskRunResult> {
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
    /**
     * Runs a schedule now and reads the client again once the bridge accepts
     * it; a failed read after acceptance is logged, not reported, since the
     * run stands. A failure is reported once, unless the person has moved on,
     * then rethrown.
     */
    runSchedule,
    /**
     * Runs a task now and reads the client again once the bridge accepts it; a
     * failed read after acceptance is logged, not reported, since the run
     * stands. A failure is reported once, unless the person has moved on, then
     * rethrown.
     */
    runTask,
  };
}
