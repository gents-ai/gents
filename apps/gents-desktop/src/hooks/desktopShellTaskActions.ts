import type {
  DesktopApiAdapter,
  ScheduleRunRequest,
  TaskRunRequest,
  TaskRunResult,
} from "@source-inc/gents-desktop-client";
import { actionFailure, shownFailure } from "./desktopShellRuntime";
import { selection, type SelectionStore } from "./selectionStore";

type TaskActionParams = {
  api: DesktopApiAdapter;
  /** the selection, whose intent a run's result is checked against */
  store: SelectionStore;
  refreshSnapshot: () => Promise<void>;
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
};

export function createDesktopShellTaskActions({
  api,
  store,
  refreshSnapshot,
  reportFailure,
}: TaskActionParams) {
  /* a run, then the client read again once the bridge accepted it; a
     failed read shows in the banner like any read, and the run stands */
  async function run(label: string, start: () => Promise<TaskRunResult>) {
    const intentGeneration = selection.captureIntent(store);
    try {
      const result = await start();
      await refreshSnapshot();
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) throw err;
      reportFailure(actionFailure(label, err));
      throw shownFailure(err);
    }
  }

  return {
    /**
     * Runs a schedule now and reads the client again once the bridge accepts
     * it. A failure to start is reported once, unless the person has moved
     * on, then rethrown.
     */
    runSchedule: (request: ScheduleRunRequest) =>
      run("run the schedule", () => api.runSchedule(request)),
    /**
     * Runs a task now and reads the client again once the bridge accepts it.
     * A failure to start is reported once, unless the person has moved on,
     * then rethrown.
     */
    runTask: (request: TaskRunRequest) =>
      run("run the task", () => api.runTask(request)),
  };
}
